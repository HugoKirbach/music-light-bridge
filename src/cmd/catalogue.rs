//! Catalogue de cas musicaux : des morceaux de synthèse décrits comme un DJ les décrirait (motif
//! du kick sur seize doubles croches, charleston, basse, mélodie, filtre, sections), dont on
//! connaît la vérité à la milliseconde. Chacun passe dans la même chaîne que le live, et la
//! lumière est jugée : sur les kicks à l'heure en groove, presque rien en break, la montée vue.

use std::f32::consts::TAU;
use std::path::PathBuf;

use super::analyze::{self, Options, Timeline};
use crate::audio::analysis::DEFAULT_THRESHOLD;
use crate::audio::synth::{Noise, RATE, bass_note, click, hat_envelope, kick};
use crate::look::Phase;
use crate::tempo::DEFAULT_BPM_RANGE;

/// Avance des lumières (latence Hue) ; une lumière à ±40 ms d'un kick est à l'heure.
const ADVANCE: f64 = 0.045;
const ON_TIME: f64 = 0.04;

/// Ce qu'est une section, musicalement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Groove,
    Break,
    BuildUp,
}

/// Son du kick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KickSound {
    /// Kick propre : sinus glissant et clic.
    Clean,
    /// Kick saturé à longue traîne (hard techno, schranz).
    Hard,
}

/// Son de la basse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BassSound {
    /// Note brève à 70 Hz (basse roulante).
    Roll,
    /// « Donk » de hard house : note pincée vers 100 Hz, presque aussi forte que le kick.
    Donk,
}

/// Une section : `bars` mesures, et sur seize doubles croches (`x` : un coup) le kick, la basse,
/// le charleston, la caisse claire et l'arpège ; une mélodie qui enfle, une nappe tenue, un souffle
/// qui monte, un filtre passe-haut sur le grave, une coupure du grave toutes les `kills` mesures,
/// et une batterie (kick, basse, caisse claire) qui se tait sur les deux derniers temps d'une mesure sur
/// `drops`.
#[derive(Debug, Clone, Copy)]
struct Section {
    part: Part,
    bars: usize,
    kicks: &'static str,
    kick_sound: KickSound,
    bass: &'static str,
    bass_sound: BassSound,
    /// Sous-basse continue qui pompe sous les kicks (« rumble »).
    rumble: bool,
    hats: &'static str,
    snares: &'static str,
    arp: &'static str,
    melody: bool,
    pad: bool,
    riser: bool,
    filtered: bool,
    kills: usize,
    drops: usize,
}

const NONE: &str = "................";

impl Section {
    fn groove(bars: usize, kicks: &'static str) -> Self {
        Self {
            part: Part::Groove,
            bars,
            kicks,
            kick_sound: KickSound::Clean,
            bass: NONE,
            bass_sound: BassSound::Roll,
            rumble: false,
            hats: "..x...x...x...x.",
            snares: NONE,
            arp: NONE,
            melody: false,
            pad: false,
            riser: false,
            filtered: false,
            kills: 0,
            drops: 0,
        }
    }

    fn quiet(bars: usize) -> Self {
        Self { part: Part::Break, kicks: NONE, hats: NONE, melody: true, ..Self::groove(bars, NONE) }
    }
}

struct Case {
    bpm: f32,
    sections: Vec<Section>,
}

/// Morceau de synthèse et vérité : instants des kicks, et (début, fin, nature) de chaque section.
struct Track {
    samples: Vec<f32>,
    kicks: Vec<f64>,
    parts: Vec<(f64, f64, Part)>,
}

fn steps(pattern: &str) -> Vec<usize> {
    pattern.chars().enumerate().filter(|&(_, c)| c == 'x').map(|(i, _)| i).collect()
}

/// Ajoute `sound(t)` à partir de `start` (s), sur `length` (s).
fn add(samples: &mut [f32], start: f64, length: f32, gain: f32, sound: impl Fn(f32) -> f32) {
    let first = (start * f64::from(RATE)) as usize;
    let count = (length * RATE) as usize;
    for (i, sample) in samples.iter_mut().skip(first).take(count).enumerate() {
        *sample += gain * sound(i as f32 / RATE);
    }
}

/// Passe-haut du premier ordre (filtre DJ qui retire le grave), en place.
fn high_pass(samples: &mut [f32], cutoff: f32) {
    let a = 1.0 / (1.0 + std::f32::consts::TAU * cutoff / RATE);
    let (mut previous_in, mut previous_out) = (0.0, 0.0);
    for sample in samples {
        let out = a * (previous_out + *sample - previous_in);
        (previous_in, previous_out) = (*sample, out);
        *sample = out;
    }
}

/// Kick saturé de hard techno : le kick propre, une longue traîne de sous-basse, et une saturation
/// qui écrase le tout.
fn hard_kick(t: f32) -> f32 {
    if !(0.0..1.0).contains(&t) {
        return 0.0;
    }
    // La queue suit la hauteur du corps (50 Hz après la chute) : un seul son qui s'éteint, sans battement.
    let sweep = 0.03;
    let tail = (-t / 0.35).exp() * (TAU * (50.0 * t + 100.0 * sweep * (1.0 - (-t / sweep).exp()))).sin();
    0.9 * (4.0 * (kick(t) + 0.4 * tail)).tanh() + click(t)
}

/// « Donk » de hard house : note pincée qui tombe de 140 à 100 Hz.
fn donk(t: f32) -> f32 {
    if !(0.0..0.25).contains(&t) {
        return 0.0;
    }
    let phase = TAU * (100.0 * t + 40.0 * 0.015 * (1.0 - (-t / 0.015).exp()));
    0.7 * (-t / 0.09).exp() * phase.sin()
}

/// Caisse claire : un corps à 190 Hz et un souffle bref.
fn snare(t: f32, hiss: f32) -> f32 {
    if !(0.0..0.2).contains(&t) {
        return 0.0;
    }
    0.25 * (-t / 0.04).exp() * (TAU * 190.0 * t).sin() + 0.35 * (-t / 0.07).exp() * hiss
}

/// Accord d'une nappe supersaw : trois notes, cinq dents de scie désaccordées chacune.
fn supersaw(t: f32) -> f32 {
    let saw = |f: f32| 2.0 * (f * t).fract() - 1.0;
    [440.0f32, 554.37, 659.25]
        .iter()
        .flat_map(|&f| [-0.006f32, -0.003, 0.0, 0.003, 0.006].map(move |d| f * (1.0 + d)))
        .map(saw)
        .sum::<f32>()
        / 15.0
}

/// Note d'arpège : pincée, brève.
fn pluck(t: f32, frequency: f32) -> f32 {
    if !(0.0..0.15).contains(&t) {
        return 0.0;
    }
    (-t / 0.05).exp() * (TAU * frequency * t).sin()
}

/// Souffle filtré dans les aigus, `seconds` s de bruit.
fn hiss(noise: &mut Noise, seconds: f32, cutoff: f32) -> Vec<f32> {
    let mut hiss: Vec<f32> = (0..(seconds * RATE) as usize).map(|_| noise.next()).collect();
    high_pass(&mut hiss, cutoff);
    high_pass(&mut hiss, cutoff);
    hiss
}

/// Début (s) et durée de la montée de souffle dont fait partie `section` (qui commence à `start`) :
/// les sections à souffle qui se suivent n'en font qu'une.
fn riser_span(case: &Case, start: f64, bar: f64, sections: &[Section], section: &Section) -> (f64, f32) {
    let index = sections.iter().position(|s| std::ptr::eq(s, section)).unwrap_or(0);
    let first = (0..=index).rev().take_while(|&i| sections[i].riser).last().unwrap_or(index);
    let last = (index..sections.len()).take_while(|&i| sections[i].riser).last().unwrap_or(index);
    let before: f64 = sections[first..index].iter().map(|s| s.bars as f64 * bar).sum();
    let length: f64 = sections[first..=last].iter().map(|s| s.bars as f64 * bar).sum();
    let _ = case;
    (start - before, length as f32)
}

fn render(case: &Case) -> Track {
    let step = 60.0 / f64::from(case.bpm) / 4.0;
    let bar = 16.0 * step;
    let lead_in = 0.5;
    let total = lead_in + case.sections.iter().map(|s| s.bars as f64 * bar).sum::<f64>() + 1.0;
    let mut samples = vec![0.0; (total * f64::from(RATE)) as usize];
    let mut noise = Noise(0x2545_f491);
    let (mut kicks, mut parts) = (Vec::new(), Vec::new());
    let mut start = lead_in;
    for section in &case.sections {
        let end = start + section.bars as f64 * bar;
        parts.push((start, end, section.part));
        // Le grave (kick, basse, rumble) à part, pour le filtrer ou le couper.
        let mut low = vec![0.0; samples.len()];
        for n in 0..section.bars {
            let bar_start = start + n as f64 * bar;
            let at = |i: usize| bar_start + i as f64 * step;
            // Mesure dont le kick s'arrête à mi-chemin (« kick drop » de hard trance, avant la phrase).
            let end = if section.drops > 0 && n % section.drops == section.drops - 1 { 8 } else { 16 };
            let kick_steps: Vec<usize> = steps(section.kicks).into_iter().filter(|&i| i < end).collect();
            for &i in &kick_steps {
                match section.kick_sound {
                    KickSound::Clean => add(&mut low, at(i), 1.0, 1.0, |t| kick(t) + click(t)),
                    KickSound::Hard => add(&mut low, at(i), 1.0, 1.0, hard_kick),
                }
                kicks.push(at(i));
            }
            if section.rumble {
                // Sous-basse continue, étouffée sous chaque kick puis qui revient (sidechain).
                for (k, &i) in kick_steps.iter().enumerate() {
                    let next = kick_steps.get(k + 1).copied().unwrap_or(end);
                    let (from, length) = (at(i) as f32, ((next - i) as f64 * step) as f32);
                    // Relâche : une sous-basse coupée net claquerait comme un kick. Avant un trou, elle
                    // s'éteint avec la queue du kick (50 ms).
                    let fade = if next == end && end < 16 { 0.05 } else { 0.01 };
                    let release = |t: f32| ((length - t) / fade).min(1.0);
                    add(&mut low, at(i), length, 0.35, |t| (1.0 - (-t / 0.08).exp()) * release(t) * (TAU * 45.0 * (from + t)).sin());
                }
            }
            for i in steps(section.bass).into_iter().filter(|&i| i < end) {
                match section.bass_sound {
                    BassSound::Roll => add(&mut low, at(i), 0.1, 0.8, bass_note),
                    BassSound::Donk => add(&mut low, at(i), 0.25, 1.0, donk),
                }
            }
            for i in steps(section.hats) {
                let hiss = hiss(&mut noise, 0.3, 4000.0);
                add(&mut samples, at(i), 0.3, 1.0, |t| hiss.get((t * RATE) as usize).copied().unwrap_or(0.0) * hat_envelope(t));
            }
            for i in steps(section.snares).into_iter().filter(|&i| i < end) {
                let hiss = hiss(&mut noise, 0.2, 1500.0);
                // En montée, le roulement de caisse claire enfle jusqu'au drop.
                let gain = if section.riser {
                    let (rise_start, rise_length) = riser_span(case, start, bar, &case.sections, section);
                    0.3 + 0.7 * ((at(i) - rise_start) as f32 / rise_length)
                } else {
                    1.0
                };
                add(&mut samples, at(i), 0.2, gain, |t| snare(t, hiss.get((t * RATE) as usize).copied().unwrap_or(0.0)));
            }
            for (k, i) in steps(section.arp).into_iter().enumerate() {
                let frequency = [659.25, 880.0, 1108.73, 880.0][k % 4];
                add(&mut samples, at(i), 0.15, 0.15, |t| pluck(t, frequency));
            }
            if section.melody {
                // Accord tenu, qui enfle et retombe sur la mesure, sans recommencer ses ondes (pas de clic).
                let (length, offset) = (bar as f32, bar_start as f32);
                add(&mut samples, bar_start, length, 0.12, |t| {
                    let swell = (std::f32::consts::PI * t / length).sin();
                    let chord = [523.25, 659.25, 783.99].iter().map(|f| (TAU * f * (offset + t)).sin());
                    swell * chord.sum::<f32>()
                });
            }
        }
        let length = (end - start) as f32;
        if section.pad {
            // Nappe tenue tout du long, qui s'installe en une demi-seconde.
            let offset = start as f32;
            add(&mut samples, start, length, 0.25, |t| (t / 0.5).min(1.0) * supersaw(offset + t));
        }
        if section.riser {
            // Souffle qui monte sans relâche jusqu'au drop, d'une section de montée à l'autre.
            let (rise_start, rise_length) = riser_span(case, start, bar, &case.sections, section);
            let hiss = hiss(&mut noise, length, 2000.0);
            let offset = (start - rise_start) as f32;
            add(&mut samples, start, length, 0.6, |t| {
                ((offset + t) / rise_length).powf(1.5) * hiss.get((t * RATE) as usize).copied().unwrap_or(0.0)
            });
        }
        if section.filtered {
            high_pass(&mut low, 400.0);
            high_pass(&mut low, 400.0);
        }
        if section.kills > 0 {
            // Coupure du grave à la table, une mesure sur `kills` : le kick ne garde que son attaque.
            for n in (section.kills - 1..section.bars).step_by(section.kills) {
                let from = ((start + n as f64 * bar) * f64::from(RATE)) as usize;
                let to = (((start + (n + 1) as f64 * bar) * f64::from(RATE)) as usize).min(low.len());
                high_pass(&mut low[from..to], 300.0);
                high_pass(&mut low[from..to], 300.0);
            }
        }
        for (sample, low) in samples.iter_mut().zip(low) {
            *sample += low;
        }
        start = end;
    }
    Track { samples, kicks, parts }
}

/// Ce que la lumière a fait dans une section.
#[derive(Debug, Default)]
struct Verdict {
    kicks: usize,
    on_time: usize,
    /// Lumières qui ne répondent à aucun kick.
    extra: usize,
    flashes_per_second: f64,
    /// Part du temps reconnue comme chaque phase.
    groove: f64,
    calm: f64,
    build_up: f64,
}

impl Verdict {
    fn on_time_share(&self) -> f64 {
        self.on_time as f64 / self.kicks.max(1) as f64
    }
}

/// Juge la lumière de `from` à `to` (s).
fn judge(track: &Track, timeline: &Timeline, from: f64, to: f64) -> Verdict {
    let lights: Vec<f64> = timeline.flashes.iter().map(|t| t + ADVANCE).filter(|t| (from..to).contains(t)).collect();
    let kicks: Vec<f64> = track.kicks.iter().copied().filter(|t| (from..to).contains(t)).collect();
    let near = |t: f64, others: &[f64]| others.iter().any(|&o| (t - o).abs() <= ON_TIME);
    let frames: Vec<Phase> = timeline.phases.iter().filter(|(t, _)| (from..to).contains(t)).map(|&(_, p)| p).collect();
    let share = |phase: Phase| frames.iter().filter(|&&p| p == phase).count() as f64 / frames.len().max(1) as f64;
    Verdict {
        kicks: kicks.len(),
        on_time: kicks.iter().filter(|&&k| near(k, &lights)).count(),
        extra: lights.iter().filter(|&&l| !near(l, &kicks)).count(),
        flashes_per_second: lights.len() as f64 / (to - from),
        groove: share(Phase::Groove),
        calm: share(Phase::Calm),
        build_up: share(Phase::BuildUp),
    }
}

/// Joue le cas et juge chaque section, passées les `settle` premières mesures de chacune.
fn play(name: &str, case: &Case, settle: usize) -> Vec<(Part, Verdict)> {
    let track = render(case);
    let options = Options { path: PathBuf::new(), threshold: DEFAULT_THRESHOLD, bpm: None, bpm_range: DEFAULT_BPM_RANGE, slice: 4.0 };
    let replay = analyze::replay(RATE as u32, &track.samples, ADVANCE, &options);
    let bar = 240.0 / f64::from(case.bpm);
    let verdicts: Vec<(Part, Verdict)> = track
        .parts
        .iter()
        .map(|&(from, to, part)| (part, judge(&track, &replay.timeline, (from + settle as f64 * bar).min(to), to)))
        .collect();
    println!("{name} :");
    for (part, verdict) in &verdicts {
        println!(
            "  {part:?} : {}/{} kicks à l'heure, {} en trop, {:.1} flash/s ; groove {:.0} %, break {:.0} %, montée {:.0} %",
            verdict.on_time,
            verdict.kicks,
            verdict.extra,
            verdict.flashes_per_second,
            100.0 * verdict.groove,
            100.0 * verdict.calm,
            100.0 * verdict.build_up
        );
    }
    verdicts
}

/// Un groove : la plupart des kicks à l'heure (donc anticipés), peu de lumières en trop.
fn assert_groove(verdict: &Verdict, on_time: f64) {
    assert!(verdict.on_time_share() >= on_time, "{verdict:?}");
    assert!(verdict.extra as f64 <= 0.15 * verdict.kicks as f64, "{verdict:?}");
}

/// Un break qui flotte : presque pas de flash, et reconnu comme break.
fn assert_floating_break(verdict: &Verdict) {
    assert!(verdict.flashes_per_second <= 0.5, "{verdict:?}");
    assert!(verdict.calm >= 0.8, "{verdict:?}");
}

#[test]
fn techno_four_on_the_floor_with_offbeat_hats_and_rolling_bass() {
    let groove = Section { bass: ".xxx.xxx.xxx.xxx", ..Section::groove(16, "x...x...x...x...") };
    let verdicts = play("Techno 4/4", &Case { bpm: 135.0, sections: vec![groove] }, 4);
    assert_groove(&verdicts[0].1, 0.9);
}

/// Limite connue : deux familles de kicks d'égale force, à un quart de temps l'une de l'autre (sur
/// les doubles croches 0 et 5), font hésiter le peigne qui place les temps. Il recale la grille de
/// l'une à l'autre toutes les cinq secondes, et le motif n'a jamais le temps de s'apprendre. Les
/// réglages du peigne qui l'en empêchent dégradent les mix 4/4 chargés du banc d'essai.
#[test]
#[ignore = "limite connue : la grille hésite entre deux familles de kicks d'égale force"]
fn hard_groove_with_a_syncopated_kick() {
    let groove = Section { hats: "xxxxxxxxxxxxxxxx", ..Section::groove(20, "x....x..x....x..") };
    let verdicts = play("Hard groove", &Case { bpm: 148.0, sections: vec![groove] }, 8);
    assert_groove(&verdicts[0].1, 0.8);
}

#[test]
fn breakbeat_kick() {
    let verdicts = play("Breakbeat", &Case { bpm: 130.0, sections: vec![Section::groove(20, "x.....x...x.....")] }, 8);
    assert_groove(&verdicts[0].1, 0.8);
}

#[test]
fn half_time_kick() {
    let verdicts = play("Demi-tempo", &Case { bpm: 140.0, sections: vec![Section::groove(20, "x.......x.......")] }, 10);
    assert_groove(&verdicts[0].1, 0.8);
}

#[test]
fn a_break_with_a_lone_melody_floats() {
    let case = Case { bpm: 140.0, sections: vec![Section::groove(12, "x...x...x...x..."), Section::quiet(8), Section::groove(8, "x...x...x...x...")] };
    let verdicts = play("Break mélodie seule", &case, 1);
    assert_floating_break(&verdicts[1].1);
    assert_groove(&verdicts[2].1, 0.85);
}

#[test]
fn a_break_with_melody_and_hats_floats() {
    let quiet = Section { hats: "x.x.x.x.x.x.x.x.", ..Section::quiet(8) };
    let case = Case { bpm: 140.0, sections: vec![Section::groove(12, "x...x...x...x..."), quiet, Section::groove(8, "x...x...x...x...")] };
    let verdicts = play("Break mélodie et charleston", &case, 1);
    assert_floating_break(&verdicts[1].1);
    assert!(verdicts[1].1.build_up <= 0.1, "{:?}", verdicts[1].1);
}

#[test]
fn a_kick_roll_builds_up_then_drops() {
    let roll = |bars, kicks| Section { part: Part::BuildUp, kicks, ..Section::quiet(bars) };
    let case = Case {
        bpm: 140.0,
        sections: vec![
            Section::groove(12, "x...x...x...x..."),
            roll(2, "x.x.x.x.x.x.x.x."),
            roll(2, "xxxxxxxxxxxxxxxx"),
            Section::groove(8, "x...x...x...x..."),
        ],
    };
    let verdicts = play("Montée en roulement", &case, 0);
    assert!(verdicts[2].1.build_up >= 0.5, "{:?}", verdicts[2].1);
    let after_drop = &verdicts[3].1;
    assert!(after_drop.groove >= 0.7, "{after_drop:?}");
}

#[test]
fn a_filtered_intro_then_the_drop() {
    let filtered = Section { filtered: true, ..Section::groove(12, "x...x...x...x...") };
    let case = Case { bpm: 140.0, sections: vec![filtered, Section::quiet(4), Section::groove(12, "x...x...x...x...")] };
    let verdicts = play("Intro filtrée, break, drop", &case, 2);
    assert_floating_break(&verdicts[1].1);
    assert_groove(&verdicts[2].1, 0.85);
}

/// Hard techno, 150 BPM : kick saturé à longue traîne, rumble de sous-basse qui pompe entre les
/// kicks, charleston entre les temps.
#[test]
fn hard_techno_with_a_distorted_kick_and_rumble() {
    let groove = Section { kick_sound: KickSound::Hard, rumble: true, ..Section::groove(16, "x...x...x...x...") };
    let verdicts = play("Hard techno", &Case { bpm: 150.0, sections: vec![groove] }, 4);
    assert_groove(&verdicts[0].1, 0.9);
}

/// Schranz, 156 BPM : kick saturé, percussions en doubles croches, et le DJ coupe le grave à la
/// table une mesure sur quatre. Le kick n'y garde que son attaque : ce n'est pas un break.
#[test]
fn schranz_with_bass_kills_stays_in_the_groove() {
    let groove = Section {
        kick_sound: KickSound::Hard,
        hats: "xxxxxxxxxxxxxxxx",
        snares: "....x.......x...",
        kills: 4,
        ..Section::groove(16, "x...x...x...x...")
    };
    let verdicts = play("Schranz, coupures du grave", &Case { bpm: 156.0, sections: vec![groove] }, 4);
    let groove = &verdicts[0].1;
    assert!(groove.groove >= 0.9, "{groove:?}");
    assert_groove(groove, 0.85);
}

/// Hard house, 148 BPM : kick sur chaque temps et « donk » sur chaque contretemps, presque aussi
/// fort que le kick. La lumière tape sur les kicks, pas sur les donks.
#[test]
fn hard_house_offbeat_donk_is_not_a_kick() {
    let groove = Section { bass: "..x...x...x...x.", bass_sound: BassSound::Donk, ..Section::groove(16, "x...x...x...x...") };
    let verdicts = play("Hard house", &Case { bpm: 148.0, sections: vec![groove] }, 4);
    assert_groove(&verdicts[0].1, 0.9);
}

/// Hard trance d'aujourd'hui (Odymek, Fenrick, L.Zwo, 2Hot2Play, Kichta), 160 BPM : kick saturé et
/// rumble comme en hard techno, charleston en doubles croches ; un break court (nappe supersaw et
/// arpège), une montée (roulement de caisse claire et souffle qui monte), puis le drop.
#[test]
fn hard_trance_break_floats_build_up_rises_then_drops() {
    let groove = Section {
        kick_sound: KickSound::Hard,
        rumble: true,
        hats: "..x...x...x...x.",
        snares: "....x.......x...",
        ..Section::groove(12, "x...x...x...x...")
    };
    let breakdown = Section { pad: true, arp: "xxxxxxxxxxxxxxxx", melody: false, ..Section::quiet(8) };
    let build = |bars, snares| Section { part: Part::BuildUp, snares, pad: true, riser: true, melody: false, ..Section::quiet(bars) };
    let case = Case {
        bpm: 160.0,
        sections: vec![groove, breakdown, build(4, "x.x.x.x.x.x.x.x."), build(4, "xxxxxxxxxxxxxxxx"), Section { bars: 8, ..groove }],
    };
    let verdicts = play("Hard trance", &case, 1);
    assert_floating_break(&verdicts[1].1);
    // Le roulement en croches reste calme ; en doubles croches, c'est la montée.
    assert!(verdicts[3].1.build_up >= 0.9, "{:?}", verdicts[3].1);
    assert!(verdicts[4].1.groove >= 0.7, "{:?}", verdicts[4].1);
}

/// Hard trance, 158 BPM : le kick et son rumble se taisent sur les deux derniers temps d'une mesure
/// sur quatre, sous l'arpège qui continue. Le groove tient : rien ne flashe dans le trou, et les
/// kicks qui reviennent sur la grille sont de nouveau anticipés.
#[test]
fn hard_trance_kick_drops_keep_the_groove() {
    let groove = Section {
        kick_sound: KickSound::Hard,
        rumble: true,
        hats: "..x...x...x...x.",
        snares: "....x.......x...",
        arp: "xxxxxxxxxxxxxxxx",
        drops: 4,
        ..Section::groove(20, "x...x...x...x...")
    };
    let verdicts = play("Hard trance, kicks coupés toutes les 4 mesures", &Case { bpm: 158.0, sections: vec![groove] }, 4);
    let groove = &verdicts[0].1;
    assert!(groove.groove >= 0.9, "{groove:?}");
    assert_groove(groove, 0.85);
}

//! Catalogue de cas musicaux : des morceaux de synthèse décrits comme un DJ les décrirait (motif
//! du kick sur seize doubles croches, charleston, basse, mélodie, filtre, sections), dont on
//! connaît la vérité à la milliseconde. Chacun passe dans la même chaîne que le live, et la
//! lumière est jugée : sur les kicks à l'heure en groove, presque rien en break, la montée vue.

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

/// Une section : `bars` mesures, et sur seize doubles croches (`x` : un coup) le kick, la basse
/// et le charleston ; une mélodie tenue, un filtre passe-haut sur le grave.
#[derive(Debug, Clone, Copy)]
struct Section {
    part: Part,
    bars: usize,
    kicks: &'static str,
    bass: &'static str,
    hats: &'static str,
    melody: bool,
    filtered: bool,
}

const NONE: &str = "................";

impl Section {
    fn groove(bars: usize, kicks: &'static str) -> Self {
        Self { part: Part::Groove, bars, kicks, bass: NONE, hats: "..x...x...x...x.", melody: false, filtered: false }
    }

    fn quiet(bars: usize) -> Self {
        Self { part: Part::Break, bars, kicks: NONE, bass: NONE, hats: NONE, melody: true, filtered: false }
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
        // Le grave (kick et basse) à part, pour le filtrer.
        let mut low = vec![0.0; samples.len()];
        for n in 0..section.bars {
            let bar_start = start + n as f64 * bar;
            for i in steps(section.kicks) {
                let at = bar_start + i as f64 * step;
                add(&mut low, at, 1.0, 1.0, |t| kick(t) + click(t));
                kicks.push(at);
            }
            for i in steps(section.bass) {
                add(&mut low, bar_start + i as f64 * step, 0.1, 0.8, bass_note);
            }
            for i in steps(section.hats) {
                let hiss: Vec<f32> = (0..(0.3 * RATE) as usize).map(|_| noise.next()).collect();
                let mut hiss = hiss;
                high_pass(&mut hiss, 4000.0);
                high_pass(&mut hiss, 4000.0);
                add(&mut samples, bar_start + i as f64 * step, 0.3, 1.0, |t| {
                    hiss.get((t * RATE) as usize).copied().unwrap_or(0.0) * hat_envelope(t)
                });
            }
            if section.melody {
                // Accord tenu, qui enfle et retombe sur la mesure, sans recommencer ses ondes (pas de clic).
                let (length, offset) = (bar as f32, bar_start as f32);
                add(&mut samples, bar_start, length, 0.12, |t| {
                    let swell = (std::f32::consts::PI * t / length).sin();
                    let chord = [523.25, 659.25, 783.99].iter().map(|f| (std::f32::consts::TAU * f * (offset + t)).sin());
                    swell * chord.sum::<f32>()
                });
            }
        }
        if section.filtered {
            high_pass(&mut low, 400.0);
            high_pass(&mut low, 400.0);
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
    let roll = |bars, kicks| Section { part: Part::BuildUp, bars, kicks, bass: NONE, hats: NONE, melody: true, filtered: false };
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

//! Banc d'essai : rejoue les captures annotées dans la chaîne du mode live et note, temps par
//! temps, ce que la lumière a fait au regard de la musique. Les annotations (passages de groove
//! avec leur tempo et un kick repère, breaks) vivent dans `bench.json`, à côté des captures.
//!
//! Les vrais temps viennent d'un oracle hors ligne : connaissant tout le passage d'avance, il
//! cherche le tempo et la phase qui font tomber le plus de force d'attaque sur la grille, près du
//! tempo et du kick annotés. Il ne dépend pas des décisions prises en direct par le moteur.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::analysis::{Biquad, DEFAULT_THRESHOLD, Envelope};
use crate::cmd::analyze::{self, Timeline};
use crate::config::Config;
use crate::look::Phase;
use crate::tempo::DEFAULT_BPM_RANGE;
use crate::wav;

const ANNOTATIONS: &str = "bench.json";
const LAST_RESULTS: &str = "bench-dernier.json";
/// Une lumière allumée à moins de 40 ms d'un temps est à l'heure ; jusqu'à 120 ms après, en retard.
const ON_TIME: f64 = 0.04;
const LATE: f64 = 0.12;
/// Le tempo de l'horloge est juste à 1 % près.
const TEMPO_TOLERANCE: f64 = 0.01;
/// Oracle : tempo cherché à ±1,5 BPM de l'annotation, phase à ±20 ms du kick repère.
const ORACLE_BPM_SPAN: f64 = 1.5;
const ORACLE_BPM_STEP: f64 = 0.02;
const ORACLE_KICK_SPAN: f64 = 0.02;
const ORACLE_PHASE_STEP: f64 = 0.005;
/// Trames de force d'attaque (s) ; le bond d'un kick finit dans les 20 ms qui suivent l'attaque.
const FRAME: f64 = 0.01;
const ATTACK_REACH: f64 = 0.02;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Bench {
    captures: Vec<Annotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Annotation {
    /// Nom du fichier WAV, dans le dossier des captures.
    file: String,
    #[serde(default)]
    note: String,
    /// Passages où un kick tombe sur chaque temps.
    #[serde(default)]
    groove: Vec<Section>,
    /// Passages sans kick, en secondes : la lumière ne doit pas y flasher.
    #[serde(default)]
    breaks: Vec<(f64, f64)>,
    /// Têtes de phrase connues (le premier temps d'une phrase de quatre mesures, donc aussi
    /// d'une mesure), en secondes : elles donnent la structure attendue.
    #[serde(default)]
    phrases: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Section {
    from: f64,
    to: f64,
    /// Tempo approximatif.
    bpm: f64,
    /// Instant d'un kick du passage, s'il est connu : il fixe la phase.
    #[serde(default)]
    kick: Option<f64>,
}

/// Bilan d'une capture.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct Score {
    beats: u32,
    on_time: u32,
    late: u32,
    /// Lumières qui ne répondent à aucun temps, dans les passages de groove.
    extra: u32,
    tempo_checks: u32,
    tempo_right: u32,
    groove_frames: u32,
    /// Images de groove où la lumière se croit en break (couleur des temps creux).
    groove_as_break: u32,
    break_seconds: f64,
    break_flashes: u32,
    /// Relevés de la place dans la structure (une par seconde, en groove, avec tête de phrase
    /// connue), têtes de mesure et de phrase justes.
    #[serde(default)]
    structure_checks: u32,
    #[serde(default)]
    bar_right: u32,
    #[serde(default)]
    phrase_right: u32,
}

impl Score {
    fn add(&mut self, other: &Score) {
        self.beats += other.beats;
        self.on_time += other.on_time;
        self.late += other.late;
        self.extra += other.extra;
        self.tempo_checks += other.tempo_checks;
        self.tempo_right += other.tempo_right;
        self.groove_frames += other.groove_frames;
        self.groove_as_break += other.groove_as_break;
        self.break_seconds += other.break_seconds;
        self.break_flashes += other.break_flashes;
        self.structure_checks += other.structure_checks;
        self.bar_right += other.bar_right;
        self.phrase_right += other.phrase_right;
    }

    fn share(part: u32, whole: u32) -> f64 {
        100.0 * f64::from(part) / f64::from(whole.max(1))
    }

    /// Note sur 100 : temps à l'heure, la moitié pour ceux en retard, moins la moitié des
    /// lumières en trop et un point par flash parasite en break.
    pub fn grade(&self) -> f64 {
        let beats = f64::from(self.beats.max(1));
        let timing = (f64::from(self.on_time) + 0.5 * f64::from(self.late) - 0.5 * f64::from(self.extra)) / beats;
        (100.0 * timing - f64::from(self.break_flashes)).max(0.0)
    }
}

/// Bilan d'une capture et détail de chaque passage.
type Evaluation = (Score, Vec<String>);

/// Grille de référence d'un passage.
#[derive(Debug, Clone, Copy)]
struct Reference {
    period: f64,
    anchor: f64,
}

impl Reference {
    fn beats(&self, from: f64, to: f64) -> impl Iterator<Item = f64> + '_ {
        let first = ((from - self.anchor) / self.period).ceil() as i64;
        let last = ((to - self.anchor) / self.period).floor() as i64;
        (first..=last).map(move |n| self.anchor + n as f64 * self.period)
    }
}

pub fn run(cfg: &Config, detail: bool, lights: bool) -> Result<()> {
    let dir = Config::captures_dir()?;
    let path = dir.join(ANNOTATIONS);
    let bench: Bench = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("annotations illisibles : {}", path.display()))?,
        Err(_) => Bench::default(),
    };
    let advance = cfg.light_advance_ms.max(0) as f64 / 1e3;
    println!("Banc d'essai : {} capture(s) annotée(s) dans {}", bench.captures.len(), dir.display());
    println!();

    let results: Vec<(Annotation, Result<Evaluation>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = bench
            .captures
            .iter()
            .map(|annotation| {
                let file = dir.join(&annotation.file);
                scope.spawn(move || (annotation.clone(), evaluate(&file, annotation, advance, lights)))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("fil du banc d'essai")).collect()
    });

    let last: BTreeMap<String, Score> = fs::read_to_string(dir.join(LAST_RESULTS))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    println!(
        "  {:<24} {:>5}  {:>9}  {:>9}  {:>7}  {:>8}  {:>6}  {:>13}  {:>12}  {:>6}  {:>6}",
        "capture", "note", "à l'heure", "en retard", "manqués", "en trop", "tempo", "groove→break", "break flashs", "mesure", "phrase"
    );
    let mut total = Score::default();
    let mut current = BTreeMap::new();
    for (annotation, result) in &results {
        let name = short_name(annotation);
        match result {
            Ok((score, details)) => {
                let delta = last.get(&annotation.file).map(|previous| score.grade() - previous.grade());
                print_row(&name, score, delta);
                if detail || lights {
                    for line in details {
                        println!("      {line}");
                    }
                }
                total.add(score);
                current.insert(annotation.file.clone(), *score);
            }
            Err(e) => println!("  {name:<24} ⚠ {e:#}"),
        }
    }
    let previous_total = (!last.is_empty()).then(|| {
        let mut sum = Score::default();
        for score in last.values() {
            sum.add(score);
        }
        sum
    });
    println!();
    print_row("Ensemble", &total, previous_total.map(|previous| total.grade() - previous.grade()));
    fs::write(dir.join(LAST_RESULTS), serde_json::to_string_pretty(&current)?)?;

    let annotated: Vec<&str> = bench.captures.iter().map(|a| a.file.as_str()).collect();
    let mut pending: Vec<PathBuf> = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "wav"))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !annotated.contains(&n)))
        .collect();
    pending.sort();
    if !pending.is_empty() {
        println!();
        println!("À annoter dans {ANNOTATIONS} :");
        for path in pending {
            let info = fs::read_to_string(path.with_extension("json")).unwrap_or_default();
            println!("  {}  {}", path.file_name().unwrap_or_default().to_string_lossy(), info.replace('\n', " "));
        }
    }
    Ok(())
}

fn short_name(annotation: &Annotation) -> String {
    let stem = annotation.file.trim_start_matches("capture-").trim_end_matches(".wav");
    let mut name = format!("{stem} {}", annotation.note);
    name.truncate(name.char_indices().nth(24).map_or(name.len(), |(i, _)| i));
    name
}

fn print_row(name: &str, score: &Score, delta: Option<f64>) {
    let delta = delta.map_or(String::new(), |d| if d.abs() < 0.5 { " (=)".to_owned() } else { format!(" ({d:+.0})") });
    let per_second = if score.break_seconds > 0.0 { f64::from(score.break_flashes) / score.break_seconds } else { 0.0 };
    println!(
        "  {name:<24} {:>4.0}{delta:<6} {:>7.0} %  {:>7.0} %  {:>5.0} %  {:>8.2}  {:>4.0} %  {:>11.0} %  {:>8.1} /s  {:>4} %  {:>4} %",
        score.grade(),
        Score::share(score.on_time, score.beats),
        Score::share(score.late, score.beats),
        Score::share(score.beats - score.on_time - score.late, score.beats),
        f64::from(score.extra) / f64::from(score.beats.max(1)),
        Score::share(score.tempo_right, score.tempo_checks),
        Score::share(score.groove_as_break, score.groove_frames),
        per_second,
        structure(score.bar_right, score.structure_checks),
        structure(score.phrase_right, score.structure_checks),
    );
}

/// Part de têtes justes, ou un tiret sans tête de phrase annotée.
fn structure(right: u32, checks: u32) -> String {
    if checks == 0 { "-".to_owned() } else { format!("{:.0}", Score::share(right, checks)) }
}

/// Rejoue une capture et la note au regard de ses annotations.
fn evaluate(file: &Path, annotation: &Annotation, advance: f64, lights: bool) -> Result<Evaluation> {
    let (rate, samples) = wav::read_mono(file)?;
    let options = analyze::Options {
        path: file.to_path_buf(),
        threshold: DEFAULT_THRESHOLD,
        bpm: None,
        bpm_range: DEFAULT_BPM_RANGE,
        slice: 4.0,
    };
    let replay = analyze::replay(rate, &samples, advance, &options);
    let reference = reference_onsets(rate, &samples);
    Ok(score(&replay.timeline, &reference, annotation, advance, lights))
}

fn score(
    timeline: &Timeline,
    reference: &[(f64, f32)],
    annotation: &Annotation,
    advance: f64,
    lights_detail: bool,
) -> Evaluation {
    let mut score = Score::default();
    let mut details = Vec::new();
    // La lumière s'allume une latence Hue après l'ordre de flash.
    let lights: Vec<f64> = timeline.flashes.iter().map(|t| t + advance).collect();
    for section in &annotation.groove {
        let reference = oracle(reference, section);
        let mut used = vec![false; lights.len()];
        let (mut beats, mut on_time, mut late) = (0, 0, 0);
        for beat in reference.beats(section.from, section.to) {
            beats += 1;
            let candidate = lights
                .iter()
                .enumerate()
                .filter(|&(i, &light)| !used[i] && (-ON_TIME..=LATE).contains(&(light - beat)))
                .min_by(|a, b| (a.1 - beat).abs().total_cmp(&(b.1 - beat).abs()));
            if let Some((i, &light)) = candidate {
                used[i] = true;
                if (light - beat).abs() <= ON_TIME { on_time += 1 } else { late += 1 }
            }
        }
        let inside = |t: f64| (section.from..section.to).contains(&t);
        let extra = lights.iter().zip(&used).filter(|&(&light, &u)| !u && inside(light)).count() as u32;
        // Décalage de chaque lumière au temps le plus proche : un calage à contretemps se voit
        // tout de suite (médiane vers ±demi-période).
        let mut offsets: Vec<f64> = lights
            .iter()
            .filter(|&&light| inside(light))
            .map(|&light| {
                let n = ((light - reference.anchor) / reference.period).round();
                light - (reference.anchor + n * reference.period)
            })
            .collect();
        if lights_detail {
            let list: Vec<String> = lights.iter().filter(|&&l| inside(l)).map(|&l| {
                let n = ((l - reference.anchor) / reference.period).round();
                format!("{l:.2}({:+.0})", (l - reference.anchor - n * reference.period) * 1e3)
            }).collect();
            details.push(list.join(" "));
        }
        offsets.sort_by(f64::total_cmp);
        let median = offsets.get(offsets.len() / 2).copied().unwrap_or(0.0);
        let bpm = 60.0 / reference.period;
        let tempos: Vec<f64> = timeline.tempos.iter().filter(|(t, _)| inside(*t)).filter_map(|(_, bpm)| *bpm).collect();
        let right = tempos.iter().filter(|&&found| (found - bpm).abs() / bpm < TEMPO_TOLERANCE).count() as u32;
        let frames: Vec<Phase> = timeline.phases.iter().filter(|(t, _)| inside(*t)).map(|&(_, p)| p).collect();
        let as_break = frames.iter().filter(|&&p| p == Phase::Calm).count() as u32;
        // Place dans la structure : la tête de phrase connue la plus proche, recalée sur la grille
        // de référence, donne pour chaque relevé le temps attendu dans la mesure et la phrase.
        let head = annotation
            .phrases
            .iter()
            .copied()
            .min_by(|a, b| (a - section.from).abs().total_cmp(&(b - section.from).abs()));
        let (mut structure_checks, mut bar_right, mut phrase_right) = (0, 0, 0);
        let mut wrong = Vec::new();
        if let Some(head) = head {
            for &(t, position) in timeline.positions.iter().filter(|(t, _)| inside(*t)) {
                let n = ((t - head) / reference.period).round() as i64;
                let (bar, phrase) = (n.rem_euclid(4) as u8, n.rem_euclid(16) as u8);
                structure_checks += 1;
                bar_right += u32::from(position.is_some_and(|p| p.beat == bar));
                phrase_right += u32::from(position.is_some_and(|p| p.phrase == phrase));
                if lights_detail {
                    let found = position.map_or("?".to_owned(), |p| format!("{}/{}", p.beat, p.phrase));
                    wrong.push(format!("{t:.0}:{found}≠{bar}/{phrase}"));
                }
            }
        }
        if !wrong.is_empty() {
            details.push(format!("structure (trouvée≠attendue) : {}", wrong.join(" ")));
        }
        details.push(format!(
            "groove {:.1}–{:.1} s : oracle {bpm:.2} BPM, kick à {:.3} s ; {on_time}/{beats} à l'heure, {late} en retard, {extra} en trop, décalage médian {:+.0} ms",
            section.from,
            section.to,
            reference.anchor + ((section.from - reference.anchor) / reference.period).ceil() * reference.period,
            median * 1e3,
        ));
        score.add(&Score {
            beats,
            on_time,
            late,
            extra,
            tempo_checks: tempos.len() as u32,
            tempo_right: right,
            groove_frames: frames.len() as u32,
            groove_as_break: as_break,
            structure_checks,
            bar_right,
            phrase_right,
            ..Score::default()
        });
    }
    for &(from, to) in &annotation.breaks {
        let flashes = lights.iter().filter(|&&t| (from..to).contains(&t)).count() as u32;
        let at: Vec<String> = lights.iter().filter(|&&t| (from..to).contains(&t)).map(|t| format!("{t:.2}")).collect();
        details.push(format!("break {from:.1}–{to:.1} s : {flashes} flash(s) {}", at.join(" ")));
        score.add(&Score { break_seconds: to - from, break_flashes: flashes, ..Score::default() });
    }
    (score, details)
}

/// Force d'attaque de référence, fixe (indépendante des réglages du moteur) : somme des bonds
/// d'énergie de trois bandes sous 250 Hz, par trames de 10 ms.
fn reference_onsets(rate: u32, samples: &[f32]) -> Vec<(f64, f32)> {
    let r = rate as f32;
    let mut bands: Vec<(Biquad, Envelope, f32, f32)> = [60.0, 120.0, 220.0]
        .iter()
        .map(|&center| (Biquad::band_pass(r, center, 1.0), Envelope::new(r, 0.002, 0.03), 0.0, 1e-4))
        .collect();
    let hop = (r * FRAME as f32).round() as usize;
    let mut frames = Vec::with_capacity(samples.len() / hop + 1);
    for (i, &x) in samples.iter().enumerate() {
        for (filter, envelope, peak, _) in &mut bands {
            *peak = peak.max(envelope.process(filter.process(x).abs()));
        }
        if (i + 1) % hop == 0 {
            let mut rise = 0.0;
            for (_, _, peak, previous) in &mut bands {
                let level = peak.max(1e-4);
                rise += (level / *previous).ln().max(0.0);
                (*previous, *peak) = (level, 0.0);
            }
            frames.push((i as f64 / f64::from(rate), rise));
        }
    }
    frames
}

/// Grille qui fait tomber le plus de force d'attaque sur ses temps, près du tempo annoté et,
/// s'il est donné, du kick repère.
fn oracle(onsets: &[(f64, f32)], section: &Section) -> Reference {
    let start = onsets.first().map_or(0.0, |&(t, _)| t);
    let strength_after = |beat: f64| -> f32 {
        // Le bond d'une attaque finit dans la trame qui se termine ~10 ms après elle : poids
        // triangulaire autour de cet instant, pour viser la milliseconde plutôt que la trame.
        let center = beat + ATTACK_REACH / 2.0;
        let first = ((center - FRAME - start) / FRAME).floor().max(0.0) as usize;
        (first..first + 3)
            .filter_map(|i| onsets.get(i))
            .map(|&(t, s)| s * (1.0 - ((t - center).abs() / FRAME) as f32).max(0.0))
            .sum()
    };
    let mut best = (f32::MIN, Reference { period: 60.0 / section.bpm, anchor: section.kick.unwrap_or(section.from) });
    let steps = (2.0 * ORACLE_BPM_SPAN / ORACLE_BPM_STEP).round() as i32;
    for step in 0..=steps {
        let bpm = section.bpm - ORACLE_BPM_SPAN + f64::from(step) * ORACLE_BPM_STEP;
        let period = 60.0 / bpm;
        let anchors: Vec<f64> = match section.kick {
            Some(kick) => {
                let n = (ORACLE_KICK_SPAN / ORACLE_PHASE_STEP).round() as i32;
                (-n..=n).map(|i| kick + f64::from(i) * ORACLE_PHASE_STEP).collect()
            }
            None => {
                let n = (period / ORACLE_PHASE_STEP) as i32;
                (0..n).map(|i| section.from + f64::from(i) * ORACLE_PHASE_STEP).collect()
            }
        };
        for anchor in anchors {
            let reference = Reference { period, anchor };
            let total: f32 = reference.beats(section.from, section.to).map(strength_after).sum();
            if total > best.0 {
                best = (total, reference);
            }
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline(beats: &[f64], flashes: &[f64]) -> Timeline {
        let mut onsets = Vec::new();
        let mut t = 0.0;
        while t < 20.0 {
            let strength = if beats.iter().any(|&b| (t - b - 0.01).abs() < 0.004) { 1.0 } else { 0.0 };
            onsets.push((t, strength));
            t += FRAME;
        }
        Timeline { flashes: flashes.to_vec(), onsets, ..Timeline::default() }
    }

    #[test]
    fn the_oracle_finds_the_grid_and_the_score_sorts_the_lights() {
        let period = 60.0 / 150.0;
        let beats: Vec<f64> = (0..45).map(|n| 0.7 + n as f64 * period).collect();
        let advance = 0.045;
        // Temps 0 à 9 : à l'heure ; 10 à 19 : 80 ms en retard ; 20 à 29 : manqués ; plus deux
        // lumières à contretemps.
        let mut flashes: Vec<f64> = beats[..10].iter().map(|b| b - advance).collect();
        flashes.extend(beats[10..20].iter().map(|b| b + 0.08 - advance));
        flashes.extend([beats[30] + period / 2.0, beats[31] + period / 2.0]);
        flashes.extend(beats[30..].iter().map(|b| b - advance));
        let annotation = Annotation {
            file: String::new(),
            note: String::new(),
            groove: vec![Section { from: 0.5, to: 18.5, bpm: 151.0, kick: None }],
            breaks: vec![],
            phrases: vec![],
        };
        let timeline = timeline(&beats, &flashes);
        let (score, details) = score(&timeline, &timeline.onsets, &annotation, advance, false);
        println!("{details:?}");
        assert_eq!(score.beats, 45);
        assert_eq!((score.on_time, score.late, score.extra), (25, 10, 2));
    }
}

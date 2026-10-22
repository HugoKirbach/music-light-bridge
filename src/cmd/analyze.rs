//! Rejoue un enregistrement (touche `r` du mode live, ou n'importe quel WAV) dans la même
//! chaîne que le mode live (détection guidée, horloge de tempo, flashs) et résume ce qui
//! se passe par tranches de quelques secondes, pour comprendre un passage où les flashs décrochent.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;

use anyhow::Result;

use crate::audio::Grid;
use crate::audio::analysis::{Analyzer, KICK_DETECTION_DELAY};
use crate::config::Config;
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::tempo::{TempoSource, Tracker};
use crate::wav;

/// Délai simulé entre un kick et son arrivée dans la boucle d'éclairage (bloc audio + détection).
const ARRIVAL: f64 = 0.012;
const FRAME: f64 = 1.0 / 60.0;
const TEMPO_UPDATE: f64 = 1.0;
/// Un kick à moins de 15 % de période d'un temps tombe « sur le temps ».
const ON_BEAT: f64 = 0.15;
/// Une tranche où moins de la moitié des temps ont un kick est signalée.
const WEAK_COVERAGE: f64 = 0.5;

pub struct Options {
    pub path: PathBuf,
    pub threshold: f32,
    pub bpm: Option<f64>,
    pub bpm_range: (f64, f64),
    /// Durée de chaque tranche du résumé (s).
    pub slice: f64,
}

/// Ce qui s'est passé pendant une tranche de l'enregistrement.
#[derive(Debug, Default)]
struct Slice {
    start: f64,
    end: f64,
    /// Tempo de l'horloge en fin de tranche.
    bpm: Option<f64>,
    beats_with_kick: BTreeSet<i64>,
    offbeat_kicks: u32,
    groove_samples: u64,
    samples: u64,
    anticipated: u32,
    reactive: u32,
    cancelled: u32,
}

impl Slice {
    /// Nombre de temps de la tranche, si le tempo est connu.
    fn beats(&self) -> Option<usize> {
        self.bpm.map(|bpm| ((self.end - self.start) * bpm / 60.0).round() as usize)
    }

    fn is_weak(&self) -> bool {
        self.beats()
            .is_some_and(|beats| beats >= 4 && (self.beats_with_kick.len() as f64) < WEAK_COVERAGE * beats as f64)
    }
}

struct Replay {
    slices: Vec<Slice>,
    tracker: Tracker,
}

pub fn run(cfg: &Config, options: Options) -> Result<()> {
    let (rate, samples) = wav::read_mono(&options.path)?;
    let advance = cfg.light_advance_ms.max(0) as f64 / 1e3;
    println!(
        "{} : {:.1} s à {rate} Hz, seuil {:.2}, avance des flashs {} ms.",
        options.path.display(),
        samples.len() as f64 / f64::from(rate),
        options.threshold,
        cfg.light_advance_ms
    );
    println!();
    println!("  début   tempo  groove  temps avec kick  kicks hors temps  flashs anticipés/réactifs/annulés");

    let replay = replay(rate, &samples, advance, &options);
    for slice in &replay.slices {
        let coverage = match slice.beats() {
            Some(beats) if beats > 0 => format!("{}/{beats}", slice.beats_with_kick.len()),
            _ => "-".to_owned(),
        };
        println!(
            "  {:>4.0} s  {:>6}  {:>4.0} %  {coverage:^15}  {:^16}  {:>4} / {:>3} / {:>3}{}",
            slice.start,
            slice.bpm.map_or("---.-".to_owned(), |bpm| format!("{bpm:.1}")),
            100.0 * slice.groove_samples as f64 / slice.samples.max(1) as f64,
            slice.offbeat_kicks,
            slice.anticipated,
            slice.reactive,
            slice.cancelled,
            if slice.is_weak() { "  ⚠" } else { "" }
        );
    }

    println!();
    match replay.tracker.clock().bpm() {
        Some(bpm) => {
            let source = match replay.tracker.source() {
                TempoSource::Auto => "automatique",
                TempoSource::Manual => "manuel",
            };
            println!("Tempo final : {bpm:.1} BPM ({source}).");
        }
        None => println!("Aucun tempo trouvé."),
    }
    let weak: Vec<String> =
        replay.slices.iter().filter(|s| s.is_weak()).map(|s| format!("{:.0} s", s.start)).collect();
    if weak.is_empty() {
        println!("Aucune tranche où les kicks décrochent.");
    } else {
        println!("⚠ Moins de la moitié des temps ont un kick à partir de : {}.", weak.join(", "));
    }
    Ok(())
}

/// Fait passer l'enregistrement dans la chaîne du mode live, au fil des échantillons.
fn replay(rate: u32, samples: &[f32], advance: f64, options: &Options) -> Replay {
    let step = 1.0 / f64::from(rate);
    let mut analyzer = Analyzer::new(rate as f32, options.threshold);
    let mut tracker = Tracker::new(options.bpm_range);
    if let Some(bpm) = options.bpm {
        tracker.set_manual(bpm, 0.0);
    }
    let mut planner = FlashPlanner::new(FlashMode::Kicks, advance, ARRIVAL + CONFIRM_MARGIN);
    let mut arrivals: VecDeque<(f64, f64)> = VecDeque::new();
    let mut slices = Vec::new();
    let mut slice = Slice::default();
    let mut next_poll = 0.0;
    let mut next_update = TEMPO_UPDATE;
    for (i, &x) in samples.iter().enumerate() {
        let t = i as f64 * step;
        let clock = tracker.clock();
        let groove = planner.in_groove(&clock, t);
        let grid = Grid { period: clock.period().unwrap_or(0.0), anchor: clock.anchor(), groove };
        slice.samples += 1;
        slice.groove_samples += u64::from(groove);

        if analyzer.process(x, grid.expectation(t)) {
            let kick = t - KICK_DETECTION_DELAY;
            tracker.on_kick(kick);
            arrivals.push_back((t + ARRIVAL, kick));
            let clock = tracker.clock();
            match clock.period().zip(clock.nearest_beat(kick)) {
                Some((period, beat)) if (kick - beat).abs() < ON_BEAT * period => {
                    slice.beats_with_kick.insert((beat / period).round() as i64);
                }
                _ => slice.offbeat_kicks += 1,
            }
        }

        let arrived = arrivals.front().is_some_and(|&(at, _)| at <= t);
        if t >= next_poll || arrived {
            let kick = if arrived { arrivals.pop_front().map(|(_, kick)| kick) } else { None };
            let clock = tracker.clock();
            match planner.poll(&clock, t, kick) {
                Action::Flash { reactive: false } => slice.anticipated += 1,
                Action::Flash { reactive: true } => slice.reactive += 1,
                Action::Cancel => slice.cancelled += 1,
                Action::None => {}
            }
            next_poll = planner.next_event(&clock, t).map_or(t + FRAME, |event| event.min(t + FRAME));
        }
        if t >= next_update {
            tracker.update(t);
            next_update += TEMPO_UPDATE;
        }
        if t >= slice.start + options.slice {
            slice.end = t;
            slice.bpm = tracker.clock().bpm();
            slices.push(std::mem::replace(&mut slice, Slice { start: t, ..Slice::default() }));
        }
    }
    slice.end = samples.len() as f64 * step;
    if slice.end - slice.start > 1.0 {
        slice.bpm = tracker.clock().bpm();
        slices.push(slice);
    }
    Replay { slices, tracker }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::analysis::DEFAULT_THRESHOLD;
    use crate::audio::synth::{RATE, bass_note, kick};
    use crate::tempo::DEFAULT_BPM_RANGE;

    /// Le scénario du set : 20 s de kicks bien détachés au-dessus d'une basse roulante, puis la
    /// basse prend le dessus au point que les kicks ne la dépassent plus que de 40 %.
    #[test]
    fn the_groove_carries_the_kicks_through_a_denser_mix() {
        let beat = 60.0 / 155.0;
        let end = 44.0;
        let samples: Vec<f32> = (0..(45.0 * RATE) as usize)
            .map(|i| {
                let t = i as f32 / RATE;
                let gain = if t < 20.0 { 1.0 } else { 0.4 };
                let current = ((t - 0.5) / beat).floor() as i32;
                ((current - 2).max(0)..=current)
                    .map(|n| 0.5 + beat * n as f32)
                    .filter(|&start| start < end)
                    .map(|start| {
                        let notes: f32 = (1..4).map(|q| 0.5 * bass_note(t - start - beat * q as f32 / 4.0)).sum();
                        gain * kick(t - start) + notes
                    })
                    .sum()
            })
            .collect();
        let options = Options {
            path: PathBuf::new(),
            threshold: DEFAULT_THRESHOLD,
            bpm: None,
            bpm_range: DEFAULT_BPM_RANGE,
            slice: 4.0,
        };
        let replay = replay(RATE as u32, &samples, 0.045, &options);
        for slice in &replay.slices {
            println!(
                "{:>4.0} s : {}/{:?} temps avec kick, {} hors temps, flashs {}/{}/{}",
                slice.start,
                slice.beats_with_kick.len(),
                slice.beats(),
                slice.offbeat_kicks,
                slice.anticipated,
                slice.reactive,
                slice.cancelled
            );
        }
        // Une fois le tempo établi, les kicks restent trouvés, y compris dans le mix dense.
        for slice in replay.slices.iter().filter(|s| s.start >= 8.0 && s.end <= end as f64) {
            let beats = slice.beats().unwrap();
            assert!(slice.beats_with_kick.len() + 1 >= beats, "{slice:?}");
            assert_eq!(slice.offbeat_kicks, 0, "{slice:?}");
        }
    }
}

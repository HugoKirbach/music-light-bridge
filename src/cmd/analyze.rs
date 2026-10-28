//! Rejoue un enregistrement (touche `r` du mode live, ou n'importe quel WAV) dans la même
//! chaîne que le mode live (détection guidée, horloge de tempo, flashs) et résume ce qui
//! se passe par tranches de quelques secondes, pour comprendre un passage où les flashs décrochent.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;

use crate::audio::Grid;
use crate::audio::analysis::{Analyzer, KICK_DETECTION_DELAY};
use crate::config::Config;
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::look::{Palette, Phase, ReactiveLook};
use crate::phrase::Position;
use crate::tempo::Tracker;
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
    /// Images (60 par seconde) passées dans chaque phase : groove, break, montée.
    phase_frames: [u32; 3],
    /// Place du dernier temps de la tranche dans la phrase (de 1 à 16), si elle est reconnue.
    phrase: Option<u8>,
}

impl Slice {
    /// Nombre de temps de la tranche, si le tempo est connu.
    fn beats(&self) -> Option<usize> {
        self.bpm.map(|bpm| ((self.end - self.start) * bpm / 60.0).round() as usize)
    }

    /// Phase la plus présente de la tranche, avec sa part du temps.
    fn phase(&self) -> String {
        let total = self.phase_frames.iter().sum::<u32>().max(1);
        let (index, frames) = (0u8..).zip(self.phase_frames).max_by_key(|&(_, frames)| frames).unwrap_or((0, 0));
        format!("{} {:.0} %", Phase::from_index(index).label(), 100.0 * f64::from(frames) / f64::from(total))
    }

    fn is_weak(&self) -> bool {
        self.beats()
            .is_some_and(|beats| beats >= 4 && (self.beats_with_kick.len() as f64) < WEAK_COVERAGE * beats as f64)
    }
}

/// Ce que la chaîne a fait, instant par instant, pour le banc d'essai.
#[derive(Debug, Default)]
pub struct Timeline {
    /// Ordres de flash (anticipés ou réactifs), et annulations de flash anticipé.
    pub flashes: Vec<f64>,
    pub cancels: Vec<f64>,
    /// Force d'attaque de chaque trame de 10 ms : (fin de trame, valeur).
    pub onsets: Vec<(f64, f32)>,
    /// Phase reconnue, à chaque image (60 par seconde).
    pub phases: Vec<(f64, Phase)>,
    /// Tempo de l'horloge, à chaque réestimation.
    pub tempos: Vec<(f64, Option<f64>)>,
    /// Place reconnue dans la mesure et la phrase, à chaque réestimation.
    pub positions: Vec<(f64, Option<Position>)>,
}

pub struct Replay {
    slices: Vec<Slice>,
    tracker: Tracker,
    pub timeline: Timeline,
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
    println!(
        "  début   tempo  groove  temps avec kick  kicks hors temps  flashs anticipés/réactifs/annulés  phrase  phase"
    );

    let replay = replay(rate, &samples, advance, &options);
    for slice in &replay.slices {
        let coverage = match slice.beats() {
            Some(beats) if beats > 0 => format!("{}/{beats}", slice.beats_with_kick.len()),
            _ => "-".to_owned(),
        };
        println!(
            "  {:>4.0} s  {:>6}  {:>4.0} %  {coverage:^15}  {:^16}  {:>4} / {:>3} / {:>3}           {:>5}   {}{}",
            slice.start,
            slice.bpm.map_or("---.-".to_owned(), |bpm| format!("{bpm:.1}")),
            100.0 * slice.groove_samples as f64 / slice.samples.max(1) as f64,
            slice.offbeat_kicks,
            slice.anticipated,
            slice.reactive,
            slice.cancelled,
            slice.phrase.map_or("-".to_owned(), |p| format!("{}/16", p + 1)),
            slice.phase(),
            if slice.is_weak() { "  ⚠" } else { "" }
        );
    }

    println!();
    match replay.tracker.clock().bpm() {
        Some(bpm) => println!("Tempo final : {bpm:.1} BPM ({}).", replay.tracker.source().label()),
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
pub fn replay(rate: u32, samples: &[f32], advance: f64, options: &Options) -> Replay {
    let step = 1.0 / f64::from(rate);
    let mut analyzer = Analyzer::new(rate as f32, options.threshold);
    let mut tracker = Tracker::new(options.bpm_range);
    if let Some(bpm) = options.bpm {
        tracker.set_manual(bpm, 0.0);
    }
    let mut planner = FlashPlanner::new(FlashMode::Kicks, advance, ARRIVAL + CONFIRM_MARGIN);
    // Kicks détectés : arrivée dans la boucle d'éclairage, instant, et déjà retenu comme kick.
    let mut arrivals: VecDeque<(f64, f64, bool)> = VecDeque::new();
    let mut slices = Vec::new();
    let mut slice = Slice::default();
    let mut timeline = Timeline::default();
    // Phase reconnue (break, montée…), comme la boucle d'éclairage la voit.
    let mut look = ReactiveLook::new(Palette::default());
    look.set_drop_advance(advance as f32);
    let mut next_frame = 0.0;
    let mut next_poll = 0.0;
    let mut next_update = TEMPO_UPDATE;
    for (i, &x) in samples.iter().enumerate() {
        let t = i as f64 * step;
        let rhythm = tracker.rhythm();
        let groove = planner.in_groove(rhythm.as_ref(), t);
        let grid = Grid::of(rhythm.as_ref(), tracker.locked(t), groove);
        slice.samples += 1;
        slice.groove_samples += u64::from(groove);

        if analyzer.process(x, grid.expectation(t)) {
            let kick = t - KICK_DETECTION_DELAY;
            // En groove, tout coup détecté est un kick. Ailleurs, comme dans la boucle d'éclairage, on
            // attend son arrivée pour juger s'il porte du grave.
            let accepted = look.phase() == Phase::Groove;
            if accepted {
                accept_kick(kick, &mut tracker, &mut slice);
            }
            arrivals.push_back((t + ARRIVAL, kick, accepted));
        }

        if let Some(strength) = analyzer.take_onset() {
            tracker.on_onset(t, strength);
            timeline.onsets.push((t, strength.kick));
        }
        let arrived = arrivals.front().is_some_and(|&(at, _, _)| at <= t);
        if t >= next_poll || arrived {
            // La lumière apprend le kick à son arrivée, avec le grave qu'il porte alors.
            let raw_bass = analyzer.levels().raw_bass;
            let kick = match if arrived { arrivals.pop_front() } else { None } {
                Some((_, kick, true)) => Some(kick),
                Some((_, kick, false)) if look.carries_bass(raw_bass) => {
                    accept_kick(kick, &mut tracker, &mut slice);
                    Some(kick)
                }
                _ => None,
            };
            if kick.is_some() {
                look.kick_detected(raw_bass);
            }
            let rhythm = tracker.rhythm();
            match planner.poll(rhythm.as_ref(), t, kick) {
                Action::Flash { reactive } => {
                    look.kick();
                    timeline.flashes.push(t);
                    if reactive { slice.reactive += 1 } else { slice.anticipated += 1 }
                }
                Action::Cancel => {
                    look.cancel_flash();
                    timeline.cancels.push(t);
                    slice.cancelled += 1;
                }
                Action::None => {}
            }
            next_poll = planner.next_event(rhythm.as_ref(), t).map_or(t + FRAME, |event| event.min(t + FRAME));
        }
        if t >= next_frame {
            look.set_grid_groove(tracker.clock().period().map(|_| groove));
            look.set_structure(tracker.structure(t));
            look.render(Duration::from_secs_f64(FRAME), analyzer.levels());
            slice.phase_frames[usize::from(look.phase().index())] += 1;
            timeline.phases.push((t, look.phase()));
            next_frame += FRAME;
        }
        if t >= next_update {
            tracker.update(t);
            timeline.tempos.push((t, tracker.clock().bpm()));
            if let Some(beat) = tracker.clock().nearest_beat(t) {
                timeline.positions.push((beat, tracker.position(beat)));
            }
            next_update += TEMPO_UPDATE;
        }
        if t >= slice.start + options.slice {
            slice.end = t;
            slice.bpm = tracker.clock().bpm();
            slice.phrase = tracker.clock().nearest_beat(t).and_then(|beat| tracker.position(beat)).map(|p| p.phrase);
            slices.push(std::mem::replace(&mut slice, Slice { start: t, ..Slice::default() }));
        }
    }
    slice.end = samples.len() as f64 * step;
    if slice.end - slice.start > 1.0 {
        slice.bpm = tracker.clock().bpm();
        slices.push(slice);
    }
    Replay { slices, tracker, timeline }
}

/// Un kick retenu : il nourrit le tempo et compte dans la tranche.
fn accept_kick(kick: f64, tracker: &mut Tracker, slice: &mut Slice) {
    tracker.on_kick(kick);
    let clock = tracker.clock();
    match clock.period().zip(clock.nearest_beat(kick)) {
        Some((period, beat)) if (kick - beat).abs() < ON_BEAT * period => {
            slice.beats_with_kick.insert((beat / period).round() as i64);
        }
        _ => slice.offbeat_kicks += 1,
    }
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

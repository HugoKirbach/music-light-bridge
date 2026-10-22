//! Horloge de tempo : estime un BPM stable à partir des kicks détectés, cale la phase des
//! temps sur ces kicks et continue de battre pendant les breaks. Les flashs peuvent alors
//! partir en avance de la latence Hue pour s'allumer pile sur le temps.
//!
//! Les instants sont exprimés en secondes depuis une origine commune.

use std::collections::VecDeque;
use std::f64::consts::TAU;

/// Plage de la détection automatique : exclut les octaves trompeuses (77,5 ou 310 pour 155 BPM).
pub const DEFAULT_BPM_RANGE: (f64, f64) = (100.0, 190.0);
/// Tempos acceptés en saisie manuelle.
pub const MANUAL_BPM_LIMITS: (f64, f64) = (40.0, 300.0);

/// Kicks pris en compte : ~40 temps à 155 BPM.
const WINDOW: f64 = 16.0;
const MIN_ONSETS: usize = 8;
/// Écart maximal entre deux kicks comparés pour l'estimation.
const MAX_PAIR_SPAN: f64 = 2.5;
/// Tolérance autour d'un multiple de la période.
const PAIR_TOLERANCE: f64 = 0.02;
const BPM_STEP: f64 = 0.1;
/// Un kick à moins de 20 % de période d'un temps est « sur le temps ».
const ON_BEAT: f64 = 0.2;
/// Part minimale de kicks sur la grille pour adopter une estimation.
const MIN_CONFIDENCE: f64 = 0.6;
/// Concentration minimale des kicks autour d'une même phase pour recaler l'horloge.
const MIN_PHASE_CONCENTRATION: f64 = 0.5;
/// Correction de phase appliquée à chaque kick sur le temps.
const PHASE_GAIN: f64 = 0.2;
/// Lissage du tempo : suit une dérive lente sans réagir aux estimations isolées.
const TEMPO_GAIN: f64 = 0.3;
/// Au-delà de 3 % d'écart, un nouveau tempo doit se confirmer avant d'être adopté.
const TEMPO_JUMP: f64 = 0.03;
const JUMP_CONFIRMATIONS: u32 = 4;
const OFFSET_SMOOTHING: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoEstimate {
    pub bpm: f64,
    /// Instant d'un temps, proche du kick le plus récent.
    pub anchor: f64,
    /// Part des kicks qui tombent sur la grille estimée.
    pub confidence: f64,
}

/// Tempo qui explique le mieux les écarts entre kicks (triés par ordre chronologique).
pub fn estimate(onsets: &[f64], bpm_range: (f64, f64)) -> Option<TempoEstimate> {
    if onsets.len() < MIN_ONSETS {
        return None;
    }
    let steps = ((bpm_range.1 - bpm_range.0) / BPM_STEP).round() as usize;
    let (period, _) = (0..=steps)
        .map(|i| 60.0 / (bpm_range.0 + i as f64 * BPM_STEP))
        .map(|period| (period, pair_score(onsets, period)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    fit_grid(onsets, period)
}

/// Score d'une période : chaque paire de kicks séparés d'environ k temps rapporte 1/k.
/// Le poids 1/k départage la vraie période de ses sous-multiples, qui expliquent aussi
/// toutes les paires mais avec des k deux fois plus grands.
fn pair_score(onsets: &[f64], period: f64) -> f64 {
    let mut score = 0.0;
    for (i, &a) in onsets.iter().enumerate() {
        for &b in &onsets[i + 1..] {
            let span = b - a;
            if span > MAX_PAIR_SPAN {
                break;
            }
            let k = (span / period).round();
            if k < 1.0 {
                continue;
            }
            let distance = (span - k * period) / PAIR_TOLERANCE;
            if distance.abs() < 1.0 {
                score += (1.0 - distance * distance) / k;
            }
        }
    }
    score
}

/// Affine la période par moindres carrés sur les kicks qui tombent sur la grille.
fn fit_grid(onsets: &[f64], period: f64) -> Option<TempoEstimate> {
    let reference = *onsets.last()?;
    let on_grid: Vec<(f64, f64)> = onsets
        .iter()
        .filter_map(|&t| {
            let n = ((t - reference) / period).round();
            ((t - reference - n * period).abs() < ON_BEAT * period).then_some((n, t))
        })
        .collect();
    if on_grid.len() < MIN_ONSETS {
        return None;
    }
    let count = on_grid.len() as f64;
    let mean_n = on_grid.iter().map(|&(n, _)| n).sum::<f64>() / count;
    let mean_t = on_grid.iter().map(|&(_, t)| t).sum::<f64>() / count;
    let (covariance, variance) = on_grid.iter().fold((0.0, 0.0), |(cov, var), &(n, t)| {
        (cov + (n - mean_n) * (t - mean_t), var + (n - mean_n).powi(2))
    });
    if variance == 0.0 {
        return None;
    }
    let period = covariance / variance;
    Some(TempoEstimate {
        bpm: 60.0 / period,
        anchor: mean_t - period * mean_n,
        confidence: count / onsets.len() as f64,
    })
}

/// Instant d'un temps calé sur la phase dominante des kicks, pour une période donnée.
fn dominant_beat(onsets: &[f64], period: f64) -> Option<f64> {
    if onsets.len() < MIN_ONSETS / 2 {
        return None;
    }
    let reference = *onsets.last()?;
    let (sin, cos) = onsets.iter().fold((0.0, 0.0), |(sin, cos), &t| {
        let angle = TAU * (t - reference) / period;
        (sin + angle.sin(), cos + angle.cos())
    });
    let concentration = sin.hypot(cos) / onsets.len() as f64;
    (concentration >= MIN_PHASE_CONCENTRATION).then(|| reference + sin.atan2(cos) / TAU * period)
}

/// Grille des temps : un temps toutes les `period` secondes, calée sur `anchor`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeatClock {
    period: Option<f64>,
    anchor: f64,
}

impl BeatClock {
    #[cfg(test)]
    pub fn new(bpm: f64, anchor: f64) -> Self {
        Self { period: Some(60.0 / bpm), anchor }
    }

    pub fn bpm(&self) -> Option<f64> {
        self.period.map(|period| 60.0 / period)
    }

    pub fn period(&self) -> Option<f64> {
        self.period
    }

    /// Instant d'un temps de la grille.
    pub fn anchor(&self) -> f64 {
        self.anchor
    }

    /// Premier temps strictement après `t`.
    pub fn next_beat_after(&self, t: f64) -> Option<f64> {
        let period = self.period?;
        Some(self.anchor + (((t - self.anchor) / period).floor() + 1.0) * period)
    }

    /// Temps le plus proche de `t`.
    pub fn nearest_beat(&self, t: f64) -> Option<f64> {
        let period = self.period?;
        Some(self.anchor + ((t - self.anchor) / period).round() * period)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempoSource {
    Auto,
    Manual,
    /// Tempo et phase imposés par la session Ableton Link.
    Link,
}

impl TempoSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "automatique",
            Self::Manual => "manuel",
            Self::Link => "Ableton Link",
        }
    }

    /// Libellé court de la ligne d'état.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "man.",
            Self::Link => "link",
        }
    }
}

/// Suivi du tempo : mémorise les kicks récents, entretient l'horloge et la recale.
#[derive(Debug)]
pub struct Tracker {
    onsets: VecDeque<f64>,
    clock: BeatClock,
    source: TempoSource,
    bpm_range: (f64, f64),
    /// Nouveau tempo en attente de confirmation, et nombre de confirmations.
    pending_jump: Option<(f64, u32)>,
    mean_offset: f64,
}

impl Tracker {
    pub fn new(bpm_range: (f64, f64)) -> Self {
        Self {
            onsets: VecDeque::new(),
            clock: BeatClock::default(),
            source: TempoSource::Auto,
            bpm_range,
            pending_jump: None,
            mean_offset: 0.0,
        }
    }

    pub fn clock(&self) -> BeatClock {
        self.clock
    }

    pub fn source(&self) -> TempoSource {
        self.source
    }

    /// Écart moyen (s) entre les kicks sur le temps et les temps de l'horloge.
    pub fn mean_offset(&self) -> f64 {
        self.mean_offset
    }

    /// Kick détecté à l'instant `t` : mémorisé pour l'estimation et utilisé pour caler la phase.
    pub fn on_kick(&mut self, t: f64) {
        self.onsets.push_back(t);
        self.forget_before(t - WINDOW);
        let (Some(period), Some(beat)) = (self.clock.period, self.clock.nearest_beat(t)) else {
            return;
        };
        let offset = t - beat;
        let on_beat = offset.abs() < ON_BEAT * period;
        if on_beat {
            self.mean_offset += OFFSET_SMOOTHING * (offset.abs() - self.mean_offset);
        }
        // Avec Link, la phase de la session fait foi : les kicks ne la déplacent pas.
        if self.source != TempoSource::Link {
            // Le repère suit le dernier kick, pour qu'un changement de période ne décale pas la phase.
            self.clock.anchor = beat;
            if on_beat {
                self.clock.anchor += PHASE_GAIN * offset;
            }
        }
    }

    /// Réestime le tempo (mode automatique) et recale la phase si les kicks ne tombent plus
    /// sur les temps ; à appeler environ une fois par seconde. Sans kick récent (break),
    /// l'horloge continue au dernier tempo connu.
    pub fn update(&mut self, now: f64) {
        self.forget_before(now - WINDOW);
        if self.source == TempoSource::Link {
            return;
        }
        let onsets = self.onsets.make_contiguous();
        let estimate = match self.source {
            TempoSource::Auto => estimate(onsets, self.bpm_range).filter(|e| e.confidence >= MIN_CONFIDENCE),
            TempoSource::Manual | TempoSource::Link => None,
        };
        if let Some(estimate) = estimate {
            self.adopt(estimate);
        }

        let onsets = self.onsets.make_contiguous();
        if let Some(period) = self.clock.period
            && let Some(dominant) = dominant_beat(onsets, period)
            && let Some(beat) = self.clock.nearest_beat(dominant)
            && (dominant - beat).abs() > ON_BEAT * period
        {
            self.clock.anchor = dominant;
        }
    }

    /// Tempo saisi : fixe la période ; la phase continue de se caler sur les kicks.
    pub fn set_manual(&mut self, bpm: f64, now: f64) {
        self.source = TempoSource::Manual;
        self.pending_jump = None;
        let anchor = self
            .clock
            .nearest_beat(now)
            .or_else(|| self.onsets.back().copied())
            .unwrap_or(now);
        self.clock = BeatClock { period: Some(60.0 / bpm), anchor };
    }

    pub fn set_auto(&mut self) {
        self.source = TempoSource::Auto;
    }

    /// Tempo et phase de la session Ableton Link : l'horloge les suit tels quels.
    pub fn set_link(&mut self, bpm: f64, beat_time: f64) {
        self.source = TempoSource::Link;
        self.pending_jump = None;
        self.clock = BeatClock { period: Some(60.0 / bpm), anchor: beat_time };
    }

    fn adopt(&mut self, estimate: TempoEstimate) {
        let period = 60.0 / estimate.bpm;
        let Some(current) = self.clock.period else {
            self.clock = BeatClock { period: Some(period), anchor: estimate.anchor };
            return;
        };
        if (period - current).abs() / current < TEMPO_JUMP {
            // Dérive lente : on la suit en douceur, la phase reste calée par les kicks.
            self.pending_jump = None;
            self.clock.period = Some(current + TEMPO_GAIN * (period - current));
            return;
        }
        // Tempo très différent (nouveau morceau) : adopté seulement s'il se confirme.
        let confirmations = match self.pending_jump {
            Some((pending, count)) if (pending - period).abs() / pending < TEMPO_JUMP / 3.0 => count + 1,
            _ => 1,
        };
        if confirmations >= JUMP_CONFIRMATIONS {
            self.pending_jump = None;
            self.clock = BeatClock { period: Some(period), anchor: estimate.anchor };
        } else {
            self.pending_jump = Some((period, confirmations));
        }
    }

    fn forget_before(&mut self, t: f64) {
        while self.onsets.front().is_some_and(|&first| first < t) {
            self.onsets.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Jitter(u32);

    impl Jitter {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            f64::from(self.0) / f64::from(u32::MAX) * 2.0 - 1.0
        }
    }

    /// Kicks réguliers de `start` à `end`, tous les `beats` temps du tempo `bpm(t)`,
    /// avec une imprécision de ±`jitter` s.
    fn kicks(start: f64, end: f64, beats: f64, bpm: impl Fn(f64) -> f64, jitter: f64) -> Vec<f64> {
        let mut noise = Jitter(0x9E37_79B9);
        let mut onsets = Vec::new();
        let mut t = start;
        while t < end {
            onsets.push(t + jitter * noise.next());
            t += beats * 60.0 / bpm(t);
        }
        onsets
    }

    /// Fait tourner le suivi comme l'application : kicks au fil de l'eau, mise à jour chaque seconde.
    fn run(tracker: &mut Tracker, onsets: &[f64], from: f64, until: f64) {
        let mut pending = onsets.iter().copied().filter(|&t| t >= from).peekable();
        let mut tick = from;
        while tick < until {
            tick += 1.0;
            while let Some(t) = pending.next_if(|&t| t < tick) {
                tracker.on_kick(t);
            }
            tracker.update(tick);
        }
    }

    /// Écart (s) entre un temps réel et le temps le plus proche de l'horloge.
    fn grid_error(tracker: &Tracker, true_beat: f64) -> f64 {
        (tracker.clock().nearest_beat(true_beat).unwrap() - true_beat).abs()
    }

    #[test]
    fn estimates_a_steady_tempo_despite_missing_and_extra_kicks() {
        let beat = 60.0 / 155.0;
        let mut onsets: Vec<f64> = kicks(0.0, 16.0, 1.0, |_| 155.0, 0.003)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 7 != 3)
            .map(|(_, t)| t)
            .collect();
        onsets.extend((0..10).map(|i| (4 * i) as f64 * beat + beat / 2.0));
        onsets.sort_by(f64::total_cmp);

        let started = std::time::Instant::now();
        let estimate = estimate(&onsets, DEFAULT_BPM_RANGE).unwrap();
        println!("estimation en {:?} : {estimate:?}", started.elapsed());
        assert!((estimate.bpm - 155.0).abs() < 0.1, "{estimate:?}");
        assert!(estimate.confidence > 0.7, "{estimate:?}");
    }

    #[test]
    fn prefers_the_beat_to_its_subdivisions() {
        let onsets = kicks(0.0, 16.0, 1.0, |_| 95.0, 0.002);
        let estimate = estimate(&onsets, (60.0, 200.0)).unwrap();
        assert!((estimate.bpm - 95.0).abs() < 0.1, "{estimate:?}");
    }

    #[test]
    fn half_time_kicks_still_give_the_tempo_in_range() {
        let onsets = kicks(0.0, 16.0, 2.0, |_| 155.0, 0.002);
        let estimate = estimate(&onsets, DEFAULT_BPM_RANGE).unwrap();
        assert!((estimate.bpm - 155.0).abs() < 0.1, "{estimate:?}");
    }

    #[test]
    fn keeps_the_beat_through_a_break() {
        let onsets = kicks(0.0, 20.0, 1.0, |_| 155.0, 0.002);
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        run(&mut tracker, &onsets, 0.0, 50.0);
        let bpm = tracker.clock().bpm().unwrap();
        assert!((bpm - 155.0).abs() < 0.05, "{bpm}");
        let beat: f64 = 60.0 / 155.0;
        let true_beat = (50.0 / beat).round() * beat;
        assert!(grid_error(&tracker, true_beat) < 0.005, "écart {}", grid_error(&tracker, true_beat));
    }

    #[test]
    fn follows_a_slow_tempo_drift() {
        let bpm = |t: f64| 150.0 + 6.0 * t / 120.0;
        let onsets = kicks(0.0, 120.0, 1.0, bpm, 0.002);
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        run(&mut tracker, &onsets, 0.0, 120.0);
        let found = tracker.clock().bpm().unwrap();
        assert!((found - 156.0).abs() < 1.0, "{found}");
        let last = *onsets.last().unwrap();
        assert!(grid_error(&tracker, last) < 0.015, "écart {}", grid_error(&tracker, last));
    }

    #[test]
    fn adopts_a_new_tempo_only_once_confirmed() {
        let mut onsets = kicks(0.0, 30.0, 1.0, |_| 128.0, 0.002);
        let switch = *onsets.last().unwrap() + 60.0 / 128.0;
        onsets.extend(kicks(switch, 60.0, 1.0, |_| 140.0, 0.002));
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);

        run(&mut tracker, &onsets, 0.0, 32.0);
        let soon_after = tracker.clock().bpm().unwrap();
        assert!((soon_after - 128.0).abs() < 0.5, "{soon_after}");

        run(&mut tracker, &onsets, 32.0, 60.0);
        let later = tracker.clock().bpm().unwrap();
        assert!((later - 140.0).abs() < 0.2, "{later}");
    }

    #[test]
    fn link_keeps_its_tempo_and_phase_whatever_the_kicks() {
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        tracker.set_link(128.0, 0.0);
        // Kicks à un autre tempo et décalés : ni le tempo ni la phase ne bougent.
        let onsets = kicks(0.05, 20.0, 1.0, |_| 140.0, 0.002);
        run(&mut tracker, &onsets, 0.0, 20.0);
        assert_eq!(tracker.source(), TempoSource::Link);
        assert_eq!(tracker.clock(), BeatClock::new(128.0, 0.0));
    }

    #[test]
    fn manual_tempo_snaps_its_phase_onto_the_kicks() {
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        tracker.set_manual(155.0, 0.0);
        // Kicks décalés de 150 ms par rapport à la grille de départ.
        let onsets = kicks(0.15, 10.0, 1.0, |_| 155.0, 0.002);
        run(&mut tracker, &onsets, 0.0, 10.0);
        let last = *onsets.last().unwrap();
        assert_eq!(tracker.source(), TempoSource::Manual);
        assert!(grid_error(&tracker, last) < 0.005, "écart {}", grid_error(&tracker, last));
    }
}

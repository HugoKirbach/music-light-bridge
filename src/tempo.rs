//! Horloge de tempo : estime un BPM stable à partir des kicks détectés, cale la phase des
//! temps sur ces kicks et continue de battre pendant les breaks. Les flashs peuvent alors
//! partir en avance de la latence Hue pour s'allumer pile sur le temps.
//!
//! Les instants sont exprimés en secondes depuis une origine commune.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::audio::analysis::Onset;
use crate::phrase::{BEATS_PER_PHRASE, PhraseTracker, Position};
use crate::rhythm::{PatternLearner, Rhythm};

/// Plage de la détection automatique : exclut les octaves trompeuses (77,5 ou 310 pour 155 BPM).
pub const DEFAULT_BPM_RANGE: (f64, f64) = (100.0, 190.0);
/// Tempos acceptés en saisie manuelle.
pub const MANUAL_BPM_LIMITS: (f64, f64) = (40.0, 300.0);

/// Kicks pris en compte : ~40 temps à 155 BPM.
const WINDOW: f64 = 16.0;
/// Fenêtre courte, consultée quand la longue ne confirme plus le tempo de l'horloge.
const RECENT_WINDOW: f64 = 6.0;
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
/// Une estimation qui confirme le tempo de l'horloge le rend sûr pour 60 s : la grille tient
/// à travers un long break, comme celle d'un logiciel de DJ. Une estimation confiante qui le
/// contredit (nouveau morceau) la libère aussitôt.
const LOCK_HOLD: f64 = 60.0;
const OFFSET_SMOOTHING: f64 = 0.1;
/// Peigne : cases de 10 ms (une trame), au moins 3 s de trames, et une position qui porte
/// 2,5 fois la moyenne (avec ses voisines) pour recaler la grille.
const COMB_BIN: f64 = 0.01;
const COMB_MIN_SPAN: f64 = 3.0;
/// Chaque trame compte au plus 2,5 dans le peigne : un roulement trois fois plus fort que les kicks
/// ne tire pas la grille vers lui pendant les 6 s où il reste dans la fenêtre.
const COMB_CAP: f32 = 2.5;
/// Trames gardées pour le peigne du tempo ; celui de la phase ne regarde que les 6 dernières
/// secondes.
const FRAME_MEMORY: f64 = 10.0;
const COMB_CONTRAST: f64 = 2.5;
/// Un autre tempo (ou une autre position des temps) ne remplace celui de l'horloge que si son
/// peigne contraste 1,25 fois plus.
const COMB_SWITCH: f64 = 1.25;
/// Tempos dont le peigne contraste à 2 % près du meilleur : le plateau dont on prend le milieu.
const COMB_PLATEAU: f64 = 0.98;

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

/// Instant d'un temps d'après le peigne sur les dernières secondes : la force d'attaque des
/// trames, rangée par position dans le temps (cases de 10 ms), s'accumule là où tombent les kicks.
/// `None` sans assez de trames (moins de 3 s) ; `Some(None)` quand le peigne garde la grille
/// actuelle : sa meilleure position ne domine pas assez la moyenne, ou ne bat pas nettement celle
/// des temps actuels (`current`, un temps de l'horloge). Dans certains morceaux, kick et
/// contretemps se valent sur quelques secondes, et la grille ne doit pas sauter de l'un à l'autre.
fn comb_beat(frames: &[(f64, f32)], period: f64, current: Option<f64>) -> Option<Option<f64>> {
    let last = frames.last()?.0;
    let recent = &frames[frames.partition_point(|&(t, _)| t < last - RECENT_WINDOW)..];
    let profile = CombProfile::new(recent, period)?;
    let (beat, contrast) = profile.best();
    let held = current.map_or(0.0, |current| profile.at(current));
    Some((contrast > COMB_CONTRAST && contrast > held * COMB_SWITCH).then_some(beat))
}

/// Tempo d'après le peigne : celui, entre `bpm_range`, dont la grille concentre le mieux la force
/// d'attaque (pas de 0,25 BPM, affiné à 0,05). Il ne dépend pas des kicks détectés un à un, que
/// les fausses détections d'un mix chargé brouillent. Le tempo `current` de l'horloge garde la
/// main tant qu'un autre ne le bat pas nettement : un rythme en 3/16 ou des croches font aussi
/// ressortir les 2/3 ou les 3/2 du tempo.
fn comb_tempo(frames: &[(f64, f32)], bpm_range: (f64, f64), current: Option<f64>) -> Option<TempoEstimate> {
    let best_of = |bpms: &mut dyn Iterator<Item = f64>| {
        bpms.filter_map(|bpm| comb(frames, 60.0 / bpm).map(|(beat, contrast)| (bpm, beat, contrast)))
            .max_by(|a, b| a.2.total_cmp(&b.2))
    };
    let steps = ((bpm_range.1 - bpm_range.0) / 0.25) as usize;
    let (coarse, _, _) = best_of(&mut (0..=steps).map(|i| bpm_range.0 + 0.25 * i as f64))?;
    // Sur quelques secondes, les tempos voisins concentrent presque aussi bien les attaques :
    // on prend le milieu de ce plateau plutôt que son bord.
    let fine: Vec<(f64, f64)> = (-20..=20)
        .map(|i| coarse + 0.05 * f64::from(i))
        .filter_map(|bpm| comb(frames, 60.0 / bpm).map(|(_, contrast)| (bpm, contrast)))
        .collect();
    let top = fine.iter().map(|&(_, contrast)| contrast).fold(f64::MIN, f64::max);
    let plateau: Vec<f64> = fine.iter().filter(|&&(_, c)| c >= COMB_PLATEAU * top).map(|&(bpm, _)| bpm).collect();
    let center = plateau.iter().sum::<f64>() / plateau.len().max(1) as f64;
    let (mut bpm, mut anchor, mut contrast) = best_of(&mut std::iter::once(center))?;
    if let Some(current) = current
        && let Some((beat, held)) = comb(frames, 60.0 / current)
        && held * COMB_SWITCH >= contrast
    {
        (bpm, anchor, contrast) = (current, beat, held);
    }
    (contrast > COMB_CONTRAST).then_some(TempoEstimate { bpm, anchor, confidence: 1.0 })
}

/// Profil du peigne : la force d'attaque des trames rangée par position dans le temps, en cases
/// de 10 ms (chacune avec ses deux voisines : un kick peut tomber à cheval sur deux trames).
struct CombProfile {
    smoothed: Vec<f64>,
    mean: f64,
    last: f64,
    period: f64,
}

impl CombProfile {
    fn new(frames: &[(f64, f32)], period: f64) -> Option<Self> {
        let (&(first, _), &(last, _)) = (frames.first()?, frames.last()?);
        if last - first < COMB_MIN_SPAN {
            return None;
        }
        let bins = ((period / COMB_BIN).round() as usize).max(4);
        let mut sums = vec![0.0f64; bins];
        for &(t, strength) in frames {
            let position = (t - last).rem_euclid(period) / period;
            sums[((position * bins as f64) as usize).min(bins - 1)] += f64::from(strength.min(COMB_CAP));
        }
        let smoothed: Vec<f64> =
            (0..bins).map(|i| sums[(i + bins - 1) % bins] + sums[i] + sums[(i + 1) % bins]).collect();
        let mean = smoothed.iter().sum::<f64>() / bins as f64;
        (mean > 0.0).then_some(Self { smoothed, mean, last, period })
    }

    /// Instant d'un temps à la meilleure position, et sa valeur rapportée à la moyenne.
    fn best(&self) -> (f64, f64) {
        let (best, &peak) = self.smoothed.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap_or((0, &0.0));
        // La trame qui porte le bond finit ~10 ms après l'attaque : on vise le début de la case.
        let beat = self.last + best as f64 / self.smoothed.len() as f64 * self.period - COMB_BIN;
        (beat, peak / self.mean)
    }

    /// Valeur (rapportée à la moyenne) de la position d'un temps placé à l'instant `beat`.
    fn at(&self, beat: f64) -> f64 {
        let bins = self.smoothed.len();
        let position = (beat + COMB_BIN - self.last).rem_euclid(self.period) / self.period;
        self.smoothed[((position * bins as f64).round() as usize) % bins] / self.mean
    }
}

/// Instant d'un temps et contraste du peigne (meilleure position sur la moyenne).
fn comb(frames: &[(f64, f32)], period: f64) -> Option<(f64, f64)> {
    CombProfile::new(frames, period).map(|profile| profile.best())
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
    /// Instant jusqu'où le tempo de l'horloge reste sûr en mode automatique : une estimation
    /// l'a confirmé il y a moins de `LOCK_HOLD`.
    locked_until: f64,
    /// Force d'attaque des dernières secondes, trame par trame : (fin de trame, valeur).
    frames: VecDeque<(f64, f32)>,
    /// Têtes de mesure et de phrase.
    phrase: PhraseTracker,
    /// Où tombent les kicks dans la mesure.
    pattern: PatternLearner,
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
            locked_until: f64::NEG_INFINITY,
            frames: VecDeque::new(),
            phrase: PhraseTracker::default(),
            pattern: PatternLearner::default(),
        }
    }

    pub fn clock(&self) -> BeatClock {
        self.clock
    }

    /// Grille et motif rythmique : où et quand les kicks sont attendus, si le tempo est connu.
    pub fn rhythm(&self) -> Option<Rhythm> {
        let period = self.clock.period()?;
        let bar = self.pattern.bar_start(period).unwrap_or(self.clock.anchor());
        Some(Rhythm { period, bar, hits: self.pattern.hits() })
    }

    pub fn source(&self) -> TempoSource {
        self.source
    }

    /// Tempo sûr à l'instant `now` : confirmé récemment par une estimation, imposé, ou suivi
    /// via Link. Les kicks peuvent alors être cherchés sur les temps.
    pub fn locked(&self, now: f64) -> bool {
        self.clock.period.is_some() && (self.source != TempoSource::Auto || now < self.locked_until)
    }

    /// Place dans la mesure et la phrase du temps le plus proche de `t`, si la structure est
    /// reconnue.
    pub fn position(&self, t: f64) -> Option<Position> {
        self.phrase.position(t, &self.clock)
    }

    /// Période du temps (s) et nombre de temps, fractionnaire, jusqu'à la prochaine tête de
    /// phrase, quand la structure est reconnue.
    pub fn structure(&self, t: f64) -> Option<(f32, f32)> {
        let period = self.clock.period()?;
        let beat = self.clock.nearest_beat(t)?;
        let phrase = u64::from(self.position(beat)?.phrase);
        let mut to_head = ((BEATS_PER_PHRASE - phrase) % BEATS_PER_PHRASE) as f64 - (t - beat) / period;
        if to_head <= 0.0 {
            to_head += BEATS_PER_PHRASE as f64;
        }
        Some((period as f32, to_head as f32))
    }

    /// Écart moyen (s) entre les kicks sur le temps et les temps de l'horloge.
    pub fn mean_offset(&self) -> f64 {
        self.mean_offset
    }

    /// Drop d'une montée : la force d'attaque d'avant (roulement de caisse claire, souffle) ne compte
    /// plus pour placer les temps, que les kicks du drop recalent aussitôt.
    pub fn on_drop(&mut self) {
        self.frames.clear();
    }

    /// Kick détecté à l'instant `t` : mémorisé pour l'estimation et utilisé pour caler la phase.
    pub fn on_kick(&mut self, t: f64) {
        self.onsets.push_back(t);
        self.pattern.on_kick(t, &self.clock);
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
            TempoSource::Auto => {
                let confident = |e: &TempoEstimate| e.confidence >= MIN_CONFIDENCE;
                // Les kicks du morceau précédent restent 16 s dans la fenêtre et y confirmeraient
                // l'ancien tempo : un tempo différent trouvé sur les dernières secondes seules
                // l'emporte (il devra encore se confirmer, à chaque mise à jour).
                let recent_start = onsets.partition_point(|&t| t < now - RECENT_WINDOW);
                let differs = |e: &TempoEstimate| {
                    self.clock.period.is_some_and(|p| (60.0 / e.bpm - p).abs() / p >= TEMPO_JUMP)
                };
                estimate(&onsets[recent_start..], self.bpm_range)
                    .filter(|e| confident(e) && differs(e))
                    .or_else(|| estimate(onsets, self.bpm_range).filter(confident))
                    .or_else(|| comb_tempo(self.frames.make_contiguous(), self.bpm_range, self.clock.bpm()))
            }
            TempoSource::Manual | TempoSource::Link => None,
        };
        if let (Some(e), Some(period)) = (estimate, self.clock.period) {
            let agrees = (60.0 / e.bpm - period).abs() / period < TEMPO_JUMP;
            self.locked_until = if agrees { now + LOCK_HOLD } else { f64::NEG_INFINITY };
        }
        if let Some(estimate) = estimate {
            self.adopt(estimate);
        }

        let Some(period) = self.clock.period else { return };
        // Position des temps : d'abord le peigne sur la force d'attaque des dernières secondes,
        // qui somme chaque temps et ne se laisse pas tromper par quelques kicks manqués ou par
        // les croches d'une montée ; sinon la phase dominante des kicks récents.
        let onsets = self.onsets.make_contiguous();
        let recent = &onsets[onsets.partition_point(|&t| t < now - RECENT_WINDOW)..];
        let frames = self.frames.make_contiguous();
        let current = self.clock.nearest_beat(now);
        let target = comb_beat(frames, period, current).unwrap_or_else(|| dominant_beat(recent, period));
        if let Some(target) = target
            && let Some(beat) = self.clock.nearest_beat(target)
            && (target - beat).abs() > ON_BEAT * period
        {
            self.clock.anchor = target;
        }
    }

    /// Force d'attaque (voir `OnsetStrength`) de la trame de 10 ms qui finit à l'instant `t`.
    pub fn on_onset(&mut self, t: f64, onset: Onset) {
        self.frames.push_back((t, onset.kick));
        self.phrase.on_frame(t, onset, &self.clock);
        self.pattern.on_frame(t, &self.clock);
        self.forget_frames_before(t - FRAME_MEMORY);
    }

    fn forget_frames_before(&mut self, t: f64) {
        while self.frames.front().is_some_and(|&(first, _)| first < t) {
            self.frames.pop_front();
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


    /// Force d'attaque synthétique : un bond par temps (`strong`), un plus faible à chaque
    /// contretemps (`weak`), rien ailleurs, une trame toutes les 10 ms de `from` à `until`.
    fn frames(tracker: &mut Tracker, bpm: f64, first_beat: f64, strong: f32, weak: f32, from: f64, until: f64) {
        let period = 60.0 / bpm;
        let mut t = from;
        while t < until {
            let position = (t - first_beat).rem_euclid(period);
            let strength = if position < 0.01 {
                strong
            } else if (position - period / 2.0).abs() < 0.005 {
                weak
            } else {
                0.0
            };
            tracker.on_onset(t, Onset { kick: strength, ..Onset::default() });
            t += 0.01;
        }
    }

    /// Après un break, la grille est restée d'un demi-temps en avance sur les kicks qui
    /// reprennent ; le peigne, qui somme leurs attaques sur les temps, la recale.
    #[test]
    fn the_comb_puts_back_a_grid_shifted_by_half_a_beat() {
        let bpm = 154.0;
        let period = 60.0 / bpm;
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        tracker.set_manual(bpm, 0.0);
        tracker.clock.anchor = period / 2.0;
        frames(&mut tracker, bpm, 0.0, 1.0, 0.5, 0.0, 6.0);
        tracker.update(6.0);
        let error = grid_error(&tracker, (6.0 / period).round() * period);
        assert!(error < 0.015, "écart {error}");
    }

    /// Aucun kick détecté un à un (mix trop chargé) : la force d'attaque seule donne le tempo.
    #[test]
    fn the_comb_finds_the_tempo_without_detected_kicks() {
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        frames(&mut tracker, 152.4, 0.13, 1.0, 0.4, 0.0, 6.0);
        tracker.update(6.0);
        let bpm = tracker.clock().bpm().unwrap();
        // Trames de 10 ms : à 1 BPM près, la phase se recale ensuite chaque seconde.
        assert!((bpm - 152.4).abs() < 1.0, "{bpm}");
        let period: f64 = 60.0 / 152.4;
        let true_beat = 0.13 + ((6.0 - 0.13) / period).round() * period;
        assert!(grid_error(&tracker, true_beat) < 0.03, "écart {}", grid_error(&tracker, true_beat));
    }
    /// Changement de morceau : 188 BPM puis 152, avec un kick sur trois perdu dans le second. Les
    /// kicks du premier restent 16 s dans la fenêtre ; les dernières secondes suffisent à trouver
    /// le nouveau tempo en moins de 10 s.
    #[test]
    fn a_new_tempo_wins_before_the_old_kicks_leave_the_window() {
        let mut onsets = kicks(0.0, 15.0, 1.0, |_| 188.0, 0.002);
        let switch = *onsets.last().unwrap() + 60.0 / 188.0;
        let second: Vec<f64> = kicks(switch, 45.0, 1.0, |_| 152.0, 0.003)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 3 != 2)
            .map(|(_, t)| t)
            .collect();
        onsets.extend(second);
        let mut tracker = Tracker::new(DEFAULT_BPM_RANGE);
        run(&mut tracker, &onsets, 0.0, switch + 9.0);
        let found = tracker.clock().bpm().unwrap();
        assert!((found - 152.0).abs() < 0.5, "{found}");
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

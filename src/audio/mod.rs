//! Entrée audio temps réel et descripteurs partagés avec la boucle d'éclairage.

pub mod analysis;
pub mod capture;
mod input;
#[cfg(test)]
pub mod synth;

pub use input::{AudioInput, Backend, list_inputs, list_outputs};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use analysis::{Expectation, Levels};

/// Demi-largeur de la fenêtre où l'horloge attend un kick, autour de chaque temps.
const KICK_WINDOW: f64 = 0.035;
/// Trames de force d'attaque gardées : un peu plus de 10 s.
const ONSET_SLOTS: usize = 1024;

/// `f32` atomique, stocké sous forme de bits.
#[derive(Debug, Default)]
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// Grille de tempo publiée par la boucle d'éclairage pour guider la détection des kicks.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Grid {
    /// Durée d'un temps (s) ; 0 tant que le tempo est inconnu.
    pub period: f64,
    /// Instant d'un temps (s depuis l'origine commune).
    pub anchor: f64,
    /// Le tempo et la phase sont sûrs (estimation confirmée, tempo imposé ou Link) : les kicks
    /// sont cherchés sur les temps même avant que le groove soit établi.
    pub locked: bool,
    /// Les kicks tombent régulièrement sur les temps.
    pub groove: bool,
}

impl Grid {
    /// Ce que la grille attend à l'instant `t` : un kick près des temps dès que le tempo est sûr
    /// ou que le groove tient, et rien entre les temps tant que le groove tient ; sinon (tempo
    /// inconnu) aucune attente.
    pub fn expectation(&self, t: f64) -> Expectation {
        if !(self.groove || self.locked) || self.period <= 0.0 {
            return Expectation::Free;
        }
        let beats = (t - self.anchor) / self.period;
        if (beats - beats.round()).abs() * self.period < KICK_WINDOW {
            Expectation::Kick
        } else if self.groove {
            Expectation::Offbeat
        } else {
            Expectation::Free
        }
    }
}

/// Descripteurs publiés par le fil audio et lus sans verrou par la boucle d'éclairage :
/// le fil audio ne doit jamais attendre.
#[derive(Debug)]
pub struct Features {
    epoch: Instant,
    kicks: AtomicU32,
    /// Instant (depuis `epoch`) où l'échantillon déclencheur du dernier kick a été capté.
    last_kick_ns: AtomicU64,
    bass: AtomicF32,
    mid: AtomicF32,
    high: AtomicF32,
    /// Équilibre grave/aigu ; `NaN` dans le silence.
    tilt: AtomicF32,
    /// Présence des médiums et aigus (dBFS) ; `NaN` dans le silence.
    presence: AtomicF32,
    threshold: AtomicF32,
    input_latency_us: AtomicU64,
    /// Coupures signalées par le pilote : des échantillons ont été perdus.
    glitches: AtomicU32,
    /// L'entrée a disparu (câble débranché, pilote réinitialisé).
    input_lost: AtomicBool,
    grid_period: AtomicU64,
    grid_anchor: AtomicU64,
    grid_locked: AtomicBool,
    grid_groove: AtomicBool,
    /// Force d'attaque, trame par trame : tampon circulaire (instant, valeur) et nombre de
    /// trames écrites. Le fil audio y écrit sans jamais attendre.
    onsets: Box<[(AtomicU64, AtomicU32)]>,
    onsets_written: AtomicU64,
}

impl Features {
    pub fn new(threshold: f32) -> Self {
        Self {
            epoch: Instant::now(),
            kicks: AtomicU32::new(0),
            last_kick_ns: AtomicU64::new(0),
            bass: AtomicF32::default(),
            mid: AtomicF32::default(),
            high: AtomicF32::default(),
            tilt: AtomicF32::new(f32::NAN),
            presence: AtomicF32::new(f32::NAN),
            threshold: AtomicF32::new(threshold),
            input_latency_us: AtomicU64::new(0),
            glitches: AtomicU32::new(0),
            input_lost: AtomicBool::new(false),
            grid_period: AtomicU64::new(0),
            grid_anchor: AtomicU64::new(0),
            grid_locked: AtomicBool::new(false),
            grid_groove: AtomicBool::new(false),
            onsets: (0..ONSET_SLOTS).map(|_| (AtomicU64::new(0), AtomicU32::new(0))).collect(),
            onsets_written: AtomicU64::new(0),
        }
    }

    pub fn glitches(&self) -> u32 {
        self.glitches.load(Ordering::Relaxed)
    }

    pub fn input_lost(&self) -> bool {
        self.input_lost.load(Ordering::Relaxed)
    }

    /// Secondes écoulées entre l'origine commune et `t` : l'unité de temps de l'horloge de tempo.
    pub fn seconds(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.epoch).as_secs_f64()
    }

    /// Instant correspondant à `seconds` depuis l'origine commune.
    pub fn instant(&self, seconds: f64) -> Instant {
        self.epoch + Duration::from_secs_f64(seconds.max(0.0))
    }

    /// Nombre de kicks détectés depuis le démarrage.
    pub fn kicks(&self) -> u32 {
        self.kicks.load(Ordering::Acquire)
    }

    /// Instant de capture du dernier kick détecté.
    pub fn last_kick(&self) -> Instant {
        self.epoch + Duration::from_nanos(self.last_kick_ns.load(Ordering::Relaxed))
    }

    pub fn levels(&self) -> Levels {
        let known = |value: f32| (!value.is_nan()).then_some(value);
        Levels {
            bass: self.bass.load(),
            mid: self.mid.load(),
            high: self.high.load(),
            tilt: known(self.tilt.load()),
            presence: known(self.presence.load()),
        }
    }

    pub fn threshold(&self) -> f32 {
        self.threshold.load()
    }

    pub fn set_threshold(&self, threshold: f32) {
        self.threshold.store(threshold);
    }

    /// Âge du bloc audio à son arrivée dans le programme, annoncé par le pilote.
    pub fn input_latency(&self) -> Duration {
        Duration::from_micros(self.input_latency_us.load(Ordering::Relaxed))
    }

    pub fn set_grid(&self, grid: Grid) {
        self.grid_period.store(grid.period.to_bits(), Ordering::Relaxed);
        self.grid_anchor.store(grid.anchor.to_bits(), Ordering::Relaxed);
        self.grid_locked.store(grid.locked, Ordering::Relaxed);
        self.grid_groove.store(grid.groove, Ordering::Relaxed);
    }

    fn grid(&self) -> Grid {
        Grid {
            period: f64::from_bits(self.grid_period.load(Ordering::Relaxed)),
            anchor: f64::from_bits(self.grid_anchor.load(Ordering::Relaxed)),
            locked: self.grid_locked.load(Ordering::Relaxed),
            groove: self.grid_groove.load(Ordering::Relaxed),
        }
    }

    /// Trames de force d'attaque écrites depuis `seen`, dans l'ordre ; met `seen` à jour. À lire
    /// au moins toutes les 10 s, sans quoi les plus anciennes sont perdues.
    pub fn read_onsets(&self, seen: &mut u64, mut each: impl FnMut(f64, f32)) {
        let written = self.onsets_written.load(Ordering::Acquire);
        let start = (*seen).max(written.saturating_sub(ONSET_SLOTS as u64 - 1));
        for index in start..written {
            let (time, value) = &self.onsets[index as usize % ONSET_SLOTS];
            each(f64::from_bits(time.load(Ordering::Relaxed)), f32::from_bits(value.load(Ordering::Relaxed)));
        }
        *seen = written;
    }

    fn publish_onset(&self, t: f64, strength: f32) {
        let written = self.onsets_written.load(Ordering::Relaxed);
        let (time, value) = &self.onsets[written as usize % ONSET_SLOTS];
        time.store(t.to_bits(), Ordering::Relaxed);
        value.store(strength.to_bits(), Ordering::Relaxed);
        // `Release` : qui voit la nouvelle trame comptée voit aussi son contenu.
        self.onsets_written.store(written + 1, Ordering::Release);
    }

    fn publish_kick(&self, captured: Instant) {
        let since_epoch = captured.saturating_duration_since(self.epoch).as_nanos() as u64;
        self.last_kick_ns.store(since_epoch, Ordering::Relaxed);
        // `Release` : qui voit le nouveau compteur voit aussi l'instant du kick.
        self.kicks.fetch_add(1, Ordering::Release);
    }

    fn publish_levels(&self, levels: Levels) {
        self.bass.store(levels.bass);
        self.mid.store(levels.mid);
        self.high.store(levels.high);
        self.tilt.store(levels.tilt.unwrap_or(f32::NAN));
        self.presence.store(levels.presence.unwrap_or(f32::NAN));
    }

    fn set_input_latency(&self, latency: Duration) {
        self.input_latency_us.store(latency.as_micros() as u64, Ordering::Relaxed);
    }

    fn record_glitch(&self) {
        self.glitches.fetch_add(1, Ordering::Relaxed);
    }

    fn record_input_lost(&self) {
        self.input_lost.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sure_tempo_expects_kicks_on_the_beats_before_the_groove() {
        let grid = |locked, groove| Grid { period: 0.4, anchor: 0.0, locked, groove };
        let (on_beat, between) = (4.01, 4.2);
        assert_eq!(grid(false, false).expectation(on_beat), Expectation::Free, "tempo pas encore sûr");
        assert_eq!(grid(true, false).expectation(on_beat), Expectation::Kick);
        assert_eq!(grid(true, false).expectation(between), Expectation::Free, "pas de méfiance sans groove");
        assert_eq!(grid(true, true).expectation(between), Expectation::Offbeat);
        assert_eq!(grid(false, true).expectation(on_beat), Expectation::Kick);
    }

    #[test]
    fn onset_frames_come_back_in_order_even_after_the_buffer_wraps() {
        let features = Features::new(1.6);
        let mut seen = 0;
        let mut read = Vec::new();
        for n in 0..(ONSET_SLOTS + 10) {
            features.publish_onset(n as f64 * 0.01, n as f32);
            if n % 300 == 0 {
                features.read_onsets(&mut seen, |_, strength| read.push(strength));
            }
        }
        features.read_onsets(&mut seen, |_, strength| read.push(strength));
        let expected: Vec<f32> = (0..(ONSET_SLOTS + 10)).map(|n| n as f32).collect();
        assert_eq!(read, expected);
    }
}

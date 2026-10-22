//! Analyse temps réel, échantillon par échantillon : filtres IIR et suiveurs d'enveloppe.
//! Sans fenêtre FFT il n'y a pas de latence de bloc : un kick est repéré quelques
//! millisecondes après son attaque.

use std::f32::consts::{FRAC_1_SQRT_2, TAU};

/// Seuil de détection par défaut : l'enveloppe des basses doit dépasser 1,6 fois son fond.
pub const DEFAULT_THRESHOLD: f32 = 1.6;
/// Retard moyen entre l'attaque d'un kick et sa détection (mesuré sur signaux de synthèse), en s.
pub const KICK_DETECTION_DELAY: f64 = 0.002;

const BASS_CUTOFF: f32 = 150.0;
const MID_CENTER: f32 = 1000.0;
const MID_Q: f32 = 0.7;
const HIGH_CUTOFF: f32 = 5000.0;

const BAND_ATTACK: f32 = 0.005;
const BAND_RELEASE: f32 = 0.15;
/// Durée pendant laquelle la normalisation se souvient d'un pic de niveau.
const GAIN_MEMORY: f32 = 5.0;
/// En dessous (~ -50 dBFS), c'est du silence : on ne l'amplifie pas.
const GAIN_FLOOR: f32 = 0.003;

const KICK_ATTACK: f32 = 0.0005;
const KICK_RELEASE: f32 = 0.04;
/// Niveau de fond des basses : monte lentement et redescend vite, il suit donc les creux
/// entre les kicks (la ligne de basse) plutôt que leur moyenne, gonflée par les kicks eux-mêmes.
const KICK_FLOOR_RISE: f32 = 0.3;
const KICK_FLOOR_FALL: f32 = 0.05;
/// Temps de montée de l'enveloppe de référence : une attaque plus rapide qu'elle est un kick.
const KICK_RISE_TIME: f32 = 0.015;
/// L'enveloppe doit dépasser sa référence de 40 % : une basse tenue ou la queue d'un kick
/// ne montent pas assez vite pour déclencher.
const KICK_RISE_RATIO: f32 = 1.4;
/// Écart minimal entre deux kicks (100 ms = doubles croches à 150 BPM).
const KICK_REFRACTORY: f32 = 0.1;
/// Niveau minimal des basses (-40 dBFS) pour qu'une attaque compte.
const KICK_FLOOR: f32 = 0.01;
/// Près d'un temps où l'horloge attend un kick, le seuil ne garde que 40 % de son excès sur 1
/// (1,6 → 1,24) : un kick noyé dans une basse dense dépasse à peine le fond.
const EXPECTED_RELAX: f32 = 0.4;
const EXPECTED_RISE_RATIO: f32 = 1.15;
/// Écart minimal avec la détection précédente quand le kick est attendu : une note de basse
/// juste avant le temps ne doit pas masquer le kick.
const EXPECTED_REFRACTORY: f32 = 0.04;
/// Groove établi, loin d'un temps : l'excès du seuil sur 1 est doublé (1,6 → 2,2) et la montée
/// doit être plus franche, pour que les notes de basse ne passent pas pour des kicks.
const OFFBEAT_STRICT: f32 = 2.0;
const OFFBEAT_RISE_RATIO: f32 = 1.8;

/// Ce que l'horloge de tempo attend à l'instant d'un échantillon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    /// Pas de groove établi (tempo inconnu, break, montée) : seuils normaux.
    Free,
    /// Près d'un temps où un kick est attendu : seuils relâchés.
    Kick,
    /// Groove établi, loin d'un temps : seuils durcis.
    Offbeat,
}

/// Sous ce seuil, les états sont remis à zéro : les nombres dénormaux qui apparaissent
/// dans les silences ralentissent fortement le processeur.
const DENORMAL: f32 = 1e-20;

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < DENORMAL { 0.0 } else { x }
}

/// Coefficient d'un filtre à un pôle de constante de temps `seconds`.
fn coefficient(rate: f32, seconds: f32) -> f32 {
    (-1.0 / (rate * seconds)).exp()
}

/// Filtre biquad en forme directe transposée II, coefficients du « Audio EQ Cookbook »
/// de R. Bristow-Johnson.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn low_pass(rate: f32, freq: f32, q: f32) -> Self {
        let (cos, alpha) = Self::prewarp(rate, freq, q);
        let b1 = 1.0 - cos;
        Self::normalized(b1 / 2.0, b1, b1 / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    pub fn high_pass(rate: f32, freq: f32, q: f32) -> Self {
        let (cos, alpha) = Self::prewarp(rate, freq, q);
        let b1 = -(1.0 + cos);
        Self::normalized(-b1 / 2.0, b1, -b1 / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    /// Passe-bande à gain unitaire à la fréquence centrale.
    pub fn band_pass(rate: f32, freq: f32, q: f32) -> Self {
        let (cos, alpha) = Self::prewarp(rate, freq, q);
        Self::normalized(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    fn prewarp(rate: f32, freq: f32, q: f32) -> (f32, f32) {
        let w = TAU * freq / rate;
        (w.cos(), w.sin() / (2.0 * q))
    }

    fn normalized(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = flush(self.b1 * x - self.a1 * y + self.z2);
        self.z2 = flush(self.b2 * x - self.a2 * y);
        y
    }
}

/// Suiveur d'enveloppe à attaque et relâchement exponentiels.
#[derive(Debug, Clone, Copy)]
pub struct Envelope {
    attack: f32,
    release: f32,
    value: f32,
}

impl Envelope {
    pub fn new(rate: f32, attack: f32, release: f32) -> Self {
        Self { attack: coefficient(rate, attack), release: coefficient(rate, release), value: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let coef = if x > self.value { self.attack } else { self.release };
        self.value = flush(x + coef * (self.value - x));
        self.value
    }
}

/// Normalisation automatique : divise par un maximum qui redescend lentement, pour que
/// la sortie reste dans [0, 1] quel que soit le volume envoyé par la table de mixage.
#[derive(Debug, Clone, Copy)]
pub struct AutoGain {
    peak: f32,
    decay: f32,
}

impl AutoGain {
    pub fn new(rate: f32, memory: f32) -> Self {
        Self { peak: GAIN_FLOOR, decay: coefficient(rate, memory) }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.peak = (self.peak * self.decay).max(x).max(GAIN_FLOOR);
        x / self.peak
    }
}

/// Détecteur de kick : l'enveloppe rapide des basses monte brusquement et dépasse
/// `threshold` fois leur niveau de fond.
#[derive(Debug, Clone, Copy)]
pub struct KickDetector {
    fast: Envelope,
    reference: Envelope,
    background: Envelope,
    threshold: f32,
    since_kick: u32,
    refractory: u32,
    expected_refractory: u32,
}

impl KickDetector {
    pub fn new(rate: f32, threshold: f32) -> Self {
        Self {
            fast: Envelope::new(rate, KICK_ATTACK, KICK_RELEASE),
            reference: Envelope::new(rate, KICK_RISE_TIME, KICK_RELEASE),
            background: Envelope::new(rate, KICK_FLOOR_RISE, KICK_FLOOR_FALL),
            threshold,
            since_kick: u32::MAX,
            refractory: (rate * KICK_REFRACTORY) as u32,
            expected_refractory: (rate * EXPECTED_REFRACTORY) as u32,
        }
    }

    /// Traite un échantillon filtré dans les basses ; `true` à l'instant où un kick est détecté.
    #[inline]
    pub fn process(&mut self, bass: f32, expectation: Expectation) -> bool {
        let level = self.fast.process(bass.abs());
        let reference = self.reference.process(level);
        let background = self.background.process(level);
        self.since_kick = self.since_kick.saturating_add(1);
        let excess = self.threshold - 1.0;
        let (threshold, rise, refractory) = match expectation {
            Expectation::Free => (self.threshold, KICK_RISE_RATIO, self.refractory),
            Expectation::Kick => (1.0 + excess * EXPECTED_RELAX, EXPECTED_RISE_RATIO, self.expected_refractory),
            Expectation::Offbeat => (1.0 + excess * OFFBEAT_STRICT, OFFBEAT_RISE_RATIO, self.refractory),
        };
        let kick = self.since_kick >= refractory
            && level > KICK_FLOOR
            && level > reference * rise
            && level > background * threshold;
        if kick {
            self.since_kick = 0;
        }
        kick
    }
}

/// Niveaux des trois bandes, normalisés dans [0, 1].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Levels {
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
}

/// Chaîne complète : séparation en bandes, niveaux normalisés et détection de kick.
#[derive(Debug, Clone)]
pub struct Analyzer {
    bass_filter: Biquad,
    mid_filter: Biquad,
    high_filter: Biquad,
    envelopes: [Envelope; 3],
    gains: [AutoGain; 3],
    levels: Levels,
    kick: KickDetector,
}

impl Analyzer {
    pub fn new(rate: f32, threshold: f32) -> Self {
        Self {
            bass_filter: Biquad::low_pass(rate, BASS_CUTOFF, FRAC_1_SQRT_2),
            mid_filter: Biquad::band_pass(rate, MID_CENTER, MID_Q),
            high_filter: Biquad::high_pass(rate, HIGH_CUTOFF, FRAC_1_SQRT_2),
            envelopes: [Envelope::new(rate, BAND_ATTACK, BAND_RELEASE); 3],
            gains: [AutoGain::new(rate, GAIN_MEMORY); 3],
            levels: Levels::default(),
            kick: KickDetector::new(rate, threshold),
        }
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.kick.threshold = threshold;
    }

    pub fn levels(&self) -> Levels {
        self.levels
    }

    /// Traite un échantillon mono ; `true` si un kick vient d'être détecté.
    #[inline]
    pub fn process(&mut self, x: f32, expectation: Expectation) -> bool {
        let bass = self.bass_filter.process(x);
        let bands = [bass, self.mid_filter.process(x), self.high_filter.process(x)];
        let [b, m, h] = std::array::from_fn(|i| {
            self.gains[i].process(self.envelopes[i].process(bands[i].abs()))
        });
        self.levels = Levels { bass: b, mid: m, high: h };
        self.kick.process(bass, expectation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::synth::{Noise, RATE, bass_note, hat_envelope, kick};

    /// Ligne de basse tenue à 55 Hz : (début en s, amplitude).
    type Bassline = Option<(f32, f32)>;

    fn render(kicks: &[f32], hats: &[f32], bassline: Bassline, seconds: f32) -> Vec<f32> {
        let mut noise = Noise(0x1234_5678);
        let mut hat_filter = Biquad::high_pass(RATE, 6000.0, FRAC_1_SQRT_2);
        (0..(seconds * RATE) as usize)
            .map(|i| {
                let t = i as f32 / RATE;
                let k: f32 = kicks.iter().map(|&start| kick(t - start)).sum();
                let h: f32 = hats.iter().map(|&start| hat_envelope(t - start)).sum();
                let b = match bassline {
                    Some((start, amplitude)) if t >= start => amplitude * (TAU * 55.0 * (t - start)).sin(),
                    _ => 0.0,
                };
                k + h * hat_filter.process(noise.next()) + b
            })
            .collect()
    }

    fn assert_detected(kicks: &[f32], found: &[f32]) {
        assert_eq!(found.len(), kicks.len(), "détections : {found:?}");
        let delays: Vec<f32> = kicks.iter().zip(found).map(|(kick, hit)| (hit - kick) * 1e3).collect();
        println!("retards de détection (ms) : {delays:.2?}");
        assert!(delays.iter().all(|d| (0.0..8.0).contains(d)), "retards (ms) : {delays:?}");
    }

    fn detections(signal: &[f32]) -> Vec<f32> {
        detections_with(signal, |_| Expectation::Free)
    }

    /// Détections avec un groove établi : kick attendu à ±35 ms de chaque temps de `beats`,
    /// seuils durcis ailleurs.
    fn detections_in_groove(signal: &[f32], beats: &[f32]) -> Vec<f32> {
        detections_with(signal, |t| {
            if beats.iter().any(|&beat| (t - beat).abs() < 0.035) {
                Expectation::Kick
            } else {
                Expectation::Offbeat
            }
        })
    }

    fn detections_with(signal: &[f32], expectation: impl Fn(f32) -> Expectation) -> Vec<f32> {
        let mut analyzer = Analyzer::new(RATE, DEFAULT_THRESHOLD);
        signal
            .iter()
            .enumerate()
            .filter_map(|(i, &x)| {
                let t = i as f32 / RATE;
                analyzer.process(x, expectation(t)).then_some(t)
            })
            .collect()
    }


    /// La ligne de basse qui démarre sur du silence compte elle-même comme une attaque :
    /// on ignore cette première détection.
    const WARM_UP: f32 = 0.1;

    #[test]
    fn detects_every_kick_within_a_few_ms() {
        let kicks: Vec<f32> = (0..8).map(|n| 0.25 + 0.5 * n as f32).collect();
        let hats: Vec<f32> = kicks.iter().map(|k| k + 0.25).collect();
        let found: Vec<f32> = detections(&render(&kicks, &hats, Some((0.0, 0.15)), 4.5))
            .into_iter()
            .filter(|&t| t > WARM_UP)
            .collect();
        assert_detected(&kicks, &found);
    }

    /// Fin de break : charleston seul, puis basse et kicks qui reprennent ensemble à 128 BPM.
    #[test]
    fn catches_the_kicks_of_a_drop() {
        let drop = 1.0;
        let beat = 60.0 / 128.0;
        let kicks: Vec<f32> = (0..6).map(|n| drop + beat * n as f32).collect();
        let hats: Vec<f32> = (0..12).map(|n| 0.1 + beat / 2.0 * n as f32).collect();
        let found = detections(&render(&kicks, &hats, Some((drop, 0.3)), 4.0));
        assert_detected(&kicks, &found);
    }

    #[test]
    fn ignores_hats_and_sustained_bass() {
        let hats: Vec<f32> = (0..16).map(|n| 0.1 + 0.25 * n as f32).collect();
        assert_eq!(detections(&render(&[], &hats, None, 4.5)), Vec::<f32>::new());
        let late: Vec<f32> = detections(&render(&[], &hats, Some((0.0, 0.15)), 4.5))
            .into_iter()
            .filter(|&t| t > WARM_UP)
            .collect();
        assert_eq!(late, Vec::<f32>::new());
    }

    /// Mix dense : kicks à peine plus forts (×1,4) qu'une basse roulante, trois notes par temps
    /// entre les kicks, qui ne laisse presque jamais redescendre le grave.
    #[test]
    fn groove_catches_kicks_buried_in_a_rolling_bassline() {
        let beat = 60.0 / 155.0;
        let kicks: Vec<f32> = (0..12).map(|n| 0.5 + beat * n as f32).collect();
        let notes: Vec<f32> =
            kicks.iter().flat_map(|&k| (1..4).map(move |i| k + beat * i as f32 / 4.0)).collect();
        let signal: Vec<f32> = (0..(6.0 * RATE) as usize)
            .map(|i| {
                let t = i as f32 / RATE;
                let k: f32 = kicks.iter().map(|&start| 0.4 * kick(t - start)).sum();
                let b: f32 = notes.iter().map(|&start| 0.5 * bass_note(t - start)).sum();
                k + b
            })
            .collect();
        let is_kick = |h: f32| kicks.iter().any(|&k| (0.0..0.015).contains(&(h - k)));
        let report = |hits: &[f32]| {
            let caught = kicks.iter().filter(|&&k| hits.iter().any(|&h| (0.0..0.015).contains(&(h - k)))).count();
            (caught, hits.iter().filter(|&&h| !is_kick(h)).count())
        };

        let blind = report(&detections(&signal));
        let guided = report(&detections_in_groove(&signal, &kicks));
        println!("mix dense (kicks, fausses détections) : sans groove {blind:?}, avec {guided:?}");
        assert_eq!(guided, (kicks.len(), 0));
    }

    #[test]
    fn bands_follow_their_frequencies() {
        let tone = |freq: f32| -> Levels {
            let mut analyzer = Analyzer::new(RATE, DEFAULT_THRESHOLD);
            for i in 0..(RATE as usize) {
                analyzer.process(0.5 * (TAU * freq * i as f32 / RATE).sin(), Expectation::Free);
            }
            analyzer.levels()
        };
        let low = tone(60.0);
        assert!(low.bass > 0.8 && low.high < 0.05, "{low:?}");
        let high = tone(9000.0);
        assert!(high.high > 0.8 && high.bass < 0.05, "{high:?}");
    }
}

//! Signaux de synthèse pour les tests : kicks, charleston, basse roulante.

use std::f32::consts::TAU;

pub const RATE: f32 = 48_000.0;

/// Kick : sinus glissant de 150 à 50 Hz, décroissance de 150 ms.
pub fn kick(t: f32) -> f32 {
    if !(0.0..1.0).contains(&t) {
        return 0.0;
    }
    let sweep = 0.03;
    let phase = TAU * (50.0 * t + 100.0 * sweep * (1.0 - (-t / sweep).exp()));
    0.8 * (-t / 0.15).exp() * phase.sin()
}

/// Enveloppe d'un coup de charleston (à appliquer à du bruit filtré dans les aigus).
pub fn hat_envelope(t: f32) -> f32 {
    if (0.0..0.3).contains(&t) { 0.3 * (-t / 0.03).exp() } else { 0.0 }
}

/// Note de basse roulante : 70 Hz, une double croche de long.
pub fn bass_note(t: f32) -> f32 {
    if !(0.0..0.09).contains(&t) {
        return 0.0;
    }
    0.45 * (-t / 0.3).exp() * (TAU * 70.0 * t).sin()
}

/// Bruit blanc pseudo-aléatoire et reproductible.
pub struct Noise(pub u32);

impl Noise {
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

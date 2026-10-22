//! Rendu du mode live : flash sur chaque kick ou note MIDI, lueur qui suit les basses, teinte
//! qui glisse du rouge (son grave) vers le bleu (son brillant), ou couleur de la dernière note.

use std::time::Duration;

use crate::audio::analysis::Levels;
use crate::hue::stream::Rgb;

const FLASH_DECAY: f32 = 0.12;
const HUE_SMOOTHING: f32 = 0.4;
const AMBIENT: f32 = 0.04;
const GLOW: f32 = 0.25;
/// Part de blanc au sommet d'un flash de kick, pour un impact plus franc.
const WHITE_PUNCH: f32 = 0.5;
const WARMEST_HUE: f32 = 0.0;
const COOLEST_HUE: f32 = 0.7;
/// Intensité d'un flash de note : de 35 % (vélocité minimale) à 100 %.
const NOTE_MIN_STRENGTH: f32 = 0.35;

#[derive(Debug, Default)]
pub struct ReactiveLook {
    flash: f32,
    /// Part de blanc du flash en cours.
    punch: f32,
    /// Flash déclenché depuis le dernier rendu : il s'affiche sans décroissance.
    fresh: bool,
    hue: f32,
    /// Couleur de la dernière note MIDI ; sans note, la teinte suit le spectre.
    note_hue: Option<f32>,
}

impl ReactiveLook {
    /// Flash de kick, à pleine intensité.
    pub fn kick(&mut self) {
        self.flash = 1.0;
        self.punch = WHITE_PUNCH;
        self.fresh = true;
    }

    /// Flash de note, à sa couleur (teinte dans [0, 1[) ; la lueur garde ensuite cette couleur.
    pub fn note(&mut self, hue: f32, velocity: u8) {
        let strength = NOTE_MIN_STRENGTH + (1.0 - NOTE_MIN_STRENGTH) * f32::from(velocity.min(127)) / 127.0;
        self.note_hue = Some(hue);
        self.hue = hue;
        if strength >= self.flash {
            self.flash = strength;
            self.punch = 0.0;
            self.fresh = true;
        }
    }

    /// Éteint le flash en cours (kick anticipé qui n'est pas venu).
    pub fn cancel_flash(&mut self) {
        self.flash = 0.0;
    }

    /// Couleur à afficher, `dt` après le rendu précédent.
    pub fn render(&mut self, dt: Duration, levels: Levels) -> Rgb {
        let dt = dt.as_secs_f32();
        if !self.fresh {
            self.flash *= (-dt / FLASH_DECAY).exp();
        }
        self.fresh = false;

        let target = self.note_hue.unwrap_or_else(|| {
            let brilliance = levels.high / (levels.bass + levels.high + 1e-3);
            WARMEST_HUE + (COOLEST_HUE - WARMEST_HUE) * brilliance
        });
        self.hue += (target - self.hue) * (1.0 - (-dt / HUE_SMOOTHING).exp());

        // Le flash part de la lueur de fond et monte jusqu'à son intensité.
        let glow = (AMBIENT + GLOW * levels.bass).min(1.0);
        let value = glow + (1.0 - glow) * self.flash;
        Rgb::from_hsv(self.hue, 1.0 - self.punch * self.flash, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);

    fn brightness(color: Rgb) -> u16 {
        color.r.max(color.g).max(color.b)
    }

    #[test]
    fn kick_flashes_then_fades_back_to_ambient() {
        let mut look = ReactiveLook::default();
        let quiet = Levels::default();
        look.kick();
        assert_eq!(brightness(look.render(FRAME, quiet)), u16::MAX);
        let mut color = Rgb::OFF;
        for _ in 0..60 {
            color = look.render(FRAME, quiet);
        }
        assert!(brightness(color) < u16::MAX / 10, "{color:?}");
    }

    #[test]
    fn bass_heavy_sound_is_warmer_than_bright_sound() {
        let settle = |levels: Levels| {
            let mut look = ReactiveLook::default();
            for _ in 0..120 {
                look.render(FRAME, levels);
            }
            look.hue
        };
        let drop = settle(Levels { bass: 0.9, mid: 0.5, high: 0.2 });
        let breakdown = settle(Levels { bass: 0.05, mid: 0.4, high: 0.8 });
        assert!(drop < breakdown, "teinte drop {drop}, break {breakdown}");
    }

    #[test]
    fn notes_flash_in_their_color_and_keep_it() {
        let mut look = ReactiveLook::default();
        // Orange (30°), pleine vélocité : saturé et à pleine intensité.
        look.note(1.0 / 12.0, 127);
        let color = look.render(FRAME, Levels::default());
        assert_eq!((color.r, color.b), (u16::MAX, 0), "{color:?}");
        assert!(color.g > u16::MAX / 3 && color.g < u16::MAX / 3 * 2, "{color:?}");

        // Un son brillant ne fait plus glisser la teinte vers le bleu.
        let bright = Levels { bass: 0.05, mid: 0.4, high: 0.9 };
        let mut later = color;
        for _ in 0..120 {
            later = look.render(FRAME, bright);
        }
        assert!(later.r > later.b, "{later:?}");

        // Une note douce flashe moins fort.
        let mut soft = ReactiveLook::default();
        soft.note(1.0 / 12.0, 20);
        assert!(brightness(soft.render(FRAME, Levels::default())) < u16::MAX / 2);
    }
}

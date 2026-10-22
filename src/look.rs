//! Rendu du mode réactif : flash sur chaque kick, lueur qui suit les basses,
//! teinte qui glisse du rouge (son grave) vers le bleu (son brillant).

use std::time::Duration;

use crate::audio::analysis::Levels;
use crate::hue::stream::Rgb;

const FLASH_DECAY: f32 = 0.12;
const HUE_SMOOTHING: f32 = 0.4;
const AMBIENT: f32 = 0.04;
const GLOW: f32 = 0.25;
/// Part de blanc au sommet du flash, pour un impact plus franc.
const WHITE_PUNCH: f32 = 0.5;
const WARMEST_HUE: f32 = 0.0;
const COOLEST_HUE: f32 = 0.7;

#[derive(Debug, Default)]
pub struct ReactiveLook {
    flash: f32,
    hue: f32,
}

impl ReactiveLook {
    /// Éteint le flash en cours (kick anticipé qui n'est pas venu).
    pub fn cancel_flash(&mut self) {
        self.flash = 0.0;
    }

    pub fn update(&mut self, dt: Duration, kick: bool, levels: Levels) -> Rgb {
        let dt = dt.as_secs_f32();
        self.flash = if kick { 1.0 } else { self.flash * (-dt / FLASH_DECAY).exp() };

        let brilliance = levels.high / (levels.bass + levels.high + 1e-3);
        let target = WARMEST_HUE + (COOLEST_HUE - WARMEST_HUE) * brilliance;
        self.hue += (target - self.hue) * (1.0 - (-dt / HUE_SMOOTHING).exp());

        // Le flash part de la lueur de fond et monte toujours jusqu'à la pleine luminosité.
        let glow = (AMBIENT + GLOW * levels.bass).min(1.0);
        let value = glow + (1.0 - glow) * self.flash;
        Rgb::from_hsv(self.hue, 1.0 - WHITE_PUNCH * self.flash, value)
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
        let flash = look.update(FRAME, true, quiet);
        assert_eq!(brightness(flash), u16::MAX);
        let mut color = flash;
        for _ in 0..60 {
            color = look.update(FRAME, false, quiet);
        }
        assert!(brightness(color) < u16::MAX / 10, "{color:?}");
    }

    #[test]
    fn bass_heavy_sound_is_warmer_than_bright_sound() {
        let settle = |levels: Levels| {
            let mut look = ReactiveLook::default();
            for _ in 0..120 {
                look.update(FRAME, false, levels);
            }
            look.hue
        };
        let drop = settle(Levels { bass: 0.9, mid: 0.5, high: 0.2 });
        let breakdown = settle(Levels { bass: 0.05, mid: 0.4, high: 0.8 });
        assert!(drop < breakdown, "teinte drop {drop}, break {breakdown}");
    }
}

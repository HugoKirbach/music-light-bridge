//! Rendu du mode live à partir de trois couleurs choisies : la principale pour le groove, une
//! pour les temps creux (plus de kick depuis un moment) et une pour les filtres (passe-haut ou
//! passe-bas qui déséquilibre fortement grave et aigu). La musique dose la luminosité : flash
//! sur chaque kick ou note MIDI, lueur qui suit les basses.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audio::analysis::Levels;
use crate::hue::stream::Rgb;

/// Part de la lueur qui suit les basses, au-dessus du fond.
const GLOW: f32 = 0.25;
/// Part de blanc au sommet d'un flash de kick, pour un impact plus franc.
const WHITE_PUNCH: f32 = 0.5;
/// Intensité d'un flash de note : de 35 % (vélocité minimale) à 100 %.
const NOTE_MIN_STRENGTH: f32 = 0.35;
/// Sans kick depuis 1,5 s, la musique est dans un temps creux.
const CALM_AFTER: f32 = 1.5;
/// Durée du fondu d'une couleur de scène à l'autre.
const SCENE_FADE: f32 = 0.6;
/// Mémoire de l'équilibre grave/aigu habituel du morceau.
const TILT_MEMORY: f32 = 30.0;
/// Écart à cet équilibre (logarithme du rapport grave/aigu) où un filtre commence à se voir
/// (≈ 14 dB) et où il est pleinement établi (≈ 26 dB).
const FILTER_START: f32 = 1.6;
const FILTER_FULL: f32 = 3.0;

/// Les trois couleurs choisies, en sRGB 8 bits. Leur luminosité plafonne celle de la scène :
/// du noir en temps creux éteint la lampe pendant les breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    /// Groove : kicks réguliers, son équilibré.
    pub primary: [u8; 3],
    /// Temps creux : break, passage sans kick.
    pub calm: [u8; 3],
    /// Filtres passe-haut ou passe-bas.
    pub filtered: [u8; 3],
}

impl Default for Palette {
    fn default() -> Self {
        Self { primary: [255, 40, 0], calm: [40, 70, 255], filtered: [180, 0, 255] }
    }
}

/// Allure des flashs, réglable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dynamics {
    /// Jusqu'où monte un flash, entre la lueur de fond (0) et la pleine luminosité (1).
    pub flash: f32,
    /// Temps de redescente d'un flash (s) : il a perdu ~63 % de son éclat au bout de ce temps.
    pub decay: f32,
    /// Lueur de fond minimale, que la lampe garde même sans son (0 à 1).
    pub floor: f32,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self { flash: 1.0, decay: 0.12, floor: 0.04 }
    }
}

type Color = [f32; 3];

fn color(srgb: [u8; 3]) -> Color {
    srgb.map(|c| f32::from(c) / 255.0)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// Rapproche `current` de `target` avec une constante de temps `time`.
fn approach(current: f32, target: f32, dt: f32, time: f32) -> f32 {
    current + (target - current) * (1.0 - (-dt / time).exp())
}

#[derive(Debug, Default)]
pub struct ReactiveLook {
    pub palette: Palette,
    pub dynamics: Dynamics,
    flash: f32,
    /// Part de blanc du flash en cours.
    punch: f32,
    /// Flash déclenché depuis le dernier rendu : il s'affiche sans décroissance.
    fresh: bool,
    /// Secondes écoulées depuis le dernier kick détecté.
    since_kick: f32,
    /// Part des couleurs « temps creux » et « filtres » dans la scène, en fondu.
    calm: f32,
    filtered: f32,
    /// Équilibre grave/aigu habituel du morceau.
    tilt_reference: Option<f32>,
    /// Couleur de la dernière note MIDI ; elle remplace celle de la scène.
    note_color: Option<Color>,
}

impl ReactiveLook {
    pub fn new(palette: Palette) -> Self {
        Self { palette, ..Self::default() }
    }

    /// Flash de kick, à pleine intensité.
    pub fn kick(&mut self) {
        self.flash = 1.0;
        self.punch = WHITE_PUNCH;
        self.fresh = true;
    }

    /// Un kick vient d'être détecté (flashé ou non) : la musique n'est pas dans un temps creux.
    pub fn kick_detected(&mut self) {
        self.since_kick = 0.0;
    }

    /// Flash de note, à sa couleur (teinte dans [0, 1[) ; la lueur garde ensuite cette couleur.
    pub fn note(&mut self, hue: f32, velocity: u8) {
        let strength = NOTE_MIN_STRENGTH + (1.0 - NOTE_MIN_STRENGTH) * f32::from(velocity.min(127)) / 127.0;
        let rgb = Rgb::from_hsv(hue, 1.0, 1.0);
        self.note_color = Some([rgb.r, rgb.g, rgb.b].map(|c| f32::from(c) / f32::from(u16::MAX)));
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
            self.flash *= (-dt / self.dynamics.decay.max(0.01)).exp();
        }
        self.fresh = false;
        self.since_kick += dt;

        let calm = if self.since_kick > CALM_AFTER { 1.0 } else { 0.0 };
        let filtered = self.filter_amount(levels.tilt, dt);
        self.calm = approach(self.calm, calm, dt, SCENE_FADE);
        self.filtered = approach(self.filtered, filtered, dt, SCENE_FADE);
        let scene = mix(
            mix(color(self.palette.primary), color(self.palette.calm), self.calm),
            color(self.palette.filtered),
            self.filtered,
        );
        let base = self.note_color.unwrap_or(scene);

        // Le flash part de la lueur de fond et monte, selon l'intensité choisie, vers le plein.
        let floor = self.dynamics.floor.clamp(0.0, 1.0);
        let glow = (floor + GLOW * (1.0 - floor) * levels.bass).min(1.0);
        let flash = self.flash * self.dynamics.flash.clamp(0.0, 1.0);
        let value = glow + (1.0 - glow) * flash;
        let white = self.punch * flash;
        let [r, g, b] = base.map(|c| ((c * (1.0 - white) + white) * value).clamp(0.0, 1.0));
        let quantize = |u: f32| (u * f32::from(u16::MAX)) as u16;
        Rgb { r: quantize(r), g: quantize(g), b: quantize(b) }
    }

    /// Part visée de la couleur « filtres » : écart entre l'équilibre grave/aigu actuel et
    /// l'habituel du morceau. Pendant un filtre, la référence ne bouge pas.
    fn filter_amount(&mut self, tilt: Option<f32>, dt: f32) -> f32 {
        let Some(tilt) = tilt else { return 0.0 };
        let reference = *self.tilt_reference.get_or_insert(tilt);
        if self.filtered < 0.5 {
            self.tilt_reference = Some(approach(reference, tilt, dt, TILT_MEMORY));
        }
        (((tilt - reference).abs() - FILTER_START) / (FILTER_FULL - FILTER_START)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);
    const FRAMES_PER_SECOND: usize = 60;

    /// Rouge pour le groove, bleu pour les temps creux, vert pour les filtres.
    const PALETTE: Palette = Palette { primary: [255, 0, 0], calm: [0, 0, 255], filtered: [0, 255, 0] };

    fn brightness(color: Rgb) -> u16 {
        color.r.max(color.g).max(color.b)
    }

    /// Joue `seconds` de musique d'équilibre `tilt`, avec un kick toutes les 0,4 s si `kicks`.
    fn play(look: &mut ReactiveLook, seconds: f32, tilt: f32, kicks: bool) -> Rgb {
        let levels = Levels { bass: 1.0, tilt: Some(tilt), ..Levels::default() };
        let mut color = Rgb::OFF;
        for frame in 0..(seconds * FRAMES_PER_SECOND as f32) as usize {
            if kicks && frame % 24 == 0 {
                look.kick_detected();
            }
            color = look.render(FRAME, levels);
        }
        color
    }

    #[test]
    fn kick_flashes_then_fades_back_to_ambient() {
        let mut look = ReactiveLook::default();
        let quiet = Levels::default();
        look.kick();
        assert_eq!(brightness(look.render(FRAME, quiet)), u16::MAX);
        let mut color = Rgb::OFF;
        for _ in 0..FRAMES_PER_SECOND {
            color = look.render(FRAME, quiet);
        }
        assert!(brightness(color) < u16::MAX / 10, "{color:?}");
    }

    #[test]
    fn the_groove_shows_the_primary_color_and_a_break_the_calm_one() {
        let mut look = ReactiveLook::new(PALETTE);
        let groove = play(&mut look, 5.0, 1.0, true);
        assert!(groove.r > 10 * groove.b.max(groove.g).max(1), "groove : {groove:?}");
        let calm = play(&mut look, 4.0, 1.0, false);
        assert!(calm.b > 10 * calm.r.max(calm.g).max(1), "temps creux : {calm:?}");
        let back = play(&mut look, 3.0, 1.0, true);
        assert!(back.r > 10 * back.b.max(back.g).max(1), "retour du groove : {back:?}");
    }

    #[test]
    fn a_filter_sweep_shows_the_filter_color() {
        let mut look = ReactiveLook::new(PALETTE);
        play(&mut look, 40.0, 1.0, true);
        // Passe-bas : les aigus chutent de ~35 dB, les kicks continuent.
        let low_pass = play(&mut look, 3.0, 5.0, true);
        assert!(low_pass.g > 10 * low_pass.r.max(low_pass.b).max(1), "passe-bas : {low_pass:?}");
        let back = play(&mut look, 3.0, 1.0, true);
        assert!(back.r > 10 * back.g.max(back.b).max(1), "fin du filtre : {back:?}");
        // Passe-haut : les basses chutent.
        let high_pass = play(&mut look, 3.0, -3.0, true);
        assert!(high_pass.g > 10 * high_pass.r.max(high_pass.b).max(1), "passe-haut : {high_pass:?}");
    }

    /// Luminosité un temps après un flash, avec l'allure donnée (lampe blanche, quelle que soit
    /// la scène, pour ne mesurer que la luminosité).
    fn after_flash(dynamics: Dynamics, frames: usize) -> u16 {
        let white = [255, 255, 255];
        let palette = Palette { primary: white, calm: white, filtered: white };
        let mut look = ReactiveLook { dynamics, palette, ..ReactiveLook::default() };
        look.kick();
        let mut color = look.render(FRAME, Levels::default());
        for _ in 0..frames {
            color = look.render(FRAME, Levels::default());
        }
        brightness(color)
    }

    #[test]
    fn flash_intensity_decay_and_floor_are_adjustable() {
        let soft = Dynamics { flash: 0.4, ..Dynamics::default() };
        let peak = after_flash(soft, 0);
        assert!(peak > u16::MAX / 3 && peak < u16::MAX / 2, "flash adouci : {peak}");

        // Redescente lente : encore bien visible au bout de 0,3 s, contre presque rien par défaut.
        let slow = Dynamics { decay: 0.6, ..Dynamics::default() };
        assert!(after_flash(slow, 18) > 3 * after_flash(Dynamics::default(), 18));

        // Avec une lueur de fond, la lampe ne retombe jamais sous ce niveau.
        let floor = Dynamics { floor: 0.3, ..Dynamics::default() };
        let rest = after_flash(floor, 120);
        assert!(rest >= (0.3 * f32::from(u16::MAX)) as u16 - 1, "fond : {rest}");
    }

    #[test]
    fn black_as_calm_color_turns_the_lamp_off_in_breaks() {
        let mut look = ReactiveLook::new(Palette { calm: [0, 0, 0], ..PALETTE });
        play(&mut look, 2.0, 1.0, true);
        let calm = play(&mut look, 5.0, 1.0, false);
        assert!(brightness(calm) < u16::MAX / 100, "{calm:?}");
    }

    #[test]
    fn notes_flash_in_their_color_and_keep_it() {
        let mut look = ReactiveLook::default();
        // Orange (30°), pleine vélocité : saturé et à pleine intensité.
        look.note(1.0 / 12.0, 127);
        let color = look.render(FRAME, Levels::default());
        assert_eq!((color.r, color.b), (u16::MAX, 0), "{color:?}");
        assert!(color.g > u16::MAX / 3 && color.g < u16::MAX / 3 * 2, "{color:?}");

        // Les couleurs de scène ne reprennent pas le dessus.
        let later = play(&mut look, 3.0, 1.0, false);
        assert!(later.r > later.b, "{later:?}");

        // Une note douce flashe moins fort.
        let mut soft = ReactiveLook::default();
        soft.note(1.0 / 12.0, 20);
        assert!(brightness(soft.render(FRAME, Levels::default())) < u16::MAX / 2);
    }
}

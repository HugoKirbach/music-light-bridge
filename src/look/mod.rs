//! Rendu du mode live à partir de trois couleurs choisies : la principale pour le groove, une
//! pour les temps creux (plus de kick depuis un moment) et une pour les filtres (passe-haut ou
//! passe-bas qui déséquilibre fortement grave et aigu). La musique dose la luminosité : flash
//! sur chaque kick ou note MIDI, lueur qui suit les basses.

use std::f32::consts::TAU;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audio::analysis::Levels;
use crate::hue::stream::Rgb;

mod lamps;
mod phases;

pub use lamps::{Accent, Spread};
pub use phases::{Phase, Pulse};
use lamps::Lamp;
use phases::PhaseTracker;

/// Part de la lueur qui suit les basses, au-dessus du fond.
const GLOW: f32 = 0.25;
/// Part de blanc au sommet d'un flash de kick, pour un impact plus franc.
const WHITE_PUNCH: f32 = 0.5;
/// En tête de phrase, toutes les lampes flashent, avec plus de blanc.
const PHRASE_PUNCH: f32 = 0.85;
/// Intensité d'un flash de note : de 35 % (vélocité minimale) à 100 %.
const NOTE_MIN_STRENGTH: f32 = 0.35;
/// Sans kick depuis 1,5 s, la musique est dans un temps creux.
/// Durée du fondu d'une couleur de scène à l'autre.
const SCENE_FADE: f32 = 0.6;
/// Mémoire de l'équilibre grave/aigu habituel du morceau.
const TILT_MEMORY: f32 = 30.0;
/// Écart à cet équilibre (logarithme du rapport grave/aigu) où un filtre commence à se voir
/// (≈ 14 dB) et où il est pleinement établi (≈ 26 dB).
const FILTER_START: f32 = 1.6;
const FILTER_FULL: f32 = 3.0;

/// Respiration : la luminosité suit l'énergie, vite en montée et lentement en descente ; un
/// kick n'y ajoute qu'une légère houle.
const BREATH_ATTACK: f32 = 0.35;
const BREATH_RELEASE: f32 = 0.9;
const BREATH_KICK: f32 = 0.25;
/// Fondu d'une animation à l'autre.
const MOTION_FADE: f32 = 0.4;
/// Vague : le flash met 120 ms à traverser la pièce.
const WAVE_SWEEP: f32 = 0.12;
/// Ondulation des breaks en vague : elle traverse la pièce en 4 s et creuse la respiration de 35 %.
const RIPPLE_PERIOD: f32 = 4.0;
const RIPPLE_DEPTH: f32 = 0.35;
/// Lampes « aigus » (graves / aigus) : jusqu'à 70 % de luminosité avec médiums et aigus.
const TREBLE_GLOW: f32 = 0.7;

/// Animation de la lumière.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    /// Suit les phases du morceau : pulse sur le groove, respiration en break, montée, drop
    #[default]
    Auto,
    /// Un flash par kick
    Pulse,
    /// La luminosité suit l'énergie, sans flash net
    Breath,
    /// La lumière s'intensifie et blanchit, flashs de plus en plus forts
    BuildUp,
}

impl Motion {
    pub const ALL: [Self; 4] = [Self::Auto, Self::Pulse, Self::Breath, Self::BuildUp];

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => "Auto : suit les phases du morceau",
            Self::Pulse => "Pulse : un flash par kick",
            Self::Breath => "Respiration : suit l'énergie, sans flash net",
            Self::BuildUp => "Montée : s'intensifie et blanchit",
        }
    }

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(index: u8) -> Self {
        Self::ALL.get(usize::from(index)).copied().unwrap_or(Self::Auto)
    }
}

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


/// Répartition des flashs entre les lampes de la zone, selon leur place dans la pièce.

#[derive(Debug)]
pub struct ReactiveLook {
    pub palette: Palette,
    pub dynamics: Dynamics,
    pub motion: Motion,
    pub spread: Spread,
    phases: PhaseTracker,
    /// Part de chaque animation (pulse, respiration, montée) dans le rendu, en fondu.
    weights: [f32; 3],
    /// Luminosité de la respiration, et énergie des médiums et aigus (lampes « aigus »).
    breath: f32,
    treble: f32,
    lamps: Vec<Lamp>,
    colors: Vec<Rgb>,
    /// Kicks flashés : leur parité fait alterner gauche et droite, et le sens de la vague.
    kicks: u64,
    /// Horloge propre (s), pour l'ondulation des breaks.
    time: f32,
    /// Secondes écoulées depuis le dernier kick détecté.
    since_kick: f32,
    /// La grille de temps confirme des kicks réguliers (`None` tant que le tempo est inconnu).
    grid_groove: Option<bool>,
    /// Part des couleurs « temps creux » et « filtres » dans la scène, en fondu.
    calm: f32,
    filtered: f32,
    /// Équilibre grave/aigu habituel du morceau.
    tilt_reference: Option<f32>,
    /// Couleur de la dernière note MIDI ; elle remplace celle de la scène.
    note_color: Option<Color>,
}

impl Default for ReactiveLook {
    fn default() -> Self {
        Self {
            palette: Palette::default(),
            dynamics: Dynamics::default(),
            motion: Motion::default(),
            spread: Spread::default(),
            phases: PhaseTracker::default(),
            weights: [0.0; 3],
            breath: 0.0,
            treble: 0.0,
            lamps: vec![Lamp::default()],
            colors: vec![Rgb::OFF],
            kicks: 0,
            time: 0.0,
            since_kick: 0.0,
            grid_groove: None,
            calm: 0.0,
            filtered: 0.0,
            tilt_reference: None,
            note_color: None,
        }
    }
}

impl ReactiveLook {
    pub fn new(palette: Palette) -> Self {
        Self { palette, ..Self::default() }
    }

    /// Place des lampes dans la pièce, de gauche à droite (x de la zone Hue, de -1 à 1), dans
    /// l'ordre où `render_lamps` rendra leurs couleurs.
    pub fn set_lamps(&mut self, positions: &[f32]) {
        let count = positions.len().max(1);
        let x = |i: usize| positions.get(i).copied().unwrap_or(0.0);
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by(|&a, &b| x(a).total_cmp(&x(b)).then(a.cmp(&b)));
        self.lamps = vec![Lamp::default(); count];
        for (rank, &i) in order.iter().enumerate() {
            let lamp = &mut self.lamps[i];
            lamp.place = if count > 1 { rank as f32 / (count - 1) as f32 } else { 0.0 };
            lamp.right = count > 1 && rank >= count / 2;
            lamp.odd = rank % 2 == 1;
        }
        self.colors = vec![Rgb::OFF; count];
    }

    /// Flash de kick, à pleine intensité, sans savoir où il tombe dans la mesure : un kick sur
    /// quatre compte comme une tête de mesure.
    pub fn kick(&mut self) {
        let accent = if self.kicks.is_multiple_of(4) { Accent::Bar } else { Accent::Beat };
        self.kick_at(accent);
    }

    /// Flash de kick, réparti entre les lampes selon la disposition et la place du kick.
    pub fn kick_at(&mut self, accent: Accent) {
        let spread = self.active_spread();
        let single = self.lamps.len() == 1;
        let parity = self.kicks % 2 == 1;
        self.kicks += 1;
        for lamp in &mut self.lamps {
            match spread {
                _ if accent == Accent::Phrase => lamp.fire(1.0, PHRASE_PUNCH),
                _ if single => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Auto | Spread::Together => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Wave => {
                    let distance = if parity { 1.0 - lamp.place } else { lamp.place };
                    lamp.pending = Some((WAVE_SWEEP * distance, 1.0, WHITE_PUNCH));
                }
                Spread::Alternate if accent != Accent::Beat || lamp.right == parity => {
                    lamp.fire(1.0, WHITE_PUNCH);
                }
                Spread::Alternate => {}
                Spread::Split if !lamp.odd => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Split => {}
            }
        }
    }

    /// État de la grille de temps à cet instant : des kicks réguliers sur les temps (`Some(true)`),
    /// ou non, ou tempo inconnu (`None`).
    pub fn set_grid_groove(&mut self, groove: Option<bool>) {
        self.grid_groove = groove;
    }

    /// Un kick vient d'être détecté (flashé ou non) : la musique n'est pas dans un temps creux.
    pub fn kick_detected(&mut self) {
        self.since_kick = 0.0;
        self.phases.kick();
    }

    /// Phase du morceau reconnue à l'écoute.
    pub fn phase(&self) -> Phase {
        self.phases.phase
    }

    /// Animation effectivement jouée : celle choisie, ou celle de la phase en mode Auto.
    pub fn active_motion(&self) -> Motion {
        match (self.motion, self.phases.phase) {
            (Motion::Auto, Phase::Groove) => Motion::Pulse,
            (Motion::Auto, Phase::Calm) => Motion::Breath,
            (Motion::Auto, Phase::BuildUp) => Motion::BuildUp,
            (motion, _) => motion,
        }
    }

    /// Disposition effectivement jouée : celle choisie, ou celle de la phase en mode Auto.
    pub fn active_spread(&self) -> Spread {
        match (self.spread, self.phases.phase) {
            (Spread::Auto, Phase::Groove) => Spread::Alternate,
            (Spread::Auto, Phase::BuildUp | Phase::Calm) => Spread::Wave,
            (spread, _) => spread,
        }
    }

    /// Flash de note, à sa couleur (teinte dans [0, 1[), sur toutes les lampes ; la lueur garde
    /// ensuite cette couleur.
    pub fn note(&mut self, hue: f32, velocity: u8) {
        let strength = NOTE_MIN_STRENGTH + (1.0 - NOTE_MIN_STRENGTH) * f32::from(velocity.min(127)) / 127.0;
        let rgb = Rgb::from_hsv(hue, 1.0, 1.0);
        self.note_color = Some([rgb.r, rgb.g, rgb.b].map(|c| f32::from(c) / f32::from(u16::MAX)));
        for lamp in &mut self.lamps {
            lamp.fire(strength, 0.0);
        }
    }

    /// Éteint les flashs en cours (kick anticipé qui n'est pas venu).
    pub fn cancel_flash(&mut self) {
        for lamp in &mut self.lamps {
            (lamp.flash, lamp.pending) = (0.0, None);
        }
    }

    /// Couleur de la première lampe, `dt` après le rendu précédent.
    pub fn render(&mut self, dt: Duration, levels: Levels) -> Rgb {
        self.render_lamps(dt, levels)[0]
    }

    /// Couleur de chaque lampe, dans l'ordre de `set_lamps`, `dt` après le rendu précédent.
    pub fn render_lamps(&mut self, dt: Duration, levels: Levels) -> &[Rgb] {
        let dt = dt.as_secs_f32();
        self.time += dt;
        self.since_kick += dt;
        let decay = (-dt / self.dynamics.decay.max(0.01)).exp();
        for lamp in &mut self.lamps {
            if !std::mem::take(&mut lamp.fresh) {
                lamp.flash *= decay;
            }
            if let Some((delay, strength, punch)) = lamp.pending {
                if delay - dt <= 0.0 {
                    lamp.pending = None;
                    lamp.fire(strength, punch);
                } else {
                    lamp.pending = Some((delay - dt, strength, punch));
                }
            }
        }

        let pulse = Pulse { since_kick: self.since_kick, grid_groove: self.grid_groove };
        let dropped = self.phases.update(dt, levels, pulse);
        if dropped && matches!(self.motion, Motion::Auto | Motion::BuildUp) {
            // Le drop : un éclat blanc à pleine puissance, partout.
            for lamp in &mut self.lamps {
                lamp.fire(1.0, 1.0);
            }
        }
        let target = match self.active_motion() {
            Motion::Breath => 1,
            Motion::BuildUp => 2,
            Motion::Auto | Motion::Pulse => 0,
        };
        if self.weights == [0.0; 3] {
            self.weights[target] = 1.0;
        }
        for (i, weight) in self.weights.iter_mut().enumerate() {
            *weight = approach(*weight, if i == target { 1.0 } else { 0.0 }, dt, MOTION_FADE);
        }

        let calm = if self.phases.grooving { 0.0 } else { 1.0 };
        let filtered = self.filter_amount(levels.tilt, dt);
        self.calm = approach(self.calm, calm, dt, SCENE_FADE);
        self.filtered = approach(self.filtered, filtered, dt, SCENE_FADE);
        let scene = mix(
            mix(color(self.palette.primary), color(self.palette.calm), self.calm),
            color(self.palette.filtered),
            self.filtered,
        );
        let base = self.note_color.unwrap_or(scene);
        let floor = self.dynamics.floor.clamp(0.0, 1.0);
        let intensity = self.dynamics.flash.clamp(0.0, 1.0);
        let glow = (floor + GLOW * (1.0 - floor) * levels.bass).min(1.0);
        let energy = 0.5 * levels.bass + 0.3 * levels.mid + 0.2 * levels.high;
        let breath_time = if energy > self.breath { BREATH_ATTACK } else { BREATH_RELEASE };
        self.breath = approach(self.breath, energy, dt, breath_time);
        let treble = 0.5 * (levels.mid + levels.high);
        let treble_time = if treble > self.treble { BREATH_ATTACK } else { BREATH_RELEASE };
        self.treble = approach(self.treble, treble, dt, treble_time);
        let build = self.phases.build;
        let spread = self.active_spread();
        let [w_pulse, w_breath, w_build] = self.weights;
        let total = (w_pulse + w_breath + w_build).max(1e-6);
        let several = self.lamps.len() > 1;

        for (lamp, out) in self.lamps.iter().zip(&mut self.colors) {
            let flash = lamp.flash * intensity;
            // Pulse : le flash part de la lueur de fond et monte, selon l'intensité choisie.
            let pulse = (glow + (1.0 - glow) * flash, lamp.punch * flash);
            // Respiration : suit l'énergie, un kick n'ajoute qu'une houle. En vague, une
            // ondulation lente traverse la pièce.
            let ripple = if several && spread == Spread::Wave {
                1.0 - RIPPLE_DEPTH * (0.5 + 0.5 * (TAU * (self.time / RIPPLE_PERIOD - lamp.place)).sin())
            } else {
                1.0
            };
            let breathing = 0.8 * self.breath * ripple + BREATH_KICK * flash;
            let breath = ((floor + (1.0 - floor) * breathing).min(1.0), 0.0);
            // Montée : la lumière s'intensifie et blanchit, les flashs gagnent en force.
            let tension = floor + (1.0 - floor) * (0.15 + 0.55 * build);
            let strength = flash * (0.4 + 0.6 * build);
            let build_up = (tension + (1.0 - tension) * strength, (lamp.punch * strength + 0.4 * build).min(1.0));

            let (mut value, mut white) = (
                (w_pulse * pulse.0 + w_breath * breath.0 + w_build * build_up.0) / total,
                (w_pulse * pulse.1 + w_breath * breath.1 + w_build * build_up.1) / total,
            );
            if several && spread == Spread::Split && lamp.odd {
                // Lampes « aigus » : elles suivent médiums et aigus, sans flash de kick.
                value = (floor + (1.0 - floor) * (TREBLE_GLOW * self.treble + 0.6 * flash)).min(1.0);
                white = 0.0;
            }
            let [r, g, b] = base.map(|c| ((c * (1.0 - white) + white) * value).clamp(0.0, 1.0));
            let quantize = |u: f32| (u * f32::from(u16::MAX)) as u16;
            *out = Rgb { r: quantize(r), g: quantize(g), b: quantize(b) };
        }
        &self.colors
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

    fn white_look(motion: Motion) -> ReactiveLook {
        let white = [255, 255, 255];
        let palette = Palette { primary: white, calm: white, filtered: white };
        ReactiveLook { motion, palette, ..ReactiveLook::default() }
    }

    /// Joue `seconds` de musique de niveaux `levels`, avec un kick (détecté et flashé) toutes
    /// les `kick_every` trames ; renvoie la dernière couleur.
    fn perform(look: &mut ReactiveLook, seconds: f32, levels: impl Fn(f32) -> Levels, kick_every: Option<usize>) -> Rgb {
        let frames = (seconds * FRAMES_PER_SECOND as f32) as usize;
        let mut color = Rgb::OFF;
        for frame in 0..frames {
            if kick_every.is_some_and(|every| frame % every == 0) {
                look.kick_detected();
                look.kick();
            }
            color = look.render(FRAME, levels(frame as f32 / FRAMES_PER_SECOND as f32));
        }
        color
    }

    fn sound(bass: f32, presence: f32) -> Levels {
        Levels { bass, mid: 0.5, high: 0.5, tilt: Some(1.0), presence: Some(presence), raw_bass: bass }
    }

    #[test]
    fn phases_follow_groove_break_build_up_and_drop() {
        let mut look = white_look(Motion::Auto);
        perform(&mut look, 6.0, |_| sound(0.8, -20.0), Some(24));
        assert_eq!(look.phase(), Phase::Groove);

        perform(&mut look, 3.0, |_| sound(0.1, -20.0), None);
        assert_eq!((look.phase(), look.active_motion()), (Phase::Calm, Motion::Breath));

        // Riser : la présence gagne ~3 dB par seconde, sans basse ni kick ; reconnu en moins de 3 s.
        let early = perform(&mut look, 3.0, |t| sound(0.1, -20.0 + 3.3 * t), None);
        assert_eq!((look.phase(), look.active_motion()), (Phase::BuildUp, Motion::BuildUp));
        let late = perform(&mut look, 4.0, |t| sound(0.1, -10.1 + 3.3 * t), None);
        assert!(brightness(late) > brightness(early) + u16::MAX / 10, "la montée s'intensifie");

        // Drop : le premier gros kick ramène le groove, dans un éclat blanc.
        let drop = perform(&mut look, 1.0 / 60.0, |_| sound(0.9, 0.0), Some(1));
        assert_eq!(look.phase(), Phase::Groove);
        assert!(drop.r.min(drop.g).min(drop.b) > u16::MAX / 20 * 17, "{drop:?}");
    }

    #[test]
    fn a_louder_groove_is_not_a_build_up() {
        let mut look = white_look(Motion::Auto);
        perform(&mut look, 4.0, |_| sound(0.8, -30.0), Some(24));
        // Les charlestons entrent d'un coup : la présence bondit, mais les basses sont là.
        perform(&mut look, 4.0, |_| sound(0.8, -15.0), Some(24));
        assert_eq!(look.phase(), Phase::Groove);
    }

    #[test]
    fn kick_rolls_are_a_build_up() {
        let mut look = white_look(Motion::Auto);
        perform(&mut look, 3.0, |_| sound(0.8, -20.0), Some(6));
        assert_eq!(look.phase(), Phase::BuildUp);
    }

    #[test]
    fn breathing_softens_the_kicks() {
        let swell = |motion: Motion| {
            let mut look = white_look(motion);
            let before = perform(&mut look, 2.0, |_| sound(0.2, -20.0), None);
            let peak = perform(&mut look, 1.0 / 60.0, |_| sound(0.2, -20.0), Some(1));
            brightness(peak) - brightness(before)
        };
        // En respiration, un kick ajoute une houle, loin du plein flash du pulse.
        assert!(swell(Motion::Breath) < u16::MAX / 3, "{}", swell(Motion::Breath));
        assert!(swell(Motion::Pulse) > 2 * swell(Motion::Breath));
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

    /// Quatre lampes de gauche à droite, données dans le désordre ; flash de 1 s pour bien voir
    /// lesquelles sont allumées.
    fn room(spread: Spread) -> ReactiveLook {
        let mut look = ReactiveLook::new(PALETTE);
        look.motion = Motion::Pulse;
        look.spread = spread;
        look.dynamics = Dynamics { flash: 1.0, decay: 1.0, floor: 0.0 };
        look.set_lamps(&[0.3, -1.0, 1.0, -0.3]);
        look
    }

    /// Lampes allumées (au-dessus de la moitié), de gauche à droite.
    fn lit(look: &mut ReactiveLook, dt: Duration) -> [bool; 4] {
        let quiet = Levels::default();
        let colors = look.render_lamps(dt, quiet).to_vec();
        // Ordre donné : 0.3, -1.0, 1.0, -0.3 ; de gauche à droite : 1, 3, 0, 2.
        [1, 3, 0, 2].map(|i| brightness(colors[i]) > u16::MAX / 2)
    }

    #[test]
    fn alternate_lights_left_then_right_and_every_lamp_on_the_bar() {
        let mut look = room(Spread::Alternate);
        look.kick_at(Accent::Bar);
        assert_eq!(lit(&mut look, FRAME), [true; 4], "tête de mesure");
        look.cancel_flash();
        look.kick_at(Accent::Beat);
        assert_eq!(lit(&mut look, FRAME), [false, false, true, true], "droite");
        look.cancel_flash();
        look.kick_at(Accent::Beat);
        assert_eq!(lit(&mut look, FRAME), [true, true, false, false], "gauche");
    }

    #[test]
    fn the_wave_crosses_the_room_then_comes_back() {
        let mut look = room(Spread::Wave);
        look.kick_at(Accent::Beat);
        assert_eq!(lit(&mut look, FRAME), [true, false, false, false], "départ à gauche");
        assert_eq!(lit(&mut look, Duration::from_millis(130)), [true; 4], "arrivée à droite");
        look.cancel_flash();
        look.kick_at(Accent::Beat);
        assert_eq!(lit(&mut look, FRAME), [false, false, false, true], "retour depuis la droite");
    }

    #[test]
    fn split_keeps_every_other_lamp_for_the_treble() {
        let mut look = room(Spread::Split);
        look.kick_at(Accent::Beat);
        assert_eq!(lit(&mut look, FRAME), [true, false, true, false]);
    }

    #[test]
    fn a_single_lamp_flashes_on_every_kick_whatever_the_spread() {
        for spread in Spread::ALL {
            let mut look = ReactiveLook::new(PALETTE);
            look.motion = Motion::Pulse;
            look.spread = spread;
            for _ in 0..3 {
                look.kick_at(Accent::Beat);
                let color = look.render(FRAME, Levels::default());
                assert!(brightness(color) > u16::MAX / 2, "{spread:?}");
                look.cancel_flash();
            }
        }
    }

    #[test]
    fn the_head_of_a_phrase_lights_every_lamp_whiter() {
        for spread in [Spread::Wave, Spread::Alternate, Spread::Split] {
            let mut look = room(spread);
            look.kick_at(Accent::Phrase);
            assert_eq!(lit(&mut look, FRAME), [true; 4], "{spread:?}");
        }
        // Plus de blanc qu'un temps ordinaire : le vert et le bleu montent avec le rouge.
        let mut look = room(Spread::Together);
        look.kick_at(Accent::Beat);
        let beat = look.render(FRAME, Levels::default());
        look.cancel_flash();
        look.kick_at(Accent::Phrase);
        let phrase = look.render(FRAME, Levels::default());
        assert!(phrase.g > beat.g && phrase.b > beat.b, "{beat:?} {phrase:?}");
    }

    /// Joue `seconds` avec la grille dans l'état `grid`, les basses à `bass`, un kick tous les
    /// `every` images (jamais si 0) ; rend la dernière phase.
    fn groove_run(look: &mut ReactiveLook, seconds: f32, grid: bool, bass: f32, every: usize) -> Phase {
        let levels = Levels { bass, raw_bass: bass, tilt: Some(0.0), ..Levels::default() };
        look.set_grid_groove(Some(grid));
        for frame in 0..(seconds * FRAMES_PER_SECOND as f32) as usize {
            if every > 0 && frame % every == 0 {
                look.kick_detected();
            }
            look.render(FRAME, levels);
        }
        look.phase()
    }

    #[test]
    fn a_break_with_a_few_hits_stays_a_break_until_the_grid_finds_the_kicks_again() {
        let mut look = ReactiveLook::new(PALETTE);
        assert_eq!(groove_run(&mut look, 10.0, true, 0.5, 24), Phase::Groove);
        // Break : la basse continue à moitié, un coup isolé toutes les 1,2 s.
        assert_eq!(groove_run(&mut look, 3.0, false, 0.25, 72), Phase::Calm);
        // Un roulement (kicks rapprochés) sans que la grille les confirme : toujours un break.
        assert_eq!(groove_run(&mut look, 1.0, false, 0.25, 12), Phase::Calm, "roulement");
        // La grille retrouve des kicks réguliers avec les basses : le groove reprend.
        assert_eq!(groove_run(&mut look, 1.0, true, 0.5, 24), Phase::Groove);
    }

    #[test]
    fn a_short_grid_hiccup_in_a_loud_groove_is_not_a_break() {
        let mut look = ReactiveLook::new(PALETTE);
        groove_run(&mut look, 10.0, true, 0.5, 24);
        assert_eq!(groove_run(&mut look, 0.8, false, 0.5, 24), Phase::Groove);
    }
}

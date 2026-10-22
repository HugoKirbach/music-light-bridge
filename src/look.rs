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

/// Une montée s'annonce quand la présence (médiums et aigus) dépasse de 4 dB sa moyenne des
/// 6 dernières secondes alors que les basses sont faibles…
const BUILD_RISE_DB: f32 = 4.0;
const PRESENCE_FAST: f32 = 1.0;
const PRESENCE_SLOW: f32 = 6.0;
const BUILD_MAX_BASS: f32 = 0.5;
/// … ou quand les kicks roulent : plus de 4,5 par seconde.
const ROLL_RATE: f32 = 4.5;
const KICK_RATE_WINDOW: f32 = 2.0;
/// Une montée dure au moins 1,5 s avant qu'un drop puisse la conclure, et retombe si plus rien
/// ne monte pendant 4 s.
const BUILD_MIN: f32 = 1.5;
const BUILD_STALL: f32 = 4.0;
/// Temps pour qu'une montée atteigne son plein.
const BUILD_RAMP: f32 = 8.0;
/// Le drop : un kick aux basses fortes, après une seconde de basses faibles.
const DROP_BASS: f32 = 0.6;
const DROP_QUIET_BASS: f32 = 0.35;
const BASS_MEMORY: f32 = 1.0;
/// Respiration : la luminosité suit l'énergie, vite en montée et lentement en descente ; un
/// kick n'y ajoute qu'une légère houle.
const BREATH_ATTACK: f32 = 0.35;
const BREATH_RELEASE: f32 = 0.9;
const BREATH_KICK: f32 = 0.25;
/// Fondu d'une animation à l'autre.
const MOTION_FADE: f32 = 0.4;

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

/// Phase du morceau, reconnue à l'écoute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// Kicks réguliers.
    #[default]
    Groove,
    /// Plus de kick : break, passage calme.
    Calm,
    /// Montée vers un drop.
    BuildUp,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Groove => "groove",
            Self::Calm => "break",
            Self::BuildUp => "montée",
        }
    }

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(index: u8) -> Self {
        match index {
            1 => Self::Calm,
            2 => Self::BuildUp,
            _ => Self::Groove,
        }
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

/// Ce qui permet de reconnaître la phase du morceau.
#[derive(Debug, Default)]
struct PhaseTracker {
    phase: Phase,
    /// Durée passée dans la phase actuelle (s).
    phase_time: f32,
    /// Horloge propre (s) et instants des derniers kicks, pour leur cadence.
    time: f32,
    kick_times: std::collections::VecDeque<f32>,
    /// Un kick a été détecté depuis le dernier rendu.
    kicked: bool,
    presence_fast: Option<f32>,
    presence_slow: Option<f32>,
    bass_memory: f32,
    /// Temps écoulé sans que rien ne monte, pendant une montée.
    stall: f32,
    /// Avancement de la montée, de 0 à 1.
    build: f32,
}

impl PhaseTracker {
    fn kick(&mut self) {
        self.kicked = true;
        self.kick_times.push_back(self.time);
    }

    /// Met la phase à jour ; `true` au drop qui conclut une montée.
    fn update(&mut self, dt: f32, levels: Levels, grooving: bool) -> bool {
        self.time += dt;
        self.phase_time += dt;
        while self.kick_times.front().is_some_and(|&t| self.time - t > KICK_RATE_WINDOW) {
            self.kick_times.pop_front();
        }
        let rolling = self.kick_times.len() as f32 / KICK_RATE_WINDOW > ROLL_RATE;
        let rise = match levels.presence {
            Some(db) => {
                let fast = approach(*self.presence_fast.get_or_insert(db), db, dt, PRESENCE_FAST);
                let slow = approach(*self.presence_slow.get_or_insert(db), db, dt, PRESENCE_SLOW);
                self.presence_fast = Some(fast);
                self.presence_slow = Some(slow);
                fast - slow
            }
            None => 0.0,
        };
        let quiet_bass_before = self.bass_memory < DROP_QUIET_BASS;
        self.bass_memory = approach(self.bass_memory, levels.bass, dt, BASS_MEMORY);
        let kicked = std::mem::take(&mut self.kicked);
        // Une simple hausse de présence ne suffit pas quand les basses sont là : c'est un drop
        // ou l'entrée d'un élément, pas une montée.
        let building = (rise > BUILD_RISE_DB && self.bass_memory < BUILD_MAX_BASS) || rolling;
        self.stall = if building { 0.0 } else { self.stall + dt };

        let mut dropped = false;
        let next = match self.phase {
            Phase::BuildUp
                if kicked
                    && self.phase_time > BUILD_MIN
                    && quiet_bass_before
                    && levels.bass > DROP_BASS
                    && !rolling =>
            {
                dropped = true;
                Phase::Groove
            }
            Phase::BuildUp if self.stall > BUILD_STALL => {
                if grooving { Phase::Groove } else { Phase::Calm }
            }
            Phase::BuildUp => Phase::BuildUp,
            _ if building => Phase::BuildUp,
            _ if grooving => Phase::Groove,
            _ => Phase::Calm,
        };
        if next != self.phase {
            self.phase = next;
            self.phase_time = 0.0;
        }
        self.build = if self.phase == Phase::BuildUp {
            let surge = ((rise - BUILD_RISE_DB) / 10.0).clamp(0.0, 1.0);
            (self.build + dt / BUILD_RAMP).max(surge).min(1.0)
        } else {
            approach(self.build, 0.0, dt, MOTION_FADE)
        };
        dropped
    }
}

#[derive(Debug, Default)]
pub struct ReactiveLook {
    pub palette: Palette,
    pub dynamics: Dynamics,
    pub motion: Motion,
    phases: PhaseTracker,
    /// Part de chaque animation (pulse, respiration, montée) dans le rendu, en fondu.
    weights: [f32; 3],
    /// Luminosité de la respiration.
    breath: f32,
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

        let dropped = self.phases.update(dt, levels, self.since_kick <= CALM_AFTER);
        if dropped && matches!(self.motion, Motion::Auto | Motion::BuildUp) {
            // Le drop : un éclat blanc à pleine puissance.
            self.flash = 1.0;
            self.punch = 1.0;
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

        let floor = self.dynamics.floor.clamp(0.0, 1.0);
        let flash = self.flash * self.dynamics.flash.clamp(0.0, 1.0);

        // Pulse : le flash part de la lueur de fond et monte, selon l'intensité choisie.
        let glow = (floor + GLOW * (1.0 - floor) * levels.bass).min(1.0);
        let pulse = (glow + (1.0 - glow) * flash, self.punch * flash);

        // Respiration : suit l'énergie, un kick n'ajoute qu'une houle.
        let energy = 0.5 * levels.bass + 0.3 * levels.mid + 0.2 * levels.high;
        let breath_time = if energy > self.breath { BREATH_ATTACK } else { BREATH_RELEASE };
        self.breath = approach(self.breath, energy, dt, breath_time);
        let breath = ((floor + (1.0 - floor) * (0.8 * self.breath + BREATH_KICK * flash)).min(1.0), 0.0);

        // Montée : la lumière s'intensifie et blanchit, les flashs gagnent en force.
        let build = self.phases.build;
        let tension = floor + (1.0 - floor) * (0.15 + 0.55 * build);
        let strength = flash * (0.4 + 0.6 * build);
        let build_up = (tension + (1.0 - tension) * strength, (self.punch * strength + 0.4 * build).min(1.0));

        let [w_pulse, w_breath, w_build] = self.weights;
        let total = (w_pulse + w_breath + w_build).max(1e-6);
        let value = (w_pulse * pulse.0 + w_breath * breath.0 + w_build * build_up.0) / total;
        let white = (w_pulse * pulse.1 + w_breath * breath.1 + w_build * build_up.1) / total;
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
        Levels { bass, mid: 0.5, high: 0.5, tilt: Some(1.0), presence: Some(presence) }
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
}

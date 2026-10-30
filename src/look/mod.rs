//! Rendu du mode live à partir de trois couleurs choisies : la principale pour le groove, une
//! pour les temps creux (plus de kick depuis un moment) et une pour les filtres (passe-haut ou
//! passe-bas qui déséquilibre fortement grave et aigu). La musique dose la luminosité : flash
//! sur chaque kick ou note MIDI, lueur qui suit les basses.
//!
//! Les luminosités se calculent sur une échelle perçue, puis passent par une courbe avant
//! d'être envoyées ; les couleurs se fondent par la teinte (OKLCh) ; les mouvements lents
//! (dégradé de teintes, houle et ondulation des breaks) se calent sur la grille des temps.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audio::analysis::Levels;
use crate::hue::stream::Rgb;

mod director;
mod lamps;
mod phases;
mod space;

pub use lamps::{Accent, Spread};
pub use space::Space;
pub use phases::{Phase, Pulse};
use director::Director;
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
/// Un filtre se tourne : l'équilibre met au moins 0,4 s à passer d'un écart de ~4 dB au seuil
/// du filtre. Des basses coupées d'un coup (break) y arrivent en un instant : sans kick, c'est un
/// temps creux et non un passe-haut.
const DRIFT_START: f32 = 0.5;
const SWEEP_MIN: f32 = 0.4;
/// Rotation des couleurs : la teinte de la scène avance d'un huitième de tour, et un drop ne la
/// fait pas avancer une seconde fois juste après une tête de phrase.
const HUE_STEP: f32 = 0.125;
const TURN_GAP: f32 = 1.0;

/// Respiration : la luminosité suit l'énergie, vite en montée et lentement en descente ; un
/// kick n'y ajoute qu'une légère houle.
const BREATH_ATTACK: f32 = 0.35;
const BREATH_RELEASE: f32 = 0.9;
const BREATH_KICK: f32 = 0.25;
/// Fondu d'une animation à l'autre.
const MOTION_FADE: f32 = 0.4;
/// Ondulation des breaks : elle parcourt la pièce en deux mesures (en 4 s sans tempo) et creuse la
/// respiration de 35 %.
const RIPPLE_PERIOD: f32 = 4.0;
const RIPPLE_DEPTH: f32 = 0.35;
/// Lampes « aigus » (graves / aigus) : jusqu'à 70 % de luminosité avec médiums et aigus.
const TREBLE_GLOW: f32 = 0.7;
/// Hors groove, un kick porte au moins le quart des basses habituelles du groove : un charleston ou
/// une mélodie n'en ont pour ainsi dire pas.
const KICK_BASS: f32 = 0.25;
/// Le bridge ne relaie qu'à ~25 Hz en Zigbee : le sommet d'un flash est tenu 40 ms, une
/// période entière, pour que la lampe le reçoive toujours à pleine force.
const FLASH_HOLD: f32 = 0.04;
/// Lueur des basses : elle monte en 30 ms et redescend en 150 ms, sans trembler à chaque note.
const GLOW_ATTACK: f32 = 0.03;
const GLOW_RELEASE: f32 = 0.15;
/// Dégradé de teintes : il traverse la pièce en 8 mesures, ou en 16 s sans tempo.
const DRIFT_BEATS: f64 = 32.0;
const DRIFT_PERIOD: f32 = 16.0;
/// Houle des breaks : une vague par demi-mesure, sommet sur le temps. Ondulation : deux mesures.
const SWELL_BEATS: f64 = 2.0;
const RIPPLE_BEATS: f64 = 8.0;
/// Un mouvement lent rejoint la grille en douceur (constante de temps, s).
const LOCK_TIME: f32 = 1.0;
/// En dessous de cette saturation (OKLab), une couleur n'a pas de teinte propre.
const ACHROMATIC: f32 = 0.02;

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

/// Rotation des couleurs au fil de la structure du morceau.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    /// Les couleurs choisies, sans rotation
    #[default]
    Off,
    /// La teinte avance toutes les 8 mesures et à chaque drop
    #[value(name = "8")]
    Bars8,
    /// La teinte avance toutes les 16 mesures et à chaque drop
    #[value(name = "16")]
    Bars16,
}

impl Rotation {
    pub const ALL: [Self; 3] = [Self::Off, Self::Bars8, Self::Bars16];

    pub fn description(self) -> &'static str {
        match self {
            Self::Off => "Fixes : les couleurs choisies, sans rotation",
            Self::Bars8 => "8 mesures : la teinte avance toutes les 8 mesures et à chaque drop",
            Self::Bars16 => "16 mesures : la teinte avance toutes les 16 mesures et à chaque drop",
        }
    }

    /// Phrases de quatre mesures entre deux rotations.
    fn phrases(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Bars8 => Some(2),
            Self::Bars16 => Some(4),
        }
    }

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(index: u8) -> Self {
        Self::ALL.get(usize::from(index)).copied().unwrap_or(Self::Off)
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

/// Allure de la lumière, réglable : flashs et mouvements d'ambiance.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dynamics {
    /// Jusqu'où monte un flash, entre la lueur de fond (0) et la pleine luminosité (1).
    pub flash: f32,
    /// Temps de redescente d'un flash (s) : il a perdu ~63 % de son éclat au bout de ce temps.
    pub decay: f32,
    /// Lueur de fond minimale, que la lampe garde même sans son (0 à 1).
    pub floor: f32,
    /// Courbe de luminosité (exposant, 1 : aucune) : plus haut, les fondus s'attardent dans le
    /// sombre et les flashs ressortent davantage sur la lueur.
    pub curve: f32,
    /// Écart de teinte (degrés) entre les lampes, de part et d'autre de la couleur de la scène ;
    /// le dégradé dérive lentement à travers la pièce.
    pub gradient: f32,
    /// Houle des breaks, calée sur le tempo : part de la respiration qui retombe entre les temps.
    pub swell: f32,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self { flash: 1.0, decay: 0.12, floor: 0.04, curve: 1.4, gradient: 20.0, swell: 0.35 }
    }
}

/// Position sur la grille des temps : temps écoulés (un multiple de 16 tombe en tête de phrase
/// quand la structure est reconnue) et durée d'un temps (s).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatTime {
    pub beats: f64,
    pub period: f32,
}

impl BeatTime {
    /// Phase (de 0 à 1) d'un cycle de `beats` temps, et durée de ce cycle (s).
    fn cycle(self, beats: f64) -> (f32, f32) {
        ((self.beats / beats).rem_euclid(1.0) as f32, self.period * beats as f32)
    }
}

/// Phase (de 0 à 1) d'un mouvement lent, qui avance seule et se cale en douceur sur la grille des
/// temps quand le tempo est connu : pas de saut quand la grille apparaît ou se recale.
#[derive(Debug, Clone, Copy, Default)]
struct Oscillator {
    phase: f32,
}

impl Oscillator {
    /// Avance de `dt` ; `grid` : phase visée et durée d'un cycle sur la grille ; sinon, un cycle
    /// libre de `free` secondes.
    fn advance(&mut self, dt: f32, grid: Option<(f32, f32)>, free: f32) -> f32 {
        let period = grid.map_or(free, |(_, period)| period).max(0.05);
        self.phase += dt / period;
        if let Some((target, _)) = grid {
            let error = (target - self.phase + 0.5).rem_euclid(1.0) - 0.5;
            self.phase += error * (1.0 - (-dt / LOCK_TIME).exp());
        }
        self.phase = self.phase.rem_euclid(1.0);
        self.phase
    }
}

type Color = [f32; 3];

fn color(srgb: [u8; 3]) -> Color {
    srgb.map(|c| f32::from(c) / 255.0)
}

/// sRGB (de 0 à 1) vers OKLab : luminosité perçue, puis axes vert-rouge et bleu-jaune.
fn oklab(c: Color) -> Color {
    let [r, g, b] = c.map(|u| if u <= 0.04045 { u / 12.92 } else { ((u + 0.055) / 1.055).powf(2.4) });
    let l = (0.412_221_47 * r + 0.536_332_5 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab vers sRGB, ramené dans [0, 1].
fn srgb([l, a, b]: Color) -> Color {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let linear = [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ];
    linear.map(|u| {
        let u = u.clamp(0.0, 1.0);
        if u <= 0.003_130_8 { 12.92 * u } else { 1.055 * u.powf(1.0 / 2.4) - 0.055 }
    })
}

/// Fondu de `a` vers `b` par la teinte (OKLCh) : il garde saturation et luminosité perçue au lieu
/// de passer par un gris terne (rouge → vert passe par l'orange et le jaune, pas par le kaki).
/// Une couleur sans teinte (noir, blanc, gris) prend celle de l'autre.
fn blend(a: Color, b: Color, t: f32) -> Color {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let ([la, aa, ba], [lb, ab, bb]) = (oklab(a), oklab(b));
    let (ca, cb) = (aa.hypot(ba), ab.hypot(bb));
    let (mut ha, mut hb) = (ba.atan2(aa), bb.atan2(ab));
    if ca < ACHROMATIC {
        ha = hb;
    }
    if cb < ACHROMATIC {
        hb = ha;
    }
    let h = ha + ((hb - ha + PI).rem_euclid(TAU) - PI) * t;
    let c = ca + (cb - ca) * t;
    srgb([la + (lb - la) * t, c * h.cos(), c * h.sin()])
}

/// Fait tourner la teinte de `c` de `turns` tours, sans changer sa saturation ni sa luminosité.
fn rotate_hue(c: Color, turns: f32) -> Color {
    let [r, g, b] = c;
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = max - min;
    if chroma <= 0.0 {
        return c;
    }
    let sector = if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    let h = (sector / 6.0 + turns).rem_euclid(1.0) * 6.0;
    let x = chroma * (1.0 - (h % 2.0 - 1.0).abs());
    let [r, g, b] = match h as u32 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    [r + min, g + min, b + min]
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
    pub rotation: Rotation,
    phases: PhaseTracker,
    /// Part de chaque animation (pulse, respiration, montée) dans le rendu, en fondu.
    weights: [f32; 3],
    /// Lueur qui suit les basses, luminosité de la respiration, et énergie des médiums et aigus
    /// (lampes « aigus »).
    glow: f32,
    breath: f32,
    treble: f32,
    /// Place sur la grille des temps, si le tempo est connu.
    beat_time: Option<BeatTime>,
    /// Mouvements lents : dérive du dégradé, houle et ondulation des breaks.
    wander: Oscillator,
    swell: Oscillator,
    ripple: Oscillator,
    lamps: Vec<Lamp>,
    colors: Vec<Rgb>,
    /// Kicks flashés : leur parité fait alterner gauche et droite, et le sens de la vague.
    kicks: u64,
    /// Secondes écoulées depuis le dernier kick détecté.
    since_kick: f32,
    /// La grille de temps confirme des kicks réguliers (`None` tant que le tempo est inconnu).
    grid_groove: Option<bool>,
    /// Part des couleurs « temps creux » et « filtres » dans la scène, en fondu.
    calm: f32,
    filtered: f32,
    /// Équilibre grave/aigu habituel du morceau.
    tilt_reference: Option<f32>,
    /// Depuis combien de temps (s) l'équilibre s'écarte de l'habituel ; depuis quand des basses
    /// coupées d'un coup le font passer le seuil des filtres.
    drift: Option<f32>,
    cut: Option<f32>,
    /// Rotation des couleurs : phrases depuis la dernière, secondes écoulées, et teinte (en tours)
    /// visée et atteinte en fondu.
    phrases: u32,
    since_turn: f32,
    hue_target: f32,
    hue: f32,
    /// Couleur de la dernière note MIDI ; elle remplace celle de la scène.
    note_color: Option<Color>,
    /// Avance des lampes (s) : le drop prévu d'une montée s'allume d'autant plus tôt ; et drop
    /// déjà montré d'avance, qui ne se rejoue pas à sa confirmation.
    drop_advance: f32,
    drop_shown: bool,
    /// Une montée vient de se conclure par son drop (voir `take_drop`).
    dropped: bool,
    /// Régisseur de la disposition Auto.
    director: Director,
    /// Dispositions que la place des lampes rend possibles.
    possible: Vec<Spread>,
}

impl Default for ReactiveLook {
    fn default() -> Self {
        Self {
            palette: Palette::default(),
            dynamics: Dynamics::default(),
            motion: Motion::default(),
            spread: Spread::default(),
            rotation: Rotation::default(),
            phases: PhaseTracker::default(),
            weights: [0.0; 3],
            glow: 0.0,
            breath: 0.0,
            treble: 0.0,
            beat_time: None,
            wander: Oscillator::default(),
            swell: Oscillator::default(),
            ripple: Oscillator::default(),
            lamps: vec![Lamp::default()],
            colors: vec![Rgb::OFF],
            kicks: 0,
            since_kick: 0.0,
            grid_groove: None,
            calm: 0.0,
            filtered: 0.0,
            tilt_reference: None,
            drift: None,
            cut: None,
            phrases: 0,
            since_turn: 0.0,
            hue_target: 0.0,
            hue: 0.0,
            note_color: None,
            drop_advance: 0.0,
            drop_shown: false,
            dropped: false,
            director: Director::default(),
            possible: vec![Spread::Auto, Spread::Together],
        }
    }
}

impl ReactiveLook {
    pub fn new(palette: Palette) -> Self {
        Self { palette, ..Self::default() }
    }

    /// Lampes alignées de gauche à droite (x de la zone Hue, de -1 à 1), faute de pièce décrite.
    #[cfg(test)]
    pub fn set_lamps(&mut self, positions: &[f32]) {
        self.set_space(&Space::line(positions));
    }

    /// Lampes dans la pièce, dans l'ordre où `render_lamps` rendra leurs couleurs : leur place
    /// règle chaque mouvement, et les mouvements possibles bornent les choix du régisseur.
    pub fn set_space(&mut self, space: &Space) {
        let coords = space.coords();
        let count = coords.len().max(1);
        self.lamps = vec![Lamp::default(); count];
        for (lamp, at) in self.lamps.iter_mut().zip(coords) {
            lamp.at = at;
        }
        self.colors = vec![Rgb::OFF; count];
        self.possible = space.possible();
        self.director.possible.clone_from(&self.possible);
    }

    /// Flash de kick, à pleine intensité, sans savoir où il tombe dans la mesure : un kick sur
    /// quatre compte comme une tête de mesure.
    pub fn kick(&mut self) {
        let accent = if self.kicks.is_multiple_of(4) { Accent::Bar } else { Accent::Beat };
        self.kick_at(accent);
    }

    /// Flash de kick, réparti entre les lampes selon la disposition et la place du kick.
    pub fn kick_at(&mut self, accent: Accent) {
        if self.phases.phase == Phase::Groove {
            self.director.kick(accent == Accent::Phrase);
        }
        if accent == Accent::Phrase
            && let Some(every) = self.rotation.phrases()
        {
            self.phrases += 1;
            if self.phrases >= every {
                self.turn();
            }
        }
        let spread = self.active_spread();
        let single = self.lamps.len() == 1;
        let kick = self.kicks as usize;
        let parity = kick % 2 == 1;
        // Rotation : la lampe du tour qui s'allume à ce kick.
        let turn = kick % self.lamps.len().max(1);
        // Poursuite le long d'un mouvement : le groupe de lampes qui s'allume à ce kick. Tout part
        // sur le kick ; le mouvement se lit d'un kick à l'autre.
        let chased_spread = matches!(spread, Spread::Wave | Spread::Diagonal | Spread::Depth | Spread::Rise | Spread::Ripple);
        let chase = chased_spread.then(|| {
            let coordinates: Vec<f32> =
                self.lamps.iter().filter_map(|lamp| lamps::sweep_coordinate(spread, &lamp.at)).collect();
            let (steps, count) = lamps::chase_steps(&coordinates);
            let back_and_forth = matches!(spread, Spread::Wave | Spread::Diagonal);
            (steps, lamps::chase_index(kick, count, back_and_forth))
        });
        self.kicks += 1;
        let bar = accent != Accent::Beat;
        for (i, lamp) in self.lamps.iter_mut().enumerate() {
            let at = lamp.at;
            let chased = chase.as_ref().is_some_and(|(steps, current)| steps[i] == *current);
            match spread {
                _ if accent == Accent::Phrase => lamp.fire(1.0, PHRASE_PUNCH),
                _ if single => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Auto | Spread::Together => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Wave | Spread::Diagonal | Spread::Depth | Spread::Rise | Spread::Ripple if bar || chased => {
                    lamp.fire(1.0, WHITE_PUNCH);
                }
                Spread::Alternate if bar || at.right == parity => lamp.fire(1.0, WHITE_PUNCH),
                Spread::PingPong if bar || at.back == parity => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Corners if bar || at.corner == parity => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Rotate if at.turn == turn => lamp.fire(1.0, WHITE_PUNCH),
                Spread::Split if !at.odd => lamp.fire(1.0, WHITE_PUNCH),
                _ => {}
            }
        }
    }

    /// La teinte de la scène avance d'un cran.
    fn turn(&mut self) {
        (self.phrases, self.since_turn) = (0, 0.0);
        self.hue_target += HUE_STEP;
    }

    /// Place actuelle sur la grille des temps (`None` sans tempo) : les mouvements lents s'y calent.
    pub fn set_beat_time(&mut self, beat_time: Option<BeatTime>) {
        self.beat_time = beat_time;
    }

    /// État de la grille de temps à cet instant : des kicks réguliers sur les temps (`Some(true)`),
    /// ou non, ou tempo inconnu (`None`).
    pub fn set_grid_groove(&mut self, groove: Option<bool>) {
        self.grid_groove = groove;
    }

    /// Un coup détecté compte comme kick. En groove, toujours ; ailleurs (break, montée), seulement
    /// s'il porte du grave (`raw_bass`, enveloppe brute des basses) : au moins le quart du niveau
    /// habituel du groove. Charleston, clap ou mélodie ne flashent donc pas : la lumière flotte.
    /// Tant que le niveau habituel n'est pas connu, tout passe.
    pub fn carries_bass(&self, raw_bass: f32) -> bool {
        self.phases.phase == Phase::Groove || raw_bass >= KICK_BASS * self.phases.groove_bass
    }

    /// Un kick vient d'être détecté (flashé ou non), avec `raw_bass` de grave à son arrivée : la
    /// musique n'est pas dans un temps creux. Seuls les kicks qui portent du grave font un roulement
    /// de montée : des charlestons en croches n'en sont pas un.
    pub fn kick_detected(&mut self, raw_bass: f32) {
        self.since_kick = 0.0;
        self.phases.kick(raw_bass >= KICK_BASS * self.phases.groove_bass);
    }

    /// `true` une fois après le drop qui conclut une montée.
    pub fn take_drop(&mut self) -> bool {
        std::mem::take(&mut self.dropped)
    }

    /// Phase du morceau reconnue à l'écoute.
    pub fn phase(&self) -> Phase {
        self.phases.phase
    }

    /// Structure du morceau, quand elle est connue : période du temps (s) et temps restant avant
    /// la prochaine tête de phrase. Une montée vise son drop sur une tête de phrase.
    pub fn set_structure(&mut self, structure: Option<(f32, f32)>) {
        self.phases.structure = structure;
    }

    /// Avancement de la montée en cours, de 0 à 1.
    pub fn build_progress(&self) -> Option<f32> {
        (self.phases.phase == Phase::BuildUp).then_some(self.phases.build)
    }

    /// Secondes avant le drop prévu de la montée en cours.
    pub fn drop_in(&self) -> Option<f32> {
        self.phases.drop_in()
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
    /// Première des dispositions `wanted` possible ici, sinon toutes ensemble.
    fn first_possible(&self, wanted: &[Spread]) -> Spread {
        wanted.iter().copied().find(|spread| self.possible.contains(spread)).unwrap_or(Spread::Together)
    }

    pub fn active_spread(&self) -> Spread {
        match (self.spread, self.phases.phase) {
            (Spread::Auto, Phase::Groove) => self.director.groove_spread(),
            // La montée grimpe vers le plafond si des lampes y sont ; le break tourne autour de la
            // pièce si les lampes l'entourent.
            (Spread::Auto, Phase::BuildUp) => self.first_possible(&[Spread::Rise, Spread::Wave, Spread::Depth]),
            (Spread::Auto, Phase::Calm) => self.first_possible(&[Spread::Rotate, Spread::Wave, Spread::Depth]),
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

    /// Régisseur : en disposition Auto, la mise en scène et la teinte varient d'une section à
    /// l'autre.
    pub fn set_director(&mut self, on: bool) {
        self.director.enabled = on;
    }

    /// Avance des lampes (s), dont le drop prévu d'une montée s'allume d'autant plus tôt.
    pub fn set_drop_advance(&mut self, advance: f32) {
        self.drop_advance = advance;
    }

    /// Drop à la main (télécommande) : éclat blanc à pleine puissance, partout.
    pub fn drop_now(&mut self) {
        self.fire_drop();
    }

    fn fire_drop(&mut self) {
        for lamp in &mut self.lamps {
            lamp.fire(1.0, 1.0);
        }
    }

    /// Éteint les flashs en cours (kick anticipé qui n'est pas venu).
    pub fn cancel_flash(&mut self) {
        for lamp in &mut self.lamps {
            (lamp.flash, lamp.hold) = (0.0, 0.0);
        }
    }

    /// Couleur de la première lampe, `dt` après le rendu précédent.
    pub fn render(&mut self, dt: Duration, levels: Levels) -> Rgb {
        self.render_lamps(dt, levels)[0]
    }

    /// Couleur de chaque lampe, dans l'ordre de `set_lamps`, `dt` après le rendu précédent.
    pub fn render_lamps(&mut self, dt: Duration, levels: Levels) -> &[Rgb] {
        let dt = dt.as_secs_f32();
        self.since_kick += dt;
        let decay = self.dynamics.decay.max(0.01);
        for lamp in &mut self.lamps {
            if !std::mem::take(&mut lamp.fresh) {
                // Le sommet est tenu, puis le flash redescend.
                let held = lamp.hold.min(dt);
                lamp.hold -= held;
                lamp.flash *= (-(dt - held) / decay).exp();
            }
        }

        let pulse = Pulse { since_kick: self.since_kick, grid_groove: self.grid_groove };
        let drops = matches!(self.motion, Motion::Auto | Motion::BuildUp);
        // Le drop : un éclat blanc à pleine puissance, partout ; d'avance quand la structure le
        // prévoit, pour qu'il tombe pile.
        if drops && self.phases.anticipate_drop(self.drop_advance) {
            self.fire_drop();
            self.drop_shown = true;
        }
        let before = self.phases.phase;
        let dropped = self.phases.update(dt, levels, pulse);
        self.dropped |= dropped;
        // Le régisseur ouvre une nouvelle section au drop et au retour du groove.
        if dropped || (before != Phase::Groove && self.phases.phase == Phase::Groove) {
            self.director.next_section();
        }
        if std::mem::take(&mut self.phases.false_drop) && std::mem::take(&mut self.drop_shown) {
            self.cancel_flash();
        }
        if dropped && drops && !std::mem::take(&mut self.drop_shown) {
            self.fire_drop();
        }
        self.since_turn += dt;
        if self.rotation == Rotation::Off {
            // Retour en fondu aux couleurs choisies (un nombre entier de tours).
            (self.hue_target, self.phrases) = (self.hue_target.round(), 0);
        } else if dropped && self.since_turn > TURN_GAP {
            self.turn();
        }
        self.hue = approach(self.hue, self.hue_target, dt, SCENE_FADE);
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
        let scene = rotate_hue(
            self.director.tint(blend(
                blend(color(self.palette.primary), color(self.palette.calm), self.calm),
                color(self.palette.filtered),
                self.filtered,
            )),
            self.hue,
        );
        let base = self.note_color.unwrap_or(scene);
        let floor = self.dynamics.floor.clamp(0.0, 1.0);
        let intensity = self.dynamics.flash.clamp(0.0, 1.0);
        let curve = self.dynamics.curve.clamp(0.5, 4.0);
        let gradient = self.dynamics.gradient.clamp(0.0, 180.0) / 360.0;
        let swell_depth = self.dynamics.swell.clamp(0.0, 1.0);
        let glow_time = if levels.bass > self.glow { GLOW_ATTACK } else { GLOW_RELEASE };
        self.glow = approach(self.glow, levels.bass, dt, glow_time);
        let glow = (floor + GLOW * (1.0 - floor) * self.glow).min(1.0);
        // Mouvements lents, calés sur la grille quand le tempo est connu. La houle n'existe qu'avec
        // un tempo : sans lui, elle battrait à côté de la musique.
        let grid = self.beat_time;
        let wander = self.wander.advance(dt, grid.map(|g| g.cycle(DRIFT_BEATS)), DRIFT_PERIOD);
        let swell_phase = self.swell.advance(dt, grid.map(|g| g.cycle(SWELL_BEATS)), 1.0);
        let swell = if grid.is_some() { 1.0 - swell_depth * (0.5 - 0.5 * (TAU * swell_phase).cos()) } else { 1.0 };
        let ripple_phase = self.ripple.advance(dt, grid.map(|g| g.cycle(RIPPLE_BEATS)), RIPPLE_PERIOD);
        let energy = 0.5 * levels.bass + 0.3 * levels.mid + 0.2 * levels.high;
        self.director.update(dt, energy);
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
            // Respiration : suit l'énergie, un kick n'ajoute qu'une houle. Une ondulation lente
            // parcourt la pièce le long du mouvement (tout autour en rotation).
            let ripple = match lamps::sweep_coordinate(spread, &lamp.at) {
                Some(place) if several => 1.0 - RIPPLE_DEPTH * (0.5 + 0.5 * (TAU * (ripple_phase - place)).sin()),
                _ => 1.0,
            };
            let breathing = 0.8 * self.breath * ripple * swell + BREATH_KICK * flash;
            let breath = ((floor + (1.0 - floor) * breathing).min(1.0), 0.0);
            // Montée : la lumière s'intensifie et blanchit, les flashs gagnent en force.
            let tension = floor + (1.0 - floor) * (0.15 + 0.55 * build);
            let strength = flash * (0.4 + 0.6 * build);
            let build_up = (tension + (1.0 - tension) * strength, (lamp.punch * strength + 0.4 * build).min(1.0));

            let (mut value, mut white) = (
                (w_pulse * pulse.0 + w_breath * breath.0 + w_build * build_up.0) / total,
                (w_pulse * pulse.1 + w_breath * breath.1 + w_build * build_up.1) / total,
            );
            if several && spread == Spread::Split && lamp.at.odd {
                // Lampes « aigus » : elles suivent médiums et aigus, sans flash de kick.
                value = (floor + (1.0 - floor) * (TREBLE_GLOW * self.treble + 0.6 * flash)).min(1.0);
                white = 0.0;
            }
            // Courbe de luminosité, entre la lueur de fond et le plein : le fond et le sommet ne
            // bougent pas.
            if floor < 1.0 {
                value = floor + (1.0 - floor) * ((value - floor) / (1.0 - floor)).clamp(0.0, 1.0).powf(curve);
            }
            // Dégradé : chaque lampe s'écarte un peu de la teinte de la scène, et l'écart dérive à
            // travers la pièce, de gauche à droite.
            let tint = rotate_hue(base, gradient * (TAU * (wander - lamp.at.x)).sin());
            let [r, g, b] = tint.map(|c| ((c * (1.0 - white) + white) * value).clamp(0.0, 1.0));
            let quantize = |u: f32| (u * f32::from(u16::MAX)) as u16;
            *out = Rgb { r: quantize(r), g: quantize(g), b: quantize(b) };
        }
        &self.colors
    }

    /// Part visée de la couleur « filtres » : écart entre l'équilibre grave/aigu actuel et
    /// l'habituel du morceau. Des basses coupées d'un coup ne comptent pas tant qu'aucun kick ne
    /// revient : c'est un break. Pendant un filtre ou un break, la référence ne bouge pas.
    fn filter_amount(&mut self, tilt: Option<f32>, dt: f32) -> f32 {
        let Some(tilt) = tilt else { return 0.0 };
        let reference = *self.tilt_reference.get_or_insert(tilt);
        let deviation = tilt - reference;
        if deviation.abs() <= DRIFT_START {
            (self.drift, self.cut) = (None, None);
        } else {
            let drift = self.drift.map_or(0.0, |t| t + dt);
            self.drift = Some(drift);
            self.cut = match self.cut {
                Some(t) => Some(t + dt),
                None if deviation < -FILTER_START && drift < SWEEP_MIN => Some(0.0),
                None => None,
            };
        }
        if self.filtered < 0.5 && self.cut.is_none() {
            self.tilt_reference = Some(approach(reference, tilt, dt, TILT_MEMORY));
        }
        if self.cut.is_some_and(|t| self.since_kick >= t) {
            return 0.0;
        }
        ((deviation.abs() - FILTER_START) / (FILTER_FULL - FILTER_START)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::space::Spot;

    const FRAME: Duration = Duration::from_micros(16_667);
    const FRAMES_PER_SECOND: usize = 60;

    /// Rouge pour le groove, bleu pour les temps creux, vert pour les filtres.
    const PALETTE: Palette = Palette { primary: [255, 0, 0], calm: [0, 0, 255], filtered: [0, 255, 0] };

    /// Rendu sans dégradé de teintes ni variantes du régisseur, pour juger les couleurs de la scène.
    fn still(palette: Palette) -> ReactiveLook {
        let mut look = ReactiveLook::new(palette);
        look.dynamics.gradient = 0.0;
        look.set_director(false);
        look
    }

    fn brightness(color: Rgb) -> u16 {
        color.r.max(color.g).max(color.b)
    }

    /// Joue `seconds` de musique d'équilibre `tilt`, avec un kick toutes les 0,4 s si `kicks`.
    fn play(look: &mut ReactiveLook, seconds: f32, tilt: f32, kicks: bool) -> Rgb {
        let levels = Levels { bass: 1.0, tilt: Some(tilt), ..Levels::default() };
        let mut color = Rgb::OFF;
        for frame in 0..(seconds * FRAMES_PER_SECOND as f32) as usize {
            if kicks && frame % 24 == 0 {
                look.kick_detected(1.0);
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
        // Sans dégradé ni variantes de teinte du régisseur, pour juger les couleurs choisies.
        let mut look = still(PALETTE);
        let groove = play(&mut look, 5.0, 1.0, true);
        assert!(groove.r > 10 * groove.b.max(groove.g).max(1), "groove : {groove:?}");
        let calm = play(&mut look, 4.0, 1.0, false);
        assert!(calm.b > 10 * calm.r.max(calm.g).max(1), "temps creux : {calm:?}");
        let back = play(&mut look, 3.0, 1.0, true);
        assert!(back.r > 10 * back.b.max(back.g).max(1), "retour du groove : {back:?}");
    }

    #[test]
    fn a_filter_sweep_shows_the_filter_color() {
        let mut look = still(PALETTE);
        play(&mut look, 40.0, 1.0, true);
        // Passe-bas : les aigus chutent de ~35 dB, les kicks continuent. Le fondu passe par la
        // teinte : il garde un soupçon de jaune tant qu'il s'achève.
        let low_pass = play(&mut look, 3.0, 5.0, true);
        assert!(low_pass.g > 5 * low_pass.r.max(low_pass.b).max(1), "passe-bas : {low_pass:?}");
        let back = play(&mut look, 3.0, 1.0, true);
        assert!(back.r > 10 * back.g.max(back.b).max(1), "fin du filtre : {back:?}");
        // Passe-haut : les basses chutent.
        let high_pass = play(&mut look, 3.0, -3.0, true);
        assert!(high_pass.g > 5 * high_pass.r.max(high_pass.b).max(1), "passe-haut : {high_pass:?}");
    }

    #[test]
    fn a_break_that_cuts_the_bass_keeps_the_calm_color() {
        let mut look = still(PALETTE);
        play(&mut look, 40.0, 1.0, true);
        // Basses coupées d'un coup, plus de kick : l'équilibre ressemble à un passe-haut.
        let calm = play(&mut look, 4.0, -3.0, false);
        assert!(calm.b > 10 * calm.r.max(calm.g).max(1), "break : {calm:?}");
        let long = play(&mut look, 30.0, -3.0, false);
        assert!(long.b > 10 * long.r.max(long.g).max(1), "long break : {long:?}");
        // Au retour du groove, l'équilibre habituel n'a pas glissé vers celui du break.
        let back = play(&mut look, 3.0, 1.0, true);
        assert!(back.r > 10 * back.g.max(back.b).max(1), "retour du groove : {back:?}");
    }

    #[test]
    fn a_high_pass_sweep_that_drops_the_kicks_shows_the_filter_color() {
        let mut look = still(PALETTE);
        play(&mut look, 40.0, 1.0, true);
        // Passe-haut tourné sur 2 s : le kick disparaît à mi-chemin.
        for frame in 0..2 * FRAMES_PER_SECOND {
            if frame < FRAMES_PER_SECOND && frame % 24 == 0 {
                look.kick_detected(1.0);
            }
            let tilt = 1.0 - 4.0 * frame as f32 / (2 * FRAMES_PER_SECOND) as f32;
            look.render(FRAME, Levels { bass: 1.0, tilt: Some(tilt), ..Levels::default() });
        }
        let high_pass = play(&mut look, 2.0, -3.0, false);
        assert!(high_pass.g > 5 * high_pass.r.max(high_pass.b).max(1), "passe-haut : {high_pass:?}");
    }

    #[test]
    fn rotate_hue_keeps_saturation_and_brightness() {
        let green = rotate_hue([1.0, 0.0, 0.0], 1.0 / 3.0);
        assert!((green[1] - 1.0).abs() < 1e-5 && green[0].abs() < 1e-5 && green[2].abs() < 1e-5, "{green:?}");
        let pale = rotate_hue([1.0, 0.5, 0.5], 0.5);
        assert!((pale[0] - 0.5).abs() < 1e-5 && (pale[1] - 1.0).abs() < 1e-5 && (pale[2] - 1.0).abs() < 1e-5);
        assert_eq!(rotate_hue([0.3, 0.3, 0.3], 0.25), [0.3, 0.3, 0.3]);
    }

    #[test]
    fn colors_turn_every_eight_bars_and_come_back_when_fixed() {
        let mut look = still(PALETTE);
        look.rotation = Rotation::Bars8;
        look.kick_at(Accent::Phrase);
        let first = play(&mut look, 2.0, 1.0, true);
        assert!(first.r > 10 * first.g.max(first.b).max(1), "après 4 mesures : {first:?}");
        // Deuxième tête de phrase : un huitième de tour, du rouge vers l'orange.
        look.kick_at(Accent::Phrase);
        let turned = play(&mut look, 2.0, 1.0, true);
        assert!(turned.g > turned.r / 2 && turned.g < turned.r && turned.b < turned.r / 20, "après 8 mesures : {turned:?}");
        look.rotation = Rotation::Off;
        let fixed = play(&mut look, 3.0, 1.0, true);
        assert!(fixed.r > 10 * fixed.g.max(fixed.b).max(1), "couleurs fixes : {fixed:?}");
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
        let soft = Dynamics { flash: 0.4, curve: 1.0, ..Dynamics::default() };
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
                look.kick_detected(1.0);
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

    /// Joue de `from` à `until` (s) un morceau à 150 BPM dont la structure est connue : une
    /// phrase de 16 temps dure 6,4 s, têtes à 0, 6,4, 12,8 et 19,2 s. Un kick toutes les
    /// `kick_every` trames, aux basses `bass(t)`. Renvoie l'avancement de la montée et le temps
    /// avant son drop.
    fn structured(look: &mut ReactiveLook, from: f32, until: f32, kick_every: usize, bass: impl Fn(f32) -> f32) -> (Option<f32>, Option<f32>) {
        let (period, phrase) = (0.4f32, 6.4f32);
        for frame in (from * 60.0).round() as usize..(until * 60.0).round() as usize {
            let t = frame as f32 / 60.0;
            look.set_structure(Some((period, (phrase - t.rem_euclid(phrase)) / period)));
            if frame % kick_every == 0 {
                look.kick_detected(1.0);
                look.kick();
            }
            look.render(FRAME, sound(bass(t), -20.0));
        }
        (look.build_progress(), look.drop_in())
    }

    #[test]
    fn a_build_up_aims_its_drop_at_a_phrase_head() {
        let mut look = white_look(Motion::Auto);
        structured(&mut look, 0.0, 4.0, 24, |_| 0.8);
        // Un roulement de kicks à 4 s : la montée vise la tête de 12,8 s (celle de 6,4 s est trop
        // proche de son début).
        let (early, drop_in) = structured(&mut look, 4.0, 6.0, 6, |_| 0.8);
        assert!(early.is_some_and(|p| p < 0.3), "{early:?}");
        assert!(drop_in.is_some_and(|s| (s - 6.8).abs() < 0.3), "{drop_in:?}");
        let (late, _) = structured(&mut look, 6.0, 11.0, 6, |_| 0.8);
        assert!(late.is_some_and(|p| (0.6..0.9).contains(&p)), "{late:?}");
        // Pas de drop à 12,8 s : il est attendu à la tête suivante, la tension reste au plein.
        let (full, drop_in) = structured(&mut look, 11.0, 13.2, 6, |_| 0.8);
        assert_eq!(full, Some(1.0));
        assert!(drop_in.is_some_and(|s| (s - 6.0).abs() < 0.3), "{drop_in:?}");
    }

    #[test]
    fn a_predicted_drop_lights_up_ahead_and_is_not_replayed() {
        let mut look = white_look(Motion::Auto);
        look.set_drop_advance(0.2);
        structured(&mut look, 0.0, 4.0, 24, |_| 0.8);
        // Roulement filtré (basses faibles) jusqu'au drop de 12,8 s.
        structured(&mut look, 4.0, 12.5, 6, |_| 0.3);
        assert!(!look.drop_shown);
        structured(&mut look, 12.5, 12.65, 6, |_| 0.3);
        assert!(look.drop_shown, "l'éclat part 0,2 s avant le drop");
        // Le drop tombe : un kick aux basses pleines, l'éclat n'est pas rejoué.
        structured(&mut look, 12.65, 12.8, 1000, |_| 0.3);
        structured(&mut look, 12.8, 14.0, 24, |_| 0.9);
        assert_eq!(look.phase(), Phase::Groove);
        assert!(!look.drop_shown && !look.phases.anticipation_missed);
    }

    #[test]
    fn a_drop_that_does_not_come_cuts_its_flash_and_is_not_predicted_again() {
        let mut look = white_look(Motion::Auto);
        look.set_drop_advance(0.2);
        structured(&mut look, 0.0, 4.0, 24, |_| 0.8);
        structured(&mut look, 4.0, 12.65, 6, |_| 0.3);
        assert!(look.drop_shown);
        // Le roulement continue, sans drop : l'éclat s'éteint 0,25 s après l'instant prévu.
        structured(&mut look, 12.65, 13.1, 6, |_| 0.3);
        assert!(!look.drop_shown && look.phases.anticipation_missed);
        assert_eq!(look.phase(), Phase::BuildUp);
        // Et la tête suivante n'est plus montrée d'avance.
        structured(&mut look, 13.1, 19.1, 6, |_| 0.3);
        assert!(!look.drop_shown);
    }

    #[test]
    fn kick_rolls_are_a_build_up() {
        let mut look = white_look(Motion::Auto);
        perform(&mut look, 6.0, |_| sound(0.8, -20.0), Some(24));
        perform(&mut look, 3.0, |_| sound(0.8, -20.0), Some(6));
        assert_eq!(look.phase(), Phase::BuildUp);
    }

    #[test]
    fn steady_eighths_are_a_groove_not_a_roll() {
        // Hard house : kick et donk en croches, cinq coups par seconde, depuis le début.
        let mut look = white_look(Motion::Auto);
        perform(&mut look, 20.0, |_| sound(0.8, -20.0), Some(12));
        assert_eq!(look.phase(), Phase::Groove);
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
        let mut look = still(Palette { calm: [0, 0, 0], ..PALETTE });
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
        look.dynamics = Dynamics { flash: 1.0, decay: 1.0, floor: 0.0, curve: 1.0, gradient: 0.0, ..Dynamics::default() };
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
    fn the_wave_goes_lamp_by_lamp_on_the_kicks_then_comes_back() {
        let mut look = room(Spread::Wave);
        // Un groupe par kick, allumé sur le kick même : gauche → droite, puis retour.
        let mut lamps = Vec::new();
        for _ in 0..6 {
            look.kick_at(Accent::Beat);
            let lit = lit(&mut look, FRAME);
            assert_eq!(lit.iter().filter(|&&on| on).count(), 1, "{lit:?}");
            lamps.push(lit.iter().position(|&on| on).unwrap());
            look.cancel_flash();
        }
        assert_eq!(lamps, [0, 1, 2, 3, 2, 1]);
        // Tête de mesure : toutes ensemble.
        look.kick_at(Accent::Bar);
        assert_eq!(lit(&mut look, FRAME), [true; 4]);
    }

    /// Une lampe par coin, de 5 × 4 m, DJ au milieu du devant : devant gauche au sol, devant droite
    /// et fond gauche à mi-hauteur, fond droite au plafond.
    fn corners(spread: Spread) -> ReactiveLook {
        let mut look = room(spread);
        let spot = |x, y, z| Spot { x, y, z };
        look.set_space(&Space {
            spots: vec![spot(0.5, 0.5, 0.0), spot(4.5, 0.5, 0.5), spot(0.5, 3.5, 0.5), spot(4.5, 3.5, 1.0)],
            dj: [2.5, 0.0],
        });
        look
    }

    /// Lampes allumées dans l'ordre de `corners` : devant gauche, devant droite, fond gauche, fond
    /// droite.
    fn lit_corners(look: &mut ReactiveLook, dt: Duration) -> [bool; 4] {
        let colors = look.render_lamps(dt, Levels::default()).to_vec();
        std::array::from_fn(|i| brightness(colors[i]) > u16::MAX / 2)
    }

    #[test]
    fn rotation_goes_around_the_room_one_lamp_per_kick() {
        let mut look = corners(Spread::Rotate);
        let mut order = Vec::new();
        for _ in 0..4 {
            look.kick_at(Accent::Beat);
            let lit = lit_corners(&mut look, FRAME);
            assert_eq!(lit.iter().filter(|&&on| on).count(), 1, "{lit:?}");
            order.push(lit.iter().position(|&on| on).unwrap());
            look.cancel_flash();
        }
        // Vue de dessus, devant en bas : devant gauche, fond gauche, fond droite, devant droite, en
        // partant de n'importe laquelle.
        let start = order.iter().position(|&i| i == 0).unwrap();
        order.rotate_left(start);
        assert_eq!(order, [0, 2, 3, 1]);
    }

    #[test]
    fn ping_pong_and_corners_answer_each_other() {
        let mut look = corners(Spread::PingPong);
        look.kick_at(Accent::Beat);
        assert_eq!(lit_corners(&mut look, FRAME), [true, true, false, false], "devant");
        look.cancel_flash();
        look.kick_at(Accent::Beat);
        assert_eq!(lit_corners(&mut look, FRAME), [false, false, true, true], "fond");
        let mut look = corners(Spread::Corners);
        look.kick_at(Accent::Beat);
        let first = lit_corners(&mut look, FRAME);
        assert!(first == [true, false, false, true] || first == [false, true, true, false], "{first:?}");
    }

    #[test]
    fn chases_start_where_they_should_and_never_wait() {
        // Onde du DJ : le devant puis le fond ; bas → haut : le sol, la mi-hauteur, le plafond.
        let cases = [
            (Spread::Ripple, vec![[true, true, false, false], [false, false, true, true]]),
            (Spread::Rise, vec![[true, false, false, false], [false, true, true, false], [false, false, false, true]]),
        ];
        for (spread, expected) in cases {
            let mut look = corners(spread);
            for (n, lamps) in expected.iter().chain(expected.first()).enumerate() {
                look.kick_at(Accent::Beat);
                assert_eq!(lit_corners(&mut look, FRAME), *lamps, "{spread:?} kick {n}");
                // Rien ne s'allume après coup : tout part sur le kick.
                assert_eq!(lit_corners(&mut look, Duration::from_millis(130)), *lamps, "{spread:?} kick {n}");
                look.cancel_flash();
            }
        }
    }

    #[test]
    fn auto_climbs_in_a_build_up_and_turns_in_a_break_when_the_room_allows() {
        let look = corners(Spread::Auto);
        assert_eq!(look.first_possible(&[Spread::Rise, Spread::Wave]), Spread::Rise);
        assert_eq!(look.first_possible(&[Spread::Rotate, Spread::Wave]), Spread::Rotate);
        // Lampes alignées au même niveau : ni montée ni rotation, la vague les remplace.
        let line = room(Spread::Auto);
        assert_eq!(line.first_possible(&[Spread::Rise, Spread::Wave]), Spread::Wave);
        assert_eq!(line.first_possible(&[Spread::Rotate, Spread::Wave]), Spread::Wave);
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
                look.kick_detected(1.0);
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
    fn percussion_on_the_beats_of_a_break_without_bass_is_not_a_groove() {
        // Capture 1792832368 : groove aux basses à 0,10 ; break à 0,02 dont les percussions
        // tombent sur les temps, et que la grille prend pour des kicks.
        let mut look = ReactiveLook::new(PALETTE);
        assert_eq!(groove_run(&mut look, 30.0, true, 0.5, 24), Phase::Groove);
        // La grille ne lâche pas : les basses seules signent le break, 2 s après être tombées.
        assert_eq!(groove_run(&mut look, 4.0, true, 0.1, 24), Phase::Calm);
        for _ in 0..16 {
            assert_eq!(groove_run(&mut look, 0.5, true, 0.1, 24), Phase::Calm, "le break oscille");
        }
        // Le drop : les basses reviennent, le groove reprend aussitôt.
        assert_eq!(groove_run(&mut look, 1.0, true, 0.5, 24), Phase::Groove);
    }

    #[test]
    fn the_drop_brings_the_groove_back_before_the_grid_confirms_it() {
        let mut look = ReactiveLook::new(PALETTE);
        groove_run(&mut look, 30.0, true, 0.5, 24);
        assert_eq!(groove_run(&mut look, 6.0, false, 0.1, 72), Phase::Calm);
        // Un impact isolé aux basses pleines : toujours le break.
        groove_run(&mut look, 1.0 / 60.0, false, 0.5, 1);
        assert_eq!(groove_run(&mut look, 2.0, false, 0.1, 0), Phase::Calm, "impact isolé");
        // Le drop : deux kicks aux basses pleines, la grille ne les a pas encore confirmés.
        assert_eq!(groove_run(&mut look, 0.5, false, 0.5, 24), Phase::Groove);
        // Et le groove tient le temps que la grille le rattrape.
        assert_eq!(groove_run(&mut look, 1.5, false, 0.5, 24), Phase::Groove);
        assert_eq!(groove_run(&mut look, 4.0, true, 0.5, 24), Phase::Groove);
    }

    #[test]
    fn a_lighter_track_grooves_again_once_the_old_bass_level_is_forgotten() {
        let mut look = ReactiveLook::new(PALETTE);
        groove_run(&mut look, 30.0, true, 0.5, 24);
        // Morceau suivant aux basses bien plus légères : un temps pris pour un break…
        assert_eq!(groove_run(&mut look, 10.0, true, 0.1, 24), Phase::Calm);
        // … puis le niveau habituel s'oublie et le groove revient.
        assert_eq!(groove_run(&mut look, 120.0, true, 0.1, 24), Phase::Groove);
    }

    #[test]
    fn a_short_grid_hiccup_in_a_loud_groove_is_not_a_break() {
        let mut look = ReactiveLook::new(PALETTE);
        groove_run(&mut look, 10.0, true, 0.5, 24);
        assert_eq!(groove_run(&mut look, 0.8, false, 0.5, 24), Phase::Groove);
    }

    #[test]
    fn the_top_of_a_flash_is_held_for_a_zigbee_period() {
        let mut look = white_look(Motion::Pulse);
        look.kick();
        look.render(FRAME, Levels::default());
        // Deux trames plus tard (33 ms), le sommet tient encore ; il redescend ensuite.
        assert_eq!(brightness(look.render(FRAME, Levels::default())), u16::MAX);
        assert_eq!(brightness(look.render(FRAME, Levels::default())), u16::MAX);
        look.render(FRAME, Levels::default());
        assert!(brightness(look.render(FRAME, Levels::default())) < u16::MAX);
    }

    #[test]
    fn the_curve_darkens_the_in_between_but_keeps_floor_and_peak() {
        let glow = |curve: f32| {
            let mut look = white_look(Motion::Pulse);
            look.dynamics.curve = curve;
            let mut color = Rgb::OFF;
            for _ in 0..60 {
                color = look.render(FRAME, Levels { bass: 1.0, ..Levels::default() });
            }
            brightness(color)
        };
        assert!(glow(2.0) < glow(1.0) * 2 / 3, "{} {}", glow(2.0), glow(1.0));
        assert_eq!(after_flash(Dynamics { curve: 2.0, ..Dynamics::default() }, 0), u16::MAX);
        let floor = Dynamics { floor: 0.3, curve: 2.0, ..Dynamics::default() };
        assert!(after_flash(floor, 120) >= (0.3 * f32::from(u16::MAX)) as u16 - 1);
    }

    #[test]
    fn blends_go_through_the_hues_not_through_grey() {
        // Rouge → vert : en RGB, le milieu est un kaki sombre (0,5 ; 0,5 ; 0) ; par la teinte, un
        // jaune orangé lumineux.
        let [r, g, b] = blend([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.5);
        assert!(r > 0.6 && g > 0.5 && b < 0.2, "{r} {g} {b}");
        // Vers le noir, la teinte reste celle de la couleur.
        let [r, g, b] = blend([1.0, 0.0, 0.0], [0.0, 0.0, 0.0], 0.5);
        assert!(r > 0.2 && g < r / 10.0 && b < r / 10.0, "{r} {g} {b}");
        // Les extrémités ne bougent pas (aller-retour OKLab).
        let [r, g, b] = blend([0.2, 0.5, 0.9], [0.0, 0.0, 0.0], 0.0);
        assert!((r - 0.2).abs() < 1e-3 && (g - 0.5).abs() < 1e-3 && (b - 0.9).abs() < 1e-3);
        let back = srgb(oklab([0.2, 0.5, 0.9]));
        assert!((back[0] - 0.2).abs() < 1e-3 && (back[1] - 0.5).abs() < 1e-3 && (back[2] - 0.9).abs() < 1e-3, "{back:?}");
    }

    #[test]
    fn the_glow_does_not_tremble_with_every_bass_note() {
        let mut look = white_look(Motion::Pulse);
        let mut swing = 0;
        let mut previous = None;
        for frame in 0..120 {
            let bass = if frame % 2 == 0 { 1.0 } else { 0.0 };
            let level = brightness(look.render(FRAME, Levels { bass, ..Levels::default() }));
            if frame > 60 {
                swing = swing.max(previous.map_or(0, |p: u16| p.abs_diff(level)));
            }
            previous = Some(level);
        }
        assert!(swing < u16::MAX / 20, "{swing}");
    }

    #[test]
    fn lamps_spread_around_the_scene_hue_and_the_gradient_drifts() {
        let mut look = ReactiveLook::new(PALETTE);
        (look.motion, look.spread) = (Motion::Pulse, Spread::Together);
        look.set_director(false);
        look.set_lamps(&[-1.0, -0.3, 0.3, 1.0]);
        let quiet = Levels { bass: 1.0, ..Levels::default() };
        let mut first = Vec::new();
        for _ in 0..12 {
            first = look.render_lamps(FRAME, quiet).to_vec();
        }
        // Rouge de part et d'autre : vers l'orange (vert) ou vers le rose (bleu).
        assert!(first.iter().any(|c| c.g > c.r / 10) && first.iter().any(|c| c.b > c.r / 10), "{first:?}");
        for _ in 0..4 * FRAMES_PER_SECOND {
            look.render_lamps(FRAME, quiet);
        }
        assert_ne!(look.render_lamps(FRAME, quiet).to_vec(), first, "le dégradé dérive");
        // Sans dégradé, toutes les lampes ont la couleur de la scène.
        look.dynamics.gradient = 0.0;
        let still = look.render_lamps(FRAME, quiet).to_vec();
        assert!(still.iter().all(|c| *c == still[0]), "{still:?}");
    }

    #[test]
    fn the_break_swell_crests_on_the_beat() {
        let mut look = white_look(Motion::Breath);
        let levels = sound(0.5, -20.0);
        let period = 0.5;
        let mut beats = 0.3;
        let at = |look: &mut ReactiveLook, beats: &mut f64| {
            *beats += FRAME.as_secs_f64() / period;
            look.set_beat_time(Some(BeatTime { beats: *beats, period: period as f32 }));
            brightness(look.render(FRAME, levels))
        };
        // Quelques secondes pour que la houle se cale sur la grille.
        for _ in 0..6 * FRAMES_PER_SECOND {
            at(&mut look, &mut beats);
        }
        let (mut crest, mut trough) = (0, u16::MAX);
        for _ in 0..2 * FRAMES_PER_SECOND {
            let level = at(&mut look, &mut beats);
            let phase = beats.rem_euclid(2.0);
            if !(0.1..1.9).contains(&phase) {
                crest = crest.max(level);
            } else if (0.9..1.1).contains(&phase) {
                trough = trough.min(level);
            }
        }
        assert!(crest > trough + u16::MAX / 20, "sommet {crest}, creux {trough}");
        // Sans houle, plus d'écart.
        look.dynamics.swell = 0.0;
        let (mut high, mut low) = (0, u16::MAX);
        for _ in 0..2 * FRAMES_PER_SECOND {
            let level = at(&mut look, &mut beats);
            (high, low) = (high.max(level), low.min(level));
        }
        assert!(high - low < u16::MAX / 100, "{high} {low}");
    }
}

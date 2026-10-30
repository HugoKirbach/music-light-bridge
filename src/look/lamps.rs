//! Lampes de la zone : leur place dans la pièce, leur flash, et la répartition des kicks entre
//! elles (ensemble, vagues et balayages, alternances, rotation, onde depuis le DJ).

use super::space::Coords;
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Spread {
    /// Suit les phases : le régisseur choisit sur le groove, vague en montée, ondulation en break
    #[default]
    Auto,
    /// Toutes les lampes ensemble
    Together,
    /// Un kick après l'autre, de gauche à droite puis retour
    Wave,
    /// Gauche et droite à tour de rôle, toutes ensemble en tête de mesure
    Alternate,
    /// Une lampe sur deux flashe sur les kicks, les autres respirent avec médiums et aigus
    Split,
    /// Un kick après l'autre, du devant (côté DJ) vers le fond
    Depth,
    /// Un kick après l'autre, d'un coin à l'autre puis retour
    Diagonal,
    /// Un kick après l'autre, du sol vers le plafond
    Rise,
    /// La lumière tourne autour de la pièce, une lampe à chaque kick
    Rotate,
    /// Un kick après l'autre, du DJ vers les lampes les plus loin
    Ripple,
    /// Devant et fond à tour de rôle
    PingPong,
    /// Coins opposés à tour de rôle
    Corners,
}

impl Spread {
    pub const ALL: [Self; 12] = [
        Self::Auto,
        Self::Together,
        Self::Wave,
        Self::Alternate,
        Self::Split,
        Self::Depth,
        Self::Diagonal,
        Self::Rise,
        Self::Rotate,
        Self::Ripple,
        Self::PingPong,
        Self::Corners,
    ];

    /// Titre court.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Together => "Ensemble",
            Self::Wave => "Vague",
            Self::Alternate => "Alternance",
            Self::Split => "Graves / aigus",
            Self::Depth => "Avant → fond",
            Self::Diagonal => "Diagonale",
            Self::Rise => "Bas → haut",
            Self::Rotate => "Rotation",
            Self::Ripple => "Onde du DJ",
            Self::PingPong => "Devant / fond",
            Self::Corners => "Coins opposés",
        }
    }

    /// Une ligne d'explication.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Auto => "Suit les phases",
            Self::Together => "Toutes à chaque kick",
            Self::Wave => "Gauche ↔ droite",
            Self::Alternate => "Gauche, puis droite",
            Self::Split => "Kicks et ambiance",
            Self::Depth => "Du DJ vers le fond",
            Self::Diagonal => "D'un coin à l'autre",
            Self::Rise => "Du sol au plafond",
            Self::Rotate => "Tourne autour de la pièce",
            Self::Ripple => "S'étend depuis le DJ",
            Self::PingPong => "Devant, puis fond",
            Self::Corners => "Une diagonale, puis l'autre",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => "Auto : le régisseur choisit sur le groove, vague (ou montée) en montée, ondulation en break",
            Self::Together => "Ensemble : toutes les lampes à chaque kick",
            Self::Wave => "Vague : un kick après l'autre, la lumière passe de gauche à droite, puis revient",
            Self::Alternate => "Alternance : gauche et droite à tour de rôle, toutes en tête de mesure",
            Self::Split => "Graves / aigus : une lampe sur deux sur les kicks, les autres respirent",
            Self::Depth => "Avant → fond : un kick après l'autre, la lumière part du côté du DJ vers le fond",
            Self::Diagonal => "Diagonale : un kick après l'autre, la lumière va d'un coin à l'autre, puis revient",
            Self::Rise => "Bas → haut : un kick après l'autre, la lumière monte du sol vers le plafond",
            Self::Rotate => "Rotation : la lumière tourne autour de la pièce, une lampe à chaque kick",
            Self::Ripple => "Onde du DJ : un kick après l'autre, la lumière part du DJ vers les lampes les plus loin",
            Self::PingPong => "Devant / fond : les lampes du devant puis celles du fond, toutes en tête de mesure",
            Self::Corners => "Coins opposés : une diagonale puis l'autre, toutes en tête de mesure",
        }
    }

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(index: u8) -> Self {
        Self::ALL.get(usize::from(index)).copied().unwrap_or(Self::Auto)
    }
}

/// Place d'un kick dans la musique : un temps quelconque, le premier d'une mesure (l'alternance y
/// flashe toutes les lampes) ou celui qui ouvre une phrase (toutes les lampes, en blanc plus franc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    Beat,
    Bar,
    Phrase,
}

/// Une lampe (ou un segment de ruban) et son flash.
#[derive(Debug, Clone, Default)]
pub(super) struct Lamp {
    /// Place dans la pièce, le long de chaque mouvement.
    pub(super) at: Coords,
    pub(super) flash: f32,
    /// Part de blanc du flash en cours.
    pub(super) punch: f32,
    /// Flash déclenché depuis le dernier rendu : il s'affiche sans décroissance.
    pub(super) fresh: bool,
    /// Temps (s) pendant lequel le sommet du flash est encore tenu.
    pub(super) hold: f32,
}

impl Lamp {
    pub(super) fn fire(&mut self, strength: f32, punch: f32) {
        if strength >= self.flash {
            (self.flash, self.punch, self.fresh, self.hold) = (strength, punch, true, FLASH_HOLD);
        }
    }
}

/// Place d'une lampe le long d'un mouvement (de 0 à 1, ou en tours pour la rotation), pour
/// l'ondulation lente des breaks ; `None` pour les dispositions sans direction.
pub(super) fn sweep_coordinate(spread: Spread, at: &Coords) -> Option<f32> {
    match spread {
        Spread::Wave | Spread::Alternate => Some(at.x),
        Spread::Depth | Spread::PingPong => Some(at.y),
        Spread::Diagonal | Spread::Corners => Some(at.diagonal),
        Spread::Rise => Some(at.z),
        Spread::Ripple => Some(at.distance),
        Spread::Rotate => Some(at.angle),
        Spread::Auto | Spread::Together | Spread::Split => None,
    }
}

/// Groupes d'une poursuite le long d'un mouvement : les lampes à la même place (à 15 % près de
/// l'étendue) forment un groupe, quatre au plus. Rend le groupe de chaque lampe et leur nombre.
pub(super) fn chase_steps(coordinates: &[f32]) -> (Vec<usize>, usize) {
    const SAME: f32 = 0.15;
    const MAX_STEPS: usize = 4;
    let mut sorted = coordinates.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mut groups: Vec<f32> = Vec::new();
    for value in sorted {
        if groups.last().is_none_or(|&start| value - start > SAME) {
            groups.push(value);
        }
    }
    let count = groups.len().max(1);
    let steps = count.min(MAX_STEPS);
    let group = |value: f32| groups.iter().rposition(|&start| value >= start - 1e-6).unwrap_or(0);
    (coordinates.iter().map(|&value| group(value) * steps / count).collect(), steps)
}

/// Groupe allumé au kick n° `kick` d'une poursuite de `steps` groupes : du premier au dernier en
/// boucle, ou aller-retour.
pub(super) fn chase_index(kick: usize, steps: usize, back_and_forth: bool) -> usize {
    if steps < 2 {
        return 0;
    }
    if !back_and_forth {
        return kick % steps;
    }
    let lap = kick % (2 * steps - 2);
    if lap < steps { lap } else { 2 * steps - 2 - lap }
}

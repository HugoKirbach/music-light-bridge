//! Lampes de la zone : leur place dans la pièce, leur flash, et la répartition des kicks entre
//! elles (ensemble, vague, alternance, graves / aigus).

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Spread {
    /// Suit les phases : alternance sur le groove, vague en montée, ondulation en break
    #[default]
    Auto,
    /// Toutes les lampes ensemble
    Together,
    /// Le flash traverse la pièce, d'un côté puis de l'autre
    Wave,
    /// Gauche et droite à tour de rôle, toutes ensemble en tête de mesure
    Alternate,
    /// Une lampe sur deux flashe sur les kicks, les autres respirent avec médiums et aigus
    Split,
}

impl Spread {
    pub const ALL: [Self; 5] = [Self::Auto, Self::Together, Self::Wave, Self::Alternate, Self::Split];

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => "Auto : alternance sur le groove, vague en montée, ondulation en break",
            Self::Together => "Ensemble : toutes les lampes à chaque kick",
            Self::Wave => "Vague : le flash traverse la pièce, d'un côté puis de l'autre",
            Self::Alternate => "Alternance : gauche et droite à tour de rôle, toutes en tête de mesure",
            Self::Split => "Graves / aigus : une lampe sur deux sur les kicks, les autres respirent",
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
    /// Place de gauche (0) à droite (1), par rang : deux lampes ne sont jamais confondues.
    pub(super) place: f32,
    /// Moitié droite de la pièce (alternance) ; une lampe sur deux par rang (graves / aigus).
    pub(super) right: bool,
    pub(super) odd: bool,
    pub(super) flash: f32,
    /// Part de blanc du flash en cours.
    pub(super) punch: f32,
    /// Flash déclenché depuis le dernier rendu : il s'affiche sans décroissance.
    pub(super) fresh: bool,
    /// Flash qui part dans quelques ms (vague) : délai restant (s), force, part de blanc.
    pub(super) pending: Option<(f32, f32, f32)>,
}

impl Lamp {
    pub(super) fn fire(&mut self, strength: f32, punch: f32) {
        if strength >= self.flash {
            (self.flash, self.punch, self.fresh) = (strength, punch, true);
        }
    }
}

//! Pièces et dispositions des lampes : une pièce rectangulaire vue de dessus (cotes en mètres,
//! place du DJ), et pour chacune plusieurs dispositions (lampes utilisées, place et hauteur de
//! chacune). La disposition active donne au live la géométrie dont il tire ses mouvements.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Pièce vue de dessus : x de gauche à droite, y du devant (côté DJ) vers le fond, en mètres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub name: String,
    pub width: f32,
    pub depth: f32,
    /// Place du DJ (m) : le devant de la pièce, d'où partent les ondes.
    pub dj: [f32; 2],
    pub layouts: Vec<Layout>,
}

/// Disposition des lampes dans une pièce.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub name: String,
    /// Place de chaque lampe (identifiant CLIP v2).
    #[serde(default)]
    pub lamps: BTreeMap<String, Placement>,
    /// Lampes de la zone laissées hors du live : elles gardent leur état.
    #[serde(default)]
    pub excluded: Vec<String>,
}

/// Place d'une lampe : position vue de dessus (m) et hauteur.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub level: Level,
}

/// Hauteur d'une lampe, en trois niveaux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Floor,
    #[default]
    Middle,
    Ceiling,
}

impl Level {
    pub const ALL: [Self; 3] = [Self::Floor, Self::Middle, Self::Ceiling];

    pub fn label(self) -> &'static str {
        match self {
            Self::Floor => "Sol",
            Self::Middle => "Mi-hauteur",
            Self::Ceiling => "Plafond",
        }
    }

    /// Hauteur de 0 (sol) à 1 (plafond).
    pub fn height(self) -> f32 {
        match self {
            Self::Floor => 0.0,
            Self::Middle => 0.5,
            Self::Ceiling => 1.0,
        }
    }
}

/// Plus petite pièce acceptée (m), et plus grande.
pub const MIN_SIZE: f32 = 1.0;
pub const MAX_SIZE: f32 = 50.0;

impl Room {
    /// Pièce de 5 × 4 m, DJ au milieu du devant, avec une disposition vide.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            width: 5.0,
            depth: 4.0,
            dj: [2.5, 0.4],
            layouts: vec![Layout::new("Par défaut")],
        }
    }

    /// Change les cotes en gardant chaque lampe et le DJ dans la pièce.
    pub fn resize(&mut self, width: f32, depth: f32) {
        self.width = width.clamp(MIN_SIZE, MAX_SIZE);
        self.depth = depth.clamp(MIN_SIZE, MAX_SIZE);
        let (w, d) = (self.width, self.depth);
        self.dj = [self.dj[0].clamp(0.0, w), self.dj[1].clamp(0.0, d)];
        for placement in self.layouts.iter_mut().flat_map(|layout| layout.lamps.values_mut()) {
            placement.x = placement.x.clamp(0.0, w);
            placement.y = placement.y.clamp(0.0, d);
        }
    }

    /// Place par défaut d'une lampe pas encore placée : à sa place de gauche à droite dans la zone
    /// Hue (`hue_x` de -1 à 1), au milieu de la profondeur.
    pub fn default_placement(&self, hue_x: f32) -> Placement {
        Placement { x: (hue_x.clamp(-1.0, 1.0) + 1.0) / 2.0 * self.width, y: self.depth * 0.6, level: Level::Middle }
    }
}

impl Layout {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), lamps: BTreeMap::new(), excluded: Vec::new() }
    }
}

/// Nom libre parmi `taken` : `base`, sinon `base 2`, `base 3`…
pub fn unique_name<'a>(base: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    let free = |name: &str| !taken.clone().any(|other| other == name);
    if free(base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} {n}")).find(|name| free(name)).expect("un nom finit par être libre")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_keeps_lamps_and_dj_inside() {
        let mut room = Room::new("Salon");
        room.layouts[0].lamps.insert("a".into(), Placement { x: 4.8, y: 3.5, level: Level::Floor });
        room.resize(3.0, 0.2);
        assert_eq!((room.width, room.depth), (3.0, MIN_SIZE));
        assert_eq!(room.dj, [2.5, 0.4]);
        let a = room.layouts[0].lamps["a"];
        assert_eq!((a.x, a.y), (3.0, 1.0));
    }

    #[test]
    fn default_placement_follows_the_hue_zone_from_left_to_right() {
        let room = Room::new("Salon");
        assert_eq!(room.default_placement(-1.0).x, 0.0);
        assert_eq!(room.default_placement(1.0).x, 5.0);
        assert_eq!(room.default_placement(0.0).y, 2.4);
    }

    #[test]
    fn names_are_made_unique() {
        let taken = ["Salon", "Salon 2"];
        assert_eq!(unique_name("Studio", taken.iter().copied()), "Studio");
        assert_eq!(unique_name("Salon", taken.iter().copied()), "Salon 3");
    }

    #[test]
    fn a_saved_layout_reads_back() {
        let mut room = Room::new("Salon");
        room.layouts[0].lamps.insert("a".into(), Placement { x: 1.0, y: 2.0, level: Level::Ceiling });
        room.layouts[0].excluded.push("b".into());
        let again: Room = serde_json::from_str(&serde_json::to_string(&room).unwrap()).unwrap();
        assert_eq!(again, room);
    }
}

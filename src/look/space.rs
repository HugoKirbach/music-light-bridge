//! Géométrie des lampes dans la pièce : pour chaque lampe, ses coordonnées le long de chaque
//! mouvement (gauche → droite, devant → fond, diagonale, hauteur, distance au DJ, angle autour
//! de la pièce) et ses groupes (moitié droite, fond, coin) ; et les mouvements que la disposition
//! rend possibles, avec la raison de ceux qui ne le sont pas.

use std::f32::consts::TAU;

use super::Spread;
use crate::room::Room;

/// Écart minimal (m) entre les lampes le long d'un mouvement pour qu'il se voie.
const MIN_SPREAD: f32 = 0.8;
/// La rotation demande des lampes tout autour : aucun trou de plus d'un demi-tour entre elles.
const MAX_ANGLE_GAP: f32 = 0.5;

/// Place d'une lampe : vue de dessus en mètres (x de gauche à droite, y du devant vers le fond)
/// et hauteur de 0 (sol) à 1 (plafond).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// Lampes dans leur pièce, avec la place du DJ (m).
#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    pub spots: Vec<Spot>,
    pub dj: [f32; 2],
}

/// Coordonnées d'une lampe le long de chaque mouvement, de 0 à 1 sur l'étendue des lampes.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Coords {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) diagonal: f32,
    pub(super) z: f32,
    pub(super) distance: f32,
    /// Angle autour du centre des lampes (tours), et rang dans le tour.
    pub(super) angle: f32,
    pub(super) turn: usize,
    /// Moitié droite, moitié du fond, et diagonale (coins opposés) : par rang, pour qu'il y ait
    /// toujours deux groupes.
    pub(super) right: bool,
    pub(super) back: bool,
    pub(super) corner: bool,
    /// Une lampe sur deux de gauche à droite (graves / aigus).
    pub(super) odd: bool,
}

impl Space {
    /// Lampes alignées de gauche à droite (x de -1 à 1, comme dans une zone Hue), à mi-hauteur,
    /// dans une pièce de 5 × 4 m : faute de pièce décrite.
    #[cfg(test)]
    pub fn line(positions: &[f32]) -> Self {
        let spots = positions.iter().map(|&x| Spot { x: (x.clamp(-1.0, 1.0) + 1.0) * 2.5, y: 2.4, z: 0.5 }).collect();
        Self { spots, dj: [2.5, 0.4] }
    }

    /// Lampes d'une pièce, dans l'ordre de `ids` (une lampe pas placée prend la place par défaut
    /// donnée par `fallback`, de -1 à 1 de gauche à droite).
    pub fn of_room<'a>(room: &Room, layout: usize, ids: impl IntoIterator<Item = (&'a str, f32)>) -> Self {
        let lamps = &room.layouts[layout.min(room.layouts.len() - 1)].lamps;
        let spots = ids
            .into_iter()
            .map(|(id, fallback)| {
                let placement = lamps.get(id).copied().unwrap_or_else(|| room.default_placement(fallback));
                Spot { x: placement.x, y: placement.y, z: placement.level.height() }
            })
            .collect();
        Self { spots, dj: room.dj }
    }

    pub(super) fn coords(&self) -> Vec<Coords> {
        let n = self.spots.len();
        let along = |value: &dyn Fn(&Spot) -> f32| normalize(&self.spots.iter().map(value).collect::<Vec<_>>());
        let x = along(&|s| s.x);
        let y = along(&|s| s.y);
        let diagonal = along(&|s| s.x + s.y);
        let z = along(&|s| s.z);
        let distance = along(&|s| self.distance(s));
        let angles = self.angles();
        let by_x = ranks(&self.spots.iter().map(|s| s.x).collect::<Vec<_>>());
        let by_y = ranks(&self.spots.iter().map(|s| s.y).collect::<Vec<_>>());
        let by_angle = ranks(&angles);
        (0..n)
            .map(|i| {
                let right = n > 1 && by_x[i] >= n / 2;
                let back = n > 1 && by_y[i] >= n / 2;
                Coords {
                    x: x[i],
                    y: y[i],
                    diagonal: diagonal[i],
                    z: z[i],
                    distance: distance[i],
                    angle: angles[i],
                    turn: by_angle[i],
                    right,
                    back,
                    corner: right == back,
                    odd: by_x[i] % 2 == 1,
                }
            })
            .collect()
    }

    /// Chaque disposition des lampes, et pourquoi elle n'est pas possible ici (`None` si elle l'est).
    pub fn movements(&self) -> Vec<(Spread, Option<&'static str>)> {
        Spread::ALL.iter().map(|&spread| (spread, self.why_not(spread))).collect()
    }

    /// Dispositions possibles ici.
    pub fn possible(&self) -> Vec<Spread> {
        self.movements().into_iter().filter(|(_, why)| why.is_none()).map(|(spread, _)| spread).collect()
    }

    #[cfg(test)]
    pub fn allows(&self, spread: Spread) -> bool {
        self.why_not(spread).is_none()
    }

    fn why_not(&self, spread: Spread) -> Option<&'static str> {
        let n = self.spots.len();
        let span = |value: &dyn Fn(&Spot) -> f32| {
            let values = self.spots.iter().map(value);
            values.clone().fold(f32::MIN, f32::max) - values.fold(f32::MAX, f32::min)
        };
        let wide = span(&|s| s.x) >= MIN_SPREAD;
        let deep = span(&|s| s.y) >= MIN_SPREAD;
        match spread {
            Spread::Auto | Spread::Together => None,
            _ if n < 2 => Some("il faut au moins deux lampes"),
            Spread::Split => None,
            Spread::Wave | Spread::Alternate if !wide => Some("lampes trop proches de gauche à droite"),
            Spread::Depth | Spread::PingPong if !deep => Some("lampes trop proches du devant au fond"),
            Spread::Diagonal if !(wide && deep) => Some("il faut des lampes étalées en largeur et en profondeur"),
            Spread::Rise if span(&|s| s.z) <= 0.0 => Some("toutes les lampes sont à la même hauteur"),
            Spread::Ripple if span(&|s| self.distance(s)) < MIN_SPREAD => Some("lampes toutes à la même distance du DJ"),
            Spread::Rotate if n < 3 => Some("il faut au moins trois lampes"),
            Spread::Rotate if !(wide && deep) || self.angle_gap() > MAX_ANGLE_GAP => {
                Some("les lampes doivent entourer la pièce")
            }
            Spread::Corners if n < 3 || !(wide && deep) => Some("il faut au moins trois lampes, étalées en largeur et en profondeur"),
            _ => None,
        }
    }

    fn distance(&self, spot: &Spot) -> f32 {
        (spot.x - self.dj[0]).hypot(spot.y - self.dj[1])
    }

    /// Angle de chaque lampe autour du centre des lampes, en tours, dans le sens horaire vu de
    /// dessus en partant de la gauche.
    fn angles(&self) -> Vec<f32> {
        let n = self.spots.len().max(1) as f32;
        let cx = self.spots.iter().map(|s| s.x).sum::<f32>() / n;
        let cy = self.spots.iter().map(|s| s.y).sum::<f32>() / n;
        self.spots.iter().map(|s| ((s.y - cy).atan2(cx - s.x) / TAU).rem_euclid(1.0)).collect()
    }

    /// Plus grand trou (tours) entre deux lampes voisines autour du centre.
    fn angle_gap(&self) -> f32 {
        let mut angles = self.angles();
        angles.sort_by(f32::total_cmp);
        let wrap = angles.first().map_or(1.0, |first| first + 1.0 - angles.last().copied().unwrap_or(0.0));
        angles.windows(2).map(|w| w[1] - w[0]).fold(wrap, f32::max)
    }
}

/// Valeurs ramenées de 0 à 1 sur leur étendue (toutes à 0 si elles sont égales).
fn normalize(values: &[f32]) -> Vec<f32> {
    let min = values.iter().copied().fold(f32::MAX, f32::min);
    let max = values.iter().copied().fold(f32::MIN, f32::max);
    let span = max - min;
    values.iter().map(|v| if span > 1e-3 { (v - min) / span } else { 0.0 }).collect()
}

/// Rang de chaque valeur (0 pour la plus petite ; à égalité, l'ordre donné).
fn ranks(values: &[f32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]).then(a.cmp(&b)));
    let mut ranks = vec![0; values.len()];
    for (rank, &i) in order.iter().enumerate() {
        ranks[i] = rank;
    }
    ranks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(spots: &[(f32, f32, f32)]) -> Space {
        Space { spots: spots.iter().map(|&(x, y, z)| Spot { x, y, z }).collect(), dj: [2.5, 0.0] }
    }

    #[test]
    fn four_lamps_in_the_corners_allow_every_movement_but_the_rise() {
        let room = space(&[(0.5, 0.5, 0.5), (4.5, 0.5, 0.5), (0.5, 3.5, 0.5), (4.5, 3.5, 0.5)]);
        let impossible: Vec<Spread> = room.movements().into_iter().filter(|(_, why)| why.is_some()).map(|(s, _)| s).collect();
        assert_eq!(impossible, [Spread::Rise]);
    }

    #[test]
    fn lamps_on_one_wall_cannot_turn_nor_go_to_the_back() {
        let wall = space(&[(0.0, 2.0, 0.0), (2.5, 2.0, 0.5), (5.0, 2.0, 1.0)]);
        for spread in [Spread::Depth, Spread::PingPong, Spread::Diagonal, Spread::Rotate, Spread::Corners] {
            assert!(!wall.allows(spread), "{spread:?}");
        }
        for spread in [Spread::Wave, Spread::Alternate, Spread::Rise, Spread::Ripple] {
            assert!(wall.allows(spread), "{spread:?}");
        }
    }

    #[test]
    fn a_single_lamp_only_flashes_together() {
        assert_eq!(space(&[(1.0, 1.0, 0.5)]).possible(), [Spread::Auto, Spread::Together]);
    }

    #[test]
    fn coordinates_follow_the_room() {
        // Devant gauche au sol, fond droite au plafond, et une au milieu.
        let coords = space(&[(0.0, 0.0, 0.0), (5.0, 4.0, 1.0), (2.5, 2.0, 0.5)]).coords();
        assert_eq!([coords[0].x, coords[1].x, coords[2].x], [0.0, 1.0, 0.5]);
        assert_eq!([coords[0].z, coords[1].z, coords[2].z], [0.0, 1.0, 0.5]);
        assert_eq!(coords[1].diagonal, 1.0);
        // Le DJ est au milieu du devant : la lampe du devant gauche est la plus proche.
        assert!(coords[0].distance < coords[1].distance);
        assert!(!coords[0].back && coords[1].back && coords[1].right);
    }

    #[test]
    fn the_turn_goes_around_the_room() {
        // Gauche, fond, droite, devant : un tour dans cet ordre.
        let coords = space(&[(0.0, 2.0, 0.5), (2.5, 4.0, 0.5), (5.0, 2.0, 0.5), (2.5, 0.0, 0.5)]).coords();
        assert_eq!(coords.iter().map(|c| c.turn).collect::<Vec<_>>(), [0, 1, 2, 3]);
    }
}

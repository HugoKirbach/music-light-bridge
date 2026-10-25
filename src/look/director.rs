//! Régisseur : en disposition Auto, il change la mise en scène du groove d'une section à
//! l'autre (drop, retour du groove, toutes les deux phrases) selon l'énergie du set, et décale
//! légèrement la teinte de la couleur principale pour que deux sections ne se ressemblent pas.

use super::{Color, Spread, approach};

/// Une section dure au moins 32 kicks (8 mesures) et change à la tête de phrase suivante ; sans
/// structure reconnue, tous les 64 kicks.
const SECTION_KICKS: u32 = 32;
const SECTION_MAX_KICKS: u32 = 64;
/// Énergie récente (4 s) comparée à celle du set (60 s) : forte au-dessus de 110 %, faible
/// sous 85 %.
const ENERGY_FAST: f32 = 4.0;
const ENERGY_SLOW: f32 = 60.0;
const HIGH_ENERGY: f32 = 1.1;
const LOW_ENERGY: f32 = 0.85;
/// Dispositions du groove selon l'énergie, parmi celles que la pièce permet.
const HIGH: [Spread; 5] = [Spread::Together, Spread::Alternate, Spread::Corners, Spread::PingPong, Spread::Rotate];
const MEDIUM: [Spread; 8] = [
    Spread::Alternate,
    Spread::Wave,
    Spread::Together,
    Spread::Diagonal,
    Spread::Split,
    Spread::Depth,
    Spread::Ripple,
    Spread::Rotate,
];
const LOW: [Spread; 4] = [Spread::Split, Spread::Wave, Spread::Ripple, Spread::Depth];
/// Décalages de teinte (en tours) des sections successives : ±20°.
const HUES: [f32; 3] = [0.0, 0.055, -0.055];
const HUE_FADE: f32 = 2.0;

#[derive(Debug)]
pub(super) struct Director {
    pub(super) enabled: bool,
    /// Dispositions que la place des lampes rend possibles.
    pub(super) possible: Vec<Spread>,
    section: usize,
    kicks: u32,
    energy_fast: f32,
    energy_slow: Option<f32>,
    spread: Spread,
    hue: f32,
}

impl Default for Director {
    fn default() -> Self {
        Self {
            enabled: true,
            possible: Spread::ALL.to_vec(),
            section: 0,
            kicks: 0,
            energy_fast: 0.0,
            energy_slow: None,
            spread: Spread::Alternate,
            hue: 0.0,
        }
    }
}

impl Director {
    /// Suit l'énergie du set (de 0 à 1) et fond la teinte vers celle de la section.
    pub(super) fn update(&mut self, dt: f32, energy: f32) {
        self.energy_fast = approach(self.energy_fast, energy, dt, ENERGY_FAST);
        let slow = self.energy_slow.get_or_insert(energy);
        *slow = approach(*slow, energy, dt, ENERGY_SLOW);
        let target = if self.enabled { HUES[self.section % HUES.len()] } else { 0.0 };
        self.hue = approach(self.hue, target, dt, HUE_FADE);
    }

    /// Un kick du groove ; à une tête de phrase, la section peut changer.
    pub(super) fn kick(&mut self, phrase_head: bool) {
        self.kicks += 1;
        if (phrase_head && self.kicks >= SECTION_KICKS) || self.kicks >= SECTION_MAX_KICKS {
            self.next_section();
        }
    }

    /// Nouvelle section : drop, retour du groove après un break, ou fin de la précédente.
    pub(super) fn next_section(&mut self) {
        self.section += 1;
        self.kicks = 0;
        let ratio = self.energy_fast / self.energy_slow.unwrap_or(self.energy_fast).max(1e-3);
        let pool: &[Spread] = if ratio >= HIGH_ENERGY {
            &HIGH
        } else if ratio <= LOW_ENERGY {
            &LOW
        } else {
            &MEDIUM
        };
        let pool: Vec<Spread> = pool.iter().copied().filter(|spread| self.possible.contains(spread)).collect();
        if pool.is_empty() {
            self.spread = Spread::Together;
            return;
        }
        let previous = self.spread;
        self.spread = (0..pool.len())
            .map(|i| pool[(self.section + i) % pool.len()])
            .find(|&spread| spread != previous)
            .unwrap_or(previous);
    }

    /// Disposition du groove : celle de la section, ou l'alternance sans régisseur.
    pub(super) fn groove_spread(&self) -> Spread {
        if self.enabled { self.spread } else { Spread::Alternate }
    }

    /// Couleur décalée de la teinte de la section.
    pub(super) fn tint(&self, color: Color) -> Color {
        rotate_hue(color, self.hue)
    }
}

/// Rotation de teinte de `turns` tours autour de l'axe des gris.
fn rotate_hue(color: Color, turns: f32) -> Color {
    if turns.abs() < 1e-4 {
        return color;
    }
    let (sin, cos) = (std::f32::consts::TAU * turns).sin_cos();
    let a = (1.0 - cos) / 3.0;
    let b = (1.0f32 / 3.0).sqrt() * sin;
    let [r, g, bl] = color;
    [
        ((cos + a) * r + (a - b) * g + (a + b) * bl).clamp(0.0, 1.0),
        ((a + b) * r + (cos + a) * g + (a - b) * bl).clamp(0.0, 1.0),
        ((a - b) * r + (a + b) * g + (cos + a) * bl).clamp(0.0, 1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_change_on_a_phrase_head_after_eight_bars() {
        let mut director = Director::default();
        let first = director.groove_spread();
        for _ in 0..31 {
            director.kick(false);
        }
        director.kick(true);
        assert_ne!(director.groove_spread(), first);
        let second = director.groove_spread();
        // Une tête de phrase trop tôt ne change rien ; sans structure, 64 kicks suffisent.
        director.kick(true);
        assert_eq!(director.groove_spread(), second);
        for _ in 0..63 {
            director.kick(false);
        }
        assert_ne!(director.groove_spread(), second);
    }

    #[test]
    fn energy_picks_the_staging() {
        let mut director = Director::default();
        for _ in 0..600 {
            director.update(0.1, 0.4);
        }
        // Le set s'emballe : section suivante en ensemble ou en alternance.
        for _ in 0..80 {
            director.update(0.1, 0.9);
        }
        director.next_section();
        assert!(HIGH.contains(&director.groove_spread()), "{:?}", director.groove_spread());
        // Il retombe : graves / aigus ou vague.
        for _ in 0..80 {
            director.update(0.1, 0.1);
        }
        director.next_section();
        assert!(LOW.contains(&director.groove_spread()), "{:?}", director.groove_spread());
    }

    #[test]
    fn hue_rotation_keeps_greys_and_turns_colors() {
        let grey = [0.5, 0.5, 0.5];
        let turned = rotate_hue(grey, 0.1);
        assert!(turned.iter().all(|c| (c - 0.5).abs() < 1e-4), "{turned:?}");
        let [r, g, b] = rotate_hue([1.0, 0.0, 0.0], 1.0 / 3.0);
        assert!(g > 0.99 && r < 0.01 && b < 0.01, "{:?}", [r, g, b]);
    }

    #[test]
    fn without_the_director_the_groove_alternates_in_its_own_color() {
        let mut director = Director { enabled: false, ..Director::default() };
        director.next_section();
        assert_eq!(director.groove_spread(), Spread::Alternate);
        for _ in 0..100 {
            director.update(0.1, 0.5);
        }
        assert_eq!(director.tint([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]);
    }
}

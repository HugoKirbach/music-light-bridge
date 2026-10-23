//! Structure du morceau, sur la grille de temps : quelle mesure ouvre la phrase (quatre mesures,
//! seize temps) et donc quel temps ouvre la mesure.
//!
//! Dans une boucle de techno, rien ne distingue la tête de mesure : un kick sur chaque temps, la
//! même basse. La structure se lit aux changements de section (break, drop, entrée d'un
//! élément), qui tombent en tête de phrase. Ils se repèrent par la nouveauté : chaque temps est
//! décrit par quelques indices (niveaux des basses, médiums et aigus, kick, attaques entre les
//! temps) ; on compare les deux mesures qui précèdent un temps aux deux qui le suivent, moins la
//! ressemblance de chaque côté. Un pic de nouveauté vote pour la position de son temps dans la
//! phrase ; les votes s'estompent lentement, et la position qui en accumule nettement le plus
//! est la tête. Tant qu'aucune ne ressort, la structure reste inconnue.

use std::collections::VecDeque;

use crate::audio::analysis::Onset;
use crate::tempo::BeatClock;

pub const BEATS_PER_BAR: u64 = 4;
pub const BEATS_PER_PHRASE: u64 = 16;
/// Fenêtre autour d'un temps où chercher son kick (s).
const ATTACK_WINDOW: (f64, f64) = (-0.03, 0.06);
/// Nouveauté : deux mesures de part et d'autre du temps.
const SIDE: usize = 8;
/// Temps gardés : les deux côtés, plus de quoi estimer l'étalement de chaque indice.
const HISTORY: usize = 48;
/// Un pic de nouveauté au-delà de 0,6 (écarts en étalements) vote, en proportion.
const PEAK: f32 = 0.6;
/// Mémoire des votes : ~80 mesures (les changements de section sont rares). La grille et le
/// compte des temps continuent pendant les breaks : la structure reconnue tient d'une section à
/// l'autre.
const DECAY: f32 = 0.997;
/// La tête retenue doit cumuler 1,5 fois les votes de la suivante, et au moins deux pics francs.
const CONFIDENCE: f32 = 1.5;
const MIN_VOTES: f32 = 1.4;

/// Ce qu'on retient d'un temps.
#[derive(Debug, Clone, Copy, Default)]
struct Beat {
    time: f64,
    kick: f32,
    /// Plus forte attaque entre ce temps et le suivant (roulements).
    between: f32,
    /// Somme des logarithmes des niveaux des trois bandes, et nombre de trames.
    energy: [f32; 3],
    frames: u32,
}

impl Beat {
    /// Indices du temps : niveaux (log) des basses, médiums et aigus, kick et attaques entre les
    /// temps (log, le kick compté double).
    fn features(&self) -> [f32; 5] {
        let [bass, mid, high] = self.energy.map(|sum| sum / self.frames.max(1) as f32);
        [bass, mid, high, 2.0 * self.kick.ln_1p(), self.between.ln_1p()]
    }
}

/// Place d'un temps dans la structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// Temps dans la mesure, de 0 (tête) à 3.
    pub beat: u8,
    /// Temps dans la phrase, de 0 (tête) à 15.
    pub phrase: u8,
}

#[derive(Debug, Default)]
pub struct PhraseTracker {
    current: Option<Beat>,
    /// Derniers temps terminés, le plus récent à la fin.
    recent: VecDeque<Beat>,
    /// Numéro du dernier temps terminé (compteur propre, continu).
    count: u64,
    votes: [f32; BEATS_PER_PHRASE as usize],
    /// Nouveauté des deux derniers temps évalués (pour ne voter qu'à un pic).
    novelty: [f32; 2],
}

impl PhraseTracker {
    /// Trame de 10 ms qui finit à l'instant `t`, rangée dans son temps de la grille : celui qui la
    /// précède, à 30 ms près (l'attaque d'un kick peut devancer le temps).
    pub fn on_frame(&mut self, t: f64, onset: Onset, clock: &BeatClock) {
        let Some(period) = clock.period() else { return };
        let Some(beat) = clock.nearest_beat(t - ATTACK_WINDOW.0 - period / 2.0) else { return };
        if self.current.is_some_and(|current| (beat - current.time).abs() > period / 2.0) {
            let finished = self.current.take().unwrap_or_default();
            self.finish(finished);
        }
        let current = self.current.get_or_insert(Beat { time: beat, ..Beat::default() });
        if (ATTACK_WINDOW.0..ATTACK_WINDOW.1).contains(&(t - current.time)) {
            current.kick = current.kick.max(onset.kick);
        } else {
            current.between = current.between.max(onset.kick);
        }
        for (sum, level) in current.energy.iter_mut().zip([onset.bass, onset.mid, onset.high]) {
            *sum += level.max(1e-5).ln();
        }
        current.frames += 1;
    }

    fn finish(&mut self, beat: Beat) {
        self.count += 1;
        self.recent.push_back(beat);
        if self.recent.len() > HISTORY {
            self.recent.pop_front();
        }
        for vote in &mut self.votes {
            *vote *= DECAY;
        }
        if self.recent.len() < 2 * SIDE {
            return;
        }
        let features: Vec<[f32; 5]> = self.recent.iter().map(Beat::features).collect();
        // Nouveauté du temps d'il y a `SIDE` temps : deux mesures avant contre deux après.
        let center = features.len() - SIDE;
        let novelty = novelty(&features, center);
        // On ne vote qu'à un pic : la nouveauté du temps précédent dépasse ses deux voisines.
        let [before, peak] = self.novelty;
        if peak >= PEAK && peak > before && peak >= novelty {
            self.vote(SIDE as u64, peak);
        }
        self.novelty = [peak, novelty];
    }

    /// Vote pour la position du temps d'il y a `ago` temps (le dernier terminé : 0).
    fn vote(&mut self, ago: u64, weight: f32) {
        let n = self.count - ago.min(self.count);
        self.votes[(n % BEATS_PER_PHRASE) as usize] += weight;
    }

    /// Place, dans la mesure et la phrase, du temps de la grille le plus proche de `t`.
    pub fn position(&self, t: f64, clock: &BeatClock) -> Option<Position> {
        let period = clock.period()?;
        let last = self.recent.back()?;
        let n = self.count as i64 + ((t - last.time) / period).round() as i64;
        let head = leader(&self.votes)? as i64;
        let phrase = (n - head).rem_euclid(BEATS_PER_PHRASE as i64) as u8;
        Some(Position { beat: phrase % BEATS_PER_BAR as u8, phrase })
    }
}

/// Nouveauté au temps `center` : écart moyen entre les `SIDE` temps d'avant et les `SIDE` d'après,
/// moins le quart de l'écart moyen à l'intérieur de chaque côté. Chaque indice est rapporté à son
/// étalement sur les temps gardés.
fn novelty(features: &[[f32; 5]], center: usize) -> f32 {
    let count = features.len() as f32;
    let spread: [f32; 5] = std::array::from_fn(|d| {
        let mean = features.iter().map(|f| f[d]).sum::<f32>() / count;
        (features.iter().map(|f| (f[d] - mean).powi(2)).sum::<f32>() / count).sqrt().max(1e-3)
    });
    let distance = |a: &[f32; 5], b: &[f32; 5]| (0..5).map(|d| ((a[d] - b[d]) / spread[d]).abs()).sum::<f32>() / 5.0;
    let before = &features[center - SIDE..center];
    let after = &features[center..center + SIDE];
    let mean_distance = |x: &[[f32; 5]], y: &[[f32; 5]]| {
        x.iter().flat_map(|a| y.iter().map(move |b| distance(a, b))).sum::<f32>() / (x.len() * y.len()) as f32
    };
    mean_distance(before, after) - 0.25 * (mean_distance(before, before) + mean_distance(after, after))
}

/// Position qui cumule le plus de votes, si elle domine nettement la suivante et en a assez.
fn leader(votes: &[f32]) -> Option<usize> {
    let mut ranked: Vec<(usize, f32)> = votes.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (best, top) = *ranked.first()?;
    let second = ranked.get(1).map_or(0.0, |&(_, v)| v);
    (top >= MIN_VOTES && top >= CONFIDENCE * second).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: f64 = 60.0 / 150.0;

    /// Un temps synthétique : force du kick, attaques entre les temps, niveaux des trois bandes.
    struct Synth {
        kick: f32,
        between: f32,
        levels: [f32; 3],
    }

    /// Alimente le suivi trame par trame avec `beat(n)` pour le temps n (à partir de 0).
    fn play(beats: u64, beat: impl Fn(u64) -> Synth) -> (PhraseTracker, BeatClock) {
        let clock = BeatClock::new(150.0, 0.0);
        let mut tracker = PhraseTracker::default();
        let mut t: f64 = 0.0;
        while t < beats as f64 * PERIOD {
            t += 0.01;
            let n = (t / PERIOD).floor() as u64;
            let offset = t - n as f64 * PERIOD;
            let s = beat(n);
            let attack = (0.005..0.015).contains(&offset);
            let half = (PERIOD / 2.0 - 0.005..PERIOD / 2.0 + 0.005).contains(&offset);
            let [bass, mid, high] = s.levels;
            let kick = if attack { s.kick } else if half { s.between } else { 0.0 };
            tracker.on_frame(t, Onset { kick, bass, mid, high, bright: 0.0 }, &clock);
        }
        (tracker, clock)
    }

    /// Techno de 112 temps, phrases de 16 temps qui commencent aux temps n ≡ 6 (mod 16) : sections
    /// qui changent à chaque phrase (groove plein, break sans kick ni basse, groove avec des aigus
    /// en plus), un roulement sur les deux derniers temps de chaque phrase.
    fn techno(n: u64) -> Synth {
        let in_phrase = (n + 10) % 16;
        let section = (n + 10) / 16 % 3;
        let between = if in_phrase >= 14 { 1.5 } else { 0.2 };
        match section {
            0 => Synth { kick: 1.0, between, levels: [0.5, 0.2, 0.1] },
            1 => Synth { kick: 0.0, between, levels: [0.03, 0.2, 0.15] },
            _ => Synth { kick: 1.0, between, levels: [0.5, 0.3, 0.4] },
        }
    }

    #[test]
    fn finds_the_head_of_the_phrase_and_of_the_bar() {
        let (tracker, clock) = play(112, techno);
        // Le temps 102 ouvre une phrase (102 + 10 = 112).
        let position = tracker.position(102.0 * PERIOD, &clock).expect("structure reconnue");
        assert_eq!((position.beat, position.phrase), (0, 0), "{position:?}");
        let position = tracker.position(107.0 * PERIOD, &clock).unwrap();
        assert_eq!((position.beat, position.phrase), (1, 5));
    }

    #[test]
    fn a_steady_loop_gives_no_structure() {
        let steady = |_| Synth { kick: 1.0, between: 0.2, levels: [0.5, 0.2, 0.1] };
        let (tracker, clock) = play(112, steady);
        assert_eq!(tracker.position(100.0 * PERIOD, &clock), None);
    }
}

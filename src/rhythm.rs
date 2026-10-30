//! Motif rythmique : où tombent les kicks dans la mesure, sur seize doubles croches. La plupart
//! des morceaux en ont un sur chaque temps ; d'autres (hard groove, breakbeat, demi-tempo) les
//! placent ailleurs. Le motif s'apprend des kicks détectés, mesure après mesure ; la lumière
//! attend et anticipe alors les kicks là où ils tombent vraiment.

use std::collections::VecDeque;

use crate::tempo::BeatClock;

/// Doubles croches d'une mesure de quatre temps.
pub const STEPS: i64 = 16;
/// Un kick sur chaque temps.
pub const FOUR_ON_THE_FLOOR: u16 = 0b0001_0001_0001_0001;
/// Une double croche fait partie du motif quand un kick y est tombé dans la plupart des dernières
/// mesures ; le motif ne quitte le kick sur chaque temps que si un temps n'en a plus du tout
/// (six mesures environ sans kick), et que les kicks hors du motif pèsent moins que ceux du motif.
const HIT_LEVEL: f32 = 0.6;
const EMPTY_BEAT: f32 = 0.1;
/// Part de chaque nouvelle mesure dans le motif appris, et mesures avant de s'y fier.
const LEARN: f32 = 0.3;
const MIN_BARS: u32 = 4;
/// Une double croche s'apprend deux doubles croches après : le temps que son kick arrive.
const SETTLE: i64 = 2;

/// Motif et grille : où et quand les kicks sont attendus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rhythm {
    /// Durée d'un temps (s).
    pub period: f64,
    /// Début d'une mesure (double croche 0), s.
    pub bar: f64,
    /// Doubles croches où un kick est attendu (bit n : la double croche n de la mesure).
    pub hits: u16,
}

impl Rhythm {
    /// Un kick sur chaque temps de l'horloge.
    #[cfg(test)]
    pub fn on_beats(clock: &BeatClock) -> Option<Self> {
        Some(Self { period: clock.period()?, bar: clock.anchor(), hits: FOUR_ON_THE_FLOOR })
    }

    /// Le même tempo, avec un kick sur chaque temps.
    pub fn beats(&self) -> Self {
        Self { hits: FOUR_ON_THE_FLOOR, ..*self }
    }

    /// Durée d'une double croche (s).
    pub fn step(&self) -> f64 {
        self.period / 4.0
    }

    fn time(&self, n: i64) -> f64 {
        self.bar + n as f64 * self.step()
    }

    fn is_hit(&self, n: i64) -> bool {
        self.hits >> n.rem_euclid(STEPS) & 1 == 1
    }

    /// Premier kick attendu à partir de `t` (inclus).
    pub fn next_hit_after(&self, t: f64) -> Option<f64> {
        let first = ((t - self.bar) / self.step()).ceil() as i64;
        (first..first + STEPS).find(|&n| self.is_hit(n)).map(|n| self.time(n))
    }

    /// Kick attendu le plus proche de `t`.
    pub fn nearest_hit(&self, t: f64) -> Option<f64> {
        let center = ((t - self.bar) / self.step()).round() as i64;
        (0..=STEPS / 2)
            .flat_map(|k| [center - k, center + k])
            .find(|&n| self.is_hit(n))
            .map(|n| self.time(n))
    }

    /// Kick attendu après celui de l'instant `hit`.
    pub fn following(&self, hit: f64) -> Option<f64> {
        self.next_hit_after(hit + self.step() / 2.0)
    }

    /// Plus long écart (s) entre deux kicks attendus du motif.
    pub fn longest_gap(&self) -> f64 {
        let hits: Vec<i64> = (0..STEPS).filter(|&n| self.is_hit(n)).collect();
        let gaps = hits.windows(2).map(|pair| pair[1] - pair[0]);
        let wrap = hits.first().zip(hits.last()).map_or(STEPS, |(first, last)| first + STEPS - last);
        gaps.fold(wrap, i64::max) as f64 * self.step()
    }

    /// Deux instants désignent le même kick attendu.
    pub fn same_hit(&self, a: f64, b: f64) -> bool {
        (a - b).abs() < self.step() / 2.0
    }
}

/// Apprend le motif des kicks détectés : pour chaque double croche de la mesure, la part des
/// dernières mesures où un kick y est tombé.
#[derive(Debug, Default)]
pub struct PatternLearner {
    weights: [f32; STEPS as usize],
    bars: u32,
    /// Un temps de l'horloge (s) et son numéro : les temps se comptent de mesure en mesure.
    reference: Option<(f64, i64)>,
    /// Dernière double croche apprise (numéro depuis le premier temps compté).
    learned: Option<i64>,
    /// Doubles croches récentes où un kick est tombé.
    kicked: VecDeque<i64>,
}

impl PatternLearner {
    /// Le temps passe : à l'instant `t`, les doubles croches finies depuis un moment s'apprennent
    /// (un kick peut arriver un peu après sa trame).
    pub fn on_frame(&mut self, t: f64, clock: &BeatClock) {
        let Some(step) = self.step_of(t, clock) else { return };
        let settled = step - SETTLE;
        let first = match self.learned {
            // Grand saut (tempo changé, longue pause) : on reprend d'ici.
            Some(learned) if settled - learned <= STEPS => learned + 1,
            _ => settled,
        };
        for n in first..=settled {
            let hit = self.kicked.contains(&n);
            self.learn(n, hit);
        }
        self.learned = Some(self.learned.map_or(settled, |learned| learned.max(settled)));
        while self.kicked.front().is_some_and(|&n| n < settled - STEPS) {
            self.kicked.pop_front();
        }
    }

    /// Un kick détecté à l'instant `t`.
    pub fn on_kick(&mut self, t: f64, clock: &BeatClock) {
        if let Some(step) = self.step_of(t, clock)
            && !self.kicked.contains(&step)
        {
            self.kicked.push_back(step);
        }
    }

    /// Numéro de la double croche la plus proche de `t`, en comptant les temps d'un repère à
    /// l'autre.
    fn step_of(&mut self, t: f64, clock: &BeatClock) -> Option<i64> {
        let (period, beat) = (clock.period()?, clock.nearest_beat(t)?);
        let count = match self.reference {
            Some((time, count)) => count + ((beat - time) / period).round() as i64,
            None => 0,
        };
        self.reference = Some((beat, count));
        Some(count * 4 + ((t - beat) / (period / 4.0)).round() as i64)
    }

    fn learn(&mut self, step: i64, hit: bool) {
        let i = step.rem_euclid(STEPS) as usize;
        self.weights[i] += LEARN * (f32::from(u8::from(hit)) - self.weights[i]);
        if i == STEPS as usize - 1 {
            self.bars += 1;
        }
    }

    /// Doubles croches du motif. Un kick sur chaque temps tant que le motif n'est pas appris et net,
    /// ou que chaque temps porte un kick : seuls les motifs qui laissent un temps vide changent le jeu.
    pub fn hits(&self) -> u16 {
        let empty_beat = (0..STEPS as usize).step_by(4).any(|i| self.weights[i] <= EMPTY_BEAT);
        let in_pattern = |i: &usize| self.weights[*i] >= HIT_LEVEL;
        let hits = (0..STEPS as usize).filter(in_pattern).fold(0u16, |hits, i| hits | 1 << i);
        // Des kicks un peu partout (roulement, détections au hasard) ne font pas un motif.
        let (on, off): (Vec<usize>, Vec<usize>) = (0..STEPS as usize).partition(in_pattern);
        let weight = |steps: &[usize]| steps.iter().map(|&i| self.weights[i]).sum::<f32>();
        let clean = weight(&off) < weight(&on);
        if self.bars < MIN_BARS || !empty_beat || hits == 0 || !clean { FOUR_ON_THE_FLOOR } else { hits }
    }

    /// Début d'une mesure (s), d'après les temps comptés.
    pub fn bar_start(&self, period: f64) -> Option<f64> {
        let (time, count) = self.reference?;
        Some(time - count.rem_euclid(4) as f64 * period)
    }
}

/// Motif lisible : `x` pour un kick, `.` sinon, de la double croche 0 à 15.
#[cfg(test)]
pub fn pattern(text: &str) -> u16 {
    text.chars().filter(|c| !c.is_whitespace()).enumerate().filter(|&(_, c)| c == 'x').fold(0, |hits, (i, _)| hits | 1 << i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: f64 = 0.4;

    fn hard_groove() -> Rhythm {
        Rhythm { period: PERIOD, bar: 10.0, hits: pattern("x....x..x....x..") }
    }

    #[test]
    fn hits_follow_the_pattern_across_bars() {
        let rhythm = hard_groove();
        let step = PERIOD / 4.0;
        let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        assert!(close(rhythm.next_hit_after(10.0), 10.0));
        assert!(close(rhythm.next_hit_after(10.01), 10.0 + 5.0 * step));
        assert!(close(rhythm.following(10.0 + 13.0 * step), 10.0 + 16.0 * step), "mesure suivante");
        assert!(close(rhythm.nearest_hit(10.0 + 6.0 * step), 10.0 + 5.0 * step));
        assert!(close(rhythm.next_hit_after(9.0), 10.0 - 8.0 * step), "mesure précédente");
    }

    /// Joue `bars` mesures dont les kicks (détectés) tombent sur les doubles croches de `kicks`, à
    /// 150 BPM.
    fn learn(kicks: &str, bars: usize) -> PatternLearner {
        let clock = BeatClock::new(150.0, 0.0);
        let mut learner = PatternLearner::default();
        let hits = pattern(kicks);
        let step = 0.1;
        for frame in 0..(bars as f64 * 16.0 * step / 0.01) as usize {
            let t = frame as f64 * 0.01;
            let n = (t / step).round() as i64;
            // Le kick arrive un peu après sa trame, avec 15 ms d'écart au temps exact.
            if hits >> n.rem_euclid(16) & 1 == 1 && (t - n as f64 * step - 0.03).abs() < 0.004 {
                learner.on_kick(n as f64 * step + 0.015, &clock);
            }
            learner.on_frame(t, &clock);
        }
        learner
    }

    #[test]
    fn a_syncopated_low_end_becomes_the_pattern() {
        assert_eq!(learn("x....x..x....x..", 6).hits(), pattern("x....x..x....x.."));
        assert_eq!(learn("x.....x...x.....", 6).hits(), pattern("x.....x...x....."), "breakbeat");
    }

    #[test]
    fn a_kick_on_every_beat_stays_four_on_the_floor() {
        assert_eq!(learn("x...x...x..xx...", 6).hits(), FOUR_ON_THE_FLOOR, "un kick de plus ne change rien");
        assert_eq!(learn("x....x..x....x..", 1).hits(), FOUR_ON_THE_FLOOR, "pas encore assez de mesures");
    }
}

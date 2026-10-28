//! Choix des instants de flash. Seuls les vrais kicks flashent ; quand les kicks tombent
//! régulièrement sur les temps, le flash du suivant part en avance de la latence Hue pour
//! s'allumer avec lui, et il est annulé si ce kick ne vient pas (début de break).
//!
//! Les instants sont exprimés en secondes, comme ceux de l'horloge de tempo.

use crate::rhythm::Rhythm;

/// Temps consécutifs confirmés par un kick à partir desquels on anticipe le suivant.
const STREAK_TO_ANTICIPATE: u32 = 2;
/// Temps consécutifs confirmés à partir desquels le groove est établi : la détection attend
/// alors les kicks sur les temps et se méfie de ce qui tombe à côté.
const GROOVE_STREAK: u32 = 4;
/// Le groove tombe si aucun kick attendu n'a été confirmé depuis 1,5 temps (un kick manqué), ou
/// depuis le plus long écart du motif et un demi-temps (demi-tempo : un kick tous les deux temps).
const GROOVE_TIMEOUT: f64 = 1.5;
const GROOVE_GRACE: f64 = 0.5;
/// Un kick à moins de 15 % de période d'un temps confirme ce temps.
const ON_BEAT: f64 = 0.15;
/// Un flash anticipé encore dû avec ce retard (réveil tardif) part quand même.
const LATE_LIMIT: f64 = 0.25;
/// Temps laissé au kick d'un flash anticipé, en plus de son délai d'arrivée (âge des blocs
/// audio) : dans un mix chargé, un kick peut être détecté une vingtaine de ms après le temps.
pub const CONFIRM_MARGIN: f64 = 0.035;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashMode {
    /// Sur chaque vrai kick, anticipé quand le kick est attendu sur le temps
    #[value(name = "kicks")]
    Kicks,
    /// Sur chaque kick détecté, sans anticipation
    #[value(name = "reactif")]
    Reactive,
    /// Sur chaque temps de l'horloge, même sans kick
    #[value(name = "temps")]
    EveryBeat,
}

impl FlashMode {
    pub fn next(self) -> Self {
        match self {
            Self::Kicks => Self::Reactive,
            Self::Reactive => Self::EveryBeat,
            Self::EveryBeat => Self::Kicks,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Kicks => "kicks",
            Self::Reactive => "réactif",
            Self::EveryBeat => "temps",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Kicks => "Kicks, anticipés quand ils tombent sur les temps",
            Self::Reactive => "Réactif : chaque kick, sans anticipation",
            Self::EveryBeat => "Chaque temps, même sans kick",
        }
    }

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Kicks,
            1 => Self::Reactive,
            _ => Self::EveryBeat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Flash ; `reactive` s'il répond à un kick qui vient d'être détecté.
    Flash { reactive: bool },
    /// Le kick anticipé n'est pas venu : éteindre le flash en cours.
    Cancel,
}

#[derive(Debug)]
pub struct FlashPlanner {
    pub mode: FlashMode,
    /// Avance des flashs anticipés : la latence de la chaîne Hue (s).
    pub advance: f64,
    /// Délai après un temps anticipé au-delà duquel son kick est déclaré absent (s).
    pub confirm_window: f64,
    /// Dernier temps confirmé par un kick, et nombre de temps consécutifs confirmés.
    last_confirmed: Option<f64>,
    streak: u32,
    /// Le dernier temps anticipé est passé sans kick, mais le groove a été gardé.
    missed: bool,
    /// Temps dont le flash est parti en avance et dont on attend le kick.
    pending: Option<f64>,
    /// Temps anticipé dont le flash a été annulé faute de kick : s'il arrive en retard, il reflashe.
    cancelled: Option<f64>,
    /// Dernier temps flashé, pour ne jamais flasher deux fois le même.
    last_flashed: Option<f64>,
}

impl FlashPlanner {
    pub fn new(mode: FlashMode, advance: f64, confirm_window: f64) -> Self {
        Self {
            mode,
            advance,
            confirm_window,
            last_confirmed: None,
            streak: 0,
            missed: false,
            pending: None,
            cancelled: None,
            last_flashed: None,
        }
    }

    /// Les kicks tombent régulièrement là où le motif les attend, et le dernier vient d'être
    /// confirmé.
    pub fn in_groove(&self, rhythm: Option<&Rhythm>, now: f64) -> bool {
        let (Some(rhythm), Some(last)) = (rhythm, self.last_confirmed) else {
            return false;
        };
        let timeout = (GROOVE_TIMEOUT * rhythm.period).max(rhythm.longest_gap() + GROOVE_GRACE * rhythm.period);
        self.streak >= GROOVE_STREAK && now - last < timeout
    }

    /// Prochain instant où la boucle d'éclairage doit se réveiller (flash anticipé ou annulation).
    pub fn next_event(&self, rhythm: Option<&Rhythm>, now: f64) -> Option<f64> {
        let flash = self.anticipated_hit(rhythm, now).map(|hit| hit - self.advance);
        let expiry = self.pending.map(|hit| hit + self.confirm_window);
        match (flash, expiry) {
            (Some(flash), Some(expiry)) => Some(flash.min(expiry)),
            (flash, expiry) => flash.or(expiry),
        }
    }

    /// Action d'éclairage à l'instant `now` ; `kick` est l'instant du kick tout juste détecté.
    pub fn poll(&mut self, rhythm: Option<&Rhythm>, now: f64, kick: Option<f64>) -> Action {
        let mut action = kick.map_or(Action::None, |t| self.on_kick(rhythm, t));
        if let Some(hit) = self.anticipated_hit(rhythm, now)
            && now >= hit - self.advance
        {
            self.last_flashed = Some(hit);
            if self.mode == FlashMode::Kicks {
                self.pending = Some(hit);
            }
            action = Action::Flash { reactive: false };
        }
        if let Some(hit) = self.pending
            && now >= hit + self.confirm_window
        {
            self.pending = None;
            if self.streak >= GROOVE_STREAK && !self.missed {
                // Un seul kick manqué en plein groove : sans doute noyé dans le mix. Le flash
                // reste, et le coup compte comme confirmé pour garder l'anticipation.
                self.missed = true;
                self.last_confirmed = Some(hit);
            } else {
                // Second kick manqué de suite, ou pas de groove (début de break) : on éteint. Sans
                // confirmation de ce coup, l'anticipation s'arrête d'elle-même ; un kick en retard
                // reflashera.
                self.cancelled = Some(hit);
                if action == Action::None {
                    action = Action::Cancel;
                }
            }
        }
        action
    }

    fn on_kick(&mut self, rhythm: Option<&Rhythm>, t: f64) -> Action {
        let on_hit = rhythm
            .and_then(|rhythm| rhythm.nearest_hit(t).map(|hit| (rhythm, hit)))
            .filter(|&(rhythm, hit)| (t - hit).abs() < ON_BEAT * rhythm.period);
        if let Some((rhythm, hit)) = on_hit {
            self.confirm(rhythm, hit);
        }
        match self.mode {
            FlashMode::EveryBeat => Action::None,
            FlashMode::Reactive => Action::Flash { reactive: true },
            FlashMode::Kicks => match on_hit {
                // Kick arrivé après l'annulation de son flash anticipé : on le rallume.
                Some((rhythm, hit)) if self.cancelled.is_some_and(|c| rhythm.same_hit(c, hit)) => {
                    self.cancelled = None;
                    Action::Flash { reactive: true }
                }
                // Kick anticipé : son flash est déjà parti.
                Some((rhythm, hit)) if self.last_flashed.is_some_and(|f| rhythm.same_hit(f, hit)) => Action::None,
                Some((_, hit)) => {
                    self.last_flashed = Some(hit);
                    Action::Flash { reactive: true }
                }
                // Kick hors du motif (roulement de montée) ou tempo encore inconnu.
                None => Action::Flash { reactive: true },
            },
        }
    }

    fn confirm(&mut self, rhythm: &Rhythm, hit: f64) {
        self.streak = match self.last_confirmed {
            Some(previous) if rhythm.following(previous).is_some_and(|next| rhythm.same_hit(next, hit)) => self.streak + 1,
            Some(previous) if rhythm.same_hit(previous, hit) => self.streak,
            _ => 1,
        };
        self.last_confirmed = Some(hit);
        self.missed = false;
        if self.pending.is_some_and(|pending| rhythm.same_hit(pending, hit)) {
            self.pending = None;
        }
    }

    /// Kick attendu dont le flash doit partir en avance, s'il y en a un.
    fn anticipated_hit(&self, rhythm: Option<&Rhythm>, now: f64) -> Option<f64> {
        let rhythm = match self.mode {
            FlashMode::Reactive => return None,
            // Chaque temps, quel que soit le motif.
            FlashMode::EveryBeat => rhythm?.beats(),
            FlashMode::Kicks => *rhythm?,
        };
        let not_flashed = self.last_flashed.map_or(f64::NEG_INFINITY, |hit| hit + rhythm.step() / 2.0);
        let hit = rhythm.next_hit_after((now + self.advance - LATE_LIMIT * rhythm.period).max(not_flashed))?;
        match self.mode {
            FlashMode::Kicks => {
                let previous = self.last_confirmed?;
                let expected = rhythm.following(previous)?;
                (self.streak >= STREAK_TO_ANTICIPATE && rhythm.same_hit(expected, hit)).then_some(hit)
            }
            _ => Some(hit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempo::BeatClock;

    const BPM: f64 = 155.0;
    const ADVANCE: f64 = 0.045;
    const CONFIRM: f64 = 0.03;
    /// Délai entre un kick et son arrivée dans la boucle d'éclairage (capture + détection).
    const ARRIVAL: f64 = 0.012;

    const ANTICIPATED: Action = Action::Flash { reactive: false };
    const REACTIVE: Action = Action::Flash { reactive: true };

    fn beats(n: f64) -> f64 {
        n * 60.0 / BPM
    }

    /// Fait tourner le planificateur comme la boucle d'éclairage : réveil aux événements qu'il
    /// demande, à l'arrivée de chaque kick, et au moins à chaque trame (60 Hz).
    fn simulate(mode: FlashMode, kicks: &[f64], until: f64) -> Vec<(f64, Action)> {
        let kicks: Vec<(f64, f64)> = kicks.iter().map(|&kick| (kick, ARRIVAL)).collect();
        simulate_arrivals(mode, &kicks, until).0
    }

    /// Comme `simulate`, chaque kick arrivant avec son propre délai ; renvoie aussi le
    /// planificateur pour inspecter son état final.
    fn simulate_arrivals(mode: FlashMode, kicks: &[(f64, f64)], until: f64) -> (Vec<(f64, Action)>, FlashPlanner) {
        simulate_rhythm(Rhythm::on_beats(&BeatClock::new(BPM, 0.0)), mode, kicks, until)
    }

    /// Comme `simulate_arrivals`, avec le motif `rhythm`.
    fn simulate_rhythm(
        rhythm: Option<Rhythm>,
        mode: FlashMode,
        kicks: &[(f64, f64)],
        until: f64,
    ) -> (Vec<(f64, Action)>, FlashPlanner) {
        let mut planner = FlashPlanner::new(mode, ADVANCE, CONFIRM);
        let mut arrivals: Vec<(f64, f64)> = kicks.iter().map(|&(kick, delay)| (kick + delay, kick)).collect();
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut arrivals = arrivals.into_iter().peekable();
        let mut actions = Vec::new();
        let mut now: f64 = 0.0;
        while now < until {
            let mut next = now + 1.0 / 60.0;
            if let Some(event) = planner.next_event(rhythm.as_ref(), now) {
                next = next.min(event.max(now));
            }
            if let Some(&(arrival, _)) = arrivals.peek() {
                next = next.min(arrival);
            }
            now = next;
            let kick = arrivals.next_if(|&(arrival, _)| arrival <= now).map(|(_, kick)| kick);
            let action = planner.poll(rhythm.as_ref(), now, kick);
            if action != Action::None {
                actions.push((now, action));
            }
        }
        (actions, planner)
    }

    fn after(actions: &[(f64, Action)], t: f64) -> Vec<Action> {
        actions.iter().filter(|&&(at, _)| at > t).map(|&(_, action)| action).collect()
    }

    #[test]
    fn anticipates_steady_kicks_from_the_third_one() {
        let kicks: Vec<f64> = (1..=20).map(|n| beats(n as f64)).collect();
        let actions = simulate(FlashMode::Kicks, &kicks, beats(20.5));
        assert_eq!(actions.len(), kicks.len(), "un flash par kick : {actions:?}");
        assert_eq!(actions[0].1, REACTIVE);
        assert_eq!(actions[1].1, REACTIVE);
        for (&(at, action), &kick) in actions[2..].iter().zip(&kicks[2..]) {
            assert_eq!(action, ANTICIPATED);
            assert!((at - (kick - ADVANCE)).abs() < 1e-9, "flash à {at}, kick à {kick}");
        }
    }

    /// En plein groove, le premier temps sans kick garde son flash (kick peut-être noyé) ; le
    /// second, anticipé lui aussi, est annulé, puis la lumière reste éteinte.
    #[test]
    fn a_break_costs_one_extra_flash_and_one_cancelled_then_stays_dark() {
        let kicks: Vec<f64> = (1..=10).map(|n| beats(n as f64)).collect();
        let actions = simulate(FlashMode::Kicks, &kicks, beats(30.0));
        assert_eq!(after(&actions, beats(10.5)), [ANTICIPATED, ANTICIPATED, Action::Cancel]);
        let cancel = actions.last().unwrap().0;
        assert!((cancel - (beats(12.0) + CONFIRM)).abs() < 1e-9);
    }

    /// Un kick noyé dans le mix, non détecté : son temps flashe quand même et le groove continue.
    #[test]
    fn a_kick_missed_in_the_groove_still_flashes() {
        let kicks: Vec<f64> = (1..=12).filter(|&n| n != 7).map(|n| beats(n as f64)).collect();
        let actions = simulate(FlashMode::Kicks, &kicks, beats(12.5));
        let flashes = after(&actions, beats(2.5));
        assert_eq!(flashes, [ANTICIPATED; 10], "un flash par temps, du 3e au 12e : {actions:?}");
    }

    #[test]
    fn kicks_back_after_a_break_flash_at_once_then_get_anticipated() {
        let mut kicks: Vec<f64> = (1..=8).map(|n| beats(n as f64)).collect();
        kicks.extend((20..=26).map(|n| beats(n as f64)));
        let actions = simulate(FlashMode::Kicks, &kicks, beats(26.5));
        let back = after(&actions, beats(19.5));
        assert_eq!(back, [REACTIVE, REACTIVE, ANTICIPATED, ANTICIPATED, ANTICIPATED, ANTICIPATED, ANTICIPATED]);
    }

    #[test]
    fn every_kick_of_a_build_up_roll_flashes() {
        // Roulement en doubles croches, puis en triolets, sans autre kick autour.
        let mut kicks: Vec<f64> = (16..24).map(|n| beats(n as f64 / 4.0)).collect();
        kicks.extend((18..24).map(|n| beats(n as f64 / 3.0)));
        let actions = simulate(FlashMode::Kicks, &kicks, beats(7.8));
        let flashes = actions.iter().filter(|(_, a)| matches!(a, Action::Flash { .. })).count();
        assert_eq!(flashes, kicks.len(), "{actions:?}");
    }

    /// Mix chargé : le kick du 6e temps est détecté après la fin de l'attente de confirmation.
    /// En plein groove, son flash n'a pas été coupé et rien ne flashe deux fois.
    #[test]
    fn a_late_kick_in_the_groove_keeps_its_flash() {
        let kicks: Vec<(f64, f64)> = (1..=10)
            .map(|n| (beats(n as f64), if n == 6 { CONFIRM + 0.02 } else { ARRIVAL }))
            .collect();
        let (actions, _) = simulate_arrivals(FlashMode::Kicks, &kicks, beats(10.5));
        assert_eq!(after(&actions, beats(5.5)), [ANTICIPATED; 5], "{actions:?}");
    }

    /// Hors groove (trois temps confirmés seulement), un kick en retard rallume son flash annulé.
    #[test]
    fn a_late_kick_before_the_groove_flashes_again() {
        let kicks: Vec<(f64, f64)> = (1..=6)
            .map(|n| (beats(n as f64), if n == 4 { CONFIRM + 0.02 } else { ARRIVAL }))
            .collect();
        let (actions, _) = simulate_arrivals(FlashMode::Kicks, &kicks, beats(6.5));
        assert_eq!(after(&actions, beats(3.5)), [ANTICIPATED, Action::Cancel, REACTIVE, ANTICIPATED, ANTICIPATED]);
    }

    #[test]
    fn groove_needs_four_beats_and_ends_after_two_missed_kicks() {
        let rhythm = Rhythm::on_beats(&BeatClock::new(BPM, 0.0));
        let three: Vec<(f64, f64)> = (1..=3).map(|n| (beats(n as f64), ARRIVAL)).collect();
        let (_, planner) = simulate_arrivals(FlashMode::Kicks, &three, beats(3.2));
        assert!(!planner.in_groove(rhythm.as_ref(), beats(3.2)));

        let six: Vec<(f64, f64)> = (1..=6).map(|n| (beats(n as f64), ARRIVAL)).collect();
        let (_, planner) = simulate_arrivals(FlashMode::Kicks, &six, beats(6.2));
        assert!(planner.in_groove(rhythm.as_ref(), beats(6.2)));
        assert!(planner.in_groove(rhythm.as_ref(), beats(7.0)), "le kick du temps suivant est attendu");
        let (_, planner) = simulate_arrivals(FlashMode::Kicks, &six, beats(7.6));
        assert!(planner.in_groove(rhythm.as_ref(), beats(7.6)), "un seul kick manqué : le groove tient");
        let (_, planner) = simulate_arrivals(FlashMode::Kicks, &six, beats(8.6));
        assert!(!planner.in_groove(rhythm.as_ref(), beats(8.6)), "deux kicks manqués de suite");
    }

    #[test]
    fn reactive_mode_never_anticipates() {
        let kicks: Vec<f64> = (1..=10).map(|n| beats(n as f64)).collect();
        let actions = simulate(FlashMode::Reactive, &kicks, beats(15.0));
        assert_eq!(actions.len(), kicks.len());
        assert!(actions.iter().all(|&(_, action)| action == REACTIVE));
    }

    /// Hard groove : kicks sur les doubles croches 0, 5, 8 et 13, jamais sur le 2e ni le 4e temps.
    #[test]
    fn a_syncopated_pattern_is_anticipated_and_grooves() {
        let hits = crate::rhythm::pattern("x....x..x....x..");
        let rhythm = Rhythm { period: 60.0 / BPM, bar: beats(1.0), hits };
        let kicks: Vec<(f64, f64)> = (0..64)
            .filter(|n| hits >> (n % 16) & 1 == 1)
            .map(|n| (beats(1.0 + n as f64 / 4.0), ARRIVAL))
            .collect();
        let until = kicks.last().unwrap().0 + 0.05;
        let (actions, planner) = simulate_rhythm(Some(rhythm), FlashMode::Kicks, &kicks, until);
        assert_eq!(actions.len(), kicks.len(), "un flash par kick : {actions:?}");
        for (&(at, action), &(kick, _)) in actions[2..].iter().zip(&kicks[2..]) {
            assert_eq!(action, ANTICIPATED);
            assert!((at - (kick - ADVANCE)).abs() < 1e-9, "flash à {at}, kick à {kick}");
        }
        assert!(planner.in_groove(Some(&rhythm), until), "groove sans kick sur chaque temps");
        // Le même jeu lu comme un kick par temps n'aurait jamais groové.
        let (_, beats_only) = simulate_arrivals(FlashMode::Kicks, &kicks, until);
        assert!(!beats_only.in_groove(Rhythm::on_beats(&BeatClock::new(BPM, 0.0)).as_ref(), until));
    }

    #[test]
    fn every_beat_mode_flashes_even_without_kicks() {
        let actions = simulate(FlashMode::EveryBeat, &[], beats(10.5));
        assert_eq!(actions.len(), 11, "{actions:?}");
        assert!(actions.iter().all(|&(_, action)| action == ANTICIPATED));
    }
}

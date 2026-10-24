//! Phases du morceau reconnues à l'écoute : groove, break (temps creux), montée et drop.

use super::*;

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
/// Temps pour qu'une montée atteigne son plein, faute de structure connue.
const BUILD_RAMP: f32 = 8.0;
/// Le drop d'une montée tombe sur une tête de phrase (16 temps), au moins 8 temps après son
/// début ; passé d'un demi-temps sans drop, il est attendu à la tête suivante.
const BEATS_PER_PHRASE: f32 = 16.0;
const BUILD_MIN_BEATS: f32 = 8.0;
const DROP_LATE: f32 = 0.5;
/// Un drop montré d'avance se confirme par un kick aux basses fortes de 0,1 s avant à 0,25 s
/// après l'instant prévu (le kick est détecté un peu après qu'il sonne).
const DROP_EARLY: f32 = 0.1;
const DROP_CONFIRM: f32 = 0.25;
/// … et des basses nettement au-dessus de celles de la montée (un kick du roulement ne suffit pas).
const DROP_JUMP: f32 = 1.3;
/// Le drop : un kick aux basses fortes, après une seconde de basses faibles.
const DROP_BASS: f32 = 0.6;
const DROP_QUIET_BASS: f32 = 0.35;
const BASS_MEMORY: f32 = 1.0;
/// Break : la grille ne trouve plus de kicks réguliers, et plus aucun kick depuis 1,5 s ou des
/// basses tombées sous 40 % de leur niveau habituel en groove (il monte en 8 s, ne baisse qu'en 60 s ;
/// niveau actuel sur 0,5 s). Des basses disparues (sous 25 %, un kick seul en garde davantage)
/// pendant 2 s font aussi un break, même si des percussions tombent sur les temps. Retour au
/// groove : kicks réguliers sur la grille, avec des basses revenues aux trois quarts de leur niveau,
/// ou au-dessus de ces 40 % depuis 8 s (des percussions de break ne tiennent pas la grille si
/// longtemps). Hors groove, le niveau habituel s'oublie lentement (90 s) :
/// un morceau aux basses plus légères finit par groover.
const CALM_AFTER: f32 = 1.5;
const GROOVE_BASS_TIME: f32 = 8.0;
const BASS_NOW_TIME: f32 = 0.5;
const BREAK_BASS: f32 = 0.4;
const RETURN_BASS: f32 = 0.75;
const RETURN_STEADY: f32 = 8.0;
const GONE_BASS: f32 = 0.25;
const BASS_GONE: f32 = 2.0;
const GROOVE_BASS_FALL: f32 = 60.0;
/// Retour immédiat au drop : deux kicks à 90 % des basses habituelles en moins d'une seconde.
const DROP_RETURN_BASS: f32 = 0.9;
const DROP_RETURN_PAIR: f32 = 1.0;
/// … une fois qu'un vrai groove (grille tenue 4 s) a fixé le niveau de basse habituel.
const GROOVE_HEARD: f32 = 4.0;
const GROOVE_BASS_FORGET: f32 = 90.0;
/// Le break se confirme quand la grille a perdu les kicks depuis 1 s et qu'il en arrive moins de
/// trois en 2 s (ou plus du tout, ou que les basses tombent).
const BREAK_CONFIRM: f32 = 1.0;
const SPARSE_KICKS: f32 = 3.0;

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

/// Ce qui permet de reconnaître la phase du morceau.
#[derive(Debug, Default)]
pub(super) struct PhaseTracker {
    pub(super) phase: Phase,
    /// Durée passée dans la phase actuelle (s).
    pub(super) phase_time: f32,
    /// Horloge propre (s) et instants des derniers kicks, pour leur cadence.
    pub(super) time: f32,
    pub(super) kick_times: std::collections::VecDeque<f32>,
    /// Un kick a été détecté depuis le dernier rendu.
    pub(super) kicked: bool,
    pub(super) presence_fast: Option<f32>,
    pub(super) presence_slow: Option<f32>,
    pub(super) bass_memory: f32,
    /// Temps écoulé sans que rien ne monte, pendant une montée.
    pub(super) stall: f32,
    /// Avancement de la montée, de 0 à 1.
    pub(super) build: f32,
    /// La musique groove (avec hystérésis) : c'est l'inverse d'un temps creux.
    pub(super) grooving: bool,
    /// Basses brutes : niveau habituel pendant le groove (lent) et niveau actuel (rapide).
    pub(super) groove_bass: f32,
    pub(super) bass_now: f32,
    /// Durée depuis laquelle la grille confirme des kicks réguliers.
    pub(super) grid_held: f32,
    /// Durée depuis laquelle la grille ne les confirme plus.
    pub(super) grid_lost: f32,
    /// Durée depuis laquelle les basses ont disparu (sous 25 % de leur niveau habituel).
    pub(super) bass_gone_time: f32,
    /// Dernier kick aux basses pleines hors groove (un deuxième coup sur coup signe le drop).
    pub(super) loud_kick: Option<f32>,
    /// Un vrai groove a été entendu (grille tenue 4 s) : le niveau de basse habituel est fiable.
    pub(super) groove_heard: bool,
    /// Structure connue : période du temps (s) et temps restant avant la prochaine tête de phrase.
    pub(super) structure: Option<(f32, f32)>,
    /// Début de la montée en cours, et instant prévu de son drop (une tête de phrase).
    pub(super) build_start: f32,
    pub(super) drop_at: Option<f32>,
    /// Drop montré d'avance (instant prévu), en attente de sa confirmation ; une montée dont le
    /// drop anticipé n'est pas venu n'en anticipe plus.
    pub(super) anticipated: Option<f32>,
    pub(super) anticipation_missed: bool,
    /// Le drop anticipé n'est pas venu : l'éclat doit s'éteindre (lu une fois).
    pub(super) false_drop: bool,
}

/// Ce que la phase sait des kicks : secondes depuis le dernier détecté, et si la grille de temps
/// les confirme régulièrement (`None` tant que le tempo est inconnu).
#[derive(Debug, Clone, Copy)]
pub struct Pulse {
    pub since_kick: f32,
    pub grid_groove: Option<bool>,
}

impl PhaseTracker {
    pub(super) fn kick(&mut self) {
        self.kicked = true;
        self.kick_times.push_back(self.time);
    }

    /// Met la phase à jour ; `true` au drop qui conclut une montée.
    pub(super) fn update(&mut self, dt: f32, levels: Levels, pulse: Pulse) -> bool {
        self.time += dt;
        self.phase_time += dt;
        self.update_groove(dt, levels.raw_bass, pulse);
        let grooving = self.grooving;
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
        let bass_before = self.bass_memory;
        let quiet_bass_before = bass_before < DROP_QUIET_BASS;
        self.bass_memory = approach(self.bass_memory, levels.bass, dt, BASS_MEMORY);
        let kicked = std::mem::take(&mut self.kicked);
        // Une simple hausse de présence ne suffit pas quand les basses sont là : c'est un drop
        // ou l'entrée d'un élément, pas une montée.
        let building = (rise > BUILD_RISE_DB && self.bass_memory < BUILD_MAX_BASS) || rolling;
        self.stall = if building { 0.0 } else { self.stall + dt };

        // Drop anticipé : confirmé par un kick aux basses fortes autour de l'instant prévu.
        let expected = self.anticipated.filter(|&at| self.time >= at - DROP_EARLY);
        let confirmed = expected.is_some() && kicked && levels.bass > DROP_BASS.max(DROP_JUMP * bass_before);
        if confirmed {
            self.anticipated = None;
        } else if self.anticipated.is_some_and(|at| self.time > at + DROP_CONFIRM) {
            self.anticipated = None;
            self.anticipation_missed = true;
            self.false_drop = true;
        }
        let mut dropped = false;
        let next = match self.phase {
            Phase::BuildUp if confirmed => {
                dropped = true;
                Phase::Groove
            }
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
        if dropped {
            // Le roulement qui menait au drop est fini : ses kicks ne relancent pas une montée.
            self.anticipated = None;
            self.kick_times.retain(|&t| t >= self.time);
        }
        if next != self.phase {
            if next == Phase::BuildUp {
                self.build_start = self.time;
                self.drop_at = None;
                self.anticipation_missed = false;
            }
            self.phase = next;
            self.phase_time = 0.0;
        }
        self.build = if self.phase == Phase::BuildUp {
            self.aim_drop();
            let surge = ((rise - BUILD_RISE_DB) / 10.0).clamp(0.0, 1.0);
            // Vers le drop prévu quand la structure est connue, sinon au fil du temps.
            let progress = match self.drop_at {
                Some(at) => self.build.max((self.time - self.build_start) / (at - self.build_start).max(1e-3)),
                None => self.build + dt / BUILD_RAMP,
            };
            progress.max(surge).min(1.0)
        } else {
            self.drop_at = None;
            approach(self.build, 0.0, dt, MOTION_FADE)
        };
        dropped
    }

    /// Prévoit le drop de la montée : la première tête de phrase au moins `BUILD_MIN_BEATS` après
    /// son début, puis la suivante chaque fois qu'il ne vient pas.
    fn aim_drop(&mut self) {
        let Some((period, to_head)) = self.structure else { return };
        let missed = self.drop_at.is_some_and(|at| self.time > at + DROP_LATE * period);
        if self.drop_at.is_some() && !missed {
            return;
        }
        let earliest = match self.drop_at {
            Some(at) => at + DROP_LATE * period,
            None => self.build_start + BUILD_MIN_BEATS * period,
        };
        let phrase = BEATS_PER_PHRASE * period;
        let mut head = self.time + to_head * period;
        while head < earliest {
            head += phrase;
        }
        self.drop_at = Some(head);
    }

    /// Secondes avant le drop prévu de la montée en cours.
    pub(super) fn drop_in(&self) -> Option<f32> {
        (self.phase == Phase::BuildUp).then_some(self.drop_at?).map(|at| at - self.time)
    }

    /// Le drop prévu tombe dans moins de `advance` : vrai une fois par drop, qui est alors
    /// attendu pour confirmation.
    pub(super) fn anticipate_drop(&mut self, advance: f32) -> bool {
        let Some(at) = self.drop_at.filter(|_| self.phase == Phase::BuildUp && !self.anticipation_missed) else {
            return false;
        };
        if self.anticipated.is_some() || at - self.time > advance {
            return false;
        }
        self.anticipated = Some(at);
        true
    }
}

impl PhaseTracker {
    /// Groove ou temps creux, avec hystérésis : un roulement ou un coup isolé pendant un break ne
    /// relance pas le groove, il faut que la grille retrouve des kicks réguliers.
    fn update_groove(&mut self, dt: f32, raw_bass: f32, pulse: Pulse) {
        self.bass_now = approach(self.bass_now, raw_bass, dt, BASS_NOW_TIME);
        let bass_low = self.groove_bass > 0.0 && self.bass_now < BREAK_BASS * self.groove_bass;
        // Le niveau habituel monte vite avec le groove mais n'en suit que lentement la baisse : un
        // break qui s'installe ne l'entraîne pas avec lui. Hors groove, il s'oublie lentement.
        if self.groove_bass == 0.0 {
            self.groove_bass = raw_bass * 0.5;
        } else if self.grooving {
            let time = if self.bass_now > self.groove_bass { GROOVE_BASS_TIME } else { GROOVE_BASS_FALL };
            self.groove_bass = approach(self.groove_bass, self.bass_now, dt, time);
        } else if self.bass_now < self.groove_bass {
            self.groove_bass = approach(self.groove_bass, self.bass_now, dt, GROOVE_BASS_FORGET);
        }
        // Plus de basse du tout (un kick seul en garde davantage) : break, même sur la grille.
        let bass_gone = self.groove_bass > 0.0 && self.bass_now < GONE_BASS * self.groove_bass;
        self.bass_gone_time = if bass_gone { self.bass_gone_time + dt } else { 0.0 };
        let kick_gap = pulse.since_kick > CALM_AFTER;
        let Some(grid) = pulse.grid_groove else {
            // Tempo inconnu : un kick récent suffit, comme avant.
            self.grooving = !kick_gap;
            return;
        };
        self.grid_held = if grid { self.grid_held + dt } else { 0.0 };
        self.grid_lost = if grid { 0.0 } else { self.grid_lost + dt };
        let bass_back = self.bass_now >= RETURN_BASS * self.groove_bass;
        // Quelques coups isolés (roulement, impact) ne font pas un groove.
        let sparse = (self.kick_times.len() as f32) < SPARSE_KICKS;
        // Le drop : deux kicks aux basses pleines coup sur coup, sans attendre que la grille les
        // confirme (un impact isolé ne suffit pas).
        let loud_kick = pulse.since_kick < 1.5 * dt && raw_bass >= DROP_RETURN_BASS * self.groove_bass;
        if self.grooving && self.grid_held >= GROOVE_HEARD {
            self.groove_heard = true;
        }
        let dropped = self.groove_heard && loud_kick && self.loud_kick.is_some_and(|t| self.time - t <= DROP_RETURN_PAIR);
        if loud_kick {
            self.loud_kick = Some(self.time);
        }
        self.grooving = if self.grooving {
            let kicks_lost = self.grid_lost >= BREAK_CONFIRM && (kick_gap || sparse || bass_low);
            !(kicks_lost || self.bass_gone_time >= BASS_GONE)
        } else if dropped {
            // La grille rattrape le drop ensuite : on ne compte pas le break comme kicks perdus.
            self.grid_lost = 0.0;
            true
        } else {
            grid && !bass_low && (bass_back || self.grid_held >= RETURN_STEADY)
        };
    }
}

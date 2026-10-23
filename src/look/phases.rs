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
/// Temps pour qu'une montée atteigne son plein.
const BUILD_RAMP: f32 = 8.0;
/// Le drop : un kick aux basses fortes, après une seconde de basses faibles.
const DROP_BASS: f32 = 0.6;
const DROP_QUIET_BASS: f32 = 0.35;
const BASS_MEMORY: f32 = 1.0;
/// Break : la grille ne trouve plus de kicks réguliers, et plus aucun kick depuis 1,5 s ou des
/// basses tombées sous 40 % de leur niveau habituel en groove (mémoire de 8 s, niveau actuel sur
/// 0,5 s). Retour au groove : kicks réguliers sur la grille, avec des basses revenues à la moitié
/// de leur niveau, ou depuis 4 s.
const CALM_AFTER: f32 = 1.5;
const GROOVE_BASS_TIME: f32 = 8.0;
const BASS_NOW_TIME: f32 = 0.5;
const BREAK_BASS: f32 = 0.4;
const RETURN_BASS: f32 = 0.5;
const RETURN_STEADY: f32 = 4.0;
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
        let quiet_bass_before = self.bass_memory < DROP_QUIET_BASS;
        self.bass_memory = approach(self.bass_memory, levels.bass, dt, BASS_MEMORY);
        let kicked = std::mem::take(&mut self.kicked);
        // Une simple hausse de présence ne suffit pas quand les basses sont là : c'est un drop
        // ou l'entrée d'un élément, pas une montée.
        let building = (rise > BUILD_RISE_DB && self.bass_memory < BUILD_MAX_BASS) || rolling;
        self.stall = if building { 0.0 } else { self.stall + dt };

        let mut dropped = false;
        let next = match self.phase {
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
        if next != self.phase {
            self.phase = next;
            self.phase_time = 0.0;
        }
        self.build = if self.phase == Phase::BuildUp {
            let surge = ((rise - BUILD_RISE_DB) / 10.0).clamp(0.0, 1.0);
            (self.build + dt / BUILD_RAMP).max(surge).min(1.0)
        } else {
            approach(self.build, 0.0, dt, MOTION_FADE)
        };
        dropped
    }
}

impl PhaseTracker {
    /// Groove ou temps creux, avec hystérésis : un roulement ou un coup isolé pendant un break ne
    /// relance pas le groove, il faut que la grille retrouve des kicks réguliers.
    fn update_groove(&mut self, dt: f32, raw_bass: f32, pulse: Pulse) {
        self.bass_now = approach(self.bass_now, raw_bass, dt, BASS_NOW_TIME);
        if self.grooving || self.groove_bass == 0.0 {
            self.groove_bass = approach(self.groove_bass.max(raw_bass * 0.5), raw_bass, dt, GROOVE_BASS_TIME);
        }
        let kick_gap = pulse.since_kick > CALM_AFTER;
        let Some(grid) = pulse.grid_groove else {
            // Tempo inconnu : un kick récent suffit, comme avant.
            self.grooving = !kick_gap;
            return;
        };
        self.grid_held = if grid { self.grid_held + dt } else { 0.0 };
        self.grid_lost = if grid { 0.0 } else { self.grid_lost + dt };
        let bass_low = self.groove_bass > 0.0 && self.bass_now < BREAK_BASS * self.groove_bass;
        let bass_back = self.bass_now >= RETURN_BASS * self.groove_bass;
        // Quelques coups isolés (roulement, impact) ne font pas un groove.
        let sparse = (self.kick_times.len() as f32) < SPARSE_KICKS;
        self.grooving = if self.grooving {
            !(self.grid_lost >= BREAK_CONFIRM && (kick_gap || sparse || bass_low))
        } else {
            grid && (bass_back || self.grid_held >= RETURN_STEADY)
        };
    }
}

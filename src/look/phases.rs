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
}

impl PhaseTracker {
    pub(super) fn kick(&mut self) {
        self.kicked = true;
        self.kick_times.push_back(self.time);
    }

    /// Met la phase à jour ; `true` au drop qui conclut une montée.
    pub(super) fn update(&mut self, dt: f32, levels: Levels, grooving: bool) -> bool {
        self.time += dt;
        self.phase_time += dt;
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

//! Ableton Link : rejoint la session du réseau local (Live, Move, Push, synthés compatibles)
//! pour obtenir le tempo et l'instant exact des temps, sans rien détecter.

use rusty_link::{AblLink, SessionState};

/// Longueur de mesure partagée avec la session : les temps s'alignent sur ceux des autres.
const QUANTUM: f64 = 4.0;
/// Tempo proposé quand on crée la session (aucun autre participant).
const DEFAULT_BPM: f64 = 120.0;

/// Ce que dit la session à un instant donné.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkBeat {
    pub bpm: f64,
    /// Instant du dernier temps entier, en secondes de l'horloge commune du programme.
    pub beat_time: f64,
    pub peers: u64,
}

pub struct LinkSession {
    link: AblLink,
    state: SessionState,
}

impl LinkSession {
    /// Rejoint (ou crée) la session ; `bpm` ne sert que si personne d'autre n'y est.
    pub fn join(bpm: Option<f64>) -> Self {
        let link = AblLink::new(bpm.unwrap_or(DEFAULT_BPM));
        link.enable(true);
        Self { link, state: SessionState::new() }
    }

    /// Lit la session ; `now` est l'instant présent en secondes de l'horloge commune.
    pub fn read(&mut self, now: f64) -> LinkBeat {
        let link_now = self.link.clock_micros();
        self.link.capture_app_session_state(&mut self.state);
        let beat = self.state.beat_at_time(link_now, QUANTUM).floor();
        let beat_micros = self.state.time_at_beat(beat, QUANTUM);
        LinkBeat {
            bpm: self.state.tempo(),
            beat_time: to_local(beat_micros, link_now, now),
            peers: self.link.num_peers(),
        }
    }
}

impl Drop for LinkSession {
    fn drop(&mut self) {
        self.link.enable(false);
    }
}

/// Convertit un instant de l'horloge de Link (µs) en secondes de l'horloge commune,
/// connaissant le même instant présent dans les deux horloges.
fn to_local(link_micros: i64, link_now: i64, now: f64) -> f64 {
    now + (link_micros - link_now) as f64 * 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_link_time_around_the_present() {
        assert_eq!(to_local(5_000_000, 5_000_000, 12.0), 12.0);
        assert!((to_local(4_750_000, 5_000_000, 12.0) - 11.75).abs() < 1e-12);
        assert!((to_local(5_387_000, 5_000_000, 12.0) - 12.387).abs() < 1e-12);
    }
}

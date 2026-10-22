//! Entrée MIDI : les notes jouées sur le synthé déclenchent des flashs à leur couleur.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow};
use midir::{Ignore, MidiInput, MidiInputConnection};

const NOTE_ON: u8 = 0x90;
const STATUS_MASK: u8 = 0xF0;
const CLIENT_NAME: &str = "dj-bridge";

/// Note et vélocité d'une note-on ; une note-on de vélocité nulle est une note-off.
pub fn note_on(message: &[u8]) -> Option<(u8, u8)> {
    match *message {
        [status, note, velocity, ..] if status & STATUS_MASK == NOTE_ON && velocity > 0 => Some((note, velocity)),
        _ => None,
    }
}

/// Teinte d'une note dans [0, 1[ : les 12 notes parcourent le cercle des quintes, pour que des
/// notes harmoniquement proches aient des couleurs voisines (do rouge, sol orange, ré jaune…).
pub fn note_hue(note: u8) -> f32 {
    f32::from(note % 12 * 7 % 12) / 12.0
}

/// Dernière note reçue, publiée sans verrou pour la boucle d'éclairage.
#[derive(Debug, Default)]
pub struct Notes {
    count: AtomicU32,
    /// `note << 8 | vélocité` de la dernière note-on.
    last: AtomicU32,
}

impl Notes {
    /// Nombre de notes reçues depuis le démarrage.
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }

    /// Note et vélocité de la dernière note-on.
    pub fn last(&self) -> (u8, u8) {
        let packed = self.last.load(Ordering::Relaxed);
        ((packed >> 8) as u8, packed as u8)
    }

    fn publish(&self, note: u8, velocity: u8) {
        self.last.store(u32::from(note) << 8 | u32::from(velocity), Ordering::Relaxed);
        // `Release` : qui voit le nouveau compteur voit aussi la note.
        self.count.fetch_add(1, Ordering::Release);
    }
}

pub fn list_inputs() -> Result<Vec<String>> {
    let input = MidiInput::new(CLIENT_NAME)?;
    Ok(input.ports().iter().filter_map(|port| input.port_name(port).ok()).collect())
}

/// Connexion ouverte à un port MIDI ; elle se ferme quand elle est abandonnée.
pub struct MidiListener {
    _connection: MidiInputConnection<()>,
    pub port: String,
}

impl MidiListener {
    /// `wanted` : morceau du nom du port ; à défaut, le premier port MIDI.
    pub fn start(wanted: Option<&str>, notes: Arc<Notes>) -> Result<Self> {
        let mut input = MidiInput::new(CLIENT_NAME)?;
        input.ignore(Ignore::All);
        let ports = input.ports();
        let port = match wanted {
            Some(name) => {
                let needle = name.to_lowercase();
                ports
                    .iter()
                    .find(|port| input.port_name(port).is_ok_and(|n| n.to_lowercase().contains(&needle)))
                    .with_context(|| format!("aucune entrée MIDI ne contient « {name} » : lance `dj-bridge devices`"))?
            }
            None => ports.first().context("aucune entrée MIDI : le synthé est-il branché en USB ?")?,
        };
        let port_name = input.port_name(port)?;
        let connection = input
            .connect(
                port,
                CLIENT_NAME,
                move |_, message, _| {
                    if let Some((note, velocity)) = note_on(message) {
                        notes.publish(note, velocity);
                    }
                },
                (),
            )
            .map_err(|e| anyhow!("connexion à « {port_name} » impossible : {e}"))?;
        Ok(Self { _connection: connection, port: port_name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_note_ons_on_every_channel() {
        assert_eq!(note_on(&[0x90, 60, 100]), Some((60, 100)));
        assert_eq!(note_on(&[0x9F, 36, 1]), Some((36, 1)));
        assert_eq!(note_on(&[0x90, 60, 0]), None, "vélocité nulle = note-off");
        assert_eq!(note_on(&[0x80, 60, 64]), None);
        assert_eq!(note_on(&[0xB0, 1, 64]), None, "contrôleur");
        assert_eq!(note_on(&[0xF8]), None, "horloge");
    }

    #[test]
    fn notes_follow_the_circle_of_fifths() {
        assert_eq!(note_hue(60), 0.0); // do
        assert_eq!(note_hue(67), 1.0 / 12.0); // sol
        assert_eq!(note_hue(62), 2.0 / 12.0); // ré
        assert_eq!(note_hue(72), note_hue(60), "même note, autre octave");
    }

    #[test]
    fn publishes_the_last_note() {
        let notes = Notes::default();
        notes.publish(64, 90);
        notes.publish(67, 127);
        assert_eq!(notes.count(), 2);
        assert_eq!(notes.last(), (67, 127));
    }
}

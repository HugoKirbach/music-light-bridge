//! Petits outils de terminal partagés par les commandes interactives.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, ClearType};

/// Mode brut (touches lues une à une), rétabli automatiquement à la sortie.
pub struct RawMode;

impl RawMode {
    pub fn enable() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Attend une touche pressée pendant au plus `timeout`.
pub fn poll_key(timeout: Duration) -> Result<Option<KeyEvent>> {
    if event::poll(timeout)?
        && let Event::Key(key) = event::read()?
        && key.kind == KeyEventKind::Press
    {
        return Ok(Some(key));
    }
    Ok(None)
}

/// Échap, q ou Ctrl+C.
pub fn is_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Réécrit la ligne d'état, coupée à la largeur du terminal pour qu'elle ne passe
/// jamais à la ligne (sinon `\r` ne la remplacerait plus).
pub fn status_line(out: &mut impl Write, text: &str) -> Result<()> {
    let width = terminal::size().map_or(80, |(w, _)| usize::from(w)).saturating_sub(1);
    let line: String = text.chars().take(width).collect();
    write!(out, "\r{line}")?;
    crossterm::queue!(out, terminal::Clear(ClearType::UntilNewLine))?;
    out.flush()?;
    Ok(())
}

/// Barre de niveau, `value` dans [0, 1].
pub fn meter(value: f32, width: usize) -> String {
    let filled = ((value.clamp(0.0, 1.0) * width as f32).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "·".repeat(width - filled))
}

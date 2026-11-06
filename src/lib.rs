//! DJ-Bridge : lumières Philips Hue pilotées par le son et le tempo,
//! avec la latence la plus faible possible.
//!
//! Deux exécutables s'en servent : `dj-bridge` (la ligne de commande, et la fenêtre avec `gui`)
//! et `dj-bridge-gui` (la fenêtre seule, sans console, pour un double-clic).

pub mod audio;
pub mod cmd;
pub mod config;
pub mod engine;
pub mod flash;
pub mod gui;
pub mod hue;
pub mod link;
pub mod look;
pub mod metronome;
pub mod midi;
pub mod opus;
pub mod phrase;
pub mod prolink;
pub mod rhythm;
pub mod setlist;
pub mod room;
pub mod tempo;
pub mod timing;
pub mod wav;

use std::sync::Arc;

use anyhow::Result;
use tokio::runtime::Runtime;

/// Exécuteur des tâches réseau (bridge, télécommande), partagé par les commandes.
pub fn runtime() -> Result<Arc<Runtime>> {
    Ok(Arc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?))
}

/// Ouvre la fenêtre de DJ-Bridge.
pub fn open_window() -> Result<()> {
    gui::run(runtime()?, config::Config::load()?)
}

//! DJ-Bridge : lumières Philips Hue pilotées par le son et le tempo,
//! avec la latence la plus faible possible.

mod cmd;
mod config;
mod hue;
mod metronome;
mod timing;

use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::Config;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Cherche les bridges Hue du réseau local
    Discover,
    /// Associe DJ-Bridge au bridge (il faudra appuyer sur son bouton)
    Pair {
        /// IP du bridge ; découverte automatique si absente
        #[arg(long)]
        ip: Option<String>,
    },
    /// Liste les zones de divertissement et choisit celle à utiliser
    Areas {
        /// Identifiant de la zone à utiliser
        #[arg(long)]
        select: Option<String>,
    },
    /// Vérifie le flux : arc-en-ciel à 60 Hz et régularité d'envoi
    Test {
        /// Durée du test (s)
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Clic + flash pour mesurer et calibrer la latence de la chaîne Hue
    Latency {
        /// Intervalle entre deux clics (ms)
        #[arg(long, default_value_t = 1000)]
        period_ms: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut cfg = Config::load()?;
    match cli.command {
        Command::Discover => cmd::setup::discover(&rt),
        Command::Pair { ip } => cmd::setup::pair(&rt, &mut cfg, ip),
        Command::Areas { select } => cmd::setup::areas(&rt, &mut cfg, select),
        Command::Test { seconds } => cmd::test::run(&rt, &cfg, Duration::from_secs(seconds)),
        Command::Latency { period_ms } => {
            cmd::latency::run(&rt, &mut cfg, Duration::from_millis(period_ms))
        }
    }
}

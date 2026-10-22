//! DJ-Bridge : lumières Philips Hue pilotées par le son et le tempo,
//! avec la latence la plus faible possible.

mod audio;
mod cmd;
mod config;
mod flash;
mod hue;
mod look;
mod metronome;
mod tempo;
mod timing;
mod wav;

use std::path::PathBuf;
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
    /// Liste les entrées audio
    Devices,
    /// Mode live : flashs sur les vrais kicks de l'entrée audio, anticipés grâce au tempo
    Live {
        /// Morceau du nom de l'entrée audio (voir `devices`) ; la Focusrite par défaut
        #[arg(long)]
        input: Option<String>,
        /// Seuil de détection des kicks (plus haut = moins de détections)
        #[arg(long, default_value_t = audio::analysis::DEFAULT_THRESHOLD)]
        threshold: f32,
        /// Tempo de départ en BPM, modifiable en direct ; détection automatique si absent
        #[arg(long)]
        bpm: Option<f64>,
        /// Tempo minimal envisagé par la détection automatique
        #[arg(long, default_value_t = tempo::DEFAULT_BPM_RANGE.0)]
        bpm_min: f64,
        /// Tempo maximal envisagé par la détection automatique
        #[arg(long, default_value_t = tempo::DEFAULT_BPM_RANGE.1)]
        bpm_max: f64,
        /// Quand flasher : sur les vrais kicks (anticipés), sur chaque kick, ou sur chaque temps
        #[arg(long, value_enum, default_value_t = flash::FlashMode::Kicks)]
        flash: flash::FlashMode,
    },
    /// Rejoue une capture audio (touche `r` du mode live) et résume la détection tranche par tranche
    Analyze {
        /// Fichier WAV à analyser
        path: PathBuf,
        /// Seuil de détection des kicks
        #[arg(long, default_value_t = audio::analysis::DEFAULT_THRESHOLD)]
        threshold: f32,
        /// Tempo imposé ; détection automatique si absent
        #[arg(long)]
        bpm: Option<f64>,
        /// Tempo minimal envisagé par la détection automatique
        #[arg(long, default_value_t = tempo::DEFAULT_BPM_RANGE.0)]
        bpm_min: f64,
        /// Tempo maximal envisagé par la détection automatique
        #[arg(long, default_value_t = tempo::DEFAULT_BPM_RANGE.1)]
        bpm_max: f64,
        /// Durée de chaque tranche du résumé (s)
        #[arg(long, default_value_t = 4.0)]
        slice: f64,
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
        Command::Devices => cmd::setup::devices(),
        Command::Live { input, threshold, bpm, bpm_min, bpm_max, flash } => cmd::live::run(
            &rt,
            &cfg,
            cmd::live::Options { input, threshold, bpm, bpm_range: (bpm_min, bpm_max), flash },
        ),
        Command::Analyze { path, threshold, bpm, bpm_min, bpm_max, slice } => cmd::analyze::run(
            &cfg,
            cmd::analyze::Options { path, threshold, bpm, bpm_range: (bpm_min, bpm_max), slice },
        ),
    }
}

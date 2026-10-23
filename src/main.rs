//! DJ-Bridge : lumières Philips Hue pilotées par le son et le tempo,
//! avec la latence la plus faible possible.

mod audio;
mod cmd;
mod config;
mod engine;
mod flash;
mod gui;
mod hue;
mod link;
mod look;
mod metronome;
mod phrase;
mod midi;
mod tempo;
mod timing;
mod wav;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use config::Config;
use look::{Dynamics, Palette};

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
    /// Rétablit l'état des lampes d'avant le flux, si une session a été interrompue
    Restore,
    /// Liste les entrées audio et MIDI
    Devices {
        /// Ouvre l'entrée audio 3 s et rend compte de ce qu'elle reçoit
        #[arg(long)]
        test: bool,
        /// Entrée à tester (morceau de son nom)
        #[arg(long)]
        input: Option<String>,
        /// Pilote audio à tester
        #[arg(long, value_enum, default_value_t = audio::Backend::Auto)]
        audio: audio::Backend,
    },
    /// Mode live dans le terminal : flashs sur les vrais kicks, anticipés grâce au tempo
    Live(LiveArgs),
    /// Fenêtre : régler le mode live puis le lancer d'un clic
    Gui,
    /// Banc d'essai : rejoue les captures annotées (bench.json) et note la lumière temps par temps
    Bench {
        /// Détaille chaque passage annoté
        #[arg(long)]
        detail: bool,
        /// Liste aussi chaque lumière et son décalage au temps le plus proche (ms)
        #[arg(long = "lumieres")]
        lights: bool,
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

/// Options communes au mode live, dans le terminal comme dans la fenêtre.
#[derive(Args)]
struct LiveArgs {
    /// Morceau du nom de l'entrée audio (voir `devices`) ; la Focusrite par défaut
    #[arg(long)]
    input: Option<String>,
    /// Pilote audio : ASIO si libre (sinon WASAPI), ASIO seul, ou WASAPI seul
    #[arg(long, value_enum, default_value_t = audio::Backend::Auto)]
    audio: audio::Backend,
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
    /// Animation : auto (suit les phases du morceau), pulse, breath (respiration) ou build-up (montée)
    #[arg(long, value_enum, default_value_t = look::Motion::Auto)]
    motion: look::Motion,
    /// Répartition des flashs entre les lampes : auto, together (ensemble), wave (vague),
    /// alternate (alternance gauche/droite) ou split (graves / aigus)
    #[arg(long, value_enum, default_value_t = look::Spread::Auto)]
    spread: look::Spread,
    /// Sans régisseur : en disposition auto, le groove alterne toujours, dans la couleur choisie
    #[arg(long)]
    no_director: bool,
    /// Rejoint la session Ableton Link : tempo et phase viennent de la session
    #[arg(long)]
    link: bool,
    /// Écoute une entrée MIDI (morceau de son nom, voir `devices` ; la première si vide) :
    /// chaque note flashe à sa couleur
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "NOM")]
    midi: Option<String>,
}

impl LiveArgs {
    fn into_options(self, palette: Palette, dynamics: Dynamics) -> engine::Options {
        engine::Options {
            input: self.input,
            audio: self.audio,
            threshold: self.threshold,
            bpm: self.bpm,
            bpm_range: (self.bpm_min, self.bpm_max),
            flash: self.flash,
            link: self.link,
            midi: self.midi.map(|name| (!name.is_empty()).then_some(name)),
            palette,
            dynamics,
            motion: self.motion,
            spread: self.spread,
            director: !self.no_director,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let mut cfg = Config::load()?;
    match cli.command {
        Command::Discover => cmd::setup::discover(&rt),
        Command::Pair { ip } => cmd::setup::pair(&rt, &mut cfg, ip),
        Command::Areas { select } => cmd::setup::areas(&rt, &mut cfg, select),
        Command::Test { seconds } => cmd::test::run(&rt, &cfg, Duration::from_secs(seconds)),
        Command::Latency { period_ms } => {
            cmd::latency::run(&rt, &mut cfg, Duration::from_millis(period_ms))
        }
        Command::Restore => cmd::setup::restore(&rt, &cfg),
        Command::Devices { test: false, .. } => cmd::setup::devices(),
        Command::Devices { test: true, input, audio } => cmd::setup::test_input(input, audio),
        Command::Live(args) => {
            let options = args.into_options(cfg.palette, cfg.dynamics);
            cmd::live::run(rt, &cfg, options)
        }
        Command::Gui => gui::run(rt, cfg),
        Command::Bench { detail, lights } => cmd::bench::run(&cfg, detail, lights),
        Command::Analyze { path, threshold, bpm, bpm_min, bpm_max, slice } => cmd::analyze::run(
            &cfg,
            cmd::analyze::Options { path, threshold, bpm, bpm_range: (bpm_min, bpm_max), slice },
        ),
    }
}

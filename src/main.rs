//! Ligne de commande de DJ-Bridge ; sans commande, elle ouvre la fenêtre.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use dj_bridge::config::Config;
use dj_bridge::{audio, cmd, engine, flash, gui, look, opus, tempo};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Sans commande : la fenêtre
    #[command(subcommand)]
    command: Option<Command>,
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
    /// Sonde OPUS-QUAD : écoute ce que l'OPUS envoie par le câble USB et rend compte de la liaison
    #[command(name = "sonde-opus")]
    Probe {
        /// Durée de l'écoute (s) ; Échap arrête avant
        #[arg(long, default_value_t = 180)]
        seconds: u64,
        /// Morceau du nom de la carte réseau à utiliser ; sinon celle où l'OPUS s'annonce
        #[arg(long)]
        interface: Option<String>,
        /// Écoute seulement, sans se présenter comme rekordbox
        #[arg(long = "passif")]
        passive: bool,
    },
    /// Crée sur le Bureau un raccourci qui ouvre la fenêtre de DJ-Bridge
    #[command(name = "raccourci")]
    Shortcut {
        /// Exécutable visé ; par défaut DJ-Bridge.exe du dossier courant, sinon dj-bridge-gui.exe
        /// à côté de cette commande
        #[arg(long = "cible")]
        target: Option<PathBuf>,
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
    /// Ouvre la télécommande mobile : une page pour téléphone sur le réseau local (adresse affichée)
    #[arg(long)]
    remote: bool,
    /// Rotation des couleurs : off (fixes), 8 ou 16 (la teinte avance toutes les 8 ou 16 mesures
    /// et à chaque drop)
    #[arg(long, value_enum, default_value_t = look::Rotation::Off)]
    rotation: look::Rotation,
    /// Rejoint la session Ableton Link : tempo et phase viennent de la session
    #[arg(long)]
    link: bool,
    /// Suit l'OPUS-QUAD par le réseau (Wi-Fi ou USB) : tempo, temps, phrases et kicks de la platine
    /// maître ; le son devient facultatif
    #[arg(long)]
    opus: bool,
    /// Avec `--opus`, les couleurs de la trame (waveform) au lieu de la palette
    #[arg(long)]
    trame: bool,
    /// Avec `--opus` : aucune entrée audio, la lumière suit l'OPUS seul
    #[arg(long = "sans-son")]
    no_sound: bool,
    /// Avec `--opus` : morceau du nom de la carte réseau de l'OPUS (« Wi-Fi »), pour s'y annoncer dès
    /// le départ
    #[arg(long = "reseau", value_name = "NOM")]
    network: Option<String>,
    /// Écoute une entrée MIDI (morceau de son nom, voir `devices` ; la première si vide) :
    /// chaque note flashe à sa couleur
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "NOM")]
    midi: Option<String>,
}

impl LiveArgs {
    fn into_options(self, cfg: &Config, remote: Option<String>) -> engine::Options {
        let (palette, dynamics) = (cfg.palette, cfg.dynamics);
        // La carte réseau se donne par un morceau de son nom.
        let opus_interface = self.network.and_then(|part| {
            let part = part.to_lowercase();
            opus::interfaces().into_iter().map(|i| i.label).find(|label| label.to_lowercase().contains(&part))
        });
        engine::Options {
            listen: !self.no_sound,
            opus_interface,
            input: self.input,
            audio: self.audio,
            threshold: self.threshold,
            bpm: self.bpm,
            bpm_range: (self.bpm_min, self.bpm_max),
            flash: self.flash,
            link: self.link,
            opus: self.opus,
            opus_offset_ms: cfg.opus_offset_ms,
            trame: self.trame,
            midi: self.midi.map(|name| (!name.is_empty()).then_some(name)),
            palette,
            dynamics,
            motion: self.motion,
            spread: self.spread,
            director: !self.no_director,
            remote,
            rotation: self.rotation,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = dj_bridge::runtime()?;
    let mut cfg = Config::load()?;
    match cli.command.unwrap_or(Command::Gui) {
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
            let remote = if args.remote { Some(cfg.remote_token()?) } else { None };
            let options = args.into_options(&cfg, remote);
            cmd::live::run(rt, &cfg, options)
        }
        Command::Gui => gui::run(rt, cfg),
        Command::Bench { detail, lights } => cmd::bench::run(&cfg, detail, lights),
        Command::Analyze { path, threshold, bpm, bpm_min, bpm_max, slice } => cmd::analyze::run(
            &cfg,
            cmd::analyze::Options { path, threshold, bpm, bpm_range: (bpm_min, bpm_max), slice },
        ),
        Command::Probe { seconds, interface, passive } => {
            cmd::probe::run(cmd::probe::Options { seconds, interface, passive })
        }
        Command::Shortcut { target } => cmd::shortcut::run(target),
    }
}

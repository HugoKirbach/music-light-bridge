//! Moteur du mode live : capture audio, horloge de tempo, flashs, Ableton Link et MIDI, chacun
//! dans son fil. Une interface (terminal ou fenêtre) le démarre, lit son état et le règle.
//!
//! Seuls les vrais kicks captés sur l'entrée audio flashent. Ils alimentent aussi l'horloge de
//! tempo ; quand ils tombent régulièrement sur les temps, le flash du kick suivant part en
//! avance de la latence Hue calibrée pour s'allumer avec lui.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use tokio::runtime::Runtime;

use crate::audio::analysis::{KICK_DETECTION_DELAY, Levels};
use crate::audio::capture::Capture;
use crate::opus::{LinkState, Playing};
use crate::audio::{AudioInput, Backend, CAPTURE_SECONDS, Features, Grid};
use crate::config::{Config, Preset};
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::hue::api;
use crate::hue::snapshot::Restored;
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::link::LinkSession;
use crate::look::{Accent, BeatTime, Dynamics, Motion, Palette, Phase, ReactiveLook, Rotation, Space, Spread};
use crate::midi::{MidiListener, Notes, note_hue};
use crate::tempo::{MANUAL_BPM_LIMITS, TempoSource, Tracker};
use crate::timing;
use crate::wav;

mod captures;
mod delays;
mod loops;
mod remote;

use captures::{auto_capture_loop, write_capture};
use loops::{light_loop, link_loop, opus_loop, tempo_loop, unpack};
pub use remote::new_token;

const TEMPO_UPDATE: Duration = Duration::from_secs(1);
/// Fréquence de lecture de la session Ableton Link (tempo et phase).
const LINK_POLL: Duration = Duration::from_millis(20);
/// Attente maximale d'un paquet de l'OPUS avant de publier la platine qui mène.
const OPUS_POLL: Duration = Duration::from_millis(20);
/// Décalage réglable des paquets de l'OPUS sur le son (ms).
pub const OPUS_OFFSET_RANGE: (f64, f64) = (-150.0, 150.0);
/// Ce que l'OPUS dit du morceau reste affiché 0,5 s après qu'il cesse de mener.
const OPUS_SHOWN: Duration = Duration::from_millis(500);
/// Bornes de l'attente du kick d'un flash anticipé (âge des blocs audio + marge).
const CONFIRM_RANGE: (f64, f64) = (0.03, 0.1);
/// Durée d'allumage des témoins de kick et de note.
const LIT: Duration = Duration::from_millis(100);
const NO_BUILD: u8 = u8::MAX;
/// Entrée audio ou bridge perdus : nouvelle tentative toutes les 2 s (le bridge, de 0,5 à 5 s).
const AUDIO_RETRY: Duration = Duration::from_secs(2);
/// Plus aucun bloc audio depuis 1 s : l'entrée est perdue.
const AUDIO_STALL: Duration = Duration::from_secs(1);
/// Rien entendu depuis 10 s : mauvaise source, ou un logiciel en ASIO que Windows ne voit pas.
const SILENT_AFTER: Duration = Duration::from_secs(10);
const BRIDGE_RETRY: (Duration, Duration) = (Duration::from_millis(500), Duration::from_secs(5));
pub const THRESHOLD_RANGE: (f32, f32) = (1.1, 4.0);
/// Retard propre maximal d'une lampe (ms) : au-delà, c'est un réglage d'avance générale.
pub const MAX_LAMP_DELAY_MS: f32 = 150.0;
const LINK_TEMPO: &str = "tempo piloté par Ableton Link : change-le depuis la session";
const OPUS_TEMPO: &str = "tempo piloté par l'OPUS-QUAD : change-le sur les platines";
/// Captures automatiques : le groove doit avoir tenu 8 s, la capture part 8 s après sa perte si
/// les basses sont restées à plus de 60 % de leur niveau pendant le groove (moyenne sur 4 s), au
/// plus une toutes les 2 min et 20 par session.
const AUTO_POLL: Duration = Duration::from_millis(250);
const AUTO_MIN_GROOVE: Duration = Duration::from_secs(8);
const AUTO_AFTER: Duration = Duration::from_secs(8);
const AUTO_BASS_MEMORY: f32 = 4.0;
const AUTO_LOUD: f32 = 0.6;
const AUTO_SPACING: Duration = Duration::from_secs(120);
const AUTO_MAX: u32 = 20;

pub struct Options {
    /// Ouvrir une entrée audio ; sans elle (avec l'OPUS seul), le son ne joue aucun rôle.
    pub listen: bool,
    pub input: Option<String>,
    pub audio: Backend,
    pub threshold: f32,
    /// Tempo imposé au démarrage ; sinon détection automatique (ou tempo proposé à Link).
    pub bpm: Option<f64>,
    pub bpm_range: (f64, f64),
    pub flash: FlashMode,
    /// Rejoindre la session Ableton Link.
    pub link: bool,
    /// Suivre l'OPUS-QUAD par le réseau : tempo, temps, phrases et kicks de la platine maître. Le
    /// son devient facultatif.
    pub opus: bool,
    /// Décalage des paquets de l'OPUS sur le son (ms) : positif s'ils arrivent après lui.
    pub opus_offset_ms: f64,
    /// Carte réseau de l'OPUS (son nom), choisie d'avance ; sinon celle où il s'annonce.
    pub opus_interface: Option<String>,
    /// Couleurs de la trame : avec l'OPUS, la waveform du morceau teinte les lampes.
    pub trame: bool,
    /// Écouter une entrée MIDI : `Some(None)` pour la première, `Some(Some(nom))` pour une autre.
    pub midi: Option<Option<String>>,
    pub palette: Palette,
    pub dynamics: Dynamics,
    pub motion: Motion,
    /// Répartition des flashs entre les lampes.
    pub spread: Spread,
    /// Régisseur de la disposition Auto.
    pub director: bool,
    /// Télécommande mobile sur le réseau local, avec ce jeton dans son adresse.
    pub remote: Option<String>,
    pub rotation: Rotation,
}

/// Ce qui est connu au démarrage.
pub struct Info {
    pub device: String,
    pub backend: &'static str,
    /// Pourquoi la capture préférée n'a pas pu être utilisée, et ce qui la remplace.
    pub fallback: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub area: String,
    /// Noms des lampes pilotées (vide si le bridge ne les a pas donnés).
    pub lamps: Vec<String>,
    pub midi_port: Option<String>,
    pub link: bool,
    pub opus: bool,
    /// Avec l'OPUS, pourquoi aucune entrée audio n'est ouverte : la lumière le suit seul.
    pub no_audio: Option<String>,
    pub advance: Duration,
    /// L'état des lampes à rétablir vient d'une session interrompue.
    pub resumed_snapshot: bool,
    /// Adresse de la télécommande mobile, si elle est ouverte.
    pub remote_url: Option<String>,
    /// Pièce et disposition des lampes, et les dispositions qu'elle rend possibles.
    pub room: String,
    pub layout: String,
    pub movements: Vec<Spread>,
}

/// Instantané de l'état, pour l'affichage.
pub struct Status {
    pub bpm: Option<f64>,
    pub source: TempoSource,
    pub mode: FlashMode,
    pub levels: Levels,
    pub threshold: f32,
    pub kick_lit: bool,
    pub note_lit: bool,
    /// Écart moyen entre les kicks et les temps de l'horloge (ms).
    pub offset_ms: f64,
    /// Réaction côté PC des flashs sur kick (ms).
    pub reaction_ms: f64,
    pub glitches: u32,
    pub input_lost: bool,
    /// La source n'a rien fait entendre depuis 10 s.
    pub silent: bool,
    /// Le bridge ne répond plus : le flux tente de se rouvrir.
    pub bridge_lost: bool,
    /// Dernière scène rappelée.
    pub preset: Option<String>,
    /// Réglages changés hors de la fenêtre (scène rappelée par note MIDI, télécommande) : la
    /// fenêtre reprend alors ceux-ci quand le compte change.
    pub outside_changes: u32,
    pub motion: Motion,
    pub spread: Spread,
    pub director: bool,
    pub palette: Palette,
    pub dynamics: Dynamics,
    /// Autres participants de la session Link, si elle est rejointe.
    pub link_peers: Option<u64>,
    /// Couleur envoyée à la lampe, en sRGB 8 bits.
    pub color: [u8; 3],
    /// Phase reconnue et animation effectivement jouée.
    pub phase: Phase,
    pub active_motion: Motion,
    /// Avancement de la montée en cours, en %.
    pub build: Option<u8>,
    /// Temps restants avant le drop prévu de la montée (tête de phrase).
    pub drop_beats: Option<u8>,
    /// Disposition effectivement jouée, et couleur envoyée à chaque lampe (canaux de la zone).
    pub active_spread: Spread,
    pub lamp_colors: Vec<[u8; 3]>,
    /// Temps en cours dans une mesure de 4 (0 à 3), si le tempo est connu. Tant que la structure
    /// n'est pas reconnue, la mesure affichée peut commencer sur n'importe quel temps.
    pub beat: Option<u8>,
    /// Temps en cours dans la phrase de quatre mesures (0 à 15), si la structure est reconnue.
    pub phrase: Option<u8>,
    /// Captures enregistrées d'elles-mêmes, le groove ayant décroché dans un passage fort.
    pub auto_captures: u32,
    /// Liaison OPUS, si elle est suivie.
    pub opus: Option<OpusStatus>,
}

/// Une lampe de la zone, pendant le live.
#[derive(Debug, Clone)]
pub struct LampInfo {
    pub id: String,
    pub name: String,
    /// Retard propre (ms) : ses flashs partent d'autant plus tôt que ceux des autres.
    pub lag_ms: f32,
    /// Le live l'allume ; sinon elle garde (ou retrouve) son état d'avant le live.
    pub enabled: bool,
}

/// État de la liaison OPUS, pour l'affichage.
#[derive(Debug, Clone)]
pub struct OpusStatus {
    pub link: LinkState,
    /// Ce que l'OPUS dit du morceau, tant qu'il mène la lumière.
    pub now: Option<OpusNow>,
    /// Les platines chargées et où en est leur lecture, pour leurs bandes.
    pub decks: Vec<crate::opus::DeckView>,
    /// La liaison n'a pas pu s'ouvrir.
    pub error: Option<String>,
}

/// Où en est la platine qui mène, publié par la boucle d'éclairage.
#[derive(Debug, Clone)]
pub struct OpusNow {
    pub deck: u8,
    pub track: u32,
    pub beat: u32,
    /// Place dans la mesure (0 à 3) et dans la phrase de la lumière (0 à 15).
    pub bar_beat: u8,
    pub phrase_beat: u8,
    /// Phrase rekordbox en cours, la suivante et les temps qui restent avant elle.
    pub phrase: Option<String>,
    pub next: Option<String>,
    pub beats_left: Option<u32>,
    /// La phase est assez sûre pour flasher.
    pub settled: bool,
    at: Instant,
}

/// Bilan affiché à l'arrêt.
pub struct Summary {
    bpm: Option<f64>,
    source: TempoSource,
    offset_ms: f64,
    flashes: u32,
    kick_flashes: u32,
    reaction_ms: f64,
    max_reaction_ms: f64,
    advance_ms: u128,
    notes: Option<u32>,
    link_peers: Option<u64>,
    input_latency_ms: f64,
    glitches: u32,
    input_lost: bool,
    audio_restarts: u32,
    bridge_restarts: u32,
    /// Retour des lampes à leur état d'avant le flux, ou la raison de son échec.
    restore: std::result::Result<Restored, String>,
}

struct Shared {
    stop: AtomicBool,
    mode: AtomicU8,
    motion: AtomicU8,
    spread: AtomicU8,
    director: AtomicBool,
    active_spread: AtomicU8,
    rotation: AtomicU8,
    /// Dernière couleur de chaque lampe, `0x00RRGGBB`.
    lamp_colors: Vec<AtomicU32>,
    /// Retard propre de chaque canal (ms, bits de `f32`), et test de synchro des lampes.
    lamp_lags: Vec<AtomicU32>,
    /// Canaux allumés par le live (les autres montrent l'état d'avant), et compte des changements.
    lamp_enabled: Vec<AtomicBool>,
    lamps_changed: AtomicU32,
    sync_test: AtomicBool,
    /// Publiés par la boucle d'éclairage.
    phase: AtomicU8,
    /// Avancement de la montée en cours (%) et temps avant son drop prévu, `NO_BUILD` sinon.
    build: AtomicU8,
    drop_beats: AtomicU8,
    active_motion: AtomicU8,
    tracker: Mutex<Tracker>,
    palette: Mutex<Palette>,
    dynamics: Mutex<Dynamics>,
    /// Le tempo vient d'Ableton Link : il ne se règle pas ici.
    link: bool,
    link_peers: AtomicU64,
    /// Liaison OPUS : platine qui mène (publiée par son fil), état de la liaison, échec à
    /// l'ouverture, décalage des paquets sur le son (µs), couleurs de la trame, et ce que la boucle
    /// d'éclairage en a tiré.
    opus: bool,
    opus_playing: Mutex<Option<Playing>>,
    opus_decks: Mutex<Vec<crate::opus::DeckView>>,
    opus_link: Mutex<LinkState>,
    opus_error: Mutex<Option<String>>,
    opus_offset_us: AtomicI64,
    trame: AtomicBool,
    opus_now: Mutex<Option<OpusNow>>,
    flashes: AtomicU32,
    /// Le groove tient (publié par la boucle d'éclairage), et captures automatiques enregistrées.
    grooving: AtomicBool,
    auto_captures: AtomicU32,
    /// Reprises : entrée audio rouverte, flux du bridge rouvert ; le bridge est injoignable.
    audio_restarts: AtomicU32,
    bridge_restarts: AtomicU32,
    bridge_lost: AtomicBool,
    /// Scènes rappelables par note MIDI, dernière rappelée, et réglages changés hors de la
    /// fenêtre (note MIDI, télécommande).
    presets: Mutex<Vec<Preset>>,
    preset: Mutex<Option<String>>,
    outside_changes: AtomicU32,
    /// Dispositions que la pièce permet.
    movements: Vec<Spread>,
    /// Drop demandé par la télécommande : éclat blanc partout.
    drop_now: AtomicBool,
    note_flashes: AtomicU32,
    /// Instant (s, horloge commune) de la dernière note, en bits de `f64`.
    last_note: AtomicU64,
    /// Dernière couleur envoyée, `0x00RRGGBB`.
    color: AtomicU32,
    /// Statistiques de réaction des flashs déclenchés par un kick.
    kick_flashes: AtomicU32,
    total_reaction_us: AtomicU64,
    max_reaction_us: AtomicU64,
}

impl Shared {
    fn tracker(&self) -> MutexGuard<'_, Tracker> {
        self.tracker.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn palette(&self) -> Palette {
        *self.palette.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn dynamics(&self) -> Dynamics {
        *self.dynamics.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn mode(&self) -> FlashMode {
        FlashMode::from_index(self.mode.load(Ordering::Relaxed))
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Rappelle une scène : couleurs, flashs, animation, disposition et régisseur.
    fn apply(&self, preset: &Preset) {
        *lock(&self.palette) = preset.palette;
        *lock(&self.dynamics) = preset.dynamics;
        self.motion.store(preset.motion.index(), Ordering::Relaxed);
        self.spread.store(preset.spread.index(), Ordering::Relaxed);
        self.mode.store(preset.flash.index(), Ordering::Relaxed);
        self.director.store(preset.director, Ordering::Relaxed);
        *lock(&self.preset) = Some(preset.name.clone());
        self.outside_changes.fetch_add(1, Ordering::Relaxed);
    }

    /// Scène rappelée par la note MIDI `note`, s'il y en a une.
    fn preset_for(&self, note: u8) -> Option<Preset> {
        lock(&self.presets).iter().find(|preset| preset.note == Some(note)).cloned()
    }

    fn record_reaction(&self, reaction: Duration) {
        let us = reaction.as_micros() as u64;
        self.kick_flashes.fetch_add(1, Ordering::Relaxed);
        self.total_reaction_us.fetch_add(us, Ordering::Relaxed);
        self.max_reaction_us.fetch_max(us, Ordering::Relaxed);
    }

    fn average_reaction_ms(&self) -> f64 {
        let flashes = self.kick_flashes.load(Ordering::Relaxed).max(1);
        self.total_reaction_us.load(Ordering::Relaxed) as f64 / f64::from(flashes) / 1e3
    }

    fn last_note(&self) -> f64 {
        f64::from_bits(self.last_note.load(Ordering::Relaxed))
    }

    /// Décalage des paquets de l'OPUS sur le son (s).
    fn opus_offset(&self) -> f64 {
        self.opus_offset_us.load(Ordering::Relaxed) as f64 * 1e-6
    }

    /// La platine qui mène et ce qu'elle dit du morceau à l'instant `now` (s), si l'OPUS est suivi
    /// et la donne.
    fn opus_guide(&self, now: f64) -> Option<(Playing, crate::opus::Guide)> {
        if !self.opus {
            return None;
        }
        let playing = lock(&self.opus_playing).clone()?;
        let guide = playing.guide(now, self.opus_offset())?;
        Some((playing, guide))
    }
}

pub struct Engine {
    pub info: Info,
    shared: Arc<Shared>,
    features: Arc<Features>,
    rt: Arc<Runtime>,
    capture: Arc<Mutex<Capture>>,
    /// Boucle d'éclairage : rend le flux pour qu'on le ferme proprement.
    light: Option<JoinHandle<(HueStream, Result<()>)>>,
    helpers: Vec<JoinHandle<()>>,
    audio: Option<AudioInput>,
    midi: Option<MidiListener>,
    /// Lampes pilotées : identifiant, nom, rangs de leurs canaux.
    lamp_map: Vec<(String, String, Vec<usize>)>,
    /// Entrée audio à rouvrir si elle se perd, et dernière tentative.
    input: (Option<String>, Backend),
    audio_retry: Instant,
    /// Serveur de la télécommande mobile.
    remote: Option<tokio::task::JoinHandle<()>>,
}

impl Engine {
    pub fn start(rt: Arc<Runtime>, cfg: &Config, options: Options) -> Result<Self> {
        let (bpm_min, bpm_max) = options.bpm_range;
        if !(MANUAL_BPM_LIMITS.0 <= bpm_min && bpm_min < bpm_max && bpm_max <= MANUAL_BPM_LIMITS.1) {
            bail!("plage de tempo invalide : {bpm_min}-{bpm_max} BPM");
        }
        if let Some(bpm) = options.bpm
            && !(MANUAL_BPM_LIMITS.0..=MANUAL_BPM_LIMITS.1).contains(&bpm)
        {
            bail!("tempo hors limites : {bpm} BPM");
        }

        let bridge = cfg.bridge()?;
        let area = rt.block_on(api::select_area(bridge, cfg.area_id.as_deref()))?;
        // Sans la liste des lampes (bridge ancien), toute la zone est pilotée.
        let lamps = rt.block_on(api::area_lamps(bridge, &area)).unwrap_or_default();
        let piloted_channels = api::piloted_channels(&area, &lamps, cfg.excluded());
        if piloted_channels.is_empty() {
            bail!("aucune lampe à piloter : coche au moins une lampe de la zone");
        }
        // Tous les canaux de la zone : ceux d'une lampe décochée restent hors du flux tant qu'on ne
        // l'active pas pendant le live.
        let channels: Vec<u8> = area.channels.iter().map(|c| c.channel_id).collect();
        let owners: Vec<Option<String>> = channels
            .iter()
            .map(|id| lamps.iter().find(|lamp| lamp.channels.contains(id)).map(|lamp| lamp.id.clone()))
            .collect();
        // Place de chaque canal dans la pièce : celle de sa lampe dans la disposition active, sinon
        // sa place de gauche à droite dans la zone Hue.
        let space = Space::of_room(
            cfg.room(),
            cfg.active_layout,
            channels.iter().map(|&id| {
                let lamp = lamps.iter().find(|lamp| lamp.channels.contains(&id));
                let hue = match lamp {
                    Some(lamp) if lamp.channels.len() == 1 => lamp.x,
                    _ => area.channels.iter().find(|c| c.channel_id == id).map_or(0.0, |c| c.position.x),
                };
                (lamp.map_or("", |lamp| lamp.id.as_str()), hue)
            }),
        );
        let movements = space.possible();
        // Lampes de la zone et leurs canaux (rangs dans `channels`), et retard de chaque canal.
        let lamp_map: Vec<(String, String, Vec<usize>)> = lamps
            .iter()
            .map(|lamp| {
                let ranks = channels.iter().enumerate().filter(|(_, id)| lamp.channels.contains(id)).map(|(i, _)| i).collect();
                (lamp.id.clone(), lamp.name.clone(), ranks)
            })
            .collect();
        let mut channel_lags = vec![0.0f32; channels.len()];
        for (id, _, ranks) in &lamp_map {
            let lag = cfg.lamp_delays_ms.get(id).copied().unwrap_or(0.0);
            for &rank in ranks {
                channel_lags[rank] = lag.clamp(0.0, MAX_LAMP_DELAY_MS);
            }
        }
        let piloted: Vec<String> = lamps
            .iter()
            .filter(|lamp| !cfg.excluded().contains(&lamp.id))
            .map(|lamp| lamp.name.clone())
            .collect();

        let features = Arc::new(Features::new(options.threshold));
        let notes = Arc::new(Notes::default());
        // Avec l'OPUS, la lumière se passe du son : une entrée absente n'empêche pas le live.
        if !options.listen && !options.opus {
            bail!("sans son, la lumière ne peut suivre que l'OPUS : choisis-le comme source");
        }
        let (audio, no_audio) = if options.listen {
            match AudioInput::start(options.input.as_deref(), options.audio, features.clone()) {
                Ok(audio) => (Some(audio), None),
                Err(e) if options.opus => (None, Some(format!("{e:#}"))),
                Err(e) => return Err(e),
            }
        } else {
            (None, None)
        };
        let capture = audio.as_ref().map_or_else(
            || Arc::new(Mutex::new(Capture::new(48_000, CAPTURE_SECONDS))),
            |audio| audio.capture.clone(),
        );
        let midi = match &options.midi {
            Some(wanted) => Some(MidiListener::start(wanted.as_deref(), notes.clone())?),
            None => None,
        };
        let stream = rt.block_on(HueStream::open(bridge, &area))?;
        let resumed_snapshot = stream.resumed;
        let advance = Duration::from_millis(cfg.light_advance_ms.max(0).unsigned_abs());

        let mut tracker = Tracker::new(options.bpm_range);
        if let Some(bpm) = options.bpm
            && !options.link
            && !options.opus
        {
            tracker.set_manual(bpm, features.seconds(Instant::now()));
        }
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            mode: AtomicU8::new(options.flash.index()),
            motion: AtomicU8::new(options.motion.index()),
            spread: AtomicU8::new(options.spread.index()),
            director: AtomicBool::new(options.director),
            active_spread: AtomicU8::new(options.spread.index()),
            rotation: AtomicU8::new(options.rotation.index()),
            lamp_colors: channels.iter().map(|_| AtomicU32::new(0)).collect(),
            lamp_lags: channel_lags.iter().map(|lag| AtomicU32::new(lag.to_bits())).collect(),
            lamp_enabled: channels.iter().map(|id| AtomicBool::new(piloted_channels.contains(id))).collect(),
            lamps_changed: AtomicU32::new(0),
            sync_test: AtomicBool::new(false),
            phase: AtomicU8::new(Phase::default().index()),
            build: AtomicU8::new(NO_BUILD),
            drop_beats: AtomicU8::new(NO_BUILD),
            active_motion: AtomicU8::new(options.motion.index()),
            tracker: Mutex::new(tracker),
            palette: Mutex::new(options.palette),
            dynamics: Mutex::new(options.dynamics),
            link: options.link,
            link_peers: AtomicU64::new(0),
            opus: options.opus,
            opus_playing: Mutex::new(None),
            opus_decks: Mutex::new(Vec::new()),
            opus_link: Mutex::new(LinkState::default()),
            opus_error: Mutex::new(None),
            opus_offset_us: AtomicI64::new(offset_us(options.opus_offset_ms)),
            trame: AtomicBool::new(options.trame),
            opus_now: Mutex::new(None),
            flashes: AtomicU32::new(0),
            grooving: AtomicBool::new(false),
            auto_captures: AtomicU32::new(0),
            audio_restarts: AtomicU32::new(0),
            bridge_restarts: AtomicU32::new(0),
            bridge_lost: AtomicBool::new(false),
            presets: Mutex::new(cfg.presets.clone()),
            preset: Mutex::new(None),
            outside_changes: AtomicU32::new(0),
            movements: movements.clone(),
            drop_now: AtomicBool::new(false),
            note_flashes: AtomicU32::new(0),
            last_note: AtomicU64::new(f64::NEG_INFINITY.to_bits()),
            color: AtomicU32::new(0),
            kick_flashes: AtomicU32::new(0),
            total_reaction_us: AtomicU64::new(0),
            max_reaction_us: AtomicU64::new(0),
        });

        let light = {
            let (rt, features, notes, shared) = (rt.clone(), features.clone(), notes.clone(), shared.clone());
            let mut stream = stream;
            let rig = loops::Rig { channels, owners, space };
            thread::Builder::new().name("lumière".into()).spawn(move || {
                let result = light_loop(&rt, &mut stream, &rig, &features, &notes, &shared, advance);
                shared.stop.store(true, Ordering::Relaxed);
                (stream, result)
            })?
        };
        let mut helpers = Vec::new();
        let (remote, remote_url) = match &options.remote {
            Some(token) => {
                let (listener, port) = remote::bind(&rt)?;
                let url = format!("http://{}:{port}/{token}/", remote::lan_address(&bridge.ip));
                let state = Arc::new((shared.clone(), features.clone()));
                (Some(rt.spawn(remote::serve(listener, state, token.clone()))), Some(url))
            }
            None => (None, None),
        };
        {
            let (features, shared) = (features.clone(), shared.clone());
            helpers.push(thread::Builder::new().name("tempo".into()).spawn(move || tempo_loop(&shared, &features))?);
        }
        // Les captures automatiques guettent la détection des kicks à l'écoute ; l'OPUS s'en passe.
        if audio.is_some() && !options.opus {
            let (features, shared, capture) = (features.clone(), shared.clone(), capture.clone());
            helpers.push(
                thread::Builder::new()
                    .name("captures auto".into())
                    .spawn(move || auto_capture_loop(&shared, &features, &capture))?,
            );
        }
        if options.link {
            let (features, shared, bpm) = (features.clone(), shared.clone(), options.bpm);
            helpers.push(thread::Builder::new().name("link".into()).spawn(move || link_loop(&shared, &features, bpm))?);
        }
        if options.opus {
            let (features, shared, interface) = (features.clone(), shared.clone(), options.opus_interface.clone());
            helpers.push(
                thread::Builder::new()
                    .name("opus".into())
                    .spawn(move || opus_loop(&shared, &features, interface.as_deref()))?,
            );
        }

        let silent_source = if options.listen { "sans son" } else { "OPUS-QUAD par le réseau" };
        let info = Info {
            device: audio.as_ref().map_or_else(|| silent_source.to_owned(), |audio| audio.device.clone()),
            backend: audio.as_ref().map_or("aucun", |audio| audio.backend),
            fallback: audio.as_ref().and_then(|audio| audio.fallback.clone()),
            sample_rate: audio.as_ref().map_or(0, |audio| audio.sample_rate),
            channels: audio.as_ref().map_or(0, |audio| audio.channels),
            area: area.metadata.name.clone(),
            lamps: piloted,
            midi_port: midi.as_ref().map(|m| m.port.clone()),
            link: options.link,
            opus: options.opus,
            no_audio,
            advance,
            resumed_snapshot,
            remote_url,
            room: cfg.room().name.clone(),
            layout: cfg.layout().name.clone(),
            movements,
        };
        Ok(Self {
            info,
            capture,
            shared,
            features,
            rt,
            light: Some(light),
            helpers,
            audio,
            midi,
            lamp_map,
            input: (options.input.clone(), options.audio),
            remote,
            audio_retry: Instant::now(),
        })
    }

    /// Le moteur tourne : ni arrêté, ni interrompu par une erreur d'envoi vers le bridge.
    pub fn is_running(&self) -> bool {
        !self.shared.stopped()
    }

    pub fn status(&self) -> Status {
        let now = Instant::now();
        let features = &self.features;
        let (clock, source, offset, position) = {
            let tracker = self.shared.tracker();
            let clock = tracker.clock();
            // Le temps en cours : le dernier passé de la grille.
            let started = clock.period().map(|period| {
                clock.anchor() + ((features.seconds(now) - clock.anchor()) / period).floor() * period
            });
            (clock, tracker.source(), tracker.mean_offset(), started.and_then(|t| tracker.position(t)))
        };
        // Quand l'OPUS mène, la mesure et la phrase sont les siennes.
        let opus_now = lock(&self.shared.opus_now).clone().filter(|n| now.saturating_duration_since(n.at) < OPUS_SHOWN);
        let position = match &opus_now {
            Some(n) => Some(crate::phrase::Position { beat: n.bar_beat, phrase: n.phrase_beat }),
            None => position,
        };
        // Sans structure reconnue, la mesure affichée commence à un temps quelconque.
        let beat = position.map(|p| p.beat).or_else(|| {
            clock.period().map(|period| ((features.seconds(now) - clock.anchor()) / period).floor().rem_euclid(4.0) as u8)
        });
        let color = self.shared.color.load(Ordering::Relaxed);
        Status {
            bpm: clock.bpm(),
            source,
            mode: self.shared.mode(),
            levels: features.levels(),
            threshold: features.threshold(),
            kick_lit: features.kicks() > 0 && now.saturating_duration_since(features.last_kick()) < LIT,
            note_lit: features.seconds(now) - self.shared.last_note() < LIT.as_secs_f64(),
            offset_ms: offset * 1e3,
            reaction_ms: self.shared.average_reaction_ms(),
            glitches: features.glitches(),
            input_lost: self.audio_lost(),
            silent: features.silence() > SILENT_AFTER,
            bridge_lost: self.shared.bridge_lost.load(Ordering::Relaxed),
            preset: lock(&self.shared.preset).clone(),
            outside_changes: self.shared.outside_changes.load(Ordering::Relaxed),
            motion: Motion::from_index(self.shared.motion.load(Ordering::Relaxed)),
            spread: Spread::from_index(self.shared.spread.load(Ordering::Relaxed)),
            director: self.shared.director.load(Ordering::Relaxed),
            palette: self.shared.palette(),
            dynamics: self.shared.dynamics(),
            link_peers: self.shared.link.then(|| self.shared.link_peers.load(Ordering::Relaxed)),
            color: unpack(color),
            phase: Phase::from_index(self.shared.phase.load(Ordering::Relaxed)),
            build: Some(self.shared.build.load(Ordering::Relaxed)).filter(|&p| p != NO_BUILD),
            drop_beats: Some(self.shared.drop_beats.load(Ordering::Relaxed)).filter(|&b| b != NO_BUILD),
            active_motion: Motion::from_index(self.shared.active_motion.load(Ordering::Relaxed)),
            active_spread: Spread::from_index(self.shared.active_spread.load(Ordering::Relaxed)),
            lamp_colors: self.shared.lamp_colors.iter().map(|c| unpack(c.load(Ordering::Relaxed))).collect(),
            beat,
            phrase: position.map(|p| p.phrase),
            auto_captures: self.shared.auto_captures.load(Ordering::Relaxed),
            opus: self.shared.opus.then(|| OpusStatus {
                link: lock(&self.shared.opus_link).clone(),
                now: opus_now,
                decks: lock(&self.shared.opus_decks).clone(),
                error: lock(&self.shared.opus_error).clone(),
            }),
        }
    }

    /// Décalage des paquets de l'OPUS sur le son (ms) : positif s'ils arrivent après lui.
    pub fn set_opus_offset(&self, ms: f64) {
        self.shared.opus_offset_us.store(offset_us(ms), Ordering::Relaxed);
    }

    /// Drop à la main (barre d'espace, télécommande) : éclat blanc à pleine puissance, partout.
    pub fn drop_now(&self) {
        self.shared.drop_now.store(true, Ordering::Relaxed);
    }

    /// Couleurs de la trame : avec l'OPUS, la waveform du morceau teinte les lampes.
    pub fn set_trame(&self, on: bool) {
        self.shared.trame.store(on, Ordering::Relaxed);
    }

    pub fn set_motion(&self, motion: Motion) {
        self.shared.motion.store(motion.index(), Ordering::Relaxed);
    }

    pub fn set_spread(&self, spread: Spread) {
        self.shared.spread.store(spread.index(), Ordering::Relaxed);
    }

    pub fn set_director(&self, on: bool) {
        self.shared.director.store(on, Ordering::Relaxed);
    }

    /// L'entrée est perdue : le pilote l'a signalé, ou plus aucun bloc n'arrive (l'ASIO se tait
    /// sans prévenir). La sortie du PC, elle, n'envoie rien quand rien ne joue.
    fn audio_lost(&self) -> bool {
        let silent = self.input.1 != Backend::Output && self.features.block_age().is_some_and(|age| age > AUDIO_STALL);
        self.features.input_lost() || silent
    }

    /// Entrée audio perdue (câble, pilote) : tente de la rouvrir toutes les 2 s. À appeler
    /// régulièrement depuis le fil qui possède le moteur ; rend le résultat d'une tentative.
    pub fn recover_audio(&mut self) -> Option<Result<()>> {
        if !self.audio_lost() || self.audio_retry.elapsed() < AUDIO_RETRY {
            return None;
        }
        self.audio_retry = Instant::now();
        // L'ancien flux doit lâcher le pilote (l'ASIO n'en accepte qu'un) avant d'en rouvrir un.
        self.audio = None;
        let (input, backend) = &self.input;
        let reopened = AudioInput::start_into(input.as_deref(), *backend, self.features.clone(), self.capture.clone());
        Some(reopened.map(|audio| {
            self.features.clear_input_lost();
            self.shared.audio_restarts.fetch_add(1, Ordering::Relaxed);
            self.audio = Some(audio);
        }))
    }

    /// Lampes de la zone : identifiant, nom, retard propre (ms), et si le live les allume.
    pub fn lamps(&self) -> Vec<LampInfo> {
        self.lamp_map
            .iter()
            .map(|(id, name, ranks)| {
                let first = ranks.first().copied();
                LampInfo {
                    id: id.clone(),
                    name: name.clone(),
                    lag_ms: first.map_or(0.0, |rank| f32::from_bits(self.shared.lamp_lags[rank].load(Ordering::Relaxed))),
                    enabled: first.is_some_and(|rank| self.shared.lamp_enabled[rank].load(Ordering::Relaxed)),
                }
            })
            .collect()
    }

    /// Allume une lampe dans le live, ou la rend à son état d'avant le live, sans l'arrêter.
    pub fn set_lamp_enabled(&self, id: &str, on: bool) {
        for (_, _, ranks) in self.lamp_map.iter().filter(|(lamp, _, _)| lamp == id) {
            for &rank in ranks {
                self.shared.lamp_enabled[rank].store(on, Ordering::Relaxed);
            }
        }
        self.shared.lamps_changed.fetch_add(1, Ordering::Release);
    }

    /// Retard propre d'une lampe : ses flashs partent d'autant plus tôt que les autres.
    pub fn set_lamp_delay(&self, id: &str, ms: f32) {
        let ms = ms.clamp(0.0, MAX_LAMP_DELAY_MS);
        for (_, _, ranks) in self.lamp_map.iter().filter(|(lamp, _, _)| lamp == id) {
            for &rank in ranks {
                self.shared.lamp_lags[rank].store(ms.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// Test de synchro : toutes les lampes flashent en blanc sur chaque temps.
    pub fn set_sync_test(&self, on: bool) {
        self.shared.sync_test.store(on, Ordering::Relaxed);
    }

    pub fn sync_test(&self) -> bool {
        self.shared.sync_test.load(Ordering::Relaxed)
    }

    pub fn set_rotation(&self, rotation: Rotation) {
        self.shared.rotation.store(rotation.index(), Ordering::Relaxed);
    }

    pub fn set_mode(&self, mode: FlashMode) {
        self.shared.mode.store(mode.index(), Ordering::Relaxed);
    }

    pub fn set_threshold(&self, threshold: f32) {
        self.features.set_threshold(threshold.clamp(THRESHOLD_RANGE.0, THRESHOLD_RANGE.1));
    }

    pub fn set_palette(&self, palette: Palette) {
        *self.shared.palette.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = palette;
    }

    pub fn set_dynamics(&self, dynamics: Dynamics) {
        *self.shared.dynamics.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = dynamics;
    }

    /// Rappelle une scène.
    pub fn apply_preset(&self, preset: &Preset) {
        self.shared.apply(preset);
    }

    /// Rappelle la scène n° `index` (à partir de 0) ; rend son nom.
    pub fn recall_preset(&self, index: usize) -> Option<String> {
        let preset = lock(&self.shared.presets).get(index).cloned()?;
        self.shared.apply(&preset);
        Some(preset.name)
    }

    /// Scènes rappelables par note MIDI.
    pub fn set_presets(&self, presets: Vec<Preset>) {
        *lock(&self.shared.presets) = presets;
    }

    /// Impose un tempo (le phase reste calée sur les kicks).
    pub fn set_bpm(&self, bpm: f64) -> Result<()> {
        self.check_tempo_control()?;
        if !(MANUAL_BPM_LIMITS.0..=MANUAL_BPM_LIMITS.1).contains(&bpm) {
            bail!("tempo hors limites : {bpm} BPM");
        }
        let now = self.features.seconds(Instant::now());
        self.shared.tracker().set_manual(bpm, now);
        Ok(())
    }

    /// Décale le tempo actuel de `delta` BPM (il devient imposé).
    pub fn nudge_bpm(&self, delta: f64) -> Result<()> {
        self.check_tempo_control()?;
        let now = self.features.seconds(Instant::now());
        let mut tracker = self.shared.tracker();
        if let Some(bpm) = tracker.clock().bpm() {
            tracker.set_manual((bpm + delta).clamp(MANUAL_BPM_LIMITS.0, MANUAL_BPM_LIMITS.1), now);
        }
        Ok(())
    }

    /// Revient à la détection automatique du tempo.
    pub fn set_auto(&self) -> Result<()> {
        self.check_tempo_control()?;
        self.shared.tracker().set_auto();
        Ok(())
    }

    /// Le tempo se règle ici, et non depuis une session Ableton Link ou l'OPUS.
    pub fn controls_tempo(&self) -> bool {
        !self.shared.link && !self.shared.opus
    }

    /// Erreur explicite si le tempo est piloté par Ableton Link ou l'OPUS.
    pub fn check_tempo_control(&self) -> Result<()> {
        if self.shared.link {
            bail!(LINK_TEMPO);
        }
        if self.shared.opus {
            bail!(OPUS_TEMPO);
        }
        Ok(())
    }

    /// Écrit les dernières secondes de l'entrée audio dans un WAV, à rejouer avec `dj-bridge analyze`.
    pub fn save_capture(&self) -> Result<PathBuf> {
        let bpm = self.shared.tracker().clock().bpm();
        write_capture(&self.capture, &json!({ "auto": false, "bpm": bpm }))
    }

    /// Arrête les fils, ferme le flux, rétablit l'état d'origine des lampes et dresse le bilan.
    pub fn stop(mut self) -> Result<Summary> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<Summary> {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(remote) = self.remote.take() {
            remote.abort();
        }
        let light = self.light.take().context("moteur déjà arrêté")?;
        let (stream, result) = light.join().map_err(|_| anyhow!("la boucle d'éclairage a planté"))?;
        for helper in self.helpers.drain(..) {
            let _ = helper.join();
        }
        self.midi = None;
        self.audio = None;
        // Le rétablissement des lampes est tenté même si la boucle d'éclairage a échoué.
        let restore = self.rt.block_on(stream.close()).map_err(|e| format!("{e:#}"));
        result?;

        let shared = &self.shared;
        let tracker = shared.tracker();
        Ok(Summary {
            bpm: tracker.clock().bpm(),
            source: tracker.source(),
            offset_ms: tracker.mean_offset() * 1e3,
            flashes: shared.flashes.load(Ordering::Relaxed),
            kick_flashes: shared.kick_flashes.load(Ordering::Relaxed),
            reaction_ms: shared.average_reaction_ms(),
            max_reaction_ms: shared.max_reaction_us.load(Ordering::Relaxed) as f64 / 1e3,
            advance_ms: self.info.advance.as_millis(),
            notes: self.info.midi_port.is_some().then(|| shared.note_flashes.load(Ordering::Relaxed)),
            link_peers: shared.link.then(|| shared.link_peers.load(Ordering::Relaxed)),
            input_latency_ms: self.features.input_latency().as_secs_f64() * 1e3,
            glitches: self.features.glitches(),
            input_lost: self.features.input_lost(),
            audio_restarts: shared.audio_restarts.load(Ordering::Relaxed),
            bridge_restarts: shared.bridge_restarts.load(Ordering::Relaxed),
            restore,
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.light.is_some() {
            let _ = self.shutdown();
        }
    }
}

impl fmt::Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.sample_rate == 0 {
            write!(f, "Mode live : « {} » → zone « {} ».", self.device, self.area)?;
        } else {
            write!(
                f,
                "Mode live : « {} » en {} ({} Hz, {} canaux) → zone « {} ».",
                self.device, self.backend, self.sample_rate, self.channels, self.area
            )?;
        }
        if let Some(reason) = &self.fallback {
            write!(f, "\n{reason}.")?;
        }
        if !self.lamps.is_empty() {
            write!(f, "\nLampes pilotées : {}.", self.lamps.join(", "))?;
        }
        let movements: Vec<&str> = self.movements.iter().filter(|&&m| m != Spread::Auto).map(|m| m.label()).collect();
        write!(f, "\nPièce « {} », configuration « {} » : {}.", self.room, self.layout, movements.join(", "))?;
        if let Some(port) = &self.midi_port {
            write!(f, "\nMIDI : « {port} » ; chaque note flashe à sa couleur.")?;
        }
        if let Some(url) = &self.remote_url {
            write!(f, "\nTélécommande mobile : {url} (téléphone sur le même Wi-Fi que le PC).")?;
        }
        if self.link {
            write!(f, "\nAbleton Link : tempo et phase suivent la session (autorise l'accès réseau si Windows le demande).")?;
        }
        if self.opus {
            write!(f, "\nOPUS-QUAD : tempo, temps, phrases et kicks suivent la platine maître par le réseau (autorise l'accès réseau si Windows le demande).")?;
        }
        if let Some(reason) = &self.no_audio {
            write!(f, "\nPas d'entrée audio ({reason}) : la lumière suit l'OPUS seul.")?;
        }
        if self.resumed_snapshot {
            write!(f, "\nLampes : l'état d'avant une session interrompue sera rétabli à la fermeture.")?;
        } else {
            write!(f, "\nLampes : leur état actuel sera rétabli à la fermeture.")?;
        }
        Ok(())
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tempo = self.bpm.map_or("inconnu".to_owned(), |bpm| format!("{bpm:.1} BPM"));
        writeln!(
            f,
            "Tempo final : {tempo} ({}). Écart moyen kicks/temps : {:.1} ms. {} flashs.",
            self.source.label(),
            self.offset_ms,
            self.flashes
        )?;
        if self.kick_flashes > 0 {
            writeln!(
                f,
                "Flashs sur kick : réaction côté PC moyenne {:.1} ms, max {:.1} ms (+ latence Hue : {} ms).",
                self.reaction_ms, self.max_reaction_ms, self.advance_ms
            )?;
        }
        if let Some(notes) = self.notes {
            writeln!(f, "Notes MIDI : {notes}.")?;
        }
        if let Some(peers) = self.link_peers {
            writeln!(f, "Ableton Link : {peers} autre(s) participant(s).")?;
        }
        write!(
            f,
            "Âge des blocs audio à leur arrivée (pilote) : {:.1} ms. Coupures audio : {}.",
            self.input_latency_ms, self.glitches
        )?;
        if self.input_lost {
            write!(f, "\nL'entrée audio a été perdue en cours de route (câble ou pilote).")?;
        }
        if self.audio_restarts > 0 {
            write!(f, "\nEntrée audio rouverte {} fois.", self.audio_restarts)?;
        }
        if self.bridge_restarts > 0 {
            write!(f, "\nFlux du bridge rouvert {} fois.", self.bridge_restarts)?;
        }
        match &self.restore {
            Ok(restored) => write!(f, "\n{restored}"),
            Err(reason) => write!(f, "\n⚠ {reason}"),
        }
    }
}


/// Décalage de l'OPUS en µs, borné à la plage réglable.
fn offset_us(ms: f64) -> i64 {
    (ms.clamp(OPUS_OFFSET_RANGE.0, OPUS_OFFSET_RANGE.1) * 1e3).round() as i64
}

/// Verrou qui survit à un fil qui a paniqué en le tenant.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

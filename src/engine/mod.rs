//! Moteur du mode live : capture audio, horloge de tempo, flashs, Ableton Link et MIDI, chacun
//! dans son fil. Une interface (terminal ou fenêtre) le démarre, lit son état et le règle.
//!
//! Seuls les vrais kicks captés sur l'entrée audio flashent. Ils alimentent aussi l'horloge de
//! tempo ; quand ils tombent régulièrement sur les temps, le flash du kick suivant part en
//! avance de la latence Hue calibrée pour s'allumer avec lui.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use tokio::runtime::Runtime;

use crate::audio::analysis::{KICK_DETECTION_DELAY, Levels};
use crate::audio::capture::Capture;
use crate::audio::{AudioInput, Backend, CAPTURE_SECONDS, Features, Grid};
use crate::config::Config;
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::hue::api;
use crate::hue::snapshot::Restored;
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::link::LinkSession;
use crate::look::{Accent, Dynamics, Motion, Palette, Phase, ReactiveLook, Spread};
use crate::midi::{MidiListener, Notes, note_hue};
use crate::tempo::{MANUAL_BPM_LIMITS, TempoSource, Tracker};
use crate::timing;
use crate::wav;

mod captures;
mod delays;
mod loops;

use captures::{auto_capture_loop, write_capture};
use loops::{light_loop, link_loop, tempo_loop, unpack};

const TEMPO_UPDATE: Duration = Duration::from_secs(1);
/// Fréquence de lecture de la session Ableton Link (tempo et phase).
const LINK_POLL: Duration = Duration::from_millis(20);
/// Bornes de l'attente du kick d'un flash anticipé (âge des blocs audio + marge).
const CONFIRM_RANGE: (f64, f64) = (0.03, 0.1);
/// Durée d'allumage des témoins de kick et de note.
const LIT: Duration = Duration::from_millis(100);
const NO_BUILD: u8 = u8::MAX;
pub const THRESHOLD_RANGE: (f32, f32) = (1.1, 4.0);
/// Retard propre maximal d'une lampe (ms) : au-delà, c'est un réglage d'avance générale.
pub const MAX_LAMP_DELAY_MS: f32 = 150.0;
const LINK_TEMPO: &str = "tempo piloté par Ableton Link : change-le depuis la session";
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
    pub input: Option<String>,
    pub audio: Backend,
    pub threshold: f32,
    /// Tempo imposé au démarrage ; sinon détection automatique (ou tempo proposé à Link).
    pub bpm: Option<f64>,
    pub bpm_range: (f64, f64),
    pub flash: FlashMode,
    /// Rejoindre la session Ableton Link.
    pub link: bool,
    /// Écouter une entrée MIDI : `Some(None)` pour la première, `Some(Some(nom))` pour une autre.
    pub midi: Option<Option<String>>,
    pub palette: Palette,
    pub dynamics: Dynamics,
    pub motion: Motion,
    /// Répartition des flashs entre les lampes.
    pub spread: Spread,
}

/// Ce qui est connu au démarrage.
pub struct Info {
    pub device: String,
    pub backend: &'static str,
    /// Pourquoi l'ASIO n'a pas pu être utilisé, en mode automatique.
    pub fallback: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub area: String,
    /// Noms des lampes pilotées (vide si le bridge ne les a pas donnés).
    pub lamps: Vec<String>,
    pub midi_port: Option<String>,
    pub link: bool,
    pub advance: Duration,
    /// L'état des lampes à rétablir vient d'une session interrompue.
    pub resumed_snapshot: bool,
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
    /// Retour des lampes à leur état d'avant le flux, ou la raison de son échec.
    restore: std::result::Result<Restored, String>,
}

struct Shared {
    stop: AtomicBool,
    mode: AtomicU8,
    motion: AtomicU8,
    spread: AtomicU8,
    active_spread: AtomicU8,
    /// Dernière couleur de chaque lampe, `0x00RRGGBB`.
    lamp_colors: Vec<AtomicU32>,
    /// Retard propre de chaque canal (ms, bits de `f32`), et test de synchro des lampes.
    lamp_lags: Vec<AtomicU32>,
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
    flashes: AtomicU32,
    /// Le groove tient (publié par la boucle d'éclairage), et captures automatiques enregistrées.
    grooving: AtomicBool,
    auto_captures: AtomicU32,
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
        let channels = api::piloted_channels(&area, &lamps, &cfg.excluded_lights);
        if channels.is_empty() {
            bail!("aucune lampe à piloter : coche au moins une lampe de la zone");
        }
        // Canaux pilotés et leur place dans la pièce, de gauche à droite : celle choisie dans la
        // fenêtre pour leur lampe, sinon celle de la zone Hue.
        let lamps_in_room: Vec<(u8, f32)> = channels
            .iter()
            .map(|&id| {
                let lamp = lamps.iter().find(|lamp| lamp.channels.contains(&id));
                let chosen = lamp.and_then(|lamp| cfg.lamp_positions.get(&lamp.id).copied());
                // Un ruban garde la place de chacun de ses segments ; une lampe, celle de la zone (ou
                // sa place par défaut si les lampes n'ont jamais été placées).
                let hue = match lamp {
                    Some(lamp) if lamp.channels.len() == 1 => lamp.x,
                    _ => area.channels.iter().find(|c| c.channel_id == id).map_or(0.0, |c| c.position.x),
                };
                (id, chosen.unwrap_or(hue))
            })
            .collect();
        // Lampes pilotées et leurs canaux (rangs dans `channels`), et retard de chaque canal.
        let lamp_map: Vec<(String, String, Vec<usize>)> = lamps
            .iter()
            .filter(|lamp| !cfg.excluded_lights.contains(&lamp.id))
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
            .filter(|lamp| !cfg.excluded_lights.contains(&lamp.id))
            .map(|lamp| lamp.name.clone())
            .collect();

        let features = Arc::new(Features::new(options.threshold));
        let notes = Arc::new(Notes::default());
        let audio = AudioInput::start(options.input.as_deref(), options.audio, features.clone())?;
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
        {
            tracker.set_manual(bpm, features.seconds(Instant::now()));
        }
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            mode: AtomicU8::new(options.flash.index()),
            motion: AtomicU8::new(options.motion.index()),
            spread: AtomicU8::new(options.spread.index()),
            active_spread: AtomicU8::new(options.spread.index()),
            lamp_colors: channels.iter().map(|_| AtomicU32::new(0)).collect(),
            lamp_lags: channel_lags.iter().map(|lag| AtomicU32::new(lag.to_bits())).collect(),
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
            flashes: AtomicU32::new(0),
            grooving: AtomicBool::new(false),
            auto_captures: AtomicU32::new(0),
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
            thread::Builder::new().name("lumière".into()).spawn(move || {
                let result = light_loop(&rt, &mut stream, &lamps_in_room, &features, &notes, &shared, advance);
                shared.stop.store(true, Ordering::Relaxed);
                (stream, result)
            })?
        };
        let mut helpers = Vec::new();
        {
            let (features, shared) = (features.clone(), shared.clone());
            helpers.push(thread::Builder::new().name("tempo".into()).spawn(move || tempo_loop(&shared, &features))?);
        }
        {
            let (features, shared, capture) = (features.clone(), shared.clone(), audio.capture.clone());
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

        let info = Info {
            device: audio.device.clone(),
            backend: audio.backend,
            fallback: audio.fallback.clone(),
            sample_rate: audio.sample_rate,
            channels: audio.channels,
            area: area.metadata.name.clone(),
            lamps: piloted,
            midi_port: midi.as_ref().map(|m| m.port.clone()),
            link: options.link,
            advance,
            resumed_snapshot,
        };
        Ok(Self {
            info,
            capture: audio.capture.clone(),
            shared,
            features,
            rt,
            light: Some(light),
            helpers,
            audio: Some(audio),
            midi,
            lamp_map,
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
            input_lost: features.input_lost(),
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
        }
    }

    pub fn set_motion(&self, motion: Motion) {
        self.shared.motion.store(motion.index(), Ordering::Relaxed);
    }

    pub fn set_spread(&self, spread: Spread) {
        self.shared.spread.store(spread.index(), Ordering::Relaxed);
    }

    /// Lampes pilotées : identifiant, nom et retard propre (ms).
    pub fn lamps(&self) -> Vec<(String, String, f32)> {
        self.lamp_map
            .iter()
            .map(|(id, name, ranks)| {
                let lag = ranks.first().map_or(0.0, |&rank| f32::from_bits(self.shared.lamp_lags[rank].load(Ordering::Relaxed)));
                (id.clone(), name.clone(), lag)
            })
            .collect()
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

    /// Le tempo se règle ici, et non depuis une session Ableton Link.
    pub fn controls_tempo(&self) -> bool {
        !self.shared.link
    }

    /// Erreur explicite si le tempo est piloté par Ableton Link.
    pub fn check_tempo_control(&self) -> Result<()> {
        if !self.controls_tempo() {
            bail!(LINK_TEMPO);
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
        write!(
            f,
            "Mode live : « {} » en {} ({} Hz, {} canaux) → zone « {} ».",
            self.device, self.backend, self.sample_rate, self.channels, self.area
        )?;
        if let Some(reason) = &self.fallback {
            write!(f, "\nASIO indisponible ({reason}) : capture en WASAPI.")?;
        }
        if !self.lamps.is_empty() {
            write!(f, "\nLampes pilotées : {}.", self.lamps.join(", "))?;
        }
        if let Some(port) = &self.midi_port {
            write!(f, "\nMIDI : « {port} » ; chaque note flashe à sa couleur.")?;
        }
        if self.link {
            write!(f, "\nAbleton Link : tempo et phase suivent la session (autorise l'accès réseau si Windows le demande).")?;
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
        match &self.restore {
            Ok(restored) => write!(f, "\n{restored}"),
            Err(reason) => write!(f, "\n⚠ {reason}"),
        }
    }
}


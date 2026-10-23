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
use crate::look::{Dynamics, Motion, Palette, Phase, ReactiveLook};
use crate::midi::{MidiListener, Notes, note_hue};
use crate::tempo::{MANUAL_BPM_LIMITS, TempoSource, Tracker};
use crate::timing;
use crate::wav;

const TEMPO_UPDATE: Duration = Duration::from_secs(1);
/// Fréquence de lecture de la session Ableton Link (tempo et phase).
const LINK_POLL: Duration = Duration::from_millis(20);
/// Bornes de l'attente du kick d'un flash anticipé (âge des blocs audio + marge).
const CONFIRM_RANGE: (f64, f64) = (0.03, 0.1);
/// Durée d'allumage des témoins de kick et de note.
const LIT: Duration = Duration::from_millis(100);
pub const THRESHOLD_RANGE: (f32, f32) = (1.1, 4.0);
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
    /// Temps en cours dans une mesure de 4 (0 à 3), si le tempo est connu. La grille ne sait pas
    /// où tombe le premier temps : la mesure affichée peut commencer sur n'importe lequel.
    pub beat: Option<u8>,
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
    /// Publiés par la boucle d'éclairage.
    phase: AtomicU8,
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
            phase: AtomicU8::new(Phase::default().index()),
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
                let result = light_loop(&rt, &mut stream, &channels, &features, &notes, &shared, advance);
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
        })
    }

    /// Le moteur tourne : ni arrêté, ni interrompu par une erreur d'envoi vers le bridge.
    pub fn is_running(&self) -> bool {
        !self.shared.stopped()
    }

    pub fn status(&self) -> Status {
        let now = Instant::now();
        let features = &self.features;
        let (clock, source, offset) = {
            let tracker = self.shared.tracker();
            (tracker.clock(), tracker.source(), tracker.mean_offset())
        };
        let beat = clock
            .period()
            .map(|period| ((features.seconds(now) - clock.anchor()) / period).floor().rem_euclid(4.0) as u8);
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
            color: [(color >> 16) as u8, (color >> 8) as u8, color as u8],
            phase: Phase::from_index(self.shared.phase.load(Ordering::Relaxed)),
            active_motion: Motion::from_index(self.shared.active_motion.load(Ordering::Relaxed)),
            beat,
            auto_captures: self.shared.auto_captures.load(Ordering::Relaxed),
        }
    }

    pub fn set_motion(&self, motion: Motion) {
        self.shared.motion.store(motion.index(), Ordering::Relaxed);
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

/// Flashe selon le planificateur (vrais kicks, anticipés quand ils sont attendus) et sur chaque
/// note MIDI, et entretient le flux à 60 Hz entre deux flashs.
fn light_loop(
    rt: &Runtime,
    stream: &mut HueStream,
    channels: &[u8],
    features: &Features,
    notes: &Notes,
    shared: &Shared,
    advance: Duration,
) -> Result<()> {
    let mut planner = FlashPlanner::new(shared.mode(), advance.as_secs_f64(), CONFIRM_RANGE.1);
    let mut look = ReactiveLook::new(shared.palette());
    let mut frame: Vec<(u8, Rgb)> = channels.iter().map(|&id| (id, Rgb::OFF)).collect();
    let mut seen = features.kicks();
    let mut seen_notes = notes.count();
    let mut last = Instant::now();
    let mut next_frame = last;
    while !shared.stopped() {
        planner.mode = shared.mode();
        // Le kick d'un flash anticipé arrive après l'âge d'un bloc audio : on l'attend jusque-là.
        planner.confirm_window = (features.input_latency().as_secs_f64() + CONFIRM_MARGIN)
            .clamp(CONFIRM_RANGE.0, CONFIRM_RANGE.1);
        let clock = shared.tracker().clock();
        let deadline = match planner.next_event(&clock, features.seconds(Instant::now())) {
            Some(event) => next_frame.min(features.instant(event)),
            None => next_frame,
        };
        timing::sleep_until_or(deadline, || features.kicks() != seen || notes.count() != seen_notes);

        let kicks = features.kicks();
        let kick = (kicks != seen).then(|| features.seconds(features.last_kick()) - KICK_DETECTION_DELAY);
        seen = kicks;
        if let Some(at) = kick {
            shared.tracker().on_kick(at);
            look.kick_detected();
        }
        let note_count = notes.count();
        let note = (note_count != seen_notes).then(|| notes.last());
        seen_notes = note_count;

        let now = Instant::now();
        let (clock, locked) = {
            let tracker = shared.tracker();
            (tracker.clock(), tracker.locked(features.seconds(now)))
        };
        let action = planner.poll(&clock, features.seconds(now), kick);
        // La détection cherche les kicks sur les temps dès que le tempo est sûr, et se méfie de
        // ce qui tombe entre eux tant que le groove tient.
        features.set_grid(Grid {
            period: clock.period().unwrap_or(0.0),
            anchor: clock.anchor(),
            locked,
            groove: planner.in_groove(&clock, features.seconds(now)),
        });
        shared.grooving.store(planner.in_groove(&clock, features.seconds(now)), Ordering::Relaxed);
        match action {
            Action::Flash { .. } => look.kick(),
            Action::Cancel => look.cancel_flash(),
            Action::None => {}
        }
        if let Some((pitch, velocity)) = note {
            look.note(note_hue(pitch), velocity);
            shared.note_flashes.fetch_add(1, Ordering::Relaxed);
            shared.last_note.store(features.seconds(now).to_bits(), Ordering::Relaxed);
        }
        look.palette = shared.palette();
        look.dynamics = shared.dynamics();
        look.motion = Motion::from_index(shared.motion.load(Ordering::Relaxed));
        let color = look.render(now - last, features.levels());
        shared.phase.store(look.phase().index(), Ordering::Relaxed);
        shared.active_motion.store(look.active_motion().index(), Ordering::Relaxed);
        last = now;
        for (_, c) in frame.iter_mut() {
            *c = color;
        }
        rt.block_on(stream.send(&frame))?;
        let packed = u32::from(color.r >> 8) << 16 | u32::from(color.g >> 8) << 8 | u32::from(color.b >> 8);
        shared.color.store(packed, Ordering::Relaxed);
        if let Action::Flash { reactive } = action {
            shared.flashes.fetch_add(1, Ordering::Relaxed);
            if reactive {
                shared.record_reaction(Instant::now().saturating_duration_since(features.last_kick()));
            }
        }
        next_frame = now + FRAME_PERIOD;
    }
    Ok(())
}

/// Transmet la force d'attaque au suivi du tempo et réestime le tempo une fois par seconde, hors
/// de la boucle d'éclairage.
fn tempo_loop(shared: &Shared, features: &Features) {
    let mut next = Instant::now() + TEMPO_UPDATE;
    let mut seen = 0;
    while !shared.stopped() {
        thread::sleep(Duration::from_millis(50));
        {
            let mut tracker = shared.tracker();
            features.read_onsets(&mut seen, |t, strength| tracker.on_onset(t, strength));
        }
        let now = Instant::now();
        if now >= next {
            shared.tracker().update(features.seconds(now));
            next = now + TEMPO_UPDATE;
        }
    }
}

/// Suit la session Ableton Link et impose son tempo et sa phase à l'horloge.
fn link_loop(shared: &Shared, features: &Features, bpm: Option<f64>) {
    let mut session = LinkSession::join(bpm);
    while !shared.stopped() {
        let beat = session.read(features.seconds(Instant::now()));
        shared.tracker().set_link(beat.bpm, beat.beat_time);
        shared.link_peers.store(beat.peers, Ordering::Relaxed);
        thread::sleep(LINK_POLL);
    }
}

/// Écrit les dernières secondes de l'entrée audio dans le dossier des captures, avec une fiche
/// JSON du même nom (contexte de la capture, pour l'annoter au banc d'essai).
fn write_capture(capture: &Mutex<Capture>, sheet: &serde_json::Value) -> Result<PathBuf> {
    let (rate, samples) = {
        let capture = capture.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (capture.rate(), capture.snapshot())
    };
    let dir = Config::captures_dir()?;
    fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let path = (0..)
        .map(|n| dir.join(if n == 0 { format!("capture-{stamp}.wav") } else { format!("capture-{stamp}-{n}.wav") }))
        .find(|path| !path.exists())
        .context("nom de capture introuvable")?;
    wav::write_mono_f32(&path, rate, &samples)?;
    fs::write(path.with_extension("json"), serde_json::to_string_pretty(sheet)?)?;
    Ok(path)
}

/// Décide des captures automatiques : quand le groove décroche alors que les basses restent aussi
/// fortes qu'avant, ce n'est pas un break, la détection a perdu les kicks. Les instants sont en
/// secondes.
#[derive(Debug, Default)]
struct AutoTrigger {
    grooving_since: Option<f64>,
    /// Basses brutes pendant le groove (moyenne glissante).
    groove_bass: f32,
    /// Perte du groove : instant, puis cumul et nombre des niveaux de basse depuis.
    lost: Option<(f64, f32, u32)>,
    last_saved: Option<f64>,
    saved: u32,
}

impl AutoTrigger {
    /// Une mesure ; rend le temps écoulé depuis la perte du groove quand il faut capturer.
    fn step(&mut self, now: f64, dt: f32, grooving: bool, bass: f32) -> Option<f64> {
        if grooving {
            self.grooving_since.get_or_insert(now);
            self.groove_bass += (bass - self.groove_bass) * (dt / AUTO_BASS_MEMORY).min(1.0);
            self.lost = None;
            return None;
        }
        if let Some(since) = self.grooving_since.take()
            && now - since >= AUTO_MIN_GROOVE.as_secs_f64()
        {
            self.lost = Some((now, 0.0, 0));
        }
        let (at, sum, count) = self.lost.as_mut()?;
        *sum += bass;
        *count += 1;
        let after = now - *at;
        if after < AUTO_AFTER.as_secs_f64() {
            return None;
        }
        let loud = *sum / *count as f32 >= AUTO_LOUD * self.groove_bass;
        let spaced = self.last_saved.is_none_or(|saved| now - saved >= AUTO_SPACING.as_secs_f64());
        self.lost = None;
        (loud && spaced && self.saved < AUTO_MAX).then(|| {
            self.last_saved = Some(now);
            self.saved += 1;
            after
        })
    }
}

/// Enregistre de lui-même les dernières secondes quand le groove décroche dans un passage fort
/// (voir `AutoTrigger`). La capture, prise 8 s après, couvre l'avant et l'après.
fn auto_capture_loop(shared: &Shared, features: &Features, capture: &Mutex<Capture>) {
    let mut trigger = AutoTrigger::default();
    let start = Instant::now();
    let dt = AUTO_POLL.as_secs_f32();
    while !shared.stopped() {
        thread::sleep(AUTO_POLL);
        let now = start.elapsed().as_secs_f64();
        let grooving = shared.grooving.load(Ordering::Relaxed);
        let Some(after) = trigger.step(now, dt, grooving, features.levels().raw_bass) else { continue };
        let sheet = json!({
            "auto": true,
            "raison": "groove perdu alors que les basses restent fortes",
            "groove_perdu_a": f64::from(CAPTURE_SECONDS) - after,
            "bpm": shared.tracker().clock().bpm(),
        });
        if write_capture(capture, &sheet).is_ok() {
            shared.auto_captures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fait tourner le déclencheur toutes les 250 ms : (durée, groove, basses) par étape.
    fn run(steps: &[(f64, bool, f32)]) -> Vec<f64> {
        let mut trigger = AutoTrigger::default();
        let (mut now, mut fired) = (0.0, Vec::new());
        for &(duration, grooving, bass) in steps {
            let end = now + duration;
            while now < end {
                now += 0.25;
                if trigger.step(now, 0.25, grooving, bass).is_some() {
                    fired.push(now);
                }
            }
        }
        fired
    }

    #[test]
    fn a_lost_groove_in_a_loud_passage_is_captured_after_eight_seconds() {
        let fired = run(&[(20.0, true, 1.0), (12.0, false, 0.9)]);
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert!((fired[0] - 28.0).abs() < 0.3, "{fired:?}");
    }

    #[test]
    fn a_break_a_short_groove_or_a_quick_recovery_is_not_captured() {
        assert!(run(&[(20.0, true, 1.0), (12.0, false, 0.1)]).is_empty(), "break : les basses tombent");
        assert!(run(&[(5.0, true, 1.0), (12.0, false, 0.9)]).is_empty(), "groove trop court");
        assert!(run(&[(20.0, true, 1.0), (4.0, false, 0.9), (10.0, true, 1.0)]).is_empty(), "groove revenu");
    }

    #[test]
    fn captures_are_spaced_by_two_minutes() {
        let lose = [(20.0, true, 1.0), (12.0, false, 0.9)];
        let fired = run(&[lose, lose, lose, lose, lose].concat());
        assert_eq!(fired.len(), 2, "{fired:?}");
        assert!(fired[1] - fired[0] >= 120.0);
    }
}

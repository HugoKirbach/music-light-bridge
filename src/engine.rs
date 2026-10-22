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
use tokio::runtime::Runtime;

use crate::audio::analysis::{KICK_DETECTION_DELAY, Levels};
use crate::audio::capture::Capture;
use crate::audio::{AudioInput, Backend, Features, Grid};
use crate::config::Config;
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::hue::api;
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::link::LinkSession;
use crate::look::{Palette, ReactiveLook};
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
    pub midi_port: Option<String>,
    pub link: bool,
    pub advance: Duration,
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
}

struct Shared {
    stop: AtomicBool,
    mode: AtomicU8,
    tracker: Mutex<Tracker>,
    palette: Mutex<Palette>,
    /// Le tempo vient d'Ableton Link : il ne se règle pas ici.
    link: bool,
    link_peers: AtomicU64,
    flashes: AtomicU32,
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
        let channels: Vec<u8> = area.channels.iter().map(|c| c.channel_id).collect();

        let features = Arc::new(Features::new(options.threshold));
        let notes = Arc::new(Notes::default());
        let audio = AudioInput::start(options.input.as_deref(), options.audio, features.clone())?;
        let midi = match &options.midi {
            Some(wanted) => Some(MidiListener::start(wanted.as_deref(), notes.clone())?),
            None => None,
        };
        let stream = rt.block_on(HueStream::open(bridge, &area))?;
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
            tracker: Mutex::new(tracker),
            palette: Mutex::new(options.palette),
            link: options.link,
            link_peers: AtomicU64::new(0),
            flashes: AtomicU32::new(0),
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
            midi_port: midi.as_ref().map(|m| m.port.clone()),
            link: options.link,
            advance,
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
        let (bpm, source, offset) = {
            let tracker = self.shared.tracker();
            (tracker.clock().bpm(), tracker.source(), tracker.mean_offset())
        };
        let color = self.shared.color.load(Ordering::Relaxed);
        Status {
            bpm,
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
        }
    }

    pub fn set_mode(&self, mode: FlashMode) {
        self.shared.mode.store(mode.index(), Ordering::Relaxed);
    }

    pub fn set_threshold(&self, threshold: f32) {
        self.features.set_threshold(threshold.clamp(THRESHOLD_RANGE.0, THRESHOLD_RANGE.1));
    }

    pub fn palette(&self) -> Palette {
        self.shared.palette()
    }

    pub fn set_palette(&self, palette: Palette) {
        *self.shared.palette.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = palette;
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
        let (rate, samples) = {
            let capture = self.capture.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            (capture.rate(), capture.snapshot())
        };
        let dir = Config::path()?.parent().context("dossier de configuration introuvable")?.join("captures");
        fs::create_dir_all(&dir)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let path = dir.join(format!("capture-{stamp}.wav"));
        wav::write_mono_f32(&path, rate, &samples)?;
        Ok(path)
    }

    /// Arrête les fils, ferme le flux (la lampe revient à l'app Hue) et dresse le bilan.
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
        self.rt.block_on(stream.close())?;
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
        if let Some(port) = &self.midi_port {
            write!(f, "\nMIDI : « {port} » ; chaque note flashe à sa couleur.")?;
        }
        if self.link {
            write!(f, "\nAbleton Link : tempo et phase suivent la session (autorise l'accès réseau si Windows le demande).")?;
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
        Ok(())
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
        let clock = shared.tracker().clock();
        let action = planner.poll(&clock, features.seconds(now), kick);
        // La détection attend les kicks sur les temps tant que le groove tient.
        features.set_grid(Grid {
            period: clock.period().unwrap_or(0.0),
            anchor: clock.anchor(),
            groove: planner.in_groove(&clock, features.seconds(now)),
        });
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
        let color = look.render(now - last, features.levels());
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

/// Réestime le tempo une fois par seconde, hors de la boucle d'éclairage.
fn tempo_loop(shared: &Shared, features: &Features) {
    let mut next = Instant::now() + TEMPO_UPDATE;
    while !shared.stopped() {
        thread::sleep(Duration::from_millis(50));
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

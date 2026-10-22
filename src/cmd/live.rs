//! Mode live : seuls les vrais kicks captés sur l'entrée audio flashent. Ils alimentent aussi
//! une horloge de tempo ; quand ils tombent régulièrement sur les temps, le flash du kick
//! suivant part en avance de la latence Hue calibrée pour s'allumer avec lui. En option, le
//! tempo et la phase viennent d'Ableton Link, et les notes d'un synthé MIDI flashent à leur couleur.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{KeyCode, KeyEvent};
use tokio::runtime::Runtime;

use super::tui::{self, RawMode};
use crate::audio::analysis::KICK_DETECTION_DELAY;
use crate::audio::capture::Capture;
use crate::audio::{AudioInput, Backend, Features, Grid};
use crate::config::Config;
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::hue::api;
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::link::LinkSession;
use crate::look::ReactiveLook;
use crate::midi::{MidiListener, Notes, note_hue};
use crate::tempo::{MANUAL_BPM_LIMITS, Tracker};
use crate::timing;
use crate::wav;

const THRESHOLD_STEP: f32 = 0.05;
const THRESHOLD_RANGE: (f32, f32) = (1.1, 4.0);
const BPM_NUDGE: f64 = 0.5;
const TEMPO_UPDATE: Duration = Duration::from_secs(1);
/// Fréquence de lecture de la session Ableton Link (tempo et phase).
const LINK_POLL: Duration = Duration::from_millis(20);
/// Bornes de l'attente du kick d'un flash anticipé (âge des blocs audio + marge).
const CONFIRM_RANGE: (f64, f64) = (0.03, 0.1);
/// Durée d'affichage d'un message (capture enregistrée) sur la ligne d'état.
const MESSAGE_DURATION: Duration = Duration::from_secs(6);
/// Durée d'allumage des témoins de kick et de note dans le terminal.
const LIT: Duration = Duration::from_millis(100);
const METER_WIDTH: usize = 5;
const MAX_TYPED: usize = 6;

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
}

struct Shared {
    stop: AtomicBool,
    mode: AtomicU8,
    tracker: Mutex<Tracker>,
    /// Le tempo vient d'Ableton Link : les touches de tempo sont sans effet.
    link: bool,
    link_peers: AtomicU64,
    flashes: AtomicU32,
    note_flashes: AtomicU32,
    /// Instant (s, horloge commune) de la dernière note, en bits de `f64`.
    last_note: AtomicU64,
    /// Statistiques de réaction des flashs déclenchés par un kick.
    kick_flashes: AtomicU32,
    total_reaction_us: AtomicU64,
    max_reaction_us: AtomicU64,
}

impl Shared {
    fn tracker(&self) -> MutexGuard<'_, Tracker> {
        self.tracker.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn mode(&self) -> FlashMode {
        FlashMode::from_index(self.mode.load(Ordering::Relaxed))
    }

    fn cycle_mode(&self) {
        self.mode.store(self.mode().next().index(), Ordering::Relaxed);
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

/// Effet d'une touche hors saisie du BPM.
enum KeyOutcome {
    Nothing,
    TypeBpm,
    Message(String),
}

pub fn run(rt: &Runtime, cfg: &Config, options: Options) -> Result<()> {
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
    let mut stream = rt.block_on(HueStream::open(bridge, &area))?;
    let advance = Duration::from_millis(cfg.light_advance_ms.max(0).unsigned_abs());

    let mut tracker = Tracker::new(options.bpm_range);
    if let Some(bpm) = options.bpm
        && !options.link
    {
        tracker.set_manual(bpm, features.seconds(Instant::now()));
    }
    let shared = Shared {
        stop: AtomicBool::new(false),
        mode: AtomicU8::new(options.flash.index()),
        tracker: Mutex::new(tracker),
        link: options.link,
        link_peers: AtomicU64::new(0),
        flashes: AtomicU32::new(0),
        note_flashes: AtomicU32::new(0),
        last_note: AtomicU64::new(f64::NEG_INFINITY.to_bits()),
        kick_flashes: AtomicU32::new(0),
        total_reaction_us: AtomicU64::new(0),
        max_reaction_us: AtomicU64::new(0),
    };

    println!(
        "Mode live : « {} » en {} ({} Hz, {} canaux) → zone « {} ».",
        audio.device, audio.backend, audio.sample_rate, audio.channels, area.metadata.name
    );
    if let Some(reason) = &audio.fallback {
        println!("ASIO indisponible ({reason}) : capture en WASAPI.");
    }
    if let Some(midi) = &midi {
        println!("MIDI : « {} » ; chaque note flashe à sa couleur.", midi.port);
    }
    if options.link {
        println!("Ableton Link : tempo et phase suivent la session (autorise l'accès réseau si Windows le demande).");
    }
    println!();
    println!("  b        saisir le BPM (chiffres puis Entrée)   + / -   BPM ±{BPM_NUDGE}");
    println!("  a        BPM automatique                       m       mode : kicks → réactif → temps");
    println!("  ↑ / ↓    seuil de détection des kicks (plus haut = moins de kicks)");
    println!("  r        enregistre les 40 dernières secondes, pour `dj-bridge analyze`");
    println!("  Échap    quitte");
    println!();
    println!("Ligne d'état : B/M/A = basses, médiums, aigus ; ● = kick ; ♪ = note MIDI ;");
    println!("♩ = tempo (auto, man. ou link·participants) ; mode de flash :");
    println!(
        "« kicks » = flash sur les vrais kicks, anticipé de {} ms quand ils tombent sur les temps ;",
        advance.as_millis()
    );
    println!("« réactif » = flash sur chaque kick sans anticipation ; « temps » = flash sur chaque temps.");
    println!("±n ms = écart moyen kicks/temps ; n ms = réaction côté PC ; ⚠n = coupures audio.");
    println!();

    let capture = audio.capture.clone();
    let (light, status) = thread::scope(|scope| {
        let status = scope.spawn(|| {
            let result = status_loop(&shared, &features, &capture);
            shared.stop.store(true, Ordering::Relaxed);
            result
        });
        scope.spawn(|| tempo_loop(&shared, &features));
        if options.link {
            scope.spawn(|| link_loop(&shared, &features, options.bpm));
        }
        let light = light_loop(rt, &mut stream, &channels, &features, &notes, &shared, advance);
        shared.stop.store(true, Ordering::Relaxed);
        let status = status.join().unwrap_or_else(|_| Err(anyhow!("affichage interrompu")));
        (light, status)
    });
    drop(midi);
    drop(audio);
    rt.block_on(stream.close())?;
    light?;
    let captures = status?;

    let tracker = shared.tracker();
    let tempo = tracker.clock().bpm().map_or("inconnu".to_owned(), |bpm| format!("{bpm:.1} BPM"));
    println!(
        "Tempo final : {tempo} ({}). Écart moyen kicks/temps : {:.1} ms. {} flashs.",
        tracker.source().label(),
        tracker.mean_offset() * 1e3,
        shared.flashes.load(Ordering::Relaxed),
    );
    if shared.kick_flashes.load(Ordering::Relaxed) > 0 {
        println!(
            "Flashs sur kick : réaction côté PC moyenne {:.1} ms, max {:.1} ms (+ latence Hue : {} ms).",
            shared.average_reaction_ms(),
            shared.max_reaction_us.load(Ordering::Relaxed) as f64 / 1e3,
            cfg.light_advance_ms,
        );
    }
    if options.midi.is_some() {
        println!("Notes MIDI : {}.", shared.note_flashes.load(Ordering::Relaxed));
    }
    if options.link {
        println!("Ableton Link : {} autre(s) participant(s).", shared.link_peers.load(Ordering::Relaxed));
    }
    println!(
        "Âge des blocs audio à leur arrivée (pilote) : {:.1} ms. Coupures audio : {}.",
        features.input_latency().as_secs_f64() * 1e3,
        features.glitches(),
    );
    if features.input_lost() {
        println!("L'entrée audio a été perdue en cours de route (câble ou pilote).");
    }
    for path in &captures {
        println!("Capture : {}", path.display());
    }
    Ok(())
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
    let mut look = ReactiveLook::default();
    let mut frame: Vec<(u8, Rgb)> = channels.iter().map(|&id| (id, Rgb::OFF)).collect();
    let mut seen = features.kicks();
    let mut seen_notes = notes.count();
    let mut last = Instant::now();
    let mut next_frame = last;
    while !shared.stop.load(Ordering::Relaxed) {
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
        let color = look.render(now - last, features.levels());
        last = now;
        for (_, c) in frame.iter_mut() {
            *c = color;
        }
        rt.block_on(stream.send(&frame))?;
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
    while !shared.stop.load(Ordering::Relaxed) {
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
    while !shared.stop.load(Ordering::Relaxed) {
        let beat = session.read(features.seconds(Instant::now()));
        shared.tracker().set_link(beat.bpm, beat.beat_time);
        shared.link_peers.store(beat.peers, Ordering::Relaxed);
        thread::sleep(LINK_POLL);
    }
}

/// Affiche la ligne d'état et traite le clavier ; renvoie les captures enregistrées.
fn status_loop(shared: &Shared, features: &Features, capture: &Mutex<Capture>) -> Result<Vec<PathBuf>> {
    let _raw = RawMode::enable()?;
    let mut out = std::io::stdout();
    // BPM en cours de saisie, après la touche `b`.
    let mut typing: Option<String> = None;
    let mut message: Option<(Instant, String)> = None;
    let mut captures = Vec::new();
    while !shared.stop.load(Ordering::Relaxed) {
        if let Some(key) = tui::poll_key(Duration::from_millis(50))? {
            let now = features.seconds(Instant::now());
            let mut show = |text: String| message = Some((Instant::now() + MESSAGE_DURATION, text));
            if let Some(text) = typing.take() {
                typing = type_bpm(shared, &key, text, now);
            } else if key.code == KeyCode::Char('r') {
                match save_capture(capture) {
                    Ok(path) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        captures.push(path);
                        show(format!("capture enregistrée : {name}"));
                    }
                    Err(e) => show(format!("capture impossible : {e:#}")),
                }
            } else {
                match handle_key(shared, features, &key, now) {
                    KeyOutcome::Nothing => {}
                    KeyOutcome::TypeBpm => typing = Some(String::new()),
                    KeyOutcome::Message(text) => show(text),
                }
            }
        }
        let line = match (&typing, &message) {
            (Some(text), _) => format!("BPM : {text}_   Entrée = valider, Échap = annuler"),
            (None, Some((until, text))) if Instant::now() < *until => text.clone(),
            _ => status_text(shared, features),
        };
        tui::status_line(&mut out, &line)?;
    }
    print!("\r\n");
    Ok(captures)
}

/// Écrit les dernières secondes de l'entrée audio dans un WAV, à rejouer avec `dj-bridge analyze`.
fn save_capture(capture: &Mutex<Capture>) -> Result<PathBuf> {
    let (rate, samples) = {
        let capture = capture.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (capture.rate(), capture.snapshot())
    };
    let dir = Config::path()?.parent().context("dossier de configuration introuvable")?.join("captures");
    fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let path = dir.join(format!("capture-{stamp}.wav"));
    wav::write_mono_f32(&path, rate, &samples)?;
    Ok(path)
}

/// Touche pendant la saisie du BPM ; renvoie la saisie en cours, ou `None` une fois terminée.
fn type_bpm(shared: &Shared, key: &KeyEvent, mut text: String, now: f64) -> Option<String> {
    match key.code {
        KeyCode::Char(c @ ('0'..='9' | '.' | ',')) if text.len() < MAX_TYPED => {
            text.push(if c == ',' { '.' } else { c });
        }
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Enter => {
            if let Ok(bpm) = text.parse::<f64>()
                && (MANUAL_BPM_LIMITS.0..=MANUAL_BPM_LIMITS.1).contains(&bpm)
            {
                shared.tracker().set_manual(bpm, now);
            }
            return None;
        }
        KeyCode::Esc => return None,
        _ => {}
    }
    Some(text)
}

fn handle_key(shared: &Shared, features: &Features, key: &KeyEvent, now: f64) -> KeyOutcome {
    if tui::is_quit(key) {
        shared.stop.store(true, Ordering::Relaxed);
        return KeyOutcome::Nothing;
    }
    match key.code {
        KeyCode::Char('b' | '+' | '-' | 'a') if shared.link => {
            return KeyOutcome::Message("tempo piloté par Ableton Link : change-le depuis la session".to_owned());
        }
        KeyCode::Char('b') => return KeyOutcome::TypeBpm,
        KeyCode::Up | KeyCode::Down => {
            let step = if key.code == KeyCode::Up { THRESHOLD_STEP } else { -THRESHOLD_STEP };
            let threshold = (features.threshold() + step).clamp(THRESHOLD_RANGE.0, THRESHOLD_RANGE.1);
            features.set_threshold(threshold);
        }
        KeyCode::Char(sign @ ('+' | '-')) => {
            let mut tracker = shared.tracker();
            if let Some(bpm) = tracker.clock().bpm() {
                let delta = if sign == '+' { BPM_NUDGE } else { -BPM_NUDGE };
                tracker.set_manual((bpm + delta).clamp(MANUAL_BPM_LIMITS.0, MANUAL_BPM_LIMITS.1), now);
            }
        }
        KeyCode::Char('a') => shared.tracker().set_auto(),
        KeyCode::Char('m') => shared.cycle_mode(),
        _ => {}
    }
    KeyOutcome::Nothing
}

fn status_text(shared: &Shared, features: &Features) -> String {
    let now = Instant::now();
    let kick_lit = features.kicks() > 0 && now.saturating_duration_since(features.last_kick()) < LIT;
    let note_lit = features.seconds(now) - shared.last_note() < LIT.as_secs_f64();
    let levels = features.levels();
    let (bpm, source, offset) = {
        let tracker = shared.tracker();
        (tracker.clock().bpm(), tracker.source(), tracker.mean_offset())
    };
    let tempo = bpm.map_or("---.-".to_owned(), |bpm| format!("{bpm:.1}"));
    let source = if shared.link {
        format!("link·{}", shared.link_peers.load(Ordering::Relaxed))
    } else {
        source.short_label().to_owned()
    };
    let mode = shared.mode();
    let metric = match mode {
        FlashMode::Reactive => format!("{:.1}ms", shared.average_reaction_ms()),
        FlashMode::Kicks | FlashMode::EveryBeat => format!("±{:.0}ms", offset * 1e3),
    };
    let alert = match features.glitches() {
        _ if features.input_lost() => " ⚠ entrée perdue".to_owned(),
        0 => String::new(),
        n => format!(" ⚠{n}"),
    };
    format!(
        "B{} M{} A{} {}{} ♩{tempo} {source} {} s{:.2} {metric}{alert}",
        tui::meter(levels.bass, METER_WIDTH),
        tui::meter(levels.mid, METER_WIDTH),
        tui::meter(levels.high, METER_WIDTH),
        if kick_lit { "●" } else { "○" },
        if note_lit { "♪" } else { " " },
        mode.label(),
        features.threshold(),
    )
}

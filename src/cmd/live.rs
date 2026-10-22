//! Mode live : seuls les vrais kicks captés sur l'entrée audio flashent. Ils alimentent aussi
//! une horloge de tempo ; quand ils tombent régulièrement sur les temps, le flash du kick
//! suivant part en avance de la latence Hue calibrée pour s'allumer avec lui.

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
use crate::audio::{AudioInput, Features, Grid};
use crate::config::Config;
use crate::flash::{Action, CONFIRM_MARGIN, FlashMode, FlashPlanner};
use crate::hue::api;
use crate::wav;
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::look::ReactiveLook;
use crate::tempo::{MANUAL_BPM_LIMITS, TempoSource, Tracker};
use crate::timing;

const THRESHOLD_STEP: f32 = 0.05;
const THRESHOLD_RANGE: (f32, f32) = (1.1, 4.0);
const BPM_NUDGE: f64 = 0.5;
const TEMPO_UPDATE: Duration = Duration::from_secs(1);
/// Bornes de l'attente du kick d'un flash anticipé (âge des blocs audio + marge).
const CONFIRM_RANGE: (f64, f64) = (0.03, 0.1);
/// Durée d'affichage d'un message (capture enregistrée) sur la ligne d'état.
const MESSAGE_DURATION: Duration = Duration::from_secs(6);
/// Durée d'allumage du témoin de kick dans le terminal.
const KICK_LIT: Duration = Duration::from_millis(100);
const METER_WIDTH: usize = 5;
const MAX_TYPED: usize = 6;

pub struct Options {
    pub input: Option<String>,
    pub threshold: f32,
    /// Tempo imposé au démarrage ; sinon détection automatique.
    pub bpm: Option<f64>,
    pub bpm_range: (f64, f64),
    pub flash: FlashMode,
}

struct Shared {
    stop: AtomicBool,
    mode: AtomicU8,
    tracker: Mutex<Tracker>,
    flashes: AtomicU32,
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
    let audio = AudioInput::start(options.input.as_deref(), features.clone())?;
    let mut stream = rt.block_on(HueStream::open(bridge, &area))?;
    let advance = Duration::from_millis(cfg.light_advance_ms.max(0).unsigned_abs());

    let mut tracker = Tracker::new(options.bpm_range);
    if let Some(bpm) = options.bpm {
        tracker.set_manual(bpm, features.seconds(Instant::now()));
    }
    let shared = Shared {
        stop: AtomicBool::new(false),
        mode: AtomicU8::new(options.flash.index()),
        tracker: Mutex::new(tracker),
        flashes: AtomicU32::new(0),
        kick_flashes: AtomicU32::new(0),
        total_reaction_us: AtomicU64::new(0),
        max_reaction_us: AtomicU64::new(0),
    };

    println!(
        "Mode live : « {} » ({} Hz, {} canaux) → zone « {} ».",
        audio.device, audio.sample_rate, audio.channels, area.metadata.name
    );
    println!();
    println!("  b        saisir le BPM (chiffres puis Entrée)   + / -   BPM ±{BPM_NUDGE}");
    println!("  a        BPM automatique                       m       mode : kicks → réactif → temps");
    println!("  ↑ / ↓    seuil de détection des kicks (plus haut = moins de kicks)");
    println!("  r        enregistre les 40 dernières secondes, pour `dj-bridge analyze`");
    println!("  Échap    quitte");
    println!();
    println!("Ligne d'état : B/M/A = basses, médiums, aigus ; ● = kick détecté ; ♩ = tempo, auto ou man.");
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
        let light = light_loop(rt, &mut stream, &channels, &features, &shared, advance);
        shared.stop.store(true, Ordering::Relaxed);
        let status = status.join().unwrap_or_else(|_| Err(anyhow!("affichage interrompu")));
        (light, status)
    });
    drop(audio);
    rt.block_on(stream.close())?;
    light?;
    let captures = status?;

    let tracker = shared.tracker();
    let tempo = tracker.clock().bpm().map_or("inconnu".to_owned(), |bpm| format!("{bpm:.1} BPM"));
    let source = match tracker.source() {
        TempoSource::Auto => "automatique",
        TempoSource::Manual => "manuel",
    };
    println!(
        "Tempo final : {tempo} ({source}). Écart moyen kicks/temps : {:.1} ms. {} flashs.",
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

/// Flashe selon le planificateur (vrais kicks, anticipés quand ils sont attendus) et entretient
/// le flux à 60 Hz entre deux flashs.
fn light_loop(
    rt: &Runtime,
    stream: &mut HueStream,
    channels: &[u8],
    features: &Features,
    shared: &Shared,
    advance: Duration,
) -> Result<()> {
    let mut planner = FlashPlanner::new(shared.mode(), advance.as_secs_f64(), CONFIRM_RANGE.1);
    let mut look = ReactiveLook::default();
    let mut frame: Vec<(u8, Rgb)> = channels.iter().map(|&id| (id, Rgb::OFF)).collect();
    let mut seen = features.kicks();
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
        timing::sleep_until_or(deadline, || features.kicks() != seen);

        let kicks = features.kicks();
        let kick = (kicks != seen).then(|| features.seconds(features.last_kick()) - KICK_DETECTION_DELAY);
        seen = kicks;
        if let Some(at) = kick {
            shared.tracker().on_kick(at);
        }

        let now = Instant::now();
        let clock = shared.tracker().clock();
        let action = planner.poll(&clock, features.seconds(now), kick);
        // La détection attend les kicks sur les temps tant que le groove tient.
        features.set_grid(Grid {
            period: clock.period().unwrap_or(0.0),
            anchor: clock.anchor(),
            groove: planner.in_groove(&clock, features.seconds(now)),
        });
        if action == Action::Cancel {
            look.cancel_flash();
        }
        let color = look.update(now - last, matches!(action, Action::Flash { .. }), features.levels());
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
            typing = match typing {
                Some(text) => type_bpm(shared, &key, text, now),
                None if key.code == KeyCode::Char('r') => {
                    let text = match save_capture(capture) {
                        Ok(path) => {
                            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                            captures.push(path);
                            format!("capture enregistrée : {name}")
                        }
                        Err(e) => format!("capture impossible : {e:#}"),
                    };
                    message = Some((Instant::now() + MESSAGE_DURATION, text));
                    None
                }
                None => handle_key(shared, features, &key, now),
            };
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

/// Touche hors saisie ; renvoie `Some` si elle ouvre la saisie du BPM.
fn handle_key(shared: &Shared, features: &Features, key: &KeyEvent, now: f64) -> Option<String> {
    if tui::is_quit(key) {
        shared.stop.store(true, Ordering::Relaxed);
        return None;
    }
    match key.code {
        KeyCode::Char('b') => return Some(String::new()),
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
    None
}

fn status_text(shared: &Shared, features: &Features) -> String {
    let lit = features.kicks() > 0
        && Instant::now().saturating_duration_since(features.last_kick()) < KICK_LIT;
    let levels = features.levels();
    let (bpm, source, offset) = {
        let tracker = shared.tracker();
        (tracker.clock().bpm(), tracker.source(), tracker.mean_offset())
    };
    let tempo = bpm.map_or("---.-".to_owned(), |bpm| format!("{bpm:.1}"));
    let source = match source {
        TempoSource::Auto => "auto",
        TempoSource::Manual => "man.",
    };
    let mode = shared.mode();
    let metric = match mode {
        FlashMode::Reactive => format!("{:.1}ms", shared.average_reaction_ms()),
        FlashMode::Kicks | FlashMode::EveryBeat => format!("±{:.0}ms", offset * 1e3),
    };
    let mode = mode.label();
    let alert = match features.glitches() {
        _ if features.input_lost() => " ⚠ entrée perdue".to_owned(),
        0 => String::new(),
        n => format!(" ⚠{n}"),
    };
    format!(
        "B{} M{} A{} {} ♩{tempo} {source} {mode} s{:.2} {metric}{alert}",
        tui::meter(levels.bass, METER_WIDTH),
        tui::meter(levels.mid, METER_WIDTH),
        tui::meter(levels.high, METER_WIDTH),
        if lit { "●" } else { "○" },
        features.threshold(),
    )
}

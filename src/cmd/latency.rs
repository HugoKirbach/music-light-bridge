//! Calibration : à chaque période, un clic sort des enceintes et la lampe flashe en blanc.
//! On ajuste l'avance du flash jusqu'à ce que les deux coïncident, à l'œil ou en filmant
//! au ralenti ; la valeur retenue sert ensuite à anticiper les flashs calés sur le tempo.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use tokio::runtime::Runtime;

use crate::config::Config;
use crate::hue::api;
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::metronome::{Metronome, OutputLatency};
use crate::timing;

const FLASH_LEN: Duration = Duration::from_millis(80);
/// Laisse le temps à la sortie audio de démarrer avant le premier clic.
const START_DELAY: Duration = Duration::from_millis(800);
const MIN_PERIOD: Duration = Duration::from_millis(300);
const ADVANCE_RANGE_MS: (i64, i64) = (-200, 600);

/// État partagé entre la boucle d'envoi (temps réel) et le thread clavier/affichage.
struct Shared {
    advance_ms: AtomicI64,
    stop: AtomicBool,
    save: AtomicBool,
    max_late_us: AtomicU64,
}

pub fn run(rt: &Runtime, cfg: &mut Config, period: Duration) -> Result<()> {
    if period < MIN_PERIOD {
        bail!("période trop courte (minimum {} ms)", MIN_PERIOD.as_millis());
    }
    let bridge = cfg.bridge()?.clone();
    let area = rt.block_on(api::select_area(&bridge, cfg.area_id.as_deref()))?;
    let channels: Vec<u8> = area.channels.iter().map(|c| c.channel_id).collect();

    let mut stream = rt.block_on(HueStream::open(&bridge, &area))?;
    let origin = Instant::now() + START_DELAY;
    let metronome = match Metronome::start(origin, period) {
        Ok(metronome) => metronome,
        Err(e) => {
            let _ = rt.block_on(stream.close());
            return Err(e);
        }
    };

    println!("Calibration de latence : zone « {} », sortie audio « {} ».", area.metadata.name, metronome.device);
    println!();
    println!("Toutes les {} ms, deux signaux partent au même instant : un tic dans les enceintes", period.as_millis());
    println!("et un flash blanc. La lampe a du retard : tu entends d'abord le tic, puis tu vois le flash.");
    println!();
    println!("  ↑ / ↓   fait partir le flash 5 ms plus tôt / plus tard");
    println!("  → / ←   pareil, par pas de 1 ms");
    println!("  Entrée  enregistre la valeur et quitte        Échap  quitte sans enregistrer");
    println!();
    println!("Appuie sur ↑ jusqu'à ce que tic et flash tombent ensemble ; si le flash passe avant le tic,");
    println!("redescends. Regarde la lampe (pas l'écran) et utilise une sortie audio filaire : le Bluetooth");
    println!("ajoute ~150 ms que le pilote ne rapporte pas.");
    println!();

    let shared = Shared {
        advance_ms: AtomicI64::new(cfg.light_advance_ms),
        stop: AtomicBool::new(false),
        save: AtomicBool::new(false),
        max_late_us: AtomicU64::new(0),
    };
    let result = thread::scope(|scope| {
        let latency = metronome.latency.clone();
        let shared = &shared;
        scope.spawn(move || {
            let result = keyboard_loop(shared, &latency);
            shared.stop.store(true, Ordering::Relaxed);
            result
        });
        let result = flash_loop(rt, &mut stream, &channels, origin, period, shared);
        shared.stop.store(true, Ordering::Relaxed);
        result
    });
    drop(metronome);
    rt.block_on(stream.close())?;
    result?;

    let advance = shared.advance_ms.load(Ordering::Relaxed);
    if shared.save.load(Ordering::Relaxed) {
        cfg.light_advance_ms = advance;
        cfg.save()?;
        println!("Avance enregistrée : {advance} ms.");
    } else {
        println!("Rien enregistré (avance testée : {advance} ms).");
    }
    Ok(())
}

/// Envoie un flash à chaque clic, en avance de `advance_ms`, et entretient le flux à 60 Hz
/// entre deux flashs. Les fronts du flash partent à l'instant exact, sans attendre la trame suivante.
fn flash_loop(
    rt: &Runtime,
    stream: &mut HueStream,
    channels: &[u8],
    origin: Instant,
    period: Duration,
    shared: &Shared,
) -> Result<()> {
    let lit: Vec<(u8, Rgb)> = channels.iter().map(|&id| (id, Rgb::WHITE)).collect();
    let dark: Vec<(u8, Rgb)> = channels.iter().map(|&id| (id, Rgb::OFF)).collect();
    let mut beat: u32 = 0;
    let mut next_frame = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        let click = origin + period * beat;
        let on = shift_earlier(click, shared.advance_ms.load(Ordering::Relaxed));
        let off = on + FLASH_LEN;
        let now = Instant::now();
        if now >= off {
            beat += 1;
            continue;
        }
        let edge = if now < on { on } else { off };
        let deadline = next_frame.min(edge);
        timing::sleep_until(deadline);

        let woke = Instant::now();
        let late = woke.saturating_duration_since(deadline).as_micros() as u64;
        shared.max_late_us.fetch_max(late, Ordering::Relaxed);
        let frame = if woke >= on && woke < off { &lit } else { &dark };
        rt.block_on(stream.send(frame))?;
        next_frame = woke + FRAME_PERIOD;
    }
    Ok(())
}

fn keyboard_loop(shared: &Shared, latency: &OutputLatency) -> Result<()> {
    terminal::enable_raw_mode()?;
    let _raw = RawModeGuard;
    let mut out = std::io::stdout();
    while !shared.stop.load(Ordering::Relaxed) {
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let step = match key.code {
                KeyCode::Up => 5,
                KeyCode::Down => -5,
                KeyCode::Right => 1,
                KeyCode::Left => -1,
                KeyCode::Enter => {
                    shared.save.store(true, Ordering::Relaxed);
                    shared.stop.store(true, Ordering::Relaxed);
                    0
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    shared.stop.store(true, Ordering::Relaxed);
                    0
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    shared.stop.store(true, Ordering::Relaxed);
                    0
                }
                _ => 0,
            };
            let advance = shared.advance_ms.load(Ordering::Relaxed) + step;
            shared
                .advance_ms
                .store(advance.clamp(ADVANCE_RANGE_MS.0, ADVANCE_RANGE_MS.1), Ordering::Relaxed);
        }
        write!(
            out,
            "\r  avance du flash : {:>4} ms │ latence sortie audio : {:>5.1} ms │ retard d'envoi max : {:>4.1} ms   ",
            shared.advance_ms.load(Ordering::Relaxed),
            latency.get().as_secs_f64() * 1e3,
            shared.max_late_us.load(Ordering::Relaxed) as f64 / 1e3,
        )?;
        out.flush()?;
    }
    write!(out, "\r\n")?;
    Ok(())
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Décale un instant vers le passé de `ms` millisecondes (vers le futur si négatif).
fn shift_earlier(t: Instant, ms: i64) -> Instant {
    let shift = Duration::from_millis(ms.unsigned_abs());
    if ms >= 0 { t - shift } else { t + shift }
}

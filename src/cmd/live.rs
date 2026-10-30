//! Mode live dans le terminal : ligne d'état et raccourcis clavier au-dessus du moteur.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent};
use tokio::runtime::Runtime;

use super::tui::{self, RawMode};
use crate::config::Config;
use crate::engine::{Engine, Options, Status};
use crate::flash::FlashMode;

const THRESHOLD_STEP: f32 = 0.05;
const BPM_NUDGE: f64 = 0.5;
/// Durée d'affichage d'un message (capture enregistrée) sur la ligne d'état.
const MESSAGE_DURATION: Duration = Duration::from_secs(6);
const METER_WIDTH: usize = 5;
const MAX_TYPED: usize = 6;

/// Effet d'une touche hors saisie du BPM.
enum KeyOutcome {
    Nothing,
    TypeBpm,
    Quit,
}

pub fn run(rt: Arc<Runtime>, cfg: &Config, options: Options) -> Result<()> {
    let mut engine = Engine::start(rt, cfg, options)?;
    println!("{}", engine.info);
    println!();
    println!("  b        saisir le BPM (chiffres puis Entrée)   + / -   BPM ±{BPM_NUDGE}");
    println!("  a        BPM automatique                       m       mode : kicks → réactif → temps");
    println!("  ↑ / ↓    seuil de détection des kicks (plus haut = moins de kicks)");
    println!("  r        enregistre les 40 dernières secondes, pour `dj-bridge analyze`");
    println!("  1 à 9    rappelle une scène enregistrée dans la fenêtre");
    println!("  Échap    quitte");
    println!();
    println!("Ligne d'état : B/M/A = basses, médiums, aigus ; ● = kick ; ♪ = note MIDI ;");
    println!("♩ = tempo (auto, man. ou link·participants) ; mode de flash :");
    println!(
        "« kicks » = flash sur les vrais kicks, anticipé de {} ms quand ils tombent sur les temps ;",
        engine.info.advance.as_millis()
    );
    println!("« réactif » = flash sur chaque kick sans anticipation ; « temps » = flash sur chaque temps.");
    println!("±n ms = écart moyen kicks/temps ; n ms = réaction côté PC ; ⚠n = coupures audio.");
    println!();

    let captures = status_loop(&mut engine);
    let summary = engine.stop()?;
    println!("{summary}");
    for path in &captures? {
        println!("Capture : {}", path.display());
    }
    Ok(())
}

/// Affiche la ligne d'état et traite le clavier ; renvoie les captures enregistrées.
fn status_loop(engine: &mut Engine) -> Result<Vec<PathBuf>> {
    let _raw = RawMode::enable()?;
    let mut out = std::io::stdout();
    // BPM en cours de saisie, après la touche `b`.
    let mut typing: Option<String> = None;
    let mut message: Option<(Instant, String)> = None;
    let mut captures = Vec::new();
    while engine.is_running() {
        if let Some(key) = tui::poll_key(Duration::from_millis(50))? {
            let mut show = |text: String| message = Some((Instant::now() + MESSAGE_DURATION, text));
            if let Some(text) = typing.take() {
                match type_bpm(engine, &key, text) {
                    Ok(next) => typing = next,
                    Err(e) => show(format!("{e:#}")),
                }
            } else if key.code == KeyCode::Char('r') {
                match engine.save_capture() {
                    Ok(path) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        captures.push(path);
                        show(format!("capture enregistrée : {name}"));
                    }
                    Err(e) => show(format!("capture impossible : {e:#}")),
                }
            } else {
                match handle_key(engine, &key) {
                    Ok(KeyOutcome::Nothing) => {}
                    Ok(KeyOutcome::TypeBpm) => typing = Some(String::new()),
                    Ok(KeyOutcome::Quit) => break,
                    Err(e) => show(format!("{e:#}")),
                }
            }
        }
        if let Some(Ok(())) = engine.recover_audio() {
            message = Some((Instant::now() + MESSAGE_DURATION, "entrée audio retrouvée".to_owned()));
        }
        let line = match (&typing, &message) {
            (Some(text), _) => format!("BPM : {text}_   Entrée = valider, Échap = annuler"),
            (None, Some((until, text))) if Instant::now() < *until => text.clone(),
            _ => status_text(&engine.status()),
        };
        tui::status_line(&mut out, &line)?;
    }
    print!("\r\n");
    Ok(captures)
}

/// Touche pendant la saisie du BPM ; renvoie la saisie en cours, ou `None` une fois terminée.
fn type_bpm(engine: &Engine, key: &KeyEvent, mut text: String) -> Result<Option<String>> {
    match key.code {
        KeyCode::Char(c @ ('0'..='9' | '.' | ',')) if text.len() < MAX_TYPED => {
            text.push(if c == ',' { '.' } else { c });
        }
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Enter => {
            if let Ok(bpm) = text.parse::<f64>() {
                engine.set_bpm(bpm)?;
            }
            return Ok(None);
        }
        KeyCode::Esc => return Ok(None),
        _ => {}
    }
    Ok(Some(text))
}

fn handle_key(engine: &Engine, key: &KeyEvent) -> Result<KeyOutcome> {
    if tui::is_quit(key) {
        return Ok(KeyOutcome::Quit);
    }
    match key.code {
        KeyCode::Char('b') => {
            engine.check_tempo_control()?;
            return Ok(KeyOutcome::TypeBpm);
        }
        KeyCode::Up | KeyCode::Down => {
            let step = if key.code == KeyCode::Up { THRESHOLD_STEP } else { -THRESHOLD_STEP };
            engine.set_threshold(engine.status().threshold + step);
        }
        KeyCode::Char(sign @ ('+' | '-')) => {
            engine.nudge_bpm(if sign == '+' { BPM_NUDGE } else { -BPM_NUDGE })?;
        }
        KeyCode::Char('a') => engine.set_auto()?,
        KeyCode::Char('m') => engine.set_mode(engine.status().mode.next()),
        KeyCode::Char(digit @ '1'..='9') => {
            let index = usize::from(digit as u8 - b'1');
            engine.recall_preset(index).ok_or_else(|| anyhow!("pas de scène n° {}", index + 1))?;
        }
        _ => {}
    }
    Ok(KeyOutcome::Nothing)
}

fn status_text(status: &Status) -> String {
    let tempo = status.bpm.map_or("---.-".to_owned(), |bpm| format!("{bpm:.1}"));
    let source = match status.link_peers {
        Some(peers) => format!("link·{peers}"),
        None => status.source.short_label().to_owned(),
    };
    let metric = match status.mode {
        FlashMode::Reactive => format!("{:.1}ms", status.reaction_ms),
        FlashMode::Kicks | FlashMode::EveryBeat => format!("±{:.0}ms", status.offset_ms),
    };
    let alert = match status.glitches {
        _ if status.bridge_lost => " ⚠ bridge injoignable, reconnexion…".to_owned(),
        _ if status.input_lost => " ⚠ entrée perdue, reconnexion…".to_owned(),
        _ if status.silent => " ⚠ aucun son capté".to_owned(),
        0 => String::new(),
        n => format!(" ⚠{n}"),
    };
    let auto = match status.auto_captures {
        0 => String::new(),
        n => format!(" ●rec{n}"),
    };
    let preset = status.preset.as_ref().map_or(String::new(), |name| format!(" ★{name}"));
    let phase = match status.build {
        Some(build) => format!("{} {build}%", status.phase.label()),
        None => status.phase.label().to_owned(),
    };
    let levels = status.levels;
    format!(
        "B{} M{} A{} {}{} ♩{tempo} {source} {} {phase} s{:.2} {metric}{preset}{alert}{auto}",
        tui::meter(levels.bass, METER_WIDTH),
        tui::meter(levels.mid, METER_WIDTH),
        tui::meter(levels.high, METER_WIDTH),
        if status.kick_lit { "●" } else { "○" },
        if status.note_lit { "♪" } else { " " },
        status.mode.label(),
        status.threshold,
    )
}

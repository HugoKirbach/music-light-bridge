//! Mise en place : découverte du bridge, appairage et choix de la zone de divertissement.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tokio::runtime::Runtime;

use crate::audio::analysis::DEFAULT_THRESHOLD;
use crate::audio::{self, AudioInput, Backend, Features};
use crate::config::Config;
use crate::hue::api;
use crate::hue::snapshot::Snapshot;
use crate::midi;

const PAIR_WAIT: Duration = Duration::from_secs(30);
const INPUT_TEST: Duration = Duration::from_secs(3);
/// Au-dessus, la bande des basses porte un signal (niveau normalisé, le silence reste à 0).
const SIGNAL_LEVEL: f32 = 0.2;

pub fn devices() -> Result<()> {
    let inputs = audio::list_inputs()?;
    if inputs.is_empty() {
        println!("Aucune entrée audio trouvée.");
    } else {
        println!("Entrées audio (→ : utilisée par `live` sans --input) :");
        for input in &inputs {
            let mark = if input.is_preferred { "→" } else { " " };
            let default = if input.is_default { "  (défaut Windows)" } else { "" };
            println!(
                "{mark} {}  {} canaux, {} Hz{default}",
                input.name, input.channels, input.sample_rate
            );
        }
    }

    println!("En ASIO, `live` ouvre le pilote « Focusrite USB ASIO » (ou celui choisi avec --input).");

    println!();
    println!("Sorties du PC captables avec `--audio output` (→ : sortie par défaut, sans --input) :");
    for (i, output) in audio::list_outputs()?.iter().enumerate() {
        println!("{} {output}", if i == 0 { "→" } else { " " });
    }

    println!();
    let ports = midi::list_inputs()?;
    if ports.is_empty() {
        println!("Aucune entrée MIDI trouvée.");
    } else {
        println!("Entrées MIDI (→ : utilisée par `live --midi` sans nom) :");
        for (i, port) in ports.iter().enumerate() {
            println!("{} {port}", if i == 0 { "→" } else { " " });
        }
    }
    Ok(())
}

/// Rétablit l'état des lampes d'avant le flux, laissé sur disque par une session interrompue.
pub fn restore(rt: &Runtime, cfg: &Config) -> Result<()> {
    let Some(snapshot) = Snapshot::leftover()? else {
        println!("Rien à rétablir : la dernière session a déjà remis les lampes dans leur état d'origine.");
        return Ok(());
    };
    let bridge = cfg.bridge()?;
    println!(
        "État pris il y a {} : {} lampe(s), {} scène(s) active(s).",
        format_age(snapshot.age()),
        snapshot.lights.len(),
        snapshot.scenes.len()
    );
    // Un flux resté ouvert par un plantage garderait la main sur les lampes.
    if let Some(area_id) = &cfg.area_id {
        let _ = rt.block_on(api::set_streaming(bridge, area_id, false));
    }
    println!("{}", rt.block_on(snapshot.restore(bridge))?);
    Ok(())
}

fn format_age(age: Duration) -> String {
    let minutes = age.as_secs() / 60;
    match minutes {
        0 => "moins d'une minute".to_owned(),
        1..60 => format!("{minutes} min"),
        _ => format!("{} h {:02}", minutes / 60, minutes % 60),
    }
}

/// Ouvre l'entrée audio quelques secondes, sans le bridge, et rend compte de ce qu'elle reçoit.
pub fn test_input(input: Option<String>, backend: Backend) -> Result<()> {
    let features = Arc::new(Features::new(DEFAULT_THRESHOLD));
    let audio = AudioInput::start(input.as_deref(), backend, features.clone())?;
    if let Some(reason) = &audio.fallback {
        println!("ASIO indisponible ({reason}) : repli sur WASAPI.");
    }
    println!(
        "« {} » en {} ({} Hz, {} canaux) : écoute pendant {} s…",
        audio.device,
        audio.backend,
        audio.sample_rate,
        audio.channels,
        INPUT_TEST.as_secs()
    );
    let end = Instant::now() + INPUT_TEST;
    let mut loudest = 0.0f32;
    while Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
        loudest = loudest.max(features.levels().bass);
    }
    println!(
        "Âge des blocs audio : {:.1} ms. Coupures : {}. Kicks détectés : {}. Signal dans les basses : {}.",
        features.input_latency().as_secs_f64() * 1e3,
        features.glitches(),
        features.kicks(),
        if loudest > SIGNAL_LEVEL { "oui" } else { "non (silence ?)" },
    );
    if features.input_lost() {
        println!("L'entrée a été perdue pendant le test.");
    }
    Ok(())
}

pub fn discover(rt: &Runtime) -> Result<()> {
    let bridges = rt.block_on(api::discover())?;
    if bridges.is_empty() {
        println!("Aucun bridge trouvé : vérifie que le PC et le bridge sont sur le même réseau.");
    }
    for bridge in &bridges {
        println!("Bridge {} → {}", bridge.id, bridge.ip);
    }
    Ok(())
}

pub fn pair(rt: &Runtime, cfg: &mut Config, ip: Option<String>) -> Result<()> {
    let ip = match ip {
        Some(ip) => ip,
        None => {
            let found = rt.block_on(api::discover())?;
            match found.as_slice() {
                [bridge] => bridge.ip.clone(),
                [] => bail!(
                    "aucun bridge trouvé : passe son IP avec --ip (app Hue > Paramètres > Bridges Hue > ⓘ)"
                ),
                _ => {
                    let ips: Vec<&str> = found.iter().map(|b| b.ip.as_str()).collect();
                    bail!("plusieurs bridges trouvés ({}) : choisis avec --ip", ips.join(", "));
                }
            }
        }
    };

    println!("Bridge {ip} : appuie sur son bouton rond (tu as {} s)…", PAIR_WAIT.as_secs());
    let credentials = rt.block_on(api::pair(&ip, PAIR_WAIT))?;
    cfg.bridge = Some(credentials);
    cfg.area_id = None;
    cfg.save()?;
    println!("Associé ! Identifiants enregistrés dans {}", Config::path()?.display());
    areas(rt, cfg, None)
}

pub fn areas(rt: &Runtime, cfg: &mut Config, select: Option<String>) -> Result<()> {
    let list = rt.block_on(api::entertainment_areas(cfg.bridge()?))?;
    if list.is_empty() {
        println!(
            "Aucune zone de divertissement. Crée-en une dans l'app Hue \
             (Paramètres > Zones de divertissement) avec ton ampoule, puis relance `dj-bridge areas`."
        );
        return Ok(());
    }

    if let Some(id) = select {
        if !list.iter().any(|area| area.id == id) {
            bail!("zone {id} introuvable");
        }
        cfg.area_id = Some(id);
        cfg.save()?;
    } else if cfg.area_id.is_none() && list.len() == 1 {
        cfg.area_id = Some(list[0].id.clone());
        cfg.save()?;
    }

    for area in &list {
        let mark = if cfg.area_id.as_deref() == Some(area.id.as_str()) { "→" } else { " " };
        println!(
            "{mark} {}  « {} »  {} canal(aux), {}",
            area.id,
            area.metadata.name,
            area.channels.len(),
            area.status
        );
    }
    Ok(())
}

//! Mise en place : découverte du bridge, appairage et choix de la zone de divertissement.

use std::time::Duration;

use anyhow::{Result, bail};
use tokio::runtime::Runtime;

use crate::config::Config;
use crate::hue::api;

const PAIR_WAIT: Duration = Duration::from_secs(30);

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

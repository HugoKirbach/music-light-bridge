//! Photo de l'état des lampes avant le flux, rétablie à sa fermeture : marche/arrêt, luminosité,
//! couleur ou blanc, et scènes actives (une scène dynamique reprend son animation). La photo est
//! aussi écrite sur disque : après un plantage, elle est reprise au lancement suivant ou
//! rétablie par `dj-bridge restore`.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::api::{self, EntertainmentArea, Light, Scene};
use crate::config::{BridgeCredentials, Config};

const FILE_NAME: &str = "lampes-avant-flux.json";
/// Une photo laissée par une session interrompue reste crue 6 h ; au-delà, les lampes ont
/// sans doute été réglées à la main depuis.
const LEFTOVER_MAX_AGE: Duration = Duration::from_secs(6 * 3600);
/// Fondu du retour à l'état d'origine (ms).
const RESTORE_FADE_MS: u32 = 400;
/// Juste après la fin du flux, le bridge met un instant à rendre la main sur les lampes.
const RELEASE_DELAY: Duration = Duration::from_millis(300);
const RESTORE_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LightColor {
    Xy { x: f64, y: f64 },
    /// Blanc, température en mireds.
    White { mirek: u16 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightState {
    pub id: String,
    pub name: String,
    pub on: bool,
    /// Luminosité en %, absente pour une lampe sans variateur.
    pub brightness: Option<f64>,
    pub color: Option<LightColor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveScene {
    pub id: String,
    pub name: String,
    /// Mode de rappel : `static` ou `dynamic_palette` (scène animée).
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Secondes écoulées depuis 1970.
    pub taken_at: u64,
    pub lights: Vec<LightState>,
    pub scenes: Vec<ActiveScene>,
}

/// Ce qui a été rétabli.
#[derive(Debug)]
pub struct Restored {
    pub lights: usize,
    pub scenes: usize,
}

impl From<Light> for LightState {
    fn from(light: Light) -> Self {
        let white = light.color_temperature.as_ref().filter(|ct| ct.mirek_valid).and_then(|ct| ct.mirek);
        let color = match (white, light.color) {
            (Some(mirek), _) => Some(LightColor::White { mirek }),
            (None, Some(color)) => Some(LightColor::Xy { x: color.xy.x, y: color.xy.y }),
            (None, None) => None,
        };
        Self {
            id: light.id,
            name: light.metadata.name,
            on: light.on.on,
            brightness: light.dimming.map(|dimming| dimming.brightness),
            color,
        }
    }
}

/// Scènes actives qui touchent l'une des lampes `ids`.
fn active_scenes(scenes: Vec<Scene>, ids: &[&str]) -> Vec<ActiveScene> {
    scenes
        .into_iter()
        .filter_map(|scene| {
            let action = scene.status?.active;
            let ours = scene
                .actions
                .iter()
                .any(|a| a.target.rtype == "light" && ids.contains(&a.target.rid.as_str()));
            (action != "inactive" && ours).then_some(ActiveScene { id: scene.id, name: scene.metadata.name, action })
        })
        .collect()
}

/// Corps CLIP v2 qui remet une lampe dans l'état photographié, en fondu.
fn restore_body(state: &LightState) -> Value {
    let mut body = json!({ "on": { "on": state.on }, "dynamics": { "duration": RESTORE_FADE_MS } });
    if let Some(brightness) = state.brightness {
        body["dimming"] = json!({ "brightness": brightness });
    }
    match state.color {
        Some(LightColor::Xy { x, y }) => body["color"] = json!({ "xy": { "x": x, "y": y } }),
        Some(LightColor::White { mirek }) => body["color_temperature"] = json!({ "mirek": mirek }),
        None => {}
    }
    body
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

impl Snapshot {
    /// Photo à rétablir à la fin du flux, et `true` si c'est celle d'une session interrompue :
    /// les lampes sont alors encore dans l'état laissé par le flux, pas dans leur état d'origine.
    pub async fn take(bridge: &BridgeCredentials, area: &EntertainmentArea) -> Result<(Self, bool)> {
        if let Some(leftover) = Self::leftover()? {
            if leftover.age() < LEFTOVER_MAX_AGE {
                return Ok((leftover, true));
            }
            Self::discard()?;
        }
        let ids: Vec<&str> = area
            .light_services
            .iter()
            .filter(|service| service.rtype == "light")
            .map(|service| service.rid.as_str())
            .collect();
        let mut lights = Vec::with_capacity(ids.len());
        for id in &ids {
            lights.push(LightState::from(api::light(bridge, id).await?));
        }
        let scenes = active_scenes(api::scenes(bridge).await?, &ids);
        let snapshot = Self { taken_at: now(), lights, scenes };
        snapshot.save()?;
        Ok((snapshot, false))
    }

    /// Rétablit les lampes, puis les scènes actives, et efface la photo du disque.
    pub async fn restore(&self, bridge: &BridgeCredentials) -> Result<Restored> {
        tokio::time::sleep(RELEASE_DELAY).await;
        for light in &self.lights {
            restore_light(bridge, light).await.with_context(|| format!("lampe « {} »", light.name))?;
        }
        for scene in &self.scenes {
            api::recall_scene(bridge, &scene.id, &scene.action)
                .await
                .with_context(|| format!("scène « {} »", scene.name))?;
        }
        Self::discard()?;
        Ok(Restored { lights: self.lights.len(), scenes: self.scenes.len() })
    }

    /// Photo laissée sur disque par une session qui n'a pas pu rétablir les lampes.
    pub fn leftover() -> Result<Option<Self>> {
        let path = Self::path()?;
        match fs::read_to_string(&path) {
            // Un fichier abîmé ne vaut pas mieux qu'aucun fichier.
            Ok(text) => Ok(serde_json::from_str(&text).ok()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("lecture de {}", path.display())),
        }
    }

    pub fn age(&self) -> Duration {
        Duration::from_secs(now().saturating_sub(self.taken_at))
    }

    /// Efface la photo du disque (rien à rétablir).
    pub fn discard() -> Result<()> {
        let path = Self::path()?;
        match fs::remove_file(&path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e).with_context(|| format!("effacement de {}", path.display())),
            _ => Ok(()),
        }
    }

    fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?).with_context(|| format!("écriture de {}", path.display()))
    }

    fn path() -> Result<PathBuf> {
        Ok(Config::path()?.with_file_name(FILE_NAME))
    }
}

/// Plusieurs essais : juste après le flux, le bridge peut encore refuser la commande.
async fn restore_light(bridge: &BridgeCredentials, state: &LightState) -> Result<()> {
    let body = restore_body(state);
    let mut attempt = 1;
    loop {
        match api::update_light(bridge, &state.id, &body).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= RESTORE_ATTEMPTS => {
                // Une lampe éteinte peut refuser qu'on règle sa couleur : on se contente de l'éteindre.
                if !state.on {
                    return api::update_light(bridge, &state.id, &json!({ "on": { "on": false } })).await;
                }
                return Err(e);
            }
            Err(_) => {
                attempt += 1;
                tokio::time::sleep(RELEASE_DELAY).await;
            }
        }
    }
}

impl fmt::Display for Restored {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lampes rétablies dans leur état d'origine ({} lampe(s)", self.lights)?;
        if self.scenes > 0 {
            write!(f, ", {} scène(s) relancée(s)", self.scenes)?;
        }
        write!(f, ").")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(json: Value) -> LightState {
        LightState::from(serde_json::from_value::<Light>(json).unwrap())
    }

    #[test]
    fn reads_white_color_and_plain_dimmable_lights() {
        let white = light(json!({
            "id": "a", "metadata": { "name": "Lumière" }, "on": { "on": true },
            "dimming": { "brightness": 71.54 },
            "color": { "xy": { "x": 0.5267, "y": 0.4133 } },
            "color_temperature": { "mirek": 500, "mirek_valid": true },
        }));
        assert_eq!(white.color, Some(LightColor::White { mirek: 500 }));
        assert_eq!(white.brightness, Some(71.54));

        let colored = light(json!({
            "id": "b", "metadata": { "name": "Bandeau" }, "on": { "on": true },
            "dimming": { "brightness": 40.0 },
            "color": { "xy": { "x": 0.17, "y": 0.05 } },
            "color_temperature": { "mirek": null, "mirek_valid": false },
        }));
        assert_eq!(colored.color, Some(LightColor::Xy { x: 0.17, y: 0.05 }));

        let plain = light(json!({
            "id": "c", "metadata": { "name": "Applique" }, "on": { "on": false },
            "dimming": { "brightness": 100.0 },
        }));
        assert_eq!((plain.on, plain.color), (false, None));
    }

    #[test]
    fn restores_with_a_fade_what_was_photographed() {
        let state = LightState {
            id: "a".into(),
            name: "Lumière".into(),
            on: true,
            brightness: Some(71.5),
            color: Some(LightColor::White { mirek: 500 }),
        };
        assert_eq!(
            restore_body(&state),
            json!({
                "on": { "on": true },
                "dynamics": { "duration": RESTORE_FADE_MS },
                "dimming": { "brightness": 71.5 },
                "color_temperature": { "mirek": 500 },
            })
        );
        let off = LightState { on: false, color: Some(LightColor::Xy { x: 0.3, y: 0.3 }), ..state };
        let body = restore_body(&off);
        assert_eq!(body["on"], json!({ "on": false }));
        assert_eq!(body["color"], json!({ "xy": { "x": 0.3, "y": 0.3 } }));
    }

    #[test]
    fn keeps_only_active_scenes_of_our_lights() {
        let scene = |id: &str, active: &str, light: &str| -> Scene {
            serde_json::from_value(json!({
                "id": id, "metadata": { "name": id }, "status": { "active": active },
                "actions": [{ "target": { "rid": light, "rtype": "light" } }],
            }))
            .unwrap()
        };
        let scenes = vec![
            scene("aurore", "dynamic_palette", "a"),
            scene("lecture", "inactive", "a"),
            scene("salon", "static", "autre"),
        ];
        let active = active_scenes(scenes, &["a"]);
        assert_eq!(active, [ActiveScene { id: "aurore".into(), name: "aurore".into(), action: "dynamic_palette".into() }]);
    }
}

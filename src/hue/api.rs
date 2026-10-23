//! API REST du bridge : appairage (API v1), zones de divertissement, lampes et scènes (CLIP v2).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::BridgeCredentials;

const DEVICE_TYPE: &str = "dj_bridge#pc";
const APP_KEY_HEADER: &str = "hue-application-key";
/// Erreur renvoyée par le bridge tant que son bouton n'a pas été pressé.
const LINK_BUTTON_NOT_PRESSED: u64 = 101;

#[derive(Debug, Deserialize)]
pub struct DiscoveredBridge {
    pub id: String,
    #[serde(rename = "internalipaddress")]
    pub ip: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntertainmentArea {
    pub id: String,
    pub metadata: Metadata,
    /// `inactive`, ou `active` si une application diffuse déjà dessus.
    pub status: String,
    pub channels: Vec<Channel>,
    /// Lampes de la zone.
    #[serde(default)]
    pub light_services: Vec<ResourceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub channel_id: u8,
    /// Services de divertissement (segments de lampe) éclairés par ce canal.
    #[serde(default)]
    pub members: Vec<ChannelMember>,
    /// Place dans la pièce, réglée dans l'app Hue (seul x sert : de -1 à gauche à 1 à droite).
    #[serde(default)]
    pub position: Position,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Position {
    pub x: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelMember {
    pub service: ResourceRef,
}

/// Service de divertissement : le côté « flux » d'une lampe.
#[derive(Debug, Clone, Deserialize)]
struct Entertainment {
    id: String,
    /// Lampe qui rend ce service.
    renderer_reference: Option<ResourceRef>,
}

/// Lampes à moins de 0,1 l'une de l'autre : jamais placées ; on les répartit de -0,8 à 0,8.
const SAME_PLACE: f32 = 0.1;
const SPREAD_PLACES: f32 = 0.8;

/// Lampe d'une zone de divertissement et canaux du flux qui l'éclairent (plusieurs pour un
/// ruban ou une lampe à dégradé).
#[derive(Debug, Clone)]
pub struct Lamp {
    pub id: String,
    pub name: String,
    pub channels: Vec<u8>,
    /// Place dans la pièce selon la zone Hue (moyenne de ses canaux, de -1 à gauche à 1 à droite).
    pub x: f32,
}

/// Référence à une autre ressource du bridge.
#[derive(Debug, Clone, Deserialize)]
pub struct ResourceRef {
    pub rid: String,
    pub rtype: String,
}

/// État d'une lampe, tel que le décrit le bridge.
#[derive(Debug, Clone, Deserialize)]
pub struct Light {
    pub id: String,
    pub metadata: Metadata,
    pub on: On,
    pub dimming: Option<Dimming>,
    pub color: Option<Color>,
    pub color_temperature: Option<ColorTemperature>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct On {
    pub on: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dimming {
    pub brightness: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Color {
    pub xy: Xy,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Xy {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColorTemperature {
    pub mirek: Option<u16>,
    /// Faux quand la lampe affiche une couleur plutôt qu'un blanc.
    pub mirek_valid: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scene {
    pub id: String,
    pub metadata: Metadata,
    pub status: Option<SceneStatus>,
    #[serde(default)]
    pub actions: Vec<SceneAction>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SceneStatus {
    /// `inactive`, `static` ou `dynamic_palette`.
    pub active: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SceneAction {
    pub target: ResourceRef,
}

#[derive(Deserialize)]
struct ClipResponse<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
    #[serde(default)]
    errors: Vec<ClipError>,
}

#[derive(Deserialize)]
struct ClipError {
    description: String,
}

/// Le bridge présente un certificat signé par l'autorité privée de Signify, inconnue de Windows.
// TODO: épingler le certificat racine Hue au lieu de désactiver la vérification.
fn bridge_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .timeout(Duration::from_secs(5))
        .build()
        .context("création du client HTTP")
}

/// Découverte via le service de Signify, auprès duquel chaque bridge déclare son IP locale.
pub async fn discover() -> Result<Vec<DiscoveredBridge>> {
    let response = reqwest::Client::new()
        .get("https://discovery.meethue.com/")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .context("service de découverte injoignable")?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("service de découverte saturé : réessaie dans quelques minutes ou passe l'IP avec --ip");
    }
    Ok(response.error_for_status()?.json().await?)
}

/// Attend l'appui sur le bouton du bridge, puis récupère la clé d'application
/// et la clé de chiffrement du flux.
pub async fn pair(ip: &str, wait: Duration) -> Result<BridgeCredentials> {
    let client = bridge_client()?;
    let url = format!("https://{ip}/api");
    let body = json!({ "devicetype": DEVICE_TYPE, "generateclientkey": true });
    let deadline = Instant::now() + wait;
    loop {
        let reply: Value = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("bridge injoignable à {ip}"))?
            .json()
            .await?;
        let entry = reply
            .get(0)
            .with_context(|| format!("réponse inattendue : {reply}"))?;
        if let Some(success) = entry.get("success") {
            let field = |name: &str| {
                success
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .with_context(|| format!("champ « {name} » absent de la réponse"))
            };
            return Ok(BridgeCredentials {
                ip: ip.to_owned(),
                app_key: field("username")?,
                client_key: field("clientkey")?,
            });
        }
        if entry.pointer("/error/type").and_then(Value::as_u64) != Some(LINK_BUTTON_NOT_PRESSED) {
            bail!("appairage refusé : {entry}");
        }
        if Instant::now() >= deadline {
            bail!("bouton du bridge non pressé dans les {} s", wait.as_secs());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn entertainment_areas(bridge: &BridgeCredentials) -> Result<Vec<EntertainmentArea>> {
    clip_get(bridge, "entertainment_configuration").await
}

pub async fn light(bridge: &BridgeCredentials, id: &str) -> Result<Light> {
    clip_get(bridge, &format!("light/{id}"))
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("lampe {id} introuvable"))
}

/// Lampes de la zone, dans l'ordre de leur premier canal, avec leur nom.
pub async fn area_lamps(bridge: &BridgeCredentials, area: &EntertainmentArea) -> Result<Vec<Lamp>> {
    let services: Vec<Entertainment> = clip_get(bridge, "entertainment").await?;
    let lights: Vec<Light> = clip_get(bridge, "light").await?;
    let mut lamps: Vec<Lamp> = Vec::new();
    for channel in &area.channels {
        let light_ids = channel.members.iter().filter_map(|member| {
            let service = services.iter().find(|s| s.id == member.service.rid)?;
            service.renderer_reference.as_ref().filter(|r| r.rtype == "light").map(|r| r.rid.as_str())
        });
        for id in light_ids {
            match lamps.iter_mut().find(|lamp| lamp.id == id) {
                Some(lamp) if !lamp.channels.contains(&channel.channel_id) => lamp.channels.push(channel.channel_id),
                Some(_) => {}
                None => lamps.push(Lamp {
                    id: id.to_owned(),
                    name: lights.iter().find(|l| l.id == id).map_or_else(|| id.to_owned(), |l| l.metadata.name.clone()),
                    channels: vec![channel.channel_id],
                    x: 0.0,
                }),
            }
        }
    }
    for lamp in &mut lamps {
        let places: Vec<f32> = area.channels.iter().filter(|c| lamp.channels.contains(&c.channel_id)).map(|c| c.position.x).collect();
        lamp.x = places.iter().sum::<f32>() / places.len().max(1) as f32;
    }
    spread_unplaced(&mut lamps);
    Ok(lamps)
}

/// Lampes jamais placées dans l'app Hue (toutes au même endroit) : réparties régulièrement de
/// gauche à droite, dans l'ordre de la zone.
fn spread_unplaced(lamps: &mut [Lamp]) {
    let (low, high) = lamps.iter().fold((f32::MAX, f32::MIN), |(lo, hi), lamp| (lo.min(lamp.x), hi.max(lamp.x)));
    if lamps.len() > 1 && high - low < SAME_PLACE {
        let last = (lamps.len() - 1) as f32;
        for (i, lamp) in lamps.iter_mut().enumerate() {
            lamp.x = -SPREAD_PLACES + 2.0 * SPREAD_PLACES * i as f32 / last;
        }
    }
}

/// Canaux à piloter : ceux des lampes qui ne sont pas écartées. Un canal partagé avec une lampe
/// gardée reste piloté.
pub fn piloted_channels(area: &EntertainmentArea, lamps: &[Lamp], excluded: &[String]) -> Vec<u8> {
    area.channels
        .iter()
        .map(|c| c.channel_id)
        .filter(|id| {
            let owners: Vec<&Lamp> = lamps.iter().filter(|lamp| lamp.channels.contains(id)).collect();
            owners.is_empty() || owners.iter().any(|lamp| !excluded.contains(&lamp.id))
        })
        .collect()
}

pub async fn scenes(bridge: &BridgeCredentials) -> Result<Vec<Scene>> {
    clip_get(bridge, "scene").await
}

/// Modifie une lampe (marche/arrêt, luminosité, couleur…) avec le corps CLIP v2 donné.
pub async fn update_light(bridge: &BridgeCredentials, id: &str, body: &Value) -> Result<()> {
    clip_put(bridge, &format!("light/{id}"), body).await
}

/// Rappelle une scène ; `action` vaut `static` ou `dynamic_palette` (scène animée).
pub async fn recall_scene(bridge: &BridgeCredentials, id: &str, action: &str) -> Result<()> {
    clip_put(bridge, &format!("scene/{id}"), &json!({ "recall": { "action": action } })).await
}

/// Zone demandée, ou la seule zone existante si aucune n'a été choisie.
pub async fn select_area(bridge: &BridgeCredentials, wanted: Option<&str>) -> Result<EntertainmentArea> {
    let mut areas = entertainment_areas(bridge).await?;
    if let Some(id) = wanted {
        return areas
            .into_iter()
            .find(|area| area.id == id)
            .with_context(|| format!("zone {id} introuvable : lance `dj-bridge areas`"));
    }
    match areas.len() {
        0 => bail!("aucune zone de divertissement : crée-en une dans l'app Hue (Paramètres > Zones de divertissement)"),
        1 => Ok(areas.remove(0)),
        _ => bail!("plusieurs zones de divertissement : choisis-en une avec `dj-bridge areas --select <id>`"),
    }
}

/// Démarre ou arrête le mode streaming de la zone (à faire avant d'ouvrir le flux DTLS).
pub async fn set_streaming(bridge: &BridgeCredentials, area_id: &str, active: bool) -> Result<()> {
    let action = if active { "start" } else { "stop" };
    clip_put(bridge, &format!("entertainment_configuration/{area_id}"), &json!({ "action": action }))
        .await
        .with_context(|| format!("action « {action} » refusée sur la zone {area_id}"))
}

fn resource_url(bridge: &BridgeCredentials, path: &str) -> String {
    format!("https://{}/clip/v2/resource/{path}", bridge.ip)
}

async fn clip_get<T: DeserializeOwned>(bridge: &BridgeCredentials, path: &str) -> Result<Vec<T>> {
    let response = bridge_client()?
        .get(resource_url(bridge, path))
        .header(APP_KEY_HEADER, &bridge.app_key)
        .send()
        .await
        .with_context(|| format!("bridge injoignable à {}", bridge.ip))?;
    clip_data(response).await
}

async fn clip_put(bridge: &BridgeCredentials, path: &str, body: &Value) -> Result<()> {
    let response = bridge_client()?
        .put(resource_url(bridge, path))
        .header(APP_KEY_HEADER, &bridge.app_key)
        .json(body)
        .send()
        .await
        .with_context(|| format!("bridge injoignable à {}", bridge.ip))?;
    clip_data::<Value>(response).await?;
    Ok(())
}

async fn clip_data<T: DeserializeOwned>(response: reqwest::Response) -> Result<Vec<T>> {
    let status = response.status();
    let text = response.text().await?;
    let parsed: ClipResponse<T> = serde_json::from_str(&text)
        .with_context(|| format!("réponse illisible ({status}) : {text}"))?;
    if !status.is_success() || !parsed.errors.is_empty() {
        let details: Vec<&str> = parsed.errors.iter().map(|e| e.description.as_str()).collect();
        bail!("le bridge répond {status} : {}", details.join(" ; "));
    }
    Ok(parsed.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(channels: &[u8]) -> EntertainmentArea {
        EntertainmentArea {
            id: "zone".into(),
            metadata: Metadata { name: "Zone".into() },
            status: "inactive".into(),
            channels: channels
                .iter()
                .map(|&channel_id| Channel { channel_id, members: Vec::new(), position: Position::default() })
                .collect(),
            light_services: Vec::new(),
        }
    }

    fn lamp(id: &str, channels: &[u8]) -> Lamp {
        Lamp { id: id.into(), name: id.into(), channels: channels.to_vec(), x: 0.0 }
    }

    #[test]
    fn excluded_lamps_leave_the_stream() {
        let area = area(&[0, 1, 2, 3]);
        let lamps = [lamp("salon", &[0]), lamp("ruban", &[1, 2]), lamp("bureau", &[3])];
        assert_eq!(piloted_channels(&area, &lamps, &[]), [0, 1, 2, 3]);
        assert_eq!(piloted_channels(&area, &lamps, &["ruban".into()]), [0, 3]);
        assert!(piloted_channels(&area, &lamps, &["salon".into(), "ruban".into(), "bureau".into()]).is_empty());
    }

    #[test]
    fn lamps_never_placed_are_spread_from_left_to_right() {
        let mut lamps = [lamp("a", &[0]), lamp("b", &[1]), lamp("c", &[2])];
        spread_unplaced(&mut lamps);
        assert_eq!(lamps.map(|l| l.x), [-0.8, 0.0, 0.8]);
        let mut placed = [lamp("a", &[0]), lamp("b", &[1])];
        placed[1].x = 0.5;
        spread_unplaced(&mut placed);
        assert_eq!(placed.map(|l| l.x), [0.0, 0.5], "déjà placées : on n'y touche pas");
    }

    #[test]
    fn unknown_or_shared_channels_stay_piloted() {
        let area = area(&[0, 1, 2]);
        let lamps = [lamp("a", &[0, 1]), lamp("b", &[1])];
        // Le canal 2 n'appartient à aucune lampe connue ; le canal 1 est partagé avec « b ».
        assert_eq!(piloted_channels(&area, &lamps, &["a".into()]), [1, 2]);
    }
}

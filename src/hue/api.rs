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

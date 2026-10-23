//! Réglages persistants, stockés dans le profil utilisateur (hors du dépôt)
//! car ils contiennent les clés d'accès au bridge.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::Backend;
use crate::audio::analysis::DEFAULT_THRESHOLD;
use crate::flash::FlashMode;
use crate::look::{Dynamics, Motion, Palette};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub bridge: Option<BridgeCredentials>,
    /// Zone de divertissement utilisée pour le flux.
    pub area_id: Option<String>,
    /// Avance donnée aux flashs pour compenser la latence de la chaîne Hue (ms).
    #[serde(default)]
    pub light_advance_ms: i64,
    /// Couleurs du mode live.
    #[serde(default)]
    pub palette: Palette,
    /// Allure des flashs du mode live.
    #[serde(default)]
    pub dynamics: Dynamics,
    /// Derniers réglages choisis dans la fenêtre.
    #[serde(default)]
    pub live: LiveSettings,
    /// Lampes de la zone laissées hors du mode live (identifiants CLIP v2) : elles gardent leur
    /// état pendant le flux. Une lampe ajoutée à la zone est donc pilotée par défaut.
    #[serde(default)]
    pub excluded_lights: Vec<String>,
}

/// Réglages du mode live choisis dans la fenêtre avant de lancer, repris à l'ouverture suivante.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveSettings {
    /// Entrée audio ; `None` : la Focusrite si elle est branchée.
    pub input: Option<String>,
    pub audio: Backend,
    /// Tempo imposé au lancement ; `None` : détection automatique.
    pub bpm: Option<f64>,
    pub link: bool,
    /// Port MIDI écouté ; `None` : pas de MIDI.
    pub midi: Option<String>,
    pub flash: FlashMode,
    pub motion: Motion,
    pub threshold: f32,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            input: None,
            audio: Backend::Auto,
            bpm: None,
            link: false,
            midi: None,
            flash: FlashMode::Kicks,
            motion: Motion::Auto,
            threshold: DEFAULT_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeCredentials {
    pub ip: String,
    /// Clé d'application (`hue-application-key`), aussi identité PSK du flux DTLS.
    pub app_key: String,
    /// Secret PSK du flux DTLS, en hexadécimal.
    pub client_key: String,
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let dir = dirs::config_dir().context("dossier de configuration introuvable")?;
        Ok(dir.join("dj-bridge").join("config.json"))
    }

    /// Dossier des captures audio (touche `r`, bouton Capturer, captures automatiques) et du
    /// banc d'essai.
    pub fn captures_dir() -> Result<PathBuf> {
        Ok(Self::path()?.parent().context("dossier de configuration introuvable")?.join("captures"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("configuration illisible : {}", path.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("lecture de {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("écriture de {}", path.display()))
    }

    pub fn bridge(&self) -> Result<&BridgeCredentials> {
        self.bridge
            .as_ref()
            .context("bridge non associé : lance d'abord `dj-bridge pair`")
    }
}

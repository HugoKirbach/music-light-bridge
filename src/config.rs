//! Réglages persistants, stockés dans le profil utilisateur (hors du dépôt)
//! car ils contiennent les clés d'accès au bridge.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::look::Palette;

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

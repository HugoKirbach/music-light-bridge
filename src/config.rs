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
use crate::look::{Dynamics, Motion, Palette, Spread};

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
    /// Place de chaque lampe dans la pièce choisie dans la fenêtre (identifiant CLIP v2 → x de -1 à
    /// gauche à 1 à droite) ; à défaut, celle de la zone Hue.
    #[serde(default)]
    pub lamp_positions: std::collections::BTreeMap<String, f32>,
    /// Retard propre de chaque lampe (ms) : une lampe plus lente reçoit ses flashs plus tôt.
    #[serde(default)]
    pub lamp_delays_ms: std::collections::BTreeMap<String, f32>,
    /// Scènes enregistrées : un clic (ou une note MIDI) rappelle couleurs, flashs et animation.
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Jeton secret de l'adresse de la télécommande mobile, créé à sa première ouverture.
    #[serde(default)]
    pub remote_token: Option<String>,
}

/// Scène enregistrée : tout ce qui fait l'allure du live, sauf la source et le tempo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub palette: Palette,
    pub dynamics: Dynamics,
    pub motion: Motion,
    pub spread: Spread,
    pub flash: FlashMode,
    #[serde(default = "enabled")]
    pub director: bool,
    /// Note MIDI qui rappelle la scène (au lieu de flasher).
    #[serde(default)]
    pub note: Option<u8>,
}

fn enabled() -> bool {
    true
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
    pub spread: Spread,
    /// Régisseur : la disposition Auto et la teinte varient d'une section à l'autre.
    pub director: bool,
    /// Ouvre la télécommande mobile pendant le live.
    pub remote: bool,
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
            spread: Spread::Auto,
            director: true,
            remote: false,
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
    /// Jeton de la télécommande mobile, créé (et enregistré) la première fois : l'adresse reste
    /// la même d'un live à l'autre, le téléphone peut la garder en favori.
    pub fn remote_token(&mut self) -> Result<String> {
        if let Some(token) = &self.remote_token {
            return Ok(token.clone());
        }
        let token = crate::engine::new_token();
        self.remote_token = Some(token.clone());
        self.save()?;
        Ok(token)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_without_director_or_note_keeps_the_director_and_no_note() {
        let json = r#"{
            "name": "Drop rouge",
            "palette": { "primary": [255, 0, 0], "calm": [0, 0, 255], "filtered": [0, 255, 0] },
            "dynamics": {},
            "motion": "auto",
            "spread": "together",
            "flash": "kicks"
        }"#;
        let preset: Preset = serde_json::from_str(json).expect("scène lisible");
        assert!(preset.director);
        assert_eq!(preset.note, None);
        assert_eq!(preset.spread, Spread::Together);
        let again: Preset = serde_json::from_str(&serde_json::to_string(&preset).unwrap()).unwrap();
        assert_eq!(again, preset);
    }

    #[test]
    fn an_old_config_has_no_presets() {
        let cfg: Config = serde_json::from_str(r#"{ "bridge": null, "area_id": null }"#).expect("ancienne config");
        assert!(cfg.presets.is_empty());
        assert!(cfg.live.director);
    }
}

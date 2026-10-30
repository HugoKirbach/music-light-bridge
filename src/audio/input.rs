//! Capture en ASIO (latence minimale) ou en WASAPI (pilote standard de Windows). Le signal est
//! mixé en mono et analysé directement dans le fil audio : aucune copie ni file d'attente
//! entre capture et détection.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    ErrorKind, FromSample, HostId, I24, InputCallbackInfo, SampleFormat, SizedSample, Stream, StreamConfig,
};

use super::Features;
use super::analysis::Analyzer;
use super::capture::Capture;

/// Entrée WASAPI choisie quand aucune n'est demandée, si elle est branchée.
const PREFERRED_INPUT: &str = "focusrite";
/// Pilote ASIO choisi quand aucun n'est demandé.
const PREFERRED_ASIO: &str = "focusrite usb";
/// Durée gardée en mémoire pour la capture (touche `r` du mode live).
pub const CAPTURE_SECONDS: u32 = 40;
/// Capture de la sortie du PC par Windows (WASAPI loopback) : un logiciel en ASIO n'y passe pas.
pub const WINDOWS_LOOPBACK: &str = "sortie du PC";
/// Capture de la sortie du PC par l'entrée Loopback de la carte son : ASIO compris.
pub const CARD_LOOPBACK: &str = "Loopback de la carte";

/// Pilote audio utilisé pour la capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// ASIO si le pilote est libre, sinon WASAPI
    Auto,
    /// ASIO : latence minimale, mais souvent un seul logiciel à la fois sur la carte
    Asio,
    /// WASAPI : pilote standard de Windows, partageable avec d'autres logiciels
    Wasapi,
    /// Sortie du PC (haut-parleurs, carte son) : capte ce qu'elle joue, par l'entrée Loopback de
    /// la carte si elle en a une (ASIO compris), sinon en WASAPI loopback
    Output,
}

/// Entrée Loopback de la carte son qui porte la sortie `output` (Scarlett 4e génération,
/// Clarett+…), parmi les entrées `inputs` : elle entend tout ce que joue la carte, y compris un
/// logiciel en ASIO comme Ableton, que le loopback de Windows ne voit pas.
pub fn card_loopback<'a>(output: &str, inputs: &'a [String]) -> Option<&'a String> {
    let card = card_name(output)?;
    inputs.iter().find(|input| input.to_lowercase().contains("loopback") && card_name(input).as_deref() == Some(card.as_str()))
}

/// Windows nomme ses appareils « Point d'accès (Carte) » : la carte est entre les dernières
/// parenthèses, qui peuvent en contenir d'autres (« Realtek(R) Audio »).
fn card_name(device: &str) -> Option<String> {
    let inner = device.trim_end().strip_suffix(')')?;
    let mut depth = 0;
    for (i, c) in inner.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' if depth > 0 => depth -= 1,
            '(' => {
                let card = inner[i + 1..].trim().to_lowercase();
                return (!card.is_empty()).then_some(card);
            }
            _ => {}
        }
    }
    None
}

/// Sorties audio du PC, captables en mode `Output` ; la première est la sortie par défaut.
pub fn list_outputs() -> Result<Vec<String>> {
    let host = cpal::host_from_id(HostId::Wasapi)?;
    let default = host.default_output_device().map(|d| d.to_string());
    let mut outputs: Vec<String> = host.output_devices()?.map(|d| d.to_string()).collect();
    if let Some(i) = default.and_then(|name| outputs.iter().position(|o| *o == name)) {
        outputs[..=i].rotate_right(1);
    }
    Ok(outputs)
}

pub struct InputInfo {
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub is_default: bool,
    /// Entrée qu'utilisera `live` sans option `--input`.
    pub is_preferred: bool,
}

pub fn list_inputs() -> Result<Vec<InputInfo>> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|d| d.to_string());
    let mut inputs: Vec<InputInfo> = Vec::new();
    for device in host.input_devices()? {
        let Ok(config) = device.default_input_config() else { continue };
        let name = device.to_string();
        inputs.push(InputInfo {
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            is_preferred: false,
        });
    }
    let preferred = inputs
        .iter()
        .position(|input| input.name.to_lowercase().contains(PREFERRED_INPUT))
        .or_else(|| inputs.iter().position(|input| input.is_default));
    if let Some(i) = preferred {
        inputs[i].is_preferred = true;
    }
    Ok(inputs)
}

pub struct AudioInput {
    _stream: Stream,
    pub device: String,
    /// « ASIO », « WASAPI » ou « sortie du PC ».
    pub backend: &'static str,
    /// Pourquoi la capture préférée n'a pas pu être utilisée (l'ASIO en mode automatique,
    /// l'entrée Loopback de la carte pour la sortie du PC), et ce qui la remplace.
    pub fallback: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Dernières secondes reçues, en mono.
    pub capture: Arc<Mutex<Capture>>,
}

impl AudioInput {
    /// `wanted` : morceau du nom de l'entrée (ou du pilote ASIO) ; à défaut, la Focusrite,
    /// et en WASAPI l'entrée par défaut de Windows si elle n'est pas branchée.
    /// En mode `Output`, `wanted` est un morceau du nom de la sortie ; à défaut, la sortie par
    /// défaut de Windows. Sa carte est écoutée par son entrée Loopback quand elle en a une.
    pub fn start(wanted: Option<&str>, backend: Backend, features: Arc<Features>) -> Result<Self> {
        Self::start_into(wanted, backend, features, Arc::new(Mutex::new(Capture::new(1, 0))))
    }

    /// Comme `start`, en enregistrant les dernières secondes dans `capture` : une entrée rouverte
    /// garde ainsi la même mémoire que ceux qui la lisent.
    pub fn start_into(wanted: Option<&str>, backend: Backend, features: Arc<Features>, capture: Arc<Mutex<Capture>>) -> Result<Self> {
        match backend {
            Backend::Asio => Self::open(HostId::Asio, wanted, features, capture),
            Backend::Wasapi => Self::open(HostId::Wasapi, wanted, features, capture),
            Backend::Output => Self::open_output(wanted, features, capture),
            Backend::Auto => match Self::open(HostId::Asio, wanted, features.clone(), capture.clone()) {
                Ok(input) => Ok(input),
                Err(asio) => {
                    let mut input = Self::open(HostId::Wasapi, wanted, features, capture)?;
                    input.fallback = Some(format!("ASIO indisponible ({asio:#}) : capture en WASAPI"));
                    Ok(input)
                }
            },
        }
    }

    fn open(host_id: HostId, wanted: Option<&str>, features: Arc<Features>, capture: Arc<Mutex<Capture>>) -> Result<Self> {
        let host = cpal::host_from_id(host_id)?;
        let device = pick_device(&host, host_id, wanted)?;
        let supported = device.default_input_config()?;
        let backend = if host_id == HostId::Asio { "ASIO" } else { "WASAPI" };
        Self::capture(device, supported, backend, features, capture)
    }

    /// Capte ce que joue une sortie : par l'entrée Loopback de sa carte si elle en a une, qui
    /// entend aussi les logiciels en ASIO ; sinon en ouvrant la sortie comme une entrée, et
    /// WASAPI capte ce que Windows y joue (loopback).
    fn open_output(wanted: Option<&str>, features: Arc<Features>, capture: Arc<Mutex<Capture>>) -> Result<Self> {
        let host = cpal::host_from_id(HostId::Wasapi)?;
        let device = match wanted {
            Some(name) => {
                let needle = name.to_lowercase();
                host.output_devices()?
                    .find(|d| d.to_string().to_lowercase().contains(&needle))
                    .with_context(|| format!("aucune sortie audio ne contient « {name} » : lance `dj-bridge devices`"))?
            }
            None => host.default_output_device().context("aucune sortie audio disponible")?,
        };
        let inputs: Vec<cpal::Device> = host.input_devices()?.collect();
        let names: Vec<String> = inputs.iter().map(ToString::to_string).collect();
        let mut fallback = None;
        if let Some(name) = card_loopback(&device.to_string(), &names) {
            let input = inputs.into_iter().find(|input| input.to_string() == *name).context("entrée Loopback disparue")?;
            let opened = input
                .default_input_config()
                .map_err(anyhow::Error::from)
                .and_then(|supported| Self::capture(input, supported, CARD_LOOPBACK, features.clone(), capture.clone()));
            match opened {
                Ok(audio) => return Ok(audio),
                Err(e) => fallback = Some(format!("Entrée « {name} » indisponible ({e:#}) : capture par Windows")),
            }
        }
        let supported = device.default_output_config()?;
        let mut audio = Self::capture(device, supported, WINDOWS_LOOPBACK, features, capture)?;
        audio.fallback = fallback;
        Ok(audio)
    }

    fn capture(
        device: cpal::Device,
        supported: cpal::SupportedStreamConfig,
        backend: &'static str,
        features: Arc<Features>,
        capture: Arc<Mutex<Capture>>,
    ) -> Result<Self> {
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let (sample_rate, channels) = (config.sample_rate, config.channels);
        *capture.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Capture::new(sample_rate, CAPTURE_SECONDS);
        let recorder = capture.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, features, recorder)?,
            SampleFormat::I16 => build::<i16>(&device, config, features, recorder)?,
            SampleFormat::I24 => build::<I24>(&device, config, features, recorder)?,
            SampleFormat::I32 => build::<i32>(&device, config, features, recorder)?,
            other => bail!("format audio non géré : {other}"),
        };
        stream.play()?;
        Ok(Self { _stream: stream, device: device.to_string(), backend, fallback: None, sample_rate, channels, capture })
    }
}

/// En ASIO, énumérer charge chaque pilote tour à tour : la recherche s'arrête au premier qui
/// convient, sans aller jusqu'aux pilotes du matériel débranché (Pioneer, etc.).
fn pick_device(host: &cpal::Host, host_id: HostId, wanted: Option<&str>) -> Result<cpal::Device> {
    let find = |needle: &str| -> Result<Option<cpal::Device>> {
        let needle = needle.to_lowercase();
        Ok(host.input_devices()?.find(|d| d.to_string().to_lowercase().contains(&needle)))
    };
    match (wanted, host_id) {
        (Some(name), _) => find(name)?.with_context(|| {
            format!("aucune entrée audio ne contient « {name} » : lance `dj-bridge devices`")
        }),
        (None, HostId::Asio) => {
            find(PREFERRED_ASIO)?.context("pilote ASIO « Focusrite USB » introuvable ou déjà utilisé")
        }
        (None, _) => match find(PREFERRED_INPUT)? {
            Some(device) => Ok(device),
            None => host.default_input_device().context("aucune entrée audio disponible"),
        },
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    features: Arc<Features>,
    capture: Arc<Mutex<Capture>>,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let sample_period = Duration::from_secs_f64(1.0 / f64::from(config.sample_rate));
    let step = sample_period.as_secs_f64();
    let mix_gain = 1.0 / channels as f32;
    let mut analyzer = Analyzer::new(config.sample_rate as f32, features.threshold());
    let monitor = features.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], info: &InputCallbackInfo| {
            let now = Instant::now();
            let timestamp = info.timestamp();
            let age = timestamp.callback.duration_since(timestamp.capture);
            features.set_input_latency(age);
            analyzer.set_threshold(features.threshold());
            let grid = features.grid();
            // Instant où le premier échantillon du bloc a été capté.
            let first = now.checked_sub(age).unwrap_or(now);
            let first_seconds = features.seconds(first);
            // Sans attente : pendant une sauvegarde, ce bloc n'est simplement pas enregistré.
            let mut recording = capture.try_lock().ok();
            for (i, frame) in data.chunks(channels).enumerate() {
                let mono = frame.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() * mix_gain;
                if let Some(recording) = recording.as_mut() {
                    recording.push(mono);
                }
                let t = first_seconds + step * i as f64;
                if analyzer.process(mono, grid.expectation(t)) {
                    features.publish_kick(first + sample_period * i as u32);
                }
                if let Some(strength) = analyzer.take_onset() {
                    features.publish_onset(t, strength);
                }
            }
            features.publish_levels(analyzer.levels());
        },
        // Pas d'affichage depuis le fil audio : il casserait la ligne d'état et peut bloquer.
        move |err: cpal::Error| {
            if err.kind() == ErrorKind::Xrun {
                monitor.record_glitch();
            } else {
                monitor.record_input_lost();
            }
        },
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_is_heard_through_its_own_loopback_input() {
        let inputs = [
            "Analogue 1 + 2 (Focusrite USB Audio)".to_owned(),
            "Loopback (Focusrite USB Audio)".to_owned(),
            "Microphone (Realtek(R) Audio)".to_owned(),
        ];
        let loopback = card_loopback("Haut-parleurs (Focusrite USB Audio)", &inputs);
        assert_eq!(loopback.map(String::as_str), Some("Loopback (Focusrite USB Audio)"));
        assert_eq!(card_loopback("Haut-parleurs (Realtek(R) Audio)", &inputs), None, "pas de Loopback sur la carte de la carte mère");
        let two = ["Loopback 1 + 2 (2- Focusrite USB Audio)".to_owned()];
        assert_eq!(card_loopback("Speakers (Focusrite USB Audio)", &two), None, "une autre Focusrite branchée");
        assert!(card_loopback("Speakers (2- Focusrite USB Audio)", &two).is_some());
        assert_eq!(card_loopback("Sortie sans carte", &inputs), None);
        assert_eq!(card_name("Haut-parleurs (Realtek(R) Audio)").as_deref(), Some("realtek(r) audio"));
    }
}

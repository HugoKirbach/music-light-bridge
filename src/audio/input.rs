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
const CAPTURE_SECONDS: u32 = 40;

/// Pilote audio utilisé pour la capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// ASIO si le pilote est libre, sinon WASAPI
    Auto,
    /// ASIO : latence minimale, mais souvent un seul logiciel à la fois sur la carte
    Asio,
    /// WASAPI : pilote standard de Windows, partageable avec d'autres logiciels
    Wasapi,
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
    /// « ASIO » ou « WASAPI ».
    pub backend: &'static str,
    /// En mode automatique, la raison pour laquelle l'ASIO n'a pas pu être utilisé.
    pub fallback: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Dernières secondes reçues, en mono.
    pub capture: Arc<Mutex<Capture>>,
}

impl AudioInput {
    /// `wanted` : morceau du nom de l'entrée (ou du pilote ASIO) ; à défaut, la Focusrite,
    /// et en WASAPI l'entrée par défaut de Windows si elle n'est pas branchée.
    pub fn start(wanted: Option<&str>, backend: Backend, features: Arc<Features>) -> Result<Self> {
        match backend {
            Backend::Asio => Self::open(HostId::Asio, wanted, features),
            Backend::Wasapi => Self::open(HostId::Wasapi, wanted, features),
            Backend::Auto => match Self::open(HostId::Asio, wanted, features.clone()) {
                Ok(input) => Ok(input),
                Err(asio) => {
                    let mut input = Self::open(HostId::Wasapi, wanted, features)?;
                    input.fallback = Some(format!("{asio:#}"));
                    Ok(input)
                }
            },
        }
    }

    fn open(host_id: HostId, wanted: Option<&str>, features: Arc<Features>) -> Result<Self> {
        let host = cpal::host_from_id(host_id)?;
        let device = pick_device(&host, host_id, wanted)?;
        let supported = device.default_input_config()?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let (sample_rate, channels) = (config.sample_rate, config.channels);
        let capture = Arc::new(Mutex::new(Capture::new(sample_rate, CAPTURE_SECONDS)));
        let recorder = capture.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, features, recorder)?,
            SampleFormat::I16 => build::<i16>(&device, config, features, recorder)?,
            SampleFormat::I24 => build::<I24>(&device, config, features, recorder)?,
            SampleFormat::I32 => build::<i32>(&device, config, features, recorder)?,
            other => bail!("format audio non géré : {other}"),
        };
        stream.play()?;
        let backend = if host_id == HostId::Asio { "ASIO" } else { "WASAPI" };
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
                let expectation = grid.expectation(first_seconds + step * i as f64);
                if analyzer.process(mono, expectation) {
                    features.publish_kick(first + sample_period * i as u32);
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

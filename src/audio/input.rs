//! Capture via le pilote audio de Windows (WASAPI). Le signal est mixé en mono et analysé
//! directement dans le fil audio : aucune copie ni file d'attente entre capture et détection.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, InputCallbackInfo, SampleFormat, SizedSample, Stream, StreamConfig};

use super::Features;
use super::analysis::Analyzer;
use super::capture::Capture;

/// Entrée choisie quand aucune n'est demandée, si elle est branchée.
const PREFERRED_INPUT: &str = "focusrite";
/// Durée gardée en mémoire pour la capture (touche `r` du mode live).
const CAPTURE_SECONDS: u32 = 40;

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
    pub sample_rate: u32,
    pub channels: u16,
    /// Dernières secondes reçues, en mono.
    pub capture: Arc<Mutex<Capture>>,
}

impl AudioInput {
    /// `wanted` : morceau du nom de l'entrée ; à défaut, la Focusrite si elle est branchée,
    /// sinon l'entrée par défaut de Windows.
    pub fn start(wanted: Option<&str>, features: Arc<Features>) -> Result<Self> {
        let device = pick_device(wanted)?;
        let supported = device.default_input_config()?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let (sample_rate, channels) = (config.sample_rate, config.channels);
        let capture = Arc::new(Mutex::new(Capture::new(sample_rate, CAPTURE_SECONDS)));
        let recorder = capture.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, features, recorder)?,
            SampleFormat::I16 => build::<i16>(&device, config, features, recorder)?,
            SampleFormat::I32 => build::<i32>(&device, config, features, recorder)?,
            other => bail!("format audio non géré : {other}"),
        };
        stream.play()?;
        Ok(Self { _stream: stream, device: device.to_string(), sample_rate, channels, capture })
    }
}

fn pick_device(wanted: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let find = |needle: &str| -> Result<Option<cpal::Device>> {
        let needle = needle.to_lowercase();
        Ok(host.input_devices()?.find(|d| d.to_string().to_lowercase().contains(&needle)))
    };
    match wanted {
        Some(name) => find(name)?.with_context(|| {
            format!("aucune entrée audio ne contient « {name} » : lance `dj-bridge devices`")
        }),
        None => match find(PREFERRED_INPUT)? {
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

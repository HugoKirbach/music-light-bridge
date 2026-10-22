//! Clic audio calé sur une horloge partagée : le n-ième clic sort des enceintes
//! à `origin + n × period`, latence de sortie annoncée par le pilote comprise.

use std::f64::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, OutputCallbackInfo, SampleFormat, SizedSample, Stream, StreamConfig};

const CLICK_LEN: f64 = 0.03;
const CLICK_FREQ: f64 = 2000.0;
const CLICK_DECAY: f64 = 0.004;
const CLICK_GAIN: f64 = 0.8;

/// Latence de sortie annoncée par le pilote, mise à jour à chaque bloc audio.
#[derive(Clone, Default)]
pub struct OutputLatency(Arc<AtomicU64>);

impl OutputLatency {
    pub fn get(&self) -> Duration {
        Duration::from_micros(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, latency: Duration) {
        self.0.store(latency.as_micros() as u64, Ordering::Relaxed);
    }
}

pub struct Metronome {
    _stream: Stream,
    pub device: String,
    pub latency: OutputLatency,
}

impl Metronome {
    pub fn start(origin: Instant, period: Duration) -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("aucune sortie audio par défaut")?;
        let supported = device.default_output_config()?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let latency = OutputLatency::default();
        let probe = latency.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, origin, period, probe)?,
            SampleFormat::I16 => build::<i16>(&device, config, origin, period, probe)?,
            SampleFormat::I32 => build::<i32>(&device, config, origin, period, probe)?,
            SampleFormat::U16 => build::<u16>(&device, config, origin, period, probe)?,
            other => bail!("format audio non géré : {other}"),
        };
        stream.play()?;
        Ok(Self { _stream: stream, device: device.to_string(), latency })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    origin: Instant,
    period: Duration,
    latency: OutputLatency,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let frame = Duration::from_secs_f64(1.0 / f64::from(config.sample_rate));
    let period_ns = period.as_nanos();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], info: &OutputCallbackInfo| {
            let timestamp = info.timestamp();
            let delay = timestamp.playback.duration_since(timestamp.callback);
            latency.set(delay);
            // Instant où le premier échantillon du bloc sortira des enceintes.
            let first = Instant::now() + delay;
            for (i, out) in data.chunks_mut(channels).enumerate() {
                let sample = click(first + frame * i as u32, origin, period_ns);
                out.fill(T::from_sample(sample));
            }
        },
        |err| eprintln!("\r\nerreur audio : {err}"),
        None,
    )?;
    Ok(stream)
}

/// Bip aigu à attaque franche : facile à repérer à l'oreille comme sur la piste son d'une vidéo.
fn click(t: Instant, origin: Instant, period_ns: u128) -> f32 {
    let Some(elapsed) = t.checked_duration_since(origin) else {
        return 0.0;
    };
    let phase = (elapsed.as_nanos() % period_ns) as f64 * 1e-9;
    if phase >= CLICK_LEN {
        return 0.0;
    }
    (CLICK_GAIN * (-phase / CLICK_DECAY).exp() * (TAU * CLICK_FREQ * phase).sin()) as f32
}

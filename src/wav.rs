//! Fichiers WAV : écriture en mono flottant 32 bits ; lecture du PCM 16/24/32 bits et du
//! flottant 32 bits, mixés en mono.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

pub fn write_mono_f32(path: &Path, rate: u32, samples: &[f32]) -> Result<()> {
    let data_len = (samples.len() * 4) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&FORMAT_FLOAT.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // canaux
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes()); // octets par seconde
    bytes.extend_from_slice(&4u16.to_le_bytes()); // octets par trame
    bytes.extend_from_slice(&32u16.to_le_bytes()); // bits par échantillon
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, bytes).with_context(|| format!("écriture de {}", path.display()))
}

/// Lit un WAV et renvoie sa fréquence d'échantillonnage et ses échantillons mixés en mono.
pub fn read_mono(path: &Path) -> Result<(u32, Vec<f32>)> {
    let bytes = fs::read(path).with_context(|| format!("lecture de {}", path.display()))?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("{} n'est pas un fichier WAV", path.display());
    }
    let u16_at = |data: &[u8], at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
    let u32_at = |data: &[u8], at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());

    let (mut format, mut data) = (None, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32_at(&bytes, at + 4) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match &bytes[at..at + 4] {
            b"fmt " if body.len() >= 16 => {
                let mut tag = u16_at(body, 0);
                if tag == FORMAT_EXTENSIBLE && body.len() >= 26 {
                    tag = u16_at(body, 24);
                }
                format = Some((tag, u16_at(body, 2), u32_at(body, 4), u16_at(body, 14)));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + size + size % 2;
    }
    let (tag, channels, rate, bits) = format.context("bloc « fmt » absent")?;
    let data = data.context("bloc « data » absent")?;
    let channels = usize::from(channels.max(1));

    let decoded: Vec<f32> = match (tag, bits) {
        (FORMAT_PCM, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| f32::from(i16::from_le_bytes(b)) / 32768.0)
            .collect(),
        (FORMAT_PCM, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|&[a, b, c]| (i32::from_le_bytes([0, a, b, c]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (FORMAT_PCM, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| i32::from_le_bytes(b) as f32 / 2_147_483_648.0)
            .collect(),
        (FORMAT_FLOAT, 32) => data.as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)).collect(),
        _ => bail!("format WAV non géré (code {tag}, {bits} bits)"),
    };
    let mono = decoded
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((rate, mono))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_mono_float_file() {
        let path = std::env::temp_dir().join(format!("dj-bridge-test-{}.wav", std::process::id()));
        let samples = [0.0, 0.5, -0.25, 1.0];
        write_mono_f32(&path, 44_100, &samples).unwrap();
        let read = read_mono(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(read.unwrap(), (44_100, samples.to_vec()));
    }
}

//! Capture d'écran de la fenêtre, pour le développement : avec `DJB_CAPTURE=chemin.png`, la
//! fenêtre s'ouvre à la taille `DJB_CAPTURE_SIZE` (« 1180x780 » par défaut), en mode expert si
//! `DJB_CAPTURE_EXPERT` est défini, se photographie après quelques images, écrit un PNG et se ferme.
//! Rien n'est enregistré dans la configuration.
//!
//! Avec `DJB_CAPTURE_JOURNAL` (un journal de la sonde) et `DJB_CAPTURE_AT` (secondes), elle montre
//! à la place la bande du morceau que l'OPUS jouait à cet instant, sans lancer de live.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use eframe::egui;

use crate::engine::OpusNow;
use crate::opus::Client;

/// Images rendues avant la photo : le temps que la mise en page se pose.
const SETTLE_FRAMES: u32 = 12;

pub(super) struct Capture {
    pub path: PathBuf,
    pub size: [f32; 2],
    pub expert: bool,
    frames: u32,
    requested: bool,
    /// La photo est écrite : la fenêtre peut se fermer.
    written: Arc<AtomicBool>,
    /// Bande du morceau rejoué depuis un journal, à montrer à la place de l'écran.
    pub preview: Option<OpusNow>,
}

/// Rejoue un journal de la sonde jusqu'à `at` (s) : où en était la platine qui menait.
fn replay(journal: &str, at: f64) -> Option<OpusNow> {
    let text = std::fs::read_to_string(journal).ok()?;
    let mut client = Client::new(Vec::new());
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        let Some(t) = f.first().and_then(|t| t.parse::<f64>().ok()).filter(|&t| t <= at) else { continue };
        if f.len() >= 5 && f[1] == "<" {
            let from: Ipv4Addr = f[3].split(':').next()?.parse().ok()?;
            client.on_packet(t, f[2].parse().ok()?, from, &hex::decode(f[4]).ok()?, &mut out);
        }
    }
    let playing = client.playing(at)?;
    let guide = playing.guide(at, 0.0)?;
    let phrase = guide.phrase.as_ref();
    Some(OpusNow {
        deck: playing.deck,
        track: playing.track.id,
        data: playing.track.clone(),
        position_ms: guide.position_ms,
        beat: guide.beat,
        bar_beat: guide.bar_beat,
        phrase_beat: guide.phrase_beat,
        phrase: phrase.map(|p| p.label.clone()),
        next: phrase.and_then(|p| p.next.clone()),
        beats_left: phrase.map(|p| p.end.saturating_sub(guide.beat)),
        settled: playing.settled,
        at: Instant::now(),
    })
}

impl Capture {
    pub fn from_env() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os("DJB_CAPTURE")?);
        let size = std::env::var("DJB_CAPTURE_SIZE")
            .ok()
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
            })
            .unwrap_or([1180.0, 780.0]);
        let expert = std::env::var_os("DJB_CAPTURE_EXPERT").is_some();
        let preview = std::env::var("DJB_CAPTURE_JOURNAL").ok().and_then(|journal| {
            let at = std::env::var("DJB_CAPTURE_AT").ok()?.parse().ok()?;
            replay(&journal, at)
        });
        Some(Self { path, size, expert, frames: 0, requested: false, written: Arc::new(AtomicBool::new(false)), preview })
    }

    /// À chaque image : demande la photo une fois la fenêtre posée ; une fois écrite, ferme.
    pub fn frame(&mut self, ctx: &egui::Context) {
        self.frames += 1;
        if self.written.load(Ordering::Acquire) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.frames >= SETTLE_FRAMES && !self.requested {
            self.requested = true;
            let (path, written, repaint) = (self.path.clone(), self.written.clone(), ctx.clone());
            ctx.request_screenshot(move |image| {
                let rgb: Vec<u8> = image.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
                if let Err(e) = std::fs::write(&path, png(image.size[0], image.size[1], &rgb)) {
                    eprintln!("capture non écrite : {e}");
                }
                written.store(true, Ordering::Release);
                repaint.request_repaint();
            });
        }
        ctx.request_repaint();
    }
}

/// PNG RGB 8 bits sans compression (blocs « stockés »), de quoi relire une capture.
fn png(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(height * (1 + 3 * width));
    for row in rgb.chunks(3 * width).take(height) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn writes_a_well_formed_png() {
        let image = png(2, 1, &[255, 0, 0, 0, 0, 255]);
        assert_eq!(&image[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&image[12..16], b"IHDR");
        assert_eq!(&image[image.len() - 8..image.len() - 4], b"IEND");
    }
}

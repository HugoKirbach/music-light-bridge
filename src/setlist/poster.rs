//! Export d'un set en image (PNG) : la date et la durée en tête, puis chaque morceau avec son heure
//! d'entrée, sa pochette, son titre et son artiste, aux couleurs de la fenêtre.

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use anyhow::{Context, Result};
use image::{Rgba, RgbaImage, imageops};

use super::{Set, timecode};

const WIDTH: u32 = 1080;
const MARGIN: i32 = 64;
const HEADER: i32 = 300;
const ROW: i32 = 132;
const FOOTER: i32 = 110;
const THUMB: u32 = 104;
const THUMB_RADIUS: f32 = 14.0;
/// Colonne de l'heure d'entrée, puis pochette, puis textes.
const TIME_WIDTH: i32 = 132;

const BG_TOP: [u8; 3] = [0x1C, 0x14, 0x10];
const BG_BOTTOM: [u8; 3] = [0x0D, 0x0E, 0x11];
const TEXT: [u8; 3] = [0xEC, 0xEE, 0xF1];
const MUTED: [u8; 3] = [0xA0, 0xA6, 0xAF];
const ACCENT: [u8; 3] = [0xFF, 0x8A, 0x2A];
const LINE: [u8; 3] = [0x26, 0x2A, 0x32];
const EMPTY: [u8; 3] = [0x1B, 0x1E, 0x24];

struct Fonts {
    regular: FontVec,
    semibold: FontVec,
    bold: FontVec,
    mono: FontVec,
}

impl Fonts {
    /// Les polices de la fenêtre (Segoe UI, Consolas), dans le dossier des polices de Windows.
    fn load() -> Result<Self> {
        let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
        let font = |file: &str| -> Result<FontVec> {
            let bytes = std::fs::read(dir.join(file)).with_context(|| format!("police {file} introuvable"))?;
            FontVec::try_from_vec(bytes).with_context(|| format!("police {file} illisible"))
        };
        Ok(Self { regular: font("segoeui.ttf")?, semibold: font("seguisb.ttf")?, bold: font("segoeuib.ttf")?, mono: font("consolab.ttf")? })
    }
}

/// L'image du set ; `folder` : son dossier, où sont les pochettes.
pub fn render(set: &Set, folder: &Path) -> Result<RgbaImage> {
    let fonts = Fonts::load()?;
    let rows = set.tracks.len().max(1) as i32;
    let height = (HEADER + rows * ROW + FOOTER) as u32;
    let mut img = RgbaImage::new(WIDTH, height);
    for (_, y, pixel) in img.enumerate_pixels_mut() {
        let t = (y as f32 / 900.0).min(1.0);
        let c = |i: usize| (f32::from(BG_TOP[i]) + (f32::from(BG_BOTTOM[i]) - f32::from(BG_TOP[i])) * t) as u8;
        *pixel = Rgba([c(0), c(1), c(2), 255]);
    }
    let right = WIDTH as i32 - MARGIN;

    // En-tête : marque, date, heure, durée et nombre de morceaux.
    let pen = |font, size, color| Pen { font, size, color };
    pen(&fonts.bold, 22.0, ACCENT).draw(&mut img, (MARGIN, 96), "SETLIST · DJ-BRIDGE", 3.0);
    pen(&fonts.bold, 54.0, TEXT).text(&mut img, (MARGIN, 168), &set.date(), right - MARGIN);
    let subtitle = format!("{} · {}", set.hour(), set.summary());
    pen(&fonts.regular, 30.0, MUTED).text(&mut img, (MARGIN, 218), &subtitle, right - MARGIN);
    fill_rect(&mut img, MARGIN, HEADER - 36, right - MARGIN, 2, LINE);

    for (i, entry) in set.tracks.iter().enumerate() {
        let top = HEADER + i as i32 * ROW;
        let middle = top + ROW / 2 - 14;
        pen(&fonts.mono, 30.0, ACCENT).text(&mut img, (MARGIN, middle + 12), &timecode(entry.at), TIME_WIDTH);
        let thumb_x = MARGIN + TIME_WIDTH;
        let thumb_y = middle - THUMB as i32 / 2 + 2;
        let artwork = entry.artwork.as_ref().and_then(|name| image::open(folder.join(name)).ok());
        match artwork {
            Some(art) => {
                let art = imageops::resize(&art.to_rgba8(), THUMB, THUMB, imageops::FilterType::CatmullRom);
                paste_rounded(&mut img, &art, thumb_x, thumb_y);
            }
            None => disc(&mut img, thumb_x, thumb_y),
        }
        let x = thumb_x + THUMB as i32 + 28;
        let width = right - x;
        pen(&fonts.semibold, 32.0, TEXT).text(&mut img, (x, middle - 2), &entry.shown_title(), width);
        let mut artist = entry.artist.clone();
        if let Some(bpm) = entry.bpm {
            let tempo = format!("{bpm:.0} BPM");
            artist = if artist.is_empty() { tempo } else { format!("{artist}   ·   {tempo}") };
        }
        pen(&fonts.regular, 26.0, MUTED).text(&mut img, (x, middle + 38), &artist, width);
        if i + 1 < set.tracks.len() {
            fill_rect(&mut img, x, top + ROW - 1, right - x, 1, LINE);
        }
    }

    let bottom = height as i32 - FOOTER / 2 + 8;
    pen(&fonts.regular, 22.0, MUTED).text(&mut img, (MARGIN, bottom), "Enregistré par DJ-Bridge depuis l'OPUS-QUAD", right - MARGIN);
    Ok(img)
}

/// Enregistre l'image du set dans `path` (PNG).
pub fn export(set: &Set, folder: &Path, path: &Path) -> Result<()> {
    render(set, folder)?.save(path).with_context(|| format!("impossible d'écrire {}", path.display()))
}

/// Police, taille (px) et couleur d'un texte.
struct Pen<'a> {
    font: &'a FontVec,
    size: f32,
    color: [u8; 3],
}

impl Pen<'_> {
    /// Écrit `content` à partir de `at` (x, ligne de base), coupé d'un « … » au-delà de `max_width`.
    fn text(&self, img: &mut RgbaImage, at: (i32, i32), content: &str, max_width: i32) {
        let scaled = self.font.as_scaled(PxScale::from(self.size));
        let width = |s: &str| measure(&scaled, s, 0.0);
        let mut shown = content.to_owned();
        if width(&shown) > max_width as f32 {
            while !shown.is_empty() && width(&format!("{shown}…")) > max_width as f32 {
                shown.pop();
            }
            shown = format!("{}…", shown.trim_end());
        }
        self.draw(img, at, &shown, 0.0);
    }

    /// Écrit `content` à partir de `at` (x, ligne de base), lettres écartées de `spacing` px.
    fn draw(&self, img: &mut RgbaImage, (x, baseline): (i32, i32), content: &str, spacing: f32) {
        let scale = PxScale::from(self.size);
        let scaled = self.font.as_scaled(scale);
        let mut caret = x as f32;
        let mut previous = None;
        for c in content.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = previous {
                caret += scaled.kern(prev, id);
            }
            let glyph = id.with_scale_and_position(scale, point(caret, baseline as f32));
            if let Some(outlined) = self.font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    blend(img, bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, self.color, coverage);
                });
            }
            caret += scaled.h_advance(id) + spacing;
            previous = Some(id);
        }
    }
}

fn measure<F: Font, S: ScaleFont<F>>(scaled: &S, content: &str, spacing: f32) -> f32 {
    let mut width = 0.0;
    let mut previous = None;
    for c in content.chars() {
        let id = scaled.glyph_id(c);
        if let Some(prev) = previous {
            width += scaled.kern(prev, id);
        }
        width += scaled.h_advance(id) + spacing;
        previous = Some(id);
    }
    width
}

fn blend(img: &mut RgbaImage, x: i32, y: i32, color: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let pixel = img.get_pixel_mut(x as u32, y as u32);
    for (channel, &target) in pixel.0.iter_mut().zip(&color) {
        *channel = (f32::from(*channel) * (1.0 - alpha) + f32::from(target) * alpha).round() as u8;
    }
}

fn fill_rect(img: &mut RgbaImage, x: i32, y: i32, width: i32, height: i32, color: [u8; 3]) {
    for py in y..y + height {
        for px in x..x + width {
            blend(img, px, py, color, 1.0);
        }
    }
}

/// Couverture (0 à 1) d'un pixel dans un carré de `size` aux coins arrondis.
fn rounded(px: u32, py: u32, size: u32, radius: f32) -> f32 {
    let edge = |p: u32| {
        let p = p as f32 + 0.5;
        if p < radius { radius - p } else if p > size as f32 - radius { p - (size as f32 - radius) } else { 0.0 }
    };
    let (dx, dy) = (edge(px), edge(py));
    if dx == 0.0 || dy == 0.0 {
        return 1.0;
    }
    (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0)
}

fn paste_rounded(img: &mut RgbaImage, art: &RgbaImage, x: i32, y: i32) {
    for (px, py, pixel) in art.enumerate_pixels() {
        let coverage = rounded(px, py, art.width(), THUMB_RADIUS);
        blend(img, x + px as i32, y + py as i32, [pixel.0[0], pixel.0[1], pixel.0[2]], coverage);
    }
}

/// Pas de pochette : un disque sur fond sombre.
fn disc(img: &mut RgbaImage, x: i32, y: i32) {
    let center = THUMB as f32 / 2.0;
    for py in 0..THUMB {
        for px in 0..THUMB {
            blend(img, x + px as i32, y + py as i32, EMPTY, rounded(px, py, THUMB, THUMB_RADIUS));
            let d = (px as f32 + 0.5 - center).hypot(py as f32 + 0.5 - center);
            let ring = |r: f32| (r + 0.5 - d).clamp(0.0, 1.0);
            blend(img, x + px as i32, y + py as i32, LINE, ring(36.0));
            blend(img, x + px as i32, y + py as i32, ACCENT, ring(9.0));
            blend(img, x + px as i32, y + py as i32, EMPTY, ring(3.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setlist::Entry;

    /// Une ligne par morceau sous l'en-tête ; les titres trop longs sont coupés, un morceau sans
    /// pochette ni titre a quand même sa ligne. `DJB_POSTER_OUT` : où écrire l'image pour la voir.
    #[test]
    fn a_long_set_gets_one_row_per_track() {
        let entry = |i: usize, title: &str| Entry {
            at: i as f64 * 271.0,
            title: title.to_owned(),
            artist: if title.is_empty() { String::new() } else { "Odymek, Fenrick".to_owned() },
            deck: 1 + (i % 2) as u8,
            bpm: Some(158.0 + i as f64),
            track_id: 100 + i as u32,
            artwork: None,
        };
        let mut tracks: Vec<Entry> = (0..10).map(|i| entry(i, &format!("Morceau {i}"))).collect();
        tracks[3] = entry(3, "Un titre de hard trance beaucoup trop long pour tenir sur une seule ligne de l'image (Extended Mix)");
        tracks[6] = entry(6, "");
        let set = Set { started: 1_793_822_640, utc_offset: 3600, duration: 2900.0, tracks };
        let img = render(&set, Path::new(".")).unwrap();
        assert_eq!((img.width(), img.height()), (WIDTH, (HEADER + 10 * ROW + FOOTER) as u32));
        if let Some(path) = std::env::var_os("DJB_POSTER_OUT") {
            img.save(path).unwrap();
        }
    }
}

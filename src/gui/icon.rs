//! Icône de DJ-Bridge : l'anneau orange de l'en-tête autour de son point, sur un carré sombre
//! arrondi. Dessinée ici pour la fenêtre ; écrite aussi en .ico (`assets/dj-bridge.ico`), que la
//! compilation grave dans les exécutables.

/// Proportions, en fraction du côté : rayon et épaisseur de l'anneau, rayon du point, arrondi des
/// coins.
const RING: f32 = 0.30;
const RING_HALF_WIDTH: f32 = 0.045;
const DOT: f32 = 0.10;
const CORNER: f32 = 0.22;
const BACKGROUND: [u8; 3] = [0x16, 0x18, 0x1D];
const ACCENT: [u8; 3] = [0xFF, 0x8A, 0x2A];

/// Pixels RGBA (alpha non prémultiplié) d'une icône de `size` × `size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (u, v) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            // Couverture d'une forme d'après sa distance signée (négative dedans), lissée sur un pixel.
            let cover = |distance: f32| (0.5 - distance * s).clamp(0.0, 1.0);
            let (dx, dy) = ((u - 0.5).abs() - (0.5 - CORNER), (v - 0.5).abs() - (0.5 - CORNER));
            let square = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - CORNER;
            let center = (u - 0.5).hypot(v - 0.5);
            let mark = cover((center - RING).abs() - RING_HALF_WIDTH).max(cover(center - DOT));
            let rgb: [u8; 3] = std::array::from_fn(|i| {
                (f32::from(BACKGROUND[i]) + (f32::from(ACCENT[i]) - f32::from(BACKGROUND[i])) * mark).round() as u8
            });
            out.extend_from_slice(&rgb);
            out.push((cover(square) * 255.0).round() as u8);
        }
    }
    out
}

/// Fichier .ico aux tailles données (32 bits par pixel, avec transparence).
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bitmap(size)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&size, image) in sizes.iter().zip(&images) {
        // 256 s'écrit 0.
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// Une image de l'icône : en-tête BMP (hauteur doublée pour le masque), pixels BGRA de bas en haut,
/// puis le masque de transparence (vide : l'alpha suffit).
fn bitmap(size: u32) -> Vec<u8> {
    let pixels = rgba(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    for value in [40u32, size, 2 * size] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for value in [0u32, size * size * 4 + mask_row * size, 0, 0, 0, 0] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for row in (0..size).rev() {
        for x in 0..size {
            let i = ((row * size + x) * 4) as usize;
            out.extend_from_slice(&[pixels[i + 2], pixels[i + 1], pixels[i], pixels[i + 3]]);
        }
    }
    out.extend(std::iter::repeat_n(0, (mask_row * size) as usize));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_the_ring_and_the_dot_on_a_rounded_square() {
        let size = 64;
        let pixels = rgba(size);
        let at = |x: u32, y: u32| &pixels[((y * size + x) * 4) as usize..][..4];
        assert_eq!(at(32, 32), [0xFF, 0x8A, 0x2A, 255], "le point au centre");
        assert_eq!(at(32, 13)[..3], ACCENT, "l'anneau");
        assert_eq!(at(32, 22)[..3], BACKGROUND, "entre l'anneau et le point");
        assert_eq!(at(0, 0)[3], 0, "coin arrondi transparent");
    }

    #[test]
    fn writes_an_ico_directory() {
        let file = ico(&[16, 256]);
        assert_eq!(&file[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(file[6], 16);
        assert_eq!(file[22], 0, "256 s'écrit 0");
        let first = u32::from_le_bytes(file[18..22].try_into().unwrap()) as usize;
        assert_eq!(first, 6 + 2 * 16);
        assert_eq!(u32::from_le_bytes(file[first..first + 4].try_into().unwrap()), 40);
    }

    /// Réécrit `assets/dj-bridge.ico` : `cargo test --lib gui::icon -- --ignored`.
    #[test]
    #[ignore]
    fn write_assets_icon() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("dj-bridge.ico");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, ico(&[16, 24, 32, 48, 64, 256])).unwrap();
    }
}

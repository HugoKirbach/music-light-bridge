//! Bande du morceau en cours, quand l'OPUS mène : les phrases rekordbox en couleur, la waveform
//! autour de la tête de lecture (le passé assombri, l'avenir à droite), les mesures, et en dessous
//! tout le morceau en miniature. On y voit ce que la lumière anticipe : le prochain drop, le
//! prochain break.

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

use super::theme::{self, SEMIBOLD};
use crate::engine::OpusNow;
use crate::opus::Track;

/// Fenêtre affichée autour de la tête de lecture (ms) : un peu de passé, beaucoup d'avenir.
const PAST_MS: f64 = 6_000.0;
const AHEAD_MS: f64 = 18_000.0;
const PHRASES_HEIGHT: f32 = 16.0;
const WAVE_HEIGHT: f32 = 60.0;
const OVERVIEW_HEIGHT: f32 = 6.0;
const GAP: f32 = 4.0;
/// Hauteur totale de la bande.
pub(super) const HEIGHT: f32 = PHRASES_HEIGHT + GAP + WAVE_HEIGHT + 2.0 * GAP + OVERVIEW_HEIGHT;

/// Couleur d'une phrase rekordbox, d'après son nom.
fn phrase_color(label: &str) -> Color32 {
    match label {
        "Up" => Color32::from_rgb(0xF5, 0x9E, 0x0B),
        "Down" => Color32::from_rgb(0x3B, 0x82, 0xF6),
        "Chorus" => Color32::from_rgb(0xEC, 0x48, 0x99),
        "Bridge" => Color32::from_rgb(0x8B, 0x5C, 0xF6),
        label if label.starts_with("Verse") => Color32::from_rgb(0x10, 0xB9, 0x81),
        _ => Color32::from_rgb(0x6B, 0x72, 0x80),
    }
}

/// Phrases du morceau : nom et étendue (ms, à vitesse normale).
fn phrases(track: &Track) -> Vec<(String, f64, f64)> {
    let (Some(song), Some(grid)) = (&track.song, &track.grid) else { return Vec::new() };
    let time = |beat: u32| grid.beats.get((beat as usize).saturating_sub(1)).map(|b| f64::from(b.time_ms));
    let end = grid.beats.last().map_or(0.0, |b| f64::from(b.time_ms));
    song.phrases
        .iter()
        .enumerate()
        .filter_map(|(i, phrase)| {
            let from = time(u32::from(phrase.beat))?;
            let to = song.phrases.get(i + 1).and_then(|next| time(u32::from(next.beat))).unwrap_or(end);
            Some((song.label(phrase.kind), from, to))
        })
        .collect()
}

/// Dessine la bande sur `width` points.
pub(super) fn strip(ui: &mut Ui, now: &OpusNow, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect.expand(2.0));
    let track = &now.data;
    let start = now.position_ms - PAST_MS;
    let span = PAST_MS + AHEAD_MS;
    let x = |ms: f64| rect.left() + ((ms - start) / span) as f32 * rect.width();
    let phrases = phrases(track);

    // Phrases : un bandeau de couleur, nommé quand il y a la place.
    let band = Rect::from_min_size(rect.min, vec2(rect.width(), PHRASES_HEIGHT));
    for (label, from, to) in &phrases {
        let (left, right) = (x(*from).max(band.left()), x(*to).min(band.right()));
        if right <= left {
            continue;
        }
        let color = phrase_color(label);
        let block = Rect::from_x_y_ranges(left..=right - 1.0, band.y_range());
        painter.rect_filled(block, CornerRadius::same(3), color.gamma_multiply(0.85));
        if block.width() > 44.0 {
            painter.text(
                pos2(block.left() + 6.0, block.center().y),
                Align2::LEFT_CENTER,
                label.to_uppercase(),
                theme::font(10.0, SEMIBOLD),
                theme::ink_on(color),
            );
        }
    }

    // Waveform : une colonne par point d'écran, le plus haut point et sa couleur ; le passé assombri.
    let wave = Rect::from_min_size(pos2(rect.left(), band.bottom() + GAP), vec2(rect.width(), WAVE_HEIGHT));
    painter.rect_filled(wave, CornerRadius::same(6), theme::black(0.35));
    if let Some(waveform) = &track.waveform {
        let columns = wave.width().floor() as usize;
        let ms_per_column = span / columns.max(1) as f64;
        for column in 0..columns {
            let from = start + column as f64 * ms_per_column;
            if from < 0.0 {
                continue;
            }
            let (first, last) = (waveform_index(waveform.per_second, from), waveform_index(waveform.per_second, from + ms_per_column));
            let Some(points) = waveform.points.get(first..=last.min(waveform.points.len().saturating_sub(1))) else { continue };
            let Some(top) = points.iter().copied().max_by(|a, b| a.height.total_cmp(&b.height)) else { continue };
            if top.height <= 0.0 {
                continue;
            }
            let max = top.red.max(top.green).max(top.blue).max(1e-3);
            let color = Color32::from_rgb(
                (top.red / max * 255.0) as u8,
                (top.green / max * 255.0) as u8,
                (top.blue / max * 255.0) as u8,
            );
            let color = if from < now.position_ms { color.gamma_multiply(0.4) } else { color };
            let half = top.height * (WAVE_HEIGHT / 2.0 - 3.0);
            let cx = wave.left() + column as f32 + 0.5;
            painter.line_segment([pos2(cx, wave.center().y - half), pos2(cx, wave.center().y + half)], Stroke::new(1.0, color));
        }
    }
    // Mesures : un trait discret à chaque premier temps.
    if let Some(grid) = &track.grid {
        for beat in grid.beats.iter().filter(|b| b.bar_beat == 1) {
            let bx = x(f64::from(beat.time_ms));
            if (wave.left()..=wave.right()).contains(&bx) {
                painter.line_segment([pos2(bx, wave.top()), pos2(bx, wave.top() + 6.0)], Stroke::new(1.0, theme::white(0.25)));
            }
        }
    }
    // Tête de lecture.
    let head = x(now.position_ms);
    painter.line_segment([pos2(head, band.top()), pos2(head, wave.bottom())], Stroke::new(2.0, Color32::WHITE));

    // Tout le morceau en miniature, avec la position.
    let overview = Rect::from_min_size(pos2(rect.left(), wave.bottom() + 2.0 * GAP), vec2(rect.width(), OVERVIEW_HEIGHT));
    let total = phrases.last().map_or(0.0, |(_, _, to)| *to).max(1.0);
    let ox = |ms: f64| overview.left() + (ms / total).clamp(0.0, 1.0) as f32 * overview.width();
    painter.rect_filled(overview, CornerRadius::same(3), theme::white(0.08));
    for (label, from, to) in &phrases {
        let block = Rect::from_x_y_ranges(ox(*from)..=(ox(*to) - 1.0).max(ox(*from)), overview.y_range());
        painter.rect_filled(block, CornerRadius::ZERO, phrase_color(label).gamma_multiply(0.7));
    }
    let marker = ox(now.position_ms);
    painter.add(egui::Shape::convex_polygon(
        vec![Pos2::new(marker - 4.0, overview.bottom() + 5.0), Pos2::new(marker + 4.0, overview.bottom() + 5.0), Pos2::new(marker, overview.top())],
        Color32::WHITE,
        Stroke::NONE,
    ));
    response.on_hover_text("Morceau en cours : phrases rekordbox, waveform (le passé à gauche de la tête de lecture), et tout le morceau en dessous");
}

fn waveform_index(per_second: f64, ms: f64) -> usize {
    (ms.max(0.0) / 1000.0 * per_second) as usize
}

//! Bandes des platines, quand l'OPUS est suivi, l'une sous l'autre comme sur une CDJ : les
//! phrases rekordbox en couleur, la waveform autour de la tête de lecture (le passé assombri,
//! l'avenir à droite), les mesures, et tout le morceau en miniature. La platine qui mène la lumière
//! est en pleine lumière, les autres en retrait ; on les voit même volume coupé.
//!
//! Deux dispositions : les platines centrales 1 et 2, ou les quatre dans l'ordre de l'OPUS
//! (3, 1, 2, 4), en bandes plus basses sans le détail des phrases.

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

use super::theme::{self, ACCENT, BOLD, SEMIBOLD};
use crate::opus::{DeckView, Track};

/// Fenêtre affichée autour de la tête de lecture (ms) : un peu de passé, beaucoup d'avenir.
const PAST_MS: f64 = 6_000.0;
const AHEAD_MS: f64 = 18_000.0;
const LABEL_WIDTH: f32 = 54.0;
const PHRASES_HEIGHT: f32 = 14.0;
const WAVE_HEIGHT: f32 = 44.0;
const COMPACT_WAVE_HEIGHT: f32 = 28.0;
const OVERVIEW_HEIGHT: f32 = 5.0;
const GAP: f32 = 3.0;
/// Écart entre deux bandes, et colonne du sélecteur 2 / 4 pistes.
const ROW_GAP: f32 = 8.0;
const SWITCH_WIDTH: f32 = 30.0;
/// Une platine qui ne mène pas la lumière est montrée en retrait.
const BACKGROUND_DECK: f32 = 0.5;

/// Disposition des bandes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    /// Les platines centrales, 1 au-dessus de 2, avec le détail des phrases.
    Two,
    /// Les quatre, dans l'ordre de l'OPUS (3, 1, 2, 4), sans le détail des phrases.
    Four,
}

impl Layout {
    fn decks(self) -> &'static [u8] {
        match self {
            Self::Two => &[1, 2],
            Self::Four => &[3, 1, 2, 4],
        }
    }

    fn row_height(self) -> f32 {
        match self {
            Self::Two => PHRASES_HEIGHT + GAP + WAVE_HEIGHT + 2.0 * GAP + OVERVIEW_HEIGHT,
            Self::Four => COMPACT_WAVE_HEIGHT + 2.0 * GAP + OVERVIEW_HEIGHT,
        }
    }

    fn wave_height(self) -> f32 {
        match self {
            Self::Two => WAVE_HEIGHT,
            Self::Four => COMPACT_WAVE_HEIGHT,
        }
    }
}

/// Un morceau vient d'arriver sur la platine 3 ou 4 : `seen` garde ceux qu'on y a déjà vus
/// (0 : aucun), et la réponse ne vaut vrai qu'au chargement, pour qu'on puisse revenir en 2 pistes.
pub(super) fn loaded_outside(seen: &mut [u32; 2], decks: &[DeckView]) -> bool {
    let mut loaded = false;
    for (seen, number) in seen.iter_mut().zip([3u8, 4]) {
        let track = decks.iter().find(|deck| deck.deck == number).map_or(0, |deck| deck.track.id);
        loaded |= track != 0 && track != *seen;
        *seen = track;
    }
    loaded
}

/// Hauteur des bandes dans la disposition `layout`.
pub(super) fn height(layout: Layout) -> f32 {
    let rows = layout.decks().len() as f32;
    rows * layout.row_height() + (rows - 1.0) * ROW_GAP
}

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

/// Les bandes de la disposition `layout`, l'une sous l'autre, avec à gauche le sélecteur 2 / 4
/// pistes ; `lead` : la platine qui mène la lumière. Rend la disposition demandée d'un clic.
pub(super) fn decks(ui: &mut Ui, decks: &[DeckView], lead: Option<u8>, width: f32, layout: Layout) -> Option<Layout> {
    let mut picked = None;
    ui.allocate_ui_with_layout(vec2(width, height(layout)), egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        picked = switch(ui, layout);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = ROW_GAP;
            let width = width - SWITCH_WIDTH - 8.0;
            for &number in layout.decks() {
                match decks.iter().find(|deck| deck.deck == number) {
                    Some(deck) => strip(ui, deck, lead == Some(number), width, layout),
                    None => empty(ui, number, width, layout),
                }
            }
        });
    });
    picked
}

/// Sélecteur 2 / 4 pistes : deux petits boutons l'un sous l'autre.
fn switch(ui: &mut Ui, layout: Layout) -> Option<Layout> {
    let mut picked = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for (choice, text, hint) in [
            (Layout::Two, "2", "2 pistes : les platines centrales 1 et 2, avec le détail des phrases"),
            (Layout::Four, "4", "4 pistes, dans l'ordre de l'OPUS : 3, 1, 2, 4"),
        ] {
            let selected = choice == layout;
            let (rect, response) = ui.allocate_exact_size(vec2(SWITCH_WIDTH, 26.0), Sense::click());
            let fill = if selected { theme::white(0.18) } else if response.hovered() { theme::white(0.08) } else { theme::black(0.3) };
            ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
            let color = if selected { Color32::WHITE } else { theme::white(0.55) };
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, theme::font(13.0, BOLD), color);
            if response.on_hover_text(hint).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !selected {
                picked = Some(choice);
            }
        }
    });
    picked
}

/// Ce que montre la colonne de gauche d'une bande : numéro de la platine, état (maître, lecture,
/// pause ou vide) et tempo.
struct DeckLabel<'a> {
    number: u8,
    state: &'a str,
    master: bool,
    bpm: Option<f64>,
}

/// La colonne de gauche d'une bande, dans `label`.
fn deck_label(painter: &egui::Painter, label: Rect, deck: DeckLabel, lead: bool, compact: bool) {
    let DeckLabel { number, state, master, bpm } = deck;
    let fade = |color: Color32| if lead { color } else { color.gamma_multiply(BACKGROUND_DECK) };
    painter.rect_filled(label.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), theme::black(if lead { 0.55 } else { 0.3 }));
    let (size, number_y, state_y) = if compact { (17.0, 13.0, 30.0) } else { (24.0, 22.0, 44.0) };
    painter.text(
        pos2(label.center().x, label.top() + number_y),
        Align2::CENTER_CENTER,
        number.to_string(),
        theme::font(size, BOLD),
        fade(Color32::WHITE),
    );
    painter.text(
        pos2(label.center().x, label.top() + state_y),
        Align2::CENTER_CENTER,
        state,
        theme::font(if compact { 8.0 } else { 9.0 }, SEMIBOLD),
        if master { ACCENT } else { fade(theme::white(0.7)) },
    );
    if let (Some(bpm), false) = (bpm, compact) {
        painter.text(
            pos2(label.center().x, label.top() + 58.0),
            Align2::CENTER_CENTER,
            format!("{bpm:.1}"),
            theme::font(10.0, SEMIBOLD),
            fade(theme::white(0.6)),
        );
    }
}

/// Bande d'une platine sans morceau chargé.
fn empty(ui: &mut Ui, number: u8, width: f32, layout: Layout) {
    let (full, _) = ui.allocate_exact_size(vec2(width, layout.row_height()), Sense::hover());
    let painter = ui.painter_at(full.expand(2.0));
    let label = Rect::from_min_size(full.min, vec2(LABEL_WIDTH, full.height()));
    let deck = DeckLabel { number, state: "VIDE", master: false, bpm: None };
    deck_label(&painter, label, deck, false, layout == Layout::Four);
    let rest = Rect::from_min_max(pos2(label.right() + 8.0, full.top()), full.max);
    painter.rect_filled(rest, CornerRadius::same(6), theme::black(0.2));
}

/// La bande d'une platine sur `width` points : son numéro et son état à gauche, le morceau à droite.
fn strip(ui: &mut Ui, deck: &DeckView, lead: bool, width: f32, layout: Layout) {
    let (full, response) = ui.allocate_exact_size(vec2(width, layout.row_height()), Sense::hover());
    let painter = ui.painter_at(full.expand(2.0));
    let fade = |color: Color32| if lead { color } else { color.gamma_multiply(BACKGROUND_DECK) };
    let compact = layout == Layout::Four;

    let label = Rect::from_min_size(full.min, vec2(LABEL_WIDTH, full.height()));
    let state = match (deck.master, deck.playing) {
        (true, _) => "MAÎTRE",
        (false, true) => "LECTURE",
        (false, false) => "PAUSE",
    };
    let shown = DeckLabel { number: deck.deck, state, master: deck.master, bpm: deck.bpm };
    deck_label(&painter, label, shown, lead, compact);

    let rect = Rect::from_min_max(pos2(label.right() + 8.0, full.top()), full.max);
    let track = &deck.track;
    let start = deck.position_ms - PAST_MS;
    let span = PAST_MS + AHEAD_MS;
    let x = |ms: f64| rect.left() + ((ms - start) / span) as f32 * rect.width();
    let phrases = phrases(track);

    // Phrases : un bandeau de couleur, nommé quand il y a la place ; pas en 4 pistes, où seule la
    // miniature du morceau les montre.
    let band = Rect::from_min_size(rect.min, vec2(rect.width(), if compact { 0.0 } else { PHRASES_HEIGHT }));
    for (name, from, to) in phrases.iter().filter(|_| !compact) {
        let (left, right) = (x(*from).max(band.left()), x(*to).min(band.right()));
        if right <= left {
            continue;
        }
        let color = fade(phrase_color(name));
        let block = Rect::from_x_y_ranges(left..=right - 1.0, band.y_range());
        painter.rect_filled(block, CornerRadius::same(3), color);
        if block.width() > 44.0 {
            painter.text(
                pos2(block.left() + 6.0, block.center().y),
                Align2::LEFT_CENTER,
                name.to_uppercase(),
                theme::font(9.5, SEMIBOLD),
                theme::ink_on(phrase_color(name)),
            );
        }
    }

    // Waveform : une colonne par point d'écran, le plus haut point et sa couleur ; le passé assombri.
    let wave_top = if compact { rect.top() } else { band.bottom() + GAP };
    let wave = Rect::from_min_size(pos2(rect.left(), wave_top), vec2(rect.width(), layout.wave_height()));
    painter.rect_filled(wave, CornerRadius::same(6), theme::black(0.35));
    if let Some(waveform) = &track.waveform {
        let columns = wave.width().floor() as usize;
        let ms_per_column = span / columns.max(1) as f64;
        let index = |ms: f64| (ms.max(0.0) / 1000.0 * waveform.per_second) as usize;
        for column in 0..columns {
            let from = start + column as f64 * ms_per_column;
            if from < 0.0 {
                continue;
            }
            let last = index(from + ms_per_column).min(waveform.points.len().saturating_sub(1));
            let Some(points) = waveform.points.get(index(from)..=last) else { continue };
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
            let color = fade(if from < deck.position_ms { color.gamma_multiply(0.4) } else { color });
            let half = top.height * (wave.height() / 2.0 - 3.0);
            let cx = wave.left() + column as f32 + 0.5;
            painter.line_segment([pos2(cx, wave.center().y - half), pos2(cx, wave.center().y + half)], Stroke::new(1.0, color));
        }
    }
    // Mesures : un trait discret à chaque premier temps.
    if let Some(grid) = &track.grid {
        for beat in grid.beats.iter().filter(|b| b.bar_beat == 1) {
            let bx = x(f64::from(beat.time_ms));
            if (wave.left()..=wave.right()).contains(&bx) {
                painter.line_segment([pos2(bx, wave.top()), pos2(bx, wave.top() + 5.0)], Stroke::new(1.0, theme::white(0.25)));
            }
        }
    }
    // Tête de lecture.
    let head = x(deck.position_ms);
    painter.line_segment([pos2(head, rect.top()), pos2(head, wave.bottom())], Stroke::new(2.0, fade(Color32::WHITE)));

    // Tout le morceau en miniature, avec la position.
    let overview = Rect::from_min_size(pos2(rect.left(), wave.bottom() + 2.0 * GAP), vec2(rect.width(), OVERVIEW_HEIGHT));
    let total = phrases.last().map_or(0.0, |(_, _, to)| *to).max(1.0);
    let ox = |ms: f64| overview.left() + (ms / total).clamp(0.0, 1.0) as f32 * overview.width();
    painter.rect_filled(overview, CornerRadius::same(2), theme::white(0.08));
    for (name, from, to) in &phrases {
        let block = Rect::from_x_y_ranges(ox(*from)..=(ox(*to) - 1.0).max(ox(*from)), overview.y_range());
        painter.rect_filled(block, CornerRadius::ZERO, fade(phrase_color(name).gamma_multiply(0.7)));
    }
    let marker = ox(deck.position_ms);
    painter.add(egui::Shape::convex_polygon(
        vec![Pos2::new(marker - 4.0, overview.bottom() + 4.0), Pos2::new(marker + 4.0, overview.bottom() + 4.0), Pos2::new(marker, overview.top())],
        fade(Color32::WHITE),
        Stroke::NONE,
    ));
    let hint = if lead {
        "Platine qui mène la lumière : phrases rekordbox, waveform (le passé à gauche de la tête de lecture), tout le morceau dessous"
    } else {
        "Autre platine chargée, montée même volume coupé ; la lumière suit la platine maître"
    };
    response.on_hover_text(hint);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn deck(number: u8, track: u32) -> DeckView {
        DeckView {
            deck: number,
            track: Arc::new(Track { id: track, ..Track::default() }),
            position_ms: 0.0,
            playing: true,
            master: false,
            on_air: false,
            bpm: None,
        }
    }

    #[test]
    fn a_track_loaded_on_deck_3_or_4_asks_for_four_tracks_once() {
        let mut seen = [0; 2];
        assert!(!loaded_outside(&mut seen, &[deck(1, 10), deck(2, 11)]), "platines centrales seules");
        assert!(loaded_outside(&mut seen, &[deck(1, 10), deck(3, 12)]), "un morceau arrive sur la 3");
        // Revenu en 2 pistes, on y reste tant que rien de nouveau n'arrive sur 3 ou 4…
        assert!(!loaded_outside(&mut seen, &[deck(1, 10), deck(3, 12)]));
        // … même quand la 3 joue ; un autre morceau sur la 4, si.
        assert!(loaded_outside(&mut seen, &[deck(3, 12), deck(4, 13)]));
        assert!(loaded_outside(&mut seen, &[deck(3, 14), deck(4, 13)]), "nouveau morceau sur la 3");
    }

    #[test]
    fn four_tracks_follow_the_opus_order() {
        assert_eq!(Layout::Two.decks(), &[1, 2]);
        assert_eq!(Layout::Four.decks(), &[3, 1, 2, 4]);
        assert!(height(Layout::Four) > height(Layout::Two));
    }
}

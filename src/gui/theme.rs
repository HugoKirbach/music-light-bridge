//! Allure de la fenêtre : couleurs, polices de Windows, style egui et petits éléments dessinés à
//! la main (cartes, sélecteur segmenté, pastilles de couleur, icônes).

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, Response,
    Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use eframe::epaint::{Mesh, text::LayoutJob, text::TextFormat};

pub const BG: Color32 = Color32::from_rgb(0x0D, 0x0E, 0x11);
pub const PANEL: Color32 = Color32::from_rgb(0x12, 0x13, 0x18);
pub const CARD: Color32 = Color32::from_rgb(0x15, 0x17, 0x1C);
pub const RAISED: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x24);
pub const SELECTED: Color32 = Color32::from_rgb(0x2A, 0x2E, 0x36);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2A, 0x32);
pub const BORDER_HOVER: Color32 = Color32::from_rgb(0x3A, 0x3F, 0x49);
pub const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEE, 0xF1);
pub const MUTED: Color32 = Color32::from_rgb(0xA0, 0xA6, 0xAF);
pub const LABEL: Color32 = Color32::from_rgb(0x8A, 0x90, 0x9A);
pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x2A);
/// Fond d'un choix sélectionné : l'accent à 10 % sur le fond.
pub const ACCENT_TINT: Color32 = Color32::from_rgb(0x34, 0x23, 0x17);
pub const LIVE: Color32 = Color32::from_rgb(0x34, 0xD1, 0x7F);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x5C);
pub const STAGE_BG: Color32 = Color32::from_rgb(0x08, 0x08, 0x0A);

pub const SEMIBOLD: &str = "semibold";
pub const BOLD: &str = "bold";
pub const MONO_BOLD: &str = "mono-bold";

pub fn font(size: f32, family: &str) -> FontId {
    FontId::new(size, FontFamily::Name(family.into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Texte dans une police et une couleur données.
pub fn text(text: impl Into<String>, font: FontId, color: Color32) -> egui::RichText {
    egui::RichText::new(text).font(font).color(color)
}

/// Texte espacé, pour les intitulés en capitales.
pub fn spaced(text: &str, font: FontId, color: Color32, spacing: f32) -> LayoutJob {
    LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font, color, extra_letter_spacing: spacing, ..Default::default() })
}

/// Intitulé de section : « 1 · SOURCE AUDIO ».
pub fn section(ui: &mut Ui, title: &str) {
    ui.label(spaced(title, font(11.0, BOLD), LABEL, 1.3));
}

/// Polices de Windows (Segoe UI, Consolas) ; celles d'egui restent en secours si elles manquent.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let proportional = fonts.families[&FontFamily::Proportional].clone();
    let monospace = fonts.families[&FontFamily::Monospace].clone();
    let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let mut family = |name: FontFamily, file: &str, fallback: &[String]| {
        let mut list = Vec::new();
        if let Ok(bytes) = std::fs::read(dir.join(file)) {
            fonts.font_data.insert(file.to_owned(), Arc::new(FontData::from_owned(bytes)));
            list.push(file.to_owned());
        }
        list.extend_from_slice(fallback);
        fonts.families.insert(name, list);
    };
    family(FontFamily::Proportional, "segoeui.ttf", &proportional);
    family(FontFamily::Name(SEMIBOLD.into()), "seguisb.ttf", &proportional);
    family(FontFamily::Name(BOLD.into()), "segoeuib.ttf", &proportional);
    family(FontFamily::Monospace, "consola.ttf", &monospace);
    family(FontFamily::Name(MONO_BOLD.into()), "consolab.ttf", &monospace);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        use egui::TextStyle;
        style.visuals = egui::Visuals::dark();
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = CARD;
        v.faint_bg_color = CARD;
        v.code_bg_color = RAISED;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = ACCENT;
        v.error_fg_color = DANGER;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0, BG);
        v.slider_trailing_fill = true;
        let radius = CornerRadius::same(8);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        for (state, fill, border) in [
            (&mut w.inactive, RAISED, BORDER),
            (&mut w.hovered, SELECTED, BORDER_HOVER),
            (&mut w.active, SELECTED, ACCENT),
            (&mut w.open, SELECTED, BORDER_HOVER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, border);
            state.fg_stroke = Stroke::new(1.5, TEXT);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 8.0);
        s.button_padding = vec2(14.0, 8.0);
        s.interact_size.y = 34.0;
        s.slider_rail_height = 4.0;
        style.text_styles = [
            (TextStyle::Small, regular(12.0)),
            (TextStyle::Body, regular(15.0)),
            (TextStyle::Button, regular(15.0)),
            (TextStyle::Heading, font(22.0, BOLD)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
    });
}

/// Réglages du dock de la scène : blanc à la place de l'accent, fonds translucides.
pub fn stage_style(ui: &mut Ui) {
    let v = &mut ui.style_mut().visuals;
    v.selection.bg_fill = Color32::WHITE;
    v.extreme_bg_color = white(0.06);
    for (state, alpha) in [
        (&mut v.widgets.inactive, 0.08),
        (&mut v.widgets.hovered, 0.14),
        (&mut v.widgets.active, 0.18),
        (&mut v.widgets.open, 0.14),
    ] {
        state.bg_fill = white(alpha);
        state.weak_bg_fill = white(alpha);
        state.bg_stroke = Stroke::new(1.0, white(0.14));
    }
    v.widgets.active.bg_stroke = Stroke::new(1.0, Color32::WHITE);
}

pub fn white(alpha: f32) -> Color32 {
    Color32::from_white_alpha((alpha * 255.0).round() as u8)
}

pub fn black(alpha: f32) -> Color32 {
    Color32::from_black_alpha((alpha * 255.0).round() as u8)
}

pub fn rgb(color: [u8; 3]) -> Color32 {
    Color32::from_rgb(color[0], color[1], color[2])
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Texte lisible sur une couleur : sombre sur les couleurs claires, clair sinon.
pub fn ink_on(color: Color32) -> Color32 {
    let luma = 0.299 * f32::from(color.r()) + 0.587 * f32::from(color.g()) + 0.114 * f32::from(color.b());
    if luma > 140.0 { BG } else { TEXT }
}

/// Dégradé radial elliptique, de `inner` au centre à `outer` sur le bord de l'ellipse, puis uni.
pub fn radial(painter: &egui::Painter, rect: Rect, center: Pos2, radii: Vec2, inner: Color32, outer: Color32) {
    const RINGS: usize = 24;
    const SEGMENTS: usize = 72;
    painter.rect_filled(rect, 0.0, outer);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, inner);
    for ring in 1..=RINGS {
        let t = ring as f32 / RINGS as f32;
        let color = mix(inner, outer, t);
        for segment in 0..SEGMENTS {
            let angle = segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(center + vec2(angle.cos() * radii.x, angle.sin() * radii.y) * t, color);
        }
    }
    let at = |ring: usize, segment: usize| (1 + (ring - 1) * SEGMENTS + segment % SEGMENTS) as u32;
    for segment in 0..SEGMENTS {
        mesh.add_triangle(0, at(1, segment), at(1, segment + 1));
    }
    for ring in 1..RINGS {
        for segment in 0..SEGMENTS {
            let (a, b) = (at(ring, segment), at(ring, segment + 1));
            let (c, d) = (at(ring + 1, segment), at(ring + 1, segment + 1));
            mesh.add_triangle(a, c, d);
            mesh.add_triangle(a, d, b);
        }
    }
    painter.with_clip_rect(rect).add(Shape::mesh(mesh));
}

#[derive(Clone, Copy)]
pub enum Icon {
    SoundCard,
    Speaker,
    Play,
    Stop,
    Record,
    Refresh,
}

pub fn icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let s = rect.width() / 22.0;
    let p = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let stroke = Stroke::new(1.8 * s, color);
    match icon {
        Icon::SoundCard => {
            painter.rect_stroke(Rect::from_min_max(p(3.0, 6.0), p(19.0, 16.0)), 2.0 * s, stroke, StrokeKind::Middle);
            painter.circle_stroke(p(8.0, 11.0), 2.0 * s, stroke);
            painter.circle_stroke(p(14.0, 11.0), 2.0 * s, stroke);
        }
        Icon::Speaker => {
            let body = vec![p(4.0, 9.0), p(8.0, 9.0), p(13.0, 5.0), p(13.0, 17.0), p(8.0, 13.0), p(4.0, 13.0)];
            painter.add(Shape::closed_line(body, stroke));
            let arc: Vec<Pos2> = (0..=8)
                .map(|i| {
                    let a = -0.9 + 1.8 * i as f32 / 8.0;
                    p(13.5 + 3.2 * a.cos(), 11.0 + 3.6 * a.sin())
                })
                .collect();
            painter.add(Shape::line(arc, stroke));
        }
        Icon::Play => {
            let points = vec![p(5.0, 3.0), p(19.0, 11.0), p(5.0, 19.0)];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_min_max(p(3.0, 3.0), p(19.0, 19.0)), 3.0 * s, color);
        }
        Icon::Record => {
            painter.circle_filled(p(11.0, 11.0), 7.0 * s, DANGER);
        }
        Icon::Refresh => {
            // Cercle presque fermé, flèche au bout.
            let arc: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = 0.6 + 4.9 * i as f32 / 20.0;
                    p(11.0 + 7.0 * a.cos(), 11.0 + 7.0 * a.sin())
                })
                .collect();
            let end = *arc.last().unwrap_or(&p(11.0, 11.0));
            painter.add(Shape::line(arc, stroke));
            let head = vec![end + vec2(-1.0, -4.5) * s, end + vec2(4.5, 1.0) * s, end + vec2(-1.5, 2.5) * s];
            painter.add(Shape::convex_polygon(head, color, Stroke::NONE));
        }
    }
}

/// Couleurs d'une carte de choix selon l'écran.
#[derive(Clone, Copy)]
pub struct CardColors {
    pub fill: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub border: Color32,
    pub border_selected: Color32,
    pub title: Color32,
    pub subtitle: Color32,
}

pub const SETUP_CARD: CardColors = CardColors {
    fill: CARD,
    hover: RAISED,
    selected: ACCENT_TINT,
    border: BORDER,
    border_selected: ACCENT,
    title: TEXT,
    subtitle: MUTED,
};

pub const STAGE_CARD: CardColors = CardColors {
    fill: Color32::TRANSPARENT,
    hover: Color32::from_rgba_premultiplied(14, 14, 14, 14),
    selected: Color32::from_rgba_premultiplied(26, 26, 26, 26),
    border: Color32::from_rgba_premultiplied(31, 31, 31, 31),
    border_selected: Color32::WHITE,
    title: Color32::WHITE,
    subtitle: Color32::from_rgb(0xB4, 0xB9, 0xC1),
};

/// Carte cliquable : icône facultative, titre, et une ligne d'explication.
pub fn card(
    ui: &mut Ui,
    size: Vec2,
    colors: &CardColors,
    selected: bool,
    with_icon: Option<Icon>,
    title: &str,
    subtitle: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let fill = if selected {
        colors.selected
    } else if response.hovered() {
        colors.hover
    } else {
        colors.fill
    };
    let (border, width) = if selected { (colors.border_selected, 1.5) } else { (colors.border, 1.0) };
    painter.rect_filled(rect, 12.0, fill);
    painter.rect_stroke(rect, 12.0, Stroke::new(width, border), StrokeKind::Inside);
    let pad = if size.y >= 96.0 { 16.0 } else { 12.0 };
    let mut y = rect.top() + pad;
    if let Some(kind) = with_icon {
        let color = if selected { ACCENT } else { MUTED };
        icon(painter, kind, Rect::from_min_size(pos2(rect.left() + pad, y), Vec2::splat(22.0)), color);
        y += 30.0;
    }
    let wrap = rect.width() - 2.0 * pad;
    let title = painter.layout(title.to_owned(), font(15.0, if selected { BOLD } else { SEMIBOLD }), colors.title, wrap);
    let title_height = title.size().y;
    painter.galley(pos2(rect.left() + pad, y), title, colors.title);
    y += title_height + 3.0;
    if !subtitle.is_empty() {
        let subtitle = painter.layout(subtitle.to_owned(), regular(13.0), colors.subtitle, wrap);
        painter.galley(pos2(rect.left() + pad, y), subtitle, colors.subtitle);
    }
    response
}

/// Sélecteur à segments ; rend l'option choisie quand elle change.
pub fn segmented(ui: &mut Ui, width: f32, options: &[&str], selected: usize) -> Option<usize> {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 48.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, RAISED);
    let inner = rect.shrink(4.0);
    let gap = 4.0;
    let segment = (inner.width() - gap * (options.len() - 1) as f32) / options.len() as f32;
    let mut chosen = None;
    for (i, label) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left() + i as f32 * (segment + gap), inner.top()), vec2(segment, inner.height()));
        let response = ui.interact(r, ui.scope_id().with(("segment", label)), Sense::click());
        let on = i == selected;
        let painter = ui.painter();
        if on {
            painter.rect_filled(r, 7.0, SELECTED);
        } else if response.hovered() {
            painter.rect_filled(r, 7.0, CARD);
        }
        let (family, color) = if on { (SEMIBOLD, TEXT) } else { ("", MUTED) };
        let font = if family.is_empty() { regular(14.0) } else { font(14.0, family) };
        painter.text(r.center(), Align2::CENTER_CENTER, *label, font, color);
        if response.clicked() && !on {
            chosen = Some(i);
        }
    }
    chosen
}

/// Grand bouton plein, avec une icône devant le texte.
pub fn big_button(ui: &mut Ui, size: Vec2, fill: Color32, ink: Color32, with_icon: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() { mix(fill, Color32::WHITE, 0.12) } else { fill };
    let painter = ui.painter();
    painter.rect_filled(rect, 10.0, fill);
    let galley = painter.layout_no_wrap(label.to_owned(), font(17.0, BOLD), ink);
    let content = 16.0 + 10.0 + galley.size().x;
    let left = rect.center().x - content / 2.0;
    icon(painter, with_icon, Rect::from_center_size(pos2(left + 8.0, rect.center().y), Vec2::splat(16.0)), ink);
    painter.galley(pos2(left + 26.0, rect.center().y - galley.size().y / 2.0), galley, ink);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Petit bouton discret avec une icône, grisé quand l'interface est désactivée.
pub fn small_button(ui: &mut Ui, with_icon: Icon, label: &str) -> Response {
    let enabled = ui.is_enabled();
    let color = if enabled { TEXT } else { LABEL };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font(13.0, SEMIBOLD), color);
    let size = vec2(12.0 + 14.0 + 8.0 + galley.size().x + 12.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let fill = if enabled && response.hovered() { SELECTED } else { RAISED };
    painter.rect_filled(rect, 8.0, fill);
    painter.rect_stroke(rect, 8.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let glyph = Rect::from_center_size(pos2(rect.left() + 12.0 + 7.0, rect.center().y), Vec2::splat(14.0));
    icon(painter, with_icon, glyph, if enabled { ACCENT } else { LABEL });
    painter.galley(pos2(glyph.right() + 8.0, rect.center().y - galley.size().y / 2.0), galley, color);
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// Choix à cocher en forme de pastille : case à gauche, libellé à droite.
pub fn chip(ui: &mut Ui, label: &str, checked: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font(14.0, SEMIBOLD), TEXT);
    let size = vec2(galley.size().x + 14.0 + 16.0 + 10.0 + 16.0, 40.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let fill = if checked {
        ACCENT_TINT
    } else if response.hovered() {
        RAISED
    } else {
        CARD
    };
    painter.rect_filled(rect, 10.0, fill);
    let border = if checked { Stroke::new(1.5, ACCENT) } else { Stroke::new(1.0, BORDER) };
    painter.rect_stroke(rect, 10.0, border, StrokeKind::Inside);
    let tick = Rect::from_center_size(pos2(rect.left() + 14.0 + 8.0, rect.center().y), Vec2::splat(16.0));
    if checked {
        painter.rect_filled(tick, 4.0, ACCENT);
        let mark = vec![tick.min + vec2(3.5, 8.0), tick.min + vec2(6.5, 11.5), tick.min + vec2(12.5, 4.5)];
        painter.add(Shape::line(mark, Stroke::new(2.0, BG)));
    } else {
        painter.rect_stroke(tick, 4.0, Stroke::new(1.5, MUTED), StrokeKind::Inside);
    }
    let color = if checked { TEXT } else { MUTED };
    painter.galley(pos2(tick.right() + 10.0, rect.center().y - galley.size().y / 2.0), galley, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Pastille de couleur avec son nom en bas à gauche ; entourée quand on la modifie.
pub fn swatch(ui: &mut Ui, size: Vec2, color: Color32, label: &str, selected: bool, ring: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 10.0, color);
    let stroke = if selected {
        Stroke::new(2.0, ring)
    } else if response.hovered() {
        Stroke::new(1.0, BORDER_HOVER)
    } else {
        Stroke::new(1.0, BORDER)
    };
    painter.rect_stroke(rect, 10.0, stroke, StrokeKind::Outside);
    let ink = ink_on(color);
    painter.text(rect.left_bottom() + vec2(8.0, -8.0), Align2::LEFT_BOTTOM, label, font(11.0, BOLD), ink);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Pastille arrondie de texte, précédée d'un point de couleur s'il y en a un.
pub fn pill(ui: &mut Ui, label: LayoutJob, fill: Color32, dot: Option<Color32>) -> Response {
    let galley = ui.painter().layout_job(label);
    let dot_width = if dot.is_some() { 16.0 } else { 0.0 };
    let size = vec2(galley.size().x + 24.0 + dot_width, galley.size().y + 12.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, size.y / 2.0, fill);
    if let Some(color) = dot {
        painter.circle_filled(pos2(rect.left() + 16.0, rect.center().y), 4.0, color);
    }
    painter.galley(pos2(rect.left() + 12.0 + dot_width, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    response
}

/// Ligne de réglage : intitulé à gauche, valeur à droite, curseur sur toute la largeur.
pub fn slider_row(ui: &mut Ui, label: &str, value: String, slider: egui::Slider<'_>, label_color: Color32) -> bool {
    ui.horizontal(|ui| {
        ui.label(text(label, regular(13.0), label_color));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(text(value, FontId::monospace(13.0), label_color));
        });
    });
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = ui.available_width();
        ui.spacing_mut().interact_size.y = 20.0;
        ui.add_space(-4.0);
        ui.add(slider.show_value(false)).changed()
    })
    .inner
}


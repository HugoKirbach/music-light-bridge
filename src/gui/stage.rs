//! Scène du live : le fond prend la couleur de la lampe, le tempo s'affiche en très grand avec les
//! temps de la mesure, et un dock en bas garde les réglages modifiables pendant le morceau.

use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, Sense, Stroke, Ui, pos2, vec2};

use super::theme::{self, BOLD, Icon, LIVE, MONO_BOLD, SEMIBOLD, STAGE_BG};
use super::{App, MODES, Tab, flash_text, format_bpm, motion_text, rotation_text, spread_text};
use crate::engine::Status;
use crate::look::{Motion, Rotation, Spread};
use crate::tempo::TempoSource;

const DOCK_WIDTH: f32 = 1040.0;
const TABS: [(Tab, &str); 5] = [
    (Tab::Motion, "Animation"),
    (Tab::Colors, "Couleurs"),
    (Tab::Flashes, "Flashs"),
    (Tab::Tempo, "Tempo"),
    (Tab::Source, "Source"),
];
const SOFT: Color32 = Color32::from_rgb(0xD9, 0xDC, 0xE1);
const DIM: Color32 = Color32::from_rgb(0xB4, 0xB9, 0xC1);

impl App {
    pub(super) fn stage(&mut self, ui: &mut Ui, status: &Status) {
        let full = ui.max_rect();
        let center = pos2(full.center().x, full.top() + full.height() * 0.38);
        let radii = vec2(full.width() * 0.44, full.height() * 0.54);
        theme::radial(ui.painter(), full, center, radii, theme::rgb(status.color), STAGE_BG);

        let mut stop = false;
        egui::Panel::top("scène-haut")
            .frame(Frame::NONE.inner_margin(Margin::symmetric(28, 20)))
            .show_separator_line(false)
            .show(ui, |ui| stop = self.stage_header(ui, status));
        egui::Panel::bottom("dock")
            .frame(Frame::NONE.inner_margin(Margin { left: 16, right: 16, top: 0, bottom: 24 }))
            .show_separator_line(false)
            .resizable(false)
            .show(ui, |ui| self.dock(ui, status));
        egui::CentralPanel::default().frame(Frame::NONE).show(ui, |ui| self.stage_center(ui, status));
        if stop {
            self.stop();
        }
    }

    /// En-tête : nom, durée du live, source captée et bouton d'arrêt ; rend vrai si on arrête.
    fn stage_header(&self, ui: &mut Ui, status: &Status) -> bool {
        let Some(engine) = &self.engine else { return false };
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.set_min_height(44.0);
            ui.label(theme::spaced("DJ-BRIDGE", theme::font(15.0, BOLD), Color32::WHITE, 2.7));
            let elapsed = self.started.elapsed().as_secs();
            let time = format!("LIVE {:02}:{:02}", elapsed / 60, elapsed % 60);
            theme::pill(ui, theme::spaced(&time, theme::font(12.0, BOLD), Color32::WHITE, 1.2), theme::black(0.45), Some(LIVE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (rect, response) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::click());
                let fill = if response.hovered() { theme::black(0.75) } else { theme::black(0.55) };
                ui.painter().circle_filled(rect.center(), 22.0, fill);
                theme::icon(ui.painter(), Icon::Stop, egui::Rect::from_center_size(rect.center(), vec2(14.0, 14.0)), Color32::WHITE);
                stop = response
                    .on_hover_text("Arrêter le live : les lampes reprennent leur état d'avant")
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
                let source = format!("{} · {}", engine.info.device, engine.info.area);
                theme::pill(ui, theme::spaced(&source, theme::regular(13.0), SOFT, 0.0), theme::black(0.45), None);
                if status.lamp_colors.len() > 1 {
                    lamp_dots(ui, &status.lamp_colors);
                }
            });
        });
        stop
    }

    /// Centre : phase du morceau, tempo, source du tempo et temps de la mesure.
    fn stage_center(&self, ui: &mut Ui, status: &Status) {
        let height = ui.available_height();
        let warning = self.warning(status);
        let others = 36.0 + 20.0 + 14.0 + 22.0 + 26.0 + 16.0 + if warning.is_some() { 52.0 } else { 0.0 };
        let bpm_size = ((height - others - 24.0) / 0.95).clamp(40.0, 200.0);
        let content = others + bpm_size * 0.95;
        ui.add_space(((height - content) / 2.0).max(0.0));
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let phase = match status.phrase {
                // Mesure dans la phrase de quatre, quand la structure est reconnue.
                Some(phrase) => format!("{} · MESURE {}/4", status.phase.label().to_uppercase(), phrase / 4 + 1),
                None => status.phase.label().to_uppercase(),
            };
            theme::pill(ui, theme::spaced(&phase, theme::font(13.0, BOLD), Color32::WHITE, 3.9), theme::black(0.45), None);
            ui.add_space(20.0);

            let bpm = format_bpm(self.shown_bpm);
            let painter = ui.painter();
            let galley = painter.layout_no_wrap(bpm.clone(), theme::font(bpm_size, MONO_BOLD), Color32::WHITE);
            let shadow = painter.layout_no_wrap(bpm, theme::font(bpm_size, MONO_BOLD), theme::black(0.3));
            let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x, bpm_size * 0.95), Sense::hover());
            let top = rect.center().y - galley.size().y / 2.0;
            ui.painter().galley(pos2(rect.left(), top + 6.0), shadow, theme::black(0.3));
            ui.painter().galley(pos2(rect.left(), top), galley, Color32::WHITE);
            ui.add_space(14.0);

            let source = match (status.source, status.link_peers) {
                (TempoSource::Link, Some(peers)) => format!("BPM · ABLETON LINK · {peers} PARTICIPANT(S)"),
                (TempoSource::Link, None) => "BPM · ABLETON LINK".to_owned(),
                (TempoSource::Manual, _) => "BPM · TEMPO IMPOSÉ".to_owned(),
                (TempoSource::Auto, _) => "BPM · DÉTECTION AUTO".to_owned(),
            };
            ui.label(theme::spaced(&source, theme::font(15.0, SEMIBOLD), Color32::from_rgb(0xE6, 0xE8, 0xEC), 4.5));
            ui.add_space(26.0);

            let (rect, response) = ui.allocate_exact_size(vec2(4.0 * 16.0 + 3.0 * 14.0, 16.0), Sense::hover());
            for i in 0..4u8 {
                let c = pos2(rect.left() + 8.0 + f32::from(i) * 30.0, rect.center().y);
                if status.beat.is_some_and(|beat| i <= beat) {
                    ui.painter().circle_filled(c, 8.0, Color32::WHITE);
                } else {
                    ui.painter().circle_stroke(c, 7.0, Stroke::new(2.0, theme::white(0.6)));
                }
            }
            response.on_hover_text(if status.phrase.is_some() {
                "Temps de la mesure, calés sur la structure du morceau"
            } else {
                "Temps de la mesure (le premier temps n'est pas encore repéré)"
            });

            if let Some(text) = warning {
                ui.add_space(28.0);
                let fill = Color32::from_rgba_unmultiplied(0x5C, 0x14, 0x14, 0xD0);
                theme::pill(ui, theme::spaced(&text, theme::font(13.0, SEMIBOLD), Color32::WHITE, 0.4), fill, None);
            }
        });
    }

    fn warning(&self, status: &Status) -> Option<String> {
        if let Some(message) = self.current_message() {
            Some(message.to_owned())
        } else if status.input_lost {
            Some("Entrée audio perdue : câble ou pilote ?".to_owned())
        } else if status.glitches > 0 {
            Some(format!("{} coupure(s) audio", status.glitches))
        } else {
            None
        }
    }

    /// Dock des réglages modifiables pendant le live, en onglets.
    fn dock(&mut self, ui: &mut Ui, status: &Status) {
        let available = ui.available_width();
        let width = available.min(DOCK_WIDTH);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(((available - width) / 2.0).floor());
            Frame::new()
                .fill(Color32::from_rgba_unmultiplied(14, 15, 18, 219))
                .stroke(Stroke::new(1.0, theme::white(0.08)))
                .corner_radius(18)
                .show(ui, |ui| ui.vertical(|ui| {
                    ui.set_width(width);
                    theme::stage_style(ui);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    Frame::new().inner_margin(Margin { left: 8, right: 8, top: 8, bottom: 0 }).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (tab, label) in TABS {
                                if tab_button(ui, label, self.tab == tab) {
                                    self.tab = tab;
                                }
                            }
                        });
                    });
                    let radius = CornerRadius { nw: 0, ne: 0, sw: 18, se: 18 };
                    Frame::new().fill(theme::white(0.04)).corner_radius(radius).inner_margin(Margin::same(18)).show(ui, |ui| {
                        ui.set_width(width - 36.0);
                        ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                        match self.tab {
                            Tab::Motion => self.motion_tab(ui, width - 36.0, status),
                            Tab::Colors => self.colors_tab(ui),
                            Tab::Flashes => self.flashes_tab(ui, width - 36.0),
                            Tab::Tempo => self.tempo_tab(ui, status),
                            Tab::Source => self.source_tab(ui, status),
                        }
                    });
                }));
        });
    }

    fn motion_tab(&mut self, ui: &mut Ui, width: f32, status: &Status) {
        ui.horizontal(|ui| {
            let size = vec2((width - 30.0) / 4.0, 76.0);
            for motion in Motion::ALL {
                let (title, mut subtitle) = (motion_text(motion).0, motion_text(motion).1.to_owned());
                if motion == Motion::Auto && self.settings.motion == Motion::Auto {
                    subtitle = format!("En ce moment : {}", motion_text(status.active_motion).0.to_lowercase());
                }
                let selected = self.settings.motion == motion;
                let card = theme::card(ui, size, &theme::STAGE_CARD, selected, None, title, &subtitle);
                if card.on_hover_text(motion.description()).clicked() {
                    self.set_motion(motion);
                }
            }
        });
        ui.horizontal(|ui| {
            let size = vec2((width - 40.0) / 5.0, 58.0);
            for spread in Spread::ALL {
                let (title, mut subtitle) = (spread_text(spread).0, spread_text(spread).1.to_owned());
                if spread == Spread::Auto && self.settings.spread == Spread::Auto {
                    subtitle = format!("En ce moment : {}", spread_text(status.active_spread).0.to_lowercase());
                }
                let selected = self.settings.spread == spread;
                let card = theme::card(ui, size, &theme::STAGE_CARD, selected, None, title, &subtitle);
                if card.on_hover_text(spread.description()).clicked() {
                    self.set_spread(spread);
                }
            }
        });
    }

    fn colors_tab(&mut self, ui: &mut Ui) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            self.swatches(ui, vec2(130.0, 72.0), Color32::WHITE);
            ui.add_space(14.0);
            ui.vertical(|ui| self.color_picker(ui, 150.0, DIM));
            ui.add_space(14.0);
            ui.vertical(|ui| {
                for rotation in Rotation::ALL {
                    let selected = self.settings.rotation == rotation;
                    let subtitle = if rotation == Rotation::Off { "Couleurs choisies" } else { "Et à chaque drop" };
                    let card = theme::card(ui, vec2(150.0, 46.0), &theme::STAGE_CARD, selected, None, rotation_text(rotation), subtitle);
                    if card.on_hover_text(rotation.description()).clicked() {
                        self.set_rotation(rotation);
                    }
                }
            });
        });
    }

    fn flashes_tab(&mut self, ui: &mut Ui, width: f32) {
        ui.columns(3, |columns| {
            for (which, column) in columns.iter_mut().enumerate() {
                self.flash_slider(column, which, DIM);
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let size = vec2((width - 20.0) / 3.0, 58.0);
            for mode in MODES {
                let (title, subtitle) = flash_text(mode);
                let selected = self.settings.flash == mode;
                let card = theme::card(ui, size, &theme::STAGE_CARD, selected, None, title, subtitle);
                if card.on_hover_text(mode.description()).clicked() {
                    self.set_flash(mode);
                }
            }
        });
    }

    fn tempo_tab(&mut self, ui: &mut Ui, status: &Status) {
        // Sorti le temps du rendu, pour que l'onglet puisse modifier l'application.
        let Some(engine) = self.engine.take() else { return };
        if engine.controls_tempo() {
            let mut result = Ok(());
            ui.horizontal(|ui| {
                ui.label(theme::text("BPM", theme::font(14.0, SEMIBOLD), Color32::WHITE));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.bpm_input)
                        .desired_width(110.0)
                        .margin(Margin::symmetric(12, 10))
                        .font(egui::FontId::monospace(16.0))
                        .hint_text(format_bpm(status.bpm)),
                );
                let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if stage_button(ui, "Imposer ce BPM") || entered {
                    result = self.parse_bpm().and_then(|bpm| match bpm {
                        Some(bpm) => engine.set_bpm(bpm),
                        None => Ok(()),
                    });
                }
                if stage_button(ui, "Détection auto") {
                    result = engine.set_auto();
                }
            });
            self.report(result);
            ui.label(theme::text(
                format!(
                    "Tempo {} · écart moyen kicks / temps ±{:.0} ms · réaction des flashs {:.0} ms",
                    status.source.label(),
                    status.offset_ms,
                    status.reaction_ms
                ),
                theme::regular(13.0),
                DIM,
            ));
        } else {
            let peers = status.link_peers.unwrap_or(0);
            ui.label(theme::text("Tempo et phase pilotés par la session Ableton Link.", theme::regular(15.0), Color32::WHITE));
            ui.label(theme::text(format!("{peers} autre(s) participant(s) dans la session."), theme::regular(13.0), DIM));
        }
        self.engine = Some(engine);
    }

    fn source_tab(&mut self, ui: &mut Ui, status: &Status) {
        let Some(engine) = self.engine.take() else { return };
        let mut capture = None;
        ui.columns(2, |columns| {
            let ui = &mut columns[0];
            let info = &engine.info;
            ui.label(theme::text(format!("{} · {}", info.device, info.backend), theme::font(15.0, SEMIBOLD), Color32::WHITE));
            let mut details = format!("{} Hz · {} canaux · zone « {} »", info.sample_rate, info.channels, info.area);
            if let Some(port) = &info.midi_port {
                details.push_str(&format!(" · MIDI « {port} »"));
            }
            ui.label(theme::text(details, theme::regular(13.0), DIM));
            if !info.lamps.is_empty() {
                ui.label(theme::text(format!("Lampes pilotées : {}", info.lamps.join(", ")), theme::regular(13.0), DIM));
            }
            if let Some(reason) = &info.fallback {
                ui.label(theme::text(format!("ASIO indisponible : {reason}"), theme::regular(12.0), DIM));
            }
            let levels = status.levels;
            for (name, level) in [("Graves", levels.bass), ("Médiums", levels.mid), ("Aigus", levels.high)] {
                meter(ui, name, level);
            }
            if theme::big_button(&mut columns[1], vec2(320.0, 44.0), theme::white(0.12), Color32::WHITE, Icon::Record, "Capturer les 40 dernières secondes")
                .on_hover_text("Sauve un WAV des 40 dernières secondes, pour `dj-bridge analyze`")
                .clicked()
            {
                capture = Some(engine.save_capture());
            }
            columns[1].add_space(6.0);
            self.threshold_slider(&mut columns[1], DIM);
        });
        match capture {
            Some(Ok(path)) => self.show(format!("Capture : {}", path.display())),
            Some(Err(e)) => self.show(format!("Capture impossible : {e:#}")),
            None => {}
        }
        self.engine = Some(engine);
    }
}

/// Onglet du dock ; rend vrai au clic.
fn tab_button(ui: &mut Ui, label: &str, selected: bool) -> bool {
    let font = if selected { theme::font(14.0, SEMIBOLD) } else { theme::regular(14.0) };
    let color = if selected { Color32::WHITE } else { DIM };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 36.0, 44.0), Sense::click());
    let fill = if selected {
        theme::white(0.08)
    } else if response.hovered() {
        theme::white(0.04)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, fill);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn stage_button(ui: &mut Ui, label: &str) -> bool {
    let button = egui::Button::new(theme::text(label, theme::font(14.0, SEMIBOLD), Color32::WHITE)).min_size(vec2(0.0, 40.0));
    ui.add(button).clicked()
}

/// Vu-mètre d'une bande, entre 0 et 1.
fn meter(ui: &mut Ui, name: &str, level: f32) {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 16.0], egui::Label::new(theme::text(name, theme::regular(12.0), DIM)));
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(260.0), 6.0), Sense::hover());
        ui.painter().rect_filled(rect, 3.0, theme::white(0.1));
        let filled = egui::Rect::from_min_size(rect.min, vec2(rect.width() * level.clamp(0.0, 1.0), rect.height()));
        ui.painter().rect_filled(filled, 3.0, Color32::WHITE);
    });
}


/// Couleur de chaque lampe, en pastilles (au plus 12), de droite à gauche dans l'en-tête.
fn lamp_dots(ui: &mut Ui, colors: &[[u8; 3]]) {
    let shown = colors.len().min(12);
    let (rect, response) = ui.allocate_exact_size(vec2(shown as f32 * 18.0 + 16.0, 30.0), Sense::hover());
    ui.painter().rect_filled(rect, 15.0, theme::black(0.45));
    for (i, color) in colors.iter().take(shown).enumerate() {
        let center = pos2(rect.left() + 17.0 + i as f32 * 18.0, rect.center().y);
        ui.painter().circle_filled(center, 6.0, theme::rgb(*color));
        ui.painter().circle_stroke(center, 6.0, Stroke::new(1.0, theme::white(0.25)));
    }
    response.on_hover_text("Couleur envoyée à chaque lampe de la zone");
}

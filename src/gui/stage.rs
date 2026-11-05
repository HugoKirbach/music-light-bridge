//! Scène du live : le fond prend la couleur de la lampe, le tempo s'affiche en très grand avec les
//! temps de la mesure, et un dock en bas garde les réglages modifiables pendant le morceau. Replié,
//! le dock ne montre que ses onglets ; un clic sur l'un d'eux l'ouvre, un second le referme.

use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, Sense, Stroke, Ui, pos2, vec2};

use super::light::{Page, STAGE_SKIN};
use super::theme::{self, BOLD, Icon, LIVE, MONO_BOLD, SEMIBOLD, STAGE_BG};
use super::{App, MODES, Tab, flash_text, format_bpm, waveform};
use crate::audio::WINDOWS_LOOPBACK;
use crate::engine::{MAX_LAMP_DELAY_MS, OPUS_OFFSET_RANGE, OpusStatus, Status};
use crate::tempo::TempoSource;

const DOCK_WIDTH: f32 = 1040.0;
/// Onglets du mode simple : de quoi changer d'ambiance et de lampes en plein morceau.
pub(super) const SIMPLE_TABS: [(Tab, &str); 3] =
    [(Tab::Light(Page::Presets), "Scènes"), (Tab::Light(Page::Colors), "Couleurs"), (Tab::Lamps, "Lampes")];
const EXPERT_TABS: [(Tab, &str); 7] = [
    (Tab::Light(Page::Motion), "Mouvement"),
    (Tab::Light(Page::Colors), "Couleurs"),
    (Tab::Light(Page::Flashes), "Flashs"),
    (Tab::Light(Page::Presets), "Scènes"),
    (Tab::Tempo, "Tempo"),
    (Tab::Lamps, "Lampes"),
    (Tab::Source, "Source"),
];
/// Que faire quand la sortie du PC n'entend pas Ableton.
const ASIO_HINT: &str = "Ableton en ASIO joue directement sur la carte, sans passer par Windows. Une carte avec \
     entrée Loopback (Scarlett 4e génération…) est écoutée d'office ; sinon, choisis MME/DirectX dans Ableton, ou \
     Ableton Link avec les flashs « Chaque temps ».";
/// Bascule du mode expert, à droite des onglets.
const MODE_CHIP: &str = "Expert";
/// Écart au-dessus de la bande du morceau, et place de la ligne des raccourcis.
const STRIP_GAP: f32 = 26.0;
const HINT: f32 = 26.0;
const SOFT: Color32 = Color32::from_rgb(0xD9, 0xDC, 0xE1);
const DIM: Color32 = Color32::from_rgb(0xB4, 0xB9, 0xC1);

impl App {
    pub(super) fn stage(&mut self, ui: &mut Ui, status: &Status) {
        self.shortcuts(ui);
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

    /// Raccourcis du live, hors saisie de texte : Espace pour le drop, 1 à 9 pour les scènes, Échap
    /// pour replier le dock.
    fn shortcuts(&mut self, ui: &Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        const DIGITS: [egui::Key; 9] = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        let (drop, scene, fold) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::Space),
                DIGITS.iter().position(|&key| input.key_pressed(key)),
                input.key_pressed(egui::Key::Escape),
            )
        });
        if drop && let Some(engine) = &self.engine {
            engine.drop_now();
        }
        if let Some(index) = scene {
            match self.cfg.presets.get(index).cloned() {
                Some(preset) => {
                    self.apply_preset(&preset);
                    self.show(format!("Scène « {} »", preset.name));
                }
                None => self.show(format!("Pas de scène n° {}", index + 1)),
            }
        }
        if fold {
            self.dock_open = false;
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
        let track = status.opus.as_ref().and_then(|opus| opus.now.as_ref()).filter(|now| now.data.grid.is_some());
        let strip = if track.is_some() { STRIP_GAP + waveform::HEIGHT } else { 0.0 };
        let others = 36.0 + 20.0 + 14.0 + 22.0 + 26.0 + 16.0 + HINT + strip + if warning.is_some() { 52.0 } else { 0.0 };
        let bpm_size = ((height - others - 24.0) / 0.95).clamp(40.0, 200.0);
        let content = others + bpm_size * 0.95;
        ui.add_space(((height - content) / 2.0).max(0.0));
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let phase = match (status.build, status.phrase) {
                // Avancement de la montée vers son drop.
                (Some(build), _) => match status.drop_beats {
                    Some(beats) => format!("{} · {build} % · DROP DANS {beats} T", status.phase.label().to_uppercase()),
                    None => format!("{} · {build} %", status.phase.label().to_uppercase()),
                },
                // Mesure dans la phrase de quatre, quand la structure est reconnue.
                (None, Some(phrase)) => format!("{} · MESURE {}/4", status.phase.label().to_uppercase(), phrase / 4 + 1),
                (None, None) => status.phase.label().to_uppercase(),
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
                (TempoSource::Opus, _) => opus_line(status),
                (_, _) if status.opus.is_some() => opus_line(status),
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

            if let Some(now) = track {
                ui.add_space(STRIP_GAP);
                let width = (ui.available_width() * 0.84).min(1000.0);
                waveform::strip(ui, now, width);
            }
            ui.add_space(12.0);
            ui.label(theme::spaced("ESPACE : DROP · 1 À 9 : SCÈNES", theme::font(11.0, SEMIBOLD), theme::white(0.5), 2.0));

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
        } else if status.bridge_lost {
            Some("Bridge injoignable : reconnexion en cours…".to_owned())
        } else if let Some(text) = status.opus.as_ref().and_then(opus_warning) {
            Some(text)
        } else if status.input_lost {
            Some("Entrée audio perdue (câble ou pilote ?) : reconnexion en cours…".to_owned())
        } else if status.silent && status.opus.is_none() {
            let text = if self.windows_loopback() {
                "Rien n'arrive de la sortie du PC : Ableton en ASIO n'y passe pas (onglet Source)"
            } else {
                "Aucun son capté depuis 10 s : vérifie l'appareil (onglet Source)"
            };
            Some(text.to_owned())
        } else if status.glitches > 0 {
            Some(format!("{} coupure(s) audio", status.glitches))
        } else {
            None
        }
    }

    /// La sortie du PC est captée par Windows, qui ne voit pas les logiciels en ASIO.
    fn windows_loopback(&self) -> bool {
        self.engine.as_ref().is_some_and(|engine| engine.info.backend == WINDOWS_LOOPBACK)
    }

    /// Dock des réglages modifiables pendant le live, en onglets (moins nombreux en mode simple).
    fn dock(&mut self, ui: &mut Ui, status: &Status) {
        let tabs: &[(Tab, &str)] = if self.settings.expert { &EXPERT_TABS } else { &SIMPLE_TABS };
        let available = ui.available_width();
        // Replié, le dock se resserre autour de ses onglets.
        let width = if self.dock_open {
            available.min(DOCK_WIDTH)
        } else {
            let tabs: f32 = tabs.iter().map(|(_, label)| tab_width(ui, label) + 4.0).sum();
            (tabs + tab_width(ui, MODE_CHIP) + 16.0).min(available)
        };
        let mut expert = None;
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
                    let bottom = if self.dock_open { 0 } else { 8 };
                    Frame::new().inner_margin(Margin { left: 8, right: 8, top: 8, bottom }).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for &(tab, label) in tabs {
                                let selected = self.dock_open && self.tab == tab;
                                if tab_button(ui, label, selected, self.dock_open) {
                                    (self.tab, self.dock_open) = (tab, !selected);
                                }
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if self.dock_open {
                                    let hide = ui.add(egui::Label::new(theme::text("Replier", theme::regular(13.0), DIM)).sense(Sense::click()));
                                    if hide.on_hover_text("Replier le dock : la scène reste dégagée").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                        self.dock_open = false;
                                    }
                                }
                                let hint = if self.settings.expert {
                                    "Mode expert : tous les réglages. Clic : revenir à l'essentiel"
                                } else {
                                    "Mode simple : l'essentiel. Clic : tous les réglages"
                                };
                                if theme::chip(ui, MODE_CHIP, self.settings.expert).on_hover_text(hint).clicked() {
                                    expert = Some(!self.settings.expert);
                                }
                            });
                        });
                    });
                    if !self.dock_open {
                        return;
                    }
                    let radius = CornerRadius { nw: 0, ne: 0, sw: 18, se: 18 };
                    Frame::new().fill(theme::white(0.04)).corner_radius(radius).inner_margin(Margin::same(18)).show(ui, |ui| {
                        ui.set_width(width - 36.0);
                        ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                        match self.tab {
                            Tab::Light(page) => self.light_page(ui, page, width - 36.0, &STAGE_SKIN, Some(status)),
                            Tab::Tempo => self.tempo_tab(ui, status),
                            Tab::Lamps => self.lamps_tab(ui, width - 36.0),
                            Tab::Source => self.source_tab(ui, status, width - 36.0),
                        }
                    });
                }));
        });
        if let Some(on) = expert {
            self.set_expert(on);
        }
    }

    fn tempo_tab(&mut self, ui: &mut Ui, status: &Status) {
        if let Some(opus) = &status.opus {
            ui.label(theme::text(
                "Tempo, temps, phrases et kicks suivent la platine maître de l'OPUS-QUAD.",
                theme::regular(15.0),
                Color32::WHITE,
            ));
            let route = match (&opus.link.opus, &opus.link.route) {
                (Some(ip), Some(route)) => format!("OPUS à {ip}, par « {route} »."),
                (Some(ip), None) => format!("OPUS à {ip}, mais aucune carte réseau du PC n'y mène."),
                (None, _) => "OPUS pas encore vu sur le réseau.".to_owned(),
            };
            ui.label(theme::text(route, theme::regular(13.0), DIM));
            if let Some(now) = &opus.now {
                let phrase = now.phrase.as_deref().unwrap_or("structure inconnue");
                let calage = if now.settled { "" } else { " · phase en cours de calage" };
                let line = format!("Platine {} · morceau {} · temps {} · {phrase}{calage}", now.deck, now.track, now.beat);
                ui.label(theme::text(line, theme::regular(13.0), DIM));
            }
            let mut offset = self.cfg.opus_offset_ms;
            let shown = format!("{offset:+.0} ms");
            let slider = egui::Slider::new(&mut offset, OPUS_OFFSET_RANGE.0..=OPUS_OFFSET_RANGE.1).step_by(5.0);
            if theme::slider_row(ui, "Retard de l'OPUS", shown, slider, DIM) {
                self.set_opus_offset(offset);
            }
            ui.label(theme::text(
                "Les flashs tombent après les kicks : monte ce retard. Avant : descends-le. Il s'ajoute à l'avance des lampes.",
                theme::regular(12.0),
                DIM,
            ));
            return;
        }
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

    /// Lampes du live : chacune s'allume ou retrouve son état d'avant d'un clic, sans arrêter le
    /// live. En mode expert, la synchro : le test fait flasher toutes les lampes en blanc sur chaque
    /// temps, et chaque lampe en retard sur les autres reçoit le sien.
    fn lamps_tab(&mut self, ui: &mut Ui, width: f32) {
        let Some(engine) = self.engine.take() else { return };
        let lamps = engine.lamps();
        let mut toggled = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for lamp in &lamps {
                let chip = theme::chip(ui, &lamp.name, lamp.enabled)
                    .on_hover_text(if lamp.enabled { "Allumée par le live : clic pour lui rendre son état d'avant" } else { "Clic pour l'allumer dans le live" });
                if chip.clicked() {
                    toggled = Some((lamp.id.clone(), !lamp.enabled));
                }
            }
        });
        ui.label(theme::text(
            "Une lampe décochée retrouve son état d'avant le live ; le choix est gardé pour les prochaines fois.",
            theme::regular(12.0),
            DIM,
        ));
        match toggled {
            Some((_, false)) if lamps.iter().filter(|lamp| lamp.enabled).count() == 1 => {
                self.show("Garde au moins une lampe pour le live.");
            }
            Some((id, on)) => {
                engine.set_lamp_enabled(&id, on);
                let excluded = &mut self.cfg.layout_mut().excluded;
                excluded.retain(|lamp| *lamp != id);
                if !on {
                    excluded.push(id);
                }
                self.save_config();
            }
            None => {}
        }
        if !self.settings.expert {
            self.engine = Some(engine);
            return;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let testing = engine.sync_test();
            if theme::chip(ui, "Test de synchro", testing)
                .on_hover_text("Toutes les lampes flashent en blanc sur chaque temps")
                .clicked()
            {
                engine.set_sync_test(!testing);
            }
            ui.label(theme::text(
                "Une lampe qui s'allume après les autres : monte son curseur jusqu'à ce qu'elles flashent ensemble.",
                theme::regular(13.0),
                DIM,
            ));
        });
        let shown = lamps.len().clamp(1, if width < 700.0 { 2 } else { 3 });
        ui.columns(shown, |columns| {
            for (i, lamp) in lamps.iter().enumerate() {
                let ui = &mut columns[i % shown];
                let mut ms = lamp.lag_ms;
                let shown_ms = format!("{ms:.0} ms");
                let slider = egui::Slider::new(&mut ms, 0.0..=MAX_LAMP_DELAY_MS).step_by(5.0);
                if theme::slider_row(ui, &lamp.name, shown_ms, slider, DIM) {
                    engine.set_lamp_delay(&lamp.id, ms);
                    if ms > 0.0 {
                        self.cfg.lamp_delays_ms.insert(lamp.id.clone(), ms);
                    } else {
                        self.cfg.lamp_delays_ms.remove(&lamp.id);
                    }
                }
            }
        });
        self.engine = Some(engine);
    }

    fn source_tab(&mut self, ui: &mut Ui, status: &Status, width: f32) {
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
                ui.label(theme::text(reason, theme::regular(12.0), DIM));
            }
            if status.silent && info.backend == WINDOWS_LOOPBACK {
                ui.label(theme::text(ASIO_HINT, theme::regular(13.0), Color32::WHITE));
            }
            if let Some(url) = &info.remote_url {
                ui.label(theme::text(format!("Télécommande : {url}"), theme::font(13.0, SEMIBOLD), Color32::WHITE));
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
        match capture {
            Some(Ok(path)) => self.show(format!("Capture : {}", path.display())),
            Some(Err(e)) => self.show(format!("Capture impossible : {e:#}")),
            None => {}
        }
        self.engine = Some(engine);
    }
}

/// Onglet du dock ; rend vrai au clic. Dock replié, les onglets sont des boutons arrondis.
fn tab_button(ui: &mut Ui, label: &str, selected: bool, open: bool) -> bool {
    let font = if selected { theme::font(14.0, SEMIBOLD) } else { theme::regular(14.0) };
    let color = if selected { Color32::WHITE } else { DIM };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let (rect, response) = ui.allocate_exact_size(vec2(tab_width(ui, label), 44.0), Sense::click());
    let fill = if selected {
        theme::white(0.08)
    } else if response.hovered() {
        theme::white(0.04)
    } else {
        Color32::TRANSPARENT
    };
    let radius = if open { CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 } } else { CornerRadius::same(10) };
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Largeur d'un onglet du dock, la même sélectionné ou non.
fn tab_width(ui: &Ui, label: &str) -> f32 {
    ui.painter().layout_no_wrap(label.to_owned(), theme::font(14.0, SEMIBOLD), Color32::WHITE).size().x + 36.0
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


/// Ligne de la source quand l'OPUS est suivi : la platine qui mène, la phrase rekordbox en cours
/// et la suivante.
fn opus_line(status: &Status) -> String {
    let Some(now) = status.opus.as_ref().and_then(|opus| opus.now.as_ref()) else {
        return "BPM · OPUS-QUAD · EN ATTENTE D'UNE PLATINE EN LECTURE".to_owned();
    };
    let mut line = format!("BPM · OPUS · PLATINE {}", now.deck);
    if let Some(phrase) = &now.phrase {
        line.push_str(&format!(" · {}", phrase.to_uppercase()));
    }
    if let (Some(next), Some(left)) = (&now.next, now.beats_left) {
        line.push_str(&format!(" → {} DANS {left} T", next.to_uppercase()));
    }
    if !now.settled {
        line.push_str(" · CALAGE");
    }
    line
}

/// Ce qui empêche l'OPUS de mener la lumière, s'il y a lieu.
fn opus_warning(opus: &OpusStatus) -> Option<String> {
    if let Some(error) = &opus.error {
        return Some(format!("Liaison OPUS impossible : {error}"));
    }
    match (opus.link.opus, opus.link.answered, opus.link.live) {
        (None, _, _) => Some("OPUS pas encore vu : même Wi-Fi que le PC ? Autorise dj-bridge sur le réseau si Windows le demande".to_owned()),
        (Some(ip), false, _) => Some(format!("OPUS vu à {ip} : en attente de sa réponse (rekordbox fermé ?)")),
        (Some(_), true, false) => Some("Plus rien de l'OPUS depuis 1 s : liaison coupée ?".to_owned()),
        (Some(_), true, true) => None,
    }
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

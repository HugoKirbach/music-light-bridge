//! Écran de préparation, avant le lancement : au centre l'essentiel de la session (source, tempo,
//! lampes, lancement), le reste replié dans « Plus d'options » ; à droite l'aperçu et les réglages
//! de la lumière, une page à la fois.

use std::sync::PoisonError;

use eframe::egui::{self, Align, Frame, Layout, Margin, Sense, Stroke, Ui, pos2, vec2};

use super::light::{PAGES, SETUP_SKIN};
use super::theme::{self, ACCENT, BG, BOLD, BORDER, CARD, DANGER, Icon, LABEL, LIVE, MUTED, PANEL, RAISED, SEMIBOLD, TEXT};
use super::{App, Bridge, MODES, TempoChoice, flash_text};
use crate::audio::{self, Backend};
use crate::tempo::DEFAULT_BPM_RANGE;

const ASIDE_WIDTH: f32 = 360.0;
const DRIVERS: [(Backend, &str); 3] =
    [(Backend::Auto, "Auto (ASIO si libre)"), (Backend::Asio, "ASIO"), (Backend::Wasapi, "WASAPI")];
const OPTION_GAP: f32 = 12.0;
const TEMPOS: [(TempoChoice, &str); 3] =
    [(TempoChoice::Auto, "Détection auto"), (TempoChoice::Manual, "BPM imposé"), (TempoChoice::Link, "Ableton Link")];

impl App {
    pub(super) fn setup(&mut self, ui: &mut Ui) {
        egui::Panel::top("en-tête")
            .exact_size(60.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(24, 0)))
            .show(ui, |ui| self.header(ui));
        egui::Panel::right("allure")
            .exact_size(ASIDE_WIDTH)
            .resizable(false)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::same(24)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.aside(ui));
            });
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(40, 24))).show(
            ui,
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_max_width(780.0);
                    self.session(ui);
                });
            },
        );
    }

    fn header(&mut self, ui: &mut Ui) {
        let bridge = self.bridge.lock().unwrap_or_else(PoisonError::into_inner);
        ui.horizontal_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            ui.painter().circle_stroke(rect.center(), 11.0, Stroke::new(2.0, ACCENT));
            ui.painter().circle_filled(rect.center(), 4.0, ACCENT);
            ui.label(theme::text("DJ-Bridge", theme::font(17.0, BOLD), TEXT));
            ui.add_space(12.0);
            let (state, dot) = match &*bridge {
                Bridge::Ready { .. } => ("PRÊT", LIVE),
                Bridge::Pending => ("CONNEXION…", LABEL),
                Bridge::Failed(_) => ("BRIDGE INJOIGNABLE", DANGER),
                Bridge::Unpaired => ("NON ASSOCIÉ", DANGER),
            };
            theme::pill(ui, theme::spaced(state, theme::font(12.0, BOLD), MUTED, 1.0), RAISED, Some(dot));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &*bridge {
                Bridge::Ready { area, lamps } => {
                    let piloted = lamps.iter().filter(|lamp| !self.cfg.excluded().contains(&lamp.id)).count();
                    let lamps = match (lamps.len(), piloted) {
                        (1, 1) => "1 lampe".to_owned(),
                        (total, piloted) if total == piloted => format!("{total} lampes"),
                        (total, piloted) => format!("{piloted} lampe(s) sur {total}"),
                    };
                    ui.label(theme::text(format!("Zone « {area} » · {lamps}"), theme::regular(13.0), MUTED));
                    ui.label(theme::text("Bridge Hue connecté", theme::regular(13.0), TEXT));
                }
                Bridge::Pending => {
                    ui.label(theme::text("Recherche de la zone de divertissement…", theme::regular(13.0), MUTED));
                }
                Bridge::Failed(reason) => {
                    ui.label(theme::text(reason, theme::regular(13.0), MUTED)).on_hover_text(reason);
                }
                Bridge::Unpaired => {
                    ui.label(theme::text("Lance d'abord `dj-bridge pair` dans un terminal.", theme::regular(13.0), MUTED));
                }
            });
        });
    }

    /// Colonne centrale : source, tempo, MIDI et lancement.
    fn session(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(theme::text("Préparer la session", theme::font(28.0, BOLD), TEXT));
        ui.label(theme::text(
            "Choisis la source et le tempo, puis lance. La lumière se règle à droite, et reste réglable pendant le live.",
            theme::regular(15.0),
            MUTED,
        ));
        let width = ui.available_width();

        ui.add_space(12.0);
        theme::section(ui, "1 · SOURCE AUDIO");
        let output = self.settings.audio == Backend::Output;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let size = vec2((width - 12.0) / 2.0, 100.0);
            let card = theme::card(
                ui,
                size,
                &theme::SETUP_CARD,
                !output,
                Some(Icon::SoundCard),
                "Carte son",
                "Platines ou synthé branchés sur la Focusrite",
            );
            if card.clicked() && output {
                self.settings.audio = self.card_backend;
                self.settings.input = None;
            }
            let card = theme::card(
                ui,
                size,
                &theme::SETUP_CARD,
                output,
                Some(Icon::Speaker),
                "Sortie du PC",
                "Ce que joue le PC : Spotify, Ableton…",
            );
            if card.clicked() && !output {
                self.card_backend = self.settings.audio;
                self.settings.audio = Backend::Output;
                self.settings.input = None;
            }
        });
        ui.add_space(4.0);
        self.device_row(ui, width);

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            theme::section(ui, "2 · LAMPES");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let pending = matches!(*self.bridge.lock().unwrap_or_else(PoisonError::into_inner), Bridge::Pending);
                let label = if pending { "Lecture de la zone…" } else { "Mettre à jour" };
                let button = ui.add_enabled_ui(!pending && self.cfg.bridge.is_some(), |ui| {
                    theme::small_button(ui, Icon::Refresh, label)
                });
                if button.inner.on_hover_text("Relire la zone de divertissement, si elle a changé dans l'app Hue").clicked() {
                    self.refresh_bridge();
                }
            });
        });
        self.lamps_row(ui);

        ui.add_space(12.0);
        theme::section(ui, "3 · TEMPO");
        self.tempo_row(ui, width);

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if theme::big_button(ui, vec2(250.0, 56.0), ACCENT, BG, Icon::Play, "Lancer le live").clicked() {
                self.start();
            }
            ui.label(theme::text("L'état actuel des lampes sera rétabli à l'arrêt.", theme::regular(13.0), MUTED));
        });

        // Le reste, replié sous le lancement pour qu'il reste à portée de main.
        ui.add_space(8.0);
        let toggled = disclosure(ui, width, self.more_options, "Plus d'options", &self.options_summary());
        if toggled {
            self.more_options = !self.more_options;
        }
        if self.more_options {
            let options = ui.scope(|ui| self.options(ui, width)).response;
            if toggled {
                // Les options s'ouvrent sous le bouton de lancement : on les amène à l'écran.
                options.scroll_to_me(Some(Align::Max));
            }
        }

        if let Some(message) = self.current_message() {
            ui.add_space(8.0);
            Frame::new()
                .fill(theme::mix(BG, DANGER, 0.14))
                .stroke(Stroke::new(1.0, theme::mix(BG, DANGER, 0.5)))
                .corner_radius(10)
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::text(message, theme::regular(14.0), TEXT));
                });
        }
        if let Some(summary) = &self.summary {
            ui.add_space(8.0);
            Frame::new()
                .fill(CARD)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(10)
                .inner_margin(Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::section(ui, "DERNIÈRE SESSION");
                    ui.label(theme::text(summary, theme::regular(13.0), MUTED));
                });
        }
    }

    /// Appareil capté : une entrée de la carte son, ou une sortie du PC.
    fn device_row(&mut self, ui: &mut Ui, width: f32) {
        let output = self.settings.audio == Backend::Output;
        ui.horizontal(|ui| {
            ui.set_width(width);
            ui.label(theme::text("Appareil", theme::regular(13.0), MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let refresh = ui.add(egui::Label::new(theme::text("Actualiser", theme::regular(13.0), ACCENT)).sense(Sense::click()));
                if refresh.on_hover_text("Relire les entrées audio et MIDI, et les lampes de la zone").clicked() {
                    self.refresh_devices();
                }
            });
        });
        let (default, names) = if output { ("Sortie par défaut de Windows", &self.outputs) } else { ("Automatique (Focusrite)", &self.inputs) };
        let selected = self.settings.input.clone().unwrap_or_else(|| default.to_owned());
        egui::ComboBox::from_id_salt("appareil")
            .width(width)
            .selected_text(theme::text(selected, theme::regular(14.0), TEXT))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.input, None, default);
                for name in names {
                    ui.selectable_value(&mut self.settings.input, Some(name.clone()), name);
                }
            });
        if output {
            // La sortie choisie, ou celle par défaut de Windows (la première de la liste).
            let chosen = self.settings.input.as_ref().or(self.outputs.first());
            let note = match chosen.and_then(|output| audio::card_loopback(output, &self.inputs)) {
                Some(loopback) => format!("Écoute l'entrée « {loopback} » de la carte : tout ce qu'elle joue, Ableton en ASIO compris."),
                None => "Capte ce que Windows joue sur cette sortie, sans rien brancher. Ableton en ASIO n'y passe pas : \
                         choisis MME/DirectX dans Ableton, ou Ableton Link."
                    .to_owned(),
            };
            ui.label(theme::text(note, theme::regular(12.0), LABEL));
        }
    }

    /// Résumé des options repliées, pour voir d'un coup d'œil ce qui sort de l'ordinaire.
    fn options_summary(&self) -> String {
        let mut parts = Vec::new();
        if self.settings.audio != Backend::Output {
            let driver = match self.settings.audio {
                Backend::Asio => "pilote ASIO",
                Backend::Wasapi => "pilote WASAPI",
                _ => "pilote auto",
            };
            parts.push(driver.to_owned());
        }
        parts.push(match &self.settings.midi {
            Some(port) if !port.is_empty() => format!("MIDI « {port} »"),
            Some(_) => "MIDI activé".to_owned(),
            None => "sans MIDI".to_owned(),
        });
        parts.push(format!("flashs : {}", flash_text(self.settings.flash).0.to_lowercase()));
        parts.push(format!("seuil {:.2}", self.settings.threshold));
        if self.settings.remote {
            parts.push("télécommande".to_owned());
        }
        parts.join(" · ")
    }

    /// Options repliées : pilote de la carte son, MIDI, télécommande, déclenchement des flashs et
    /// seuil des kicks.
    fn options(&mut self, ui: &mut Ui, width: f32) {
        ui.add_space(4.0);
        if self.settings.audio != Backend::Output {
            ui.label(theme::text("Pilote de la carte son", theme::regular(13.0), MUTED));
            let selected = DRIVERS.iter().find(|(b, _)| *b == self.settings.audio).map_or("Auto", |(_, l)| *l);
            egui::ComboBox::from_id_salt("pilote")
                .width(width.min(320.0))
                .selected_text(theme::text(selected, theme::regular(14.0), TEXT))
                .show_ui(ui, |ui| {
                    for (backend, label) in DRIVERS {
                        ui.selectable_value(&mut self.settings.audio, backend, label);
                    }
                })
                .response
                .on_hover_text("ASIO a la latence la plus faible, mais n'accepte souvent qu'un logiciel à la fois");
            ui.add_space(OPTION_GAP);
        }
        theme::section(ui, "MIDI");
        self.midi_row(ui);
        ui.add_space(OPTION_GAP);
        theme::section(ui, "TÉLÉCOMMANDE");
        self.remote_row(ui);
        ui.add_space(OPTION_GAP);
        theme::section(ui, "DÉCLENCHEMENT DES FLASHS");
        ui.horizontal(|ui| {
            let size = vec2((width - 16.0) / 3.0, 58.0);
            for mode in MODES {
                let (title, subtitle) = flash_text(mode);
                let selected = self.settings.flash == mode;
                let card = theme::card(ui, size, &theme::SETUP_CARD, selected, None, title, subtitle);
                if card.on_hover_text(mode.description()).clicked() {
                    self.set_flash(mode);
                }
            }
        });
        ui.add_space(4.0);
        ui.allocate_ui(vec2(width.min(420.0), 0.0), |ui| self.threshold_slider(ui, MUTED));
    }

    /// Lampes de la zone, à cocher : celles décochées gardent leur état pendant le live.
    fn lamps_row(&mut self, ui: &mut Ui) {
        let bridge = self.bridge.clone();
        let bridge = bridge.lock().unwrap_or_else(PoisonError::into_inner);
        let lamps = match &*bridge {
            Bridge::Ready { lamps, .. } => lamps,
            Bridge::Failed(reason) => {
                ui.label(theme::text(format!("Zone illisible : {reason}"), theme::regular(13.0), DANGER));
                return;
            }
            _ => {
                ui.label(theme::text("Les lampes s'affichent dès que le bridge répond.", theme::regular(13.0), LABEL));
                return;
            }
        };
        let mut toggled = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for lamp in lamps {
                let checked = !self.cfg.excluded().contains(&lamp.id);
                let chip = theme::chip(ui, &lamp.name, checked);
                let chip = if lamp.channels.len() > 1 {
                    chip.on_hover_text(format!("{} segments, éclairés ensemble", lamp.channels.len()))
                } else {
                    chip
                };
                if chip.clicked() {
                    toggled = Some((lamp.id.clone(), checked));
                }
            }
        });
        let piloted = lamps.iter().filter(|lamp| !self.cfg.excluded().contains(&lamp.id)).count();
        let lamps = lamps.clone();
        drop(bridge);
        match toggled {
            Some((_, true)) if piloted == 1 => self.show("Garde au moins une lampe pour le live."),
            Some((id, true)) => {
                self.cfg.layout_mut().excluded.push(id);
                self.save_config();
            }
            Some((id, false)) => {
                self.cfg.layout_mut().excluded.retain(|excluded| *excluded != id);
                self.save_config();
            }
            None => {}
        }
        ui.label(theme::text(
            "Lampes décochées : elles gardent leur état pendant le live (choix propre à chaque configuration). Zone modifiée dans l'app Hue ? « Mettre à jour » la relit.",
            theme::regular(12.0),
            LABEL,
        ));

        // Le plan de la pièce, replié : il sert à la mise en place, pas à chaque session.
        let summary = format!("« {} » · configuration « {} »", self.cfg.room().name, self.cfg.layout().name);
        if disclosure(ui, ui.available_width(), self.show_room, "Pièce et plan des lampes", &summary) {
            self.show_room = !self.show_room;
        }
        if self.show_room {
            self.room_panel(ui, &lamps);
        }
    }

    fn tempo_row(&mut self, ui: &mut Ui, width: f32) {
        ui.allocate_ui_with_layout(vec2(width, 48.0), Layout::left_to_right(Align::Max), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let segmented_width = ((width - 12.0) * 0.64).floor();
            let current = TEMPOS.iter().position(|(t, _)| *t == self.tempo).unwrap_or(0);
            let labels = TEMPOS.map(|(_, label)| label);
            if let Some(i) = theme::segmented(ui, segmented_width, &labels, current) {
                self.tempo = TEMPOS[i].0;
            }
            let rest = width - segmented_width - 12.0;
            ui.vertical(|ui| {
                ui.set_width(rest);
                let (label, hint) = match self.tempo {
                    TempoChoice::Auto => ("Plage", ""),
                    TempoChoice::Manual => ("BPM imposé", "128"),
                    TempoChoice::Link => ("BPM si tu crées la session", "120"),
                };
                ui.label(theme::text(label, theme::regular(13.0), MUTED));
                if self.tempo == TempoChoice::Auto {
                    let (min, max) = DEFAULT_BPM_RANGE;
                    Frame::new().fill(RAISED).corner_radius(8).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::text(format!("{min:.0} – {max:.0} BPM"), egui::FontId::monospace(14.0), TEXT));
                    })
                    .response
                    .on_hover_text("Tempos cherchés par la détection automatique");
                } else {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.bpm_input)
                            .desired_width(rest)
                            .margin(Margin::symmetric(12, 10))
                            .font(egui::FontId::monospace(15.0))
                            .hint_text(hint),
                    );
                }
            });
        });
        let note = match self.tempo {
            TempoChoice::Auto => "Le tempo se cale sur les kicks ; il tient pendant les breaks.",
            TempoChoice::Manual => "Le tempo reste fixe ; seule la phase se cale sur les kicks.",
            TempoChoice::Link => "Tempo et phase suivent la session Link (Ableton, synthé…).",
        };
        ui.label(theme::text(note, theme::regular(12.0), LABEL));
    }

    fn midi_row(&mut self, ui: &mut Ui) {
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(32.0);
                ui.horizontal(|ui| {
                    let mut enabled = self.settings.midi.is_some();
                    let label = theme::text("Les notes du synthé flashent à leur couleur", theme::regular(14.0), TEXT);
                    if ui.checkbox(&mut enabled, label).changed() {
                        self.settings.midi = enabled.then(|| self.midi_ports.first().cloned().unwrap_or_default());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.midi_ports.is_empty() {
                            ui.label(theme::text("Aucun appareil MIDI branché", theme::regular(13.0), MUTED));
                        } else if let Some(port) = self.settings.midi.as_mut() {
                            egui::ComboBox::from_id_salt("midi")
                                .width(220.0)
                                .selected_text(theme::text(port.clone(), theme::regular(14.0), TEXT))
                                .show_ui(ui, |ui| {
                                    for name in &self.midi_ports {
                                        ui.selectable_value(port, name.clone(), name);
                                    }
                                });
                        } else {
                            let count = self.midi_ports.len();
                            let text = if count == 1 { "1 appareil MIDI".to_owned() } else { format!("{count} appareils MIDI") };
                            ui.label(theme::text(text, theme::regular(13.0), MUTED));
                        }
                    });
                });
            });
    }

    fn remote_row(&mut self, ui: &mut Ui) {
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(32.0);
                ui.horizontal(|ui| {
                    let label = theme::text("Piloter le live depuis un téléphone", theme::regular(14.0), TEXT);
                    ui.checkbox(&mut self.settings.remote, label);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let hint = "Même Wi-Fi que le PC ; l'adresse s'affiche au lancement";
                        ui.label(theme::text(hint, theme::regular(13.0), MUTED));
                    });
                });
            });
    }

    /// Colonne de droite : aperçu de la couleur, puis les réglages de la lumière, une page à la fois.
    fn aside(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        let width = ui.available_width();

        theme::section(ui, "LUMIÈRE");
        let (rect, _) = ui.allocate_exact_size(vec2(width, 96.0), Sense::hover());
        let primary = theme::rgb(self.palette.primary);
        let radius = rect.size().length() / 2.0 * 0.75;
        theme::radial(ui.painter(), rect, rect.center(), vec2(radius, radius), primary, theme::mix(primary, BG, 0.8));
        // Coins arrondis : un liseré de la couleur du panneau recouvre ceux du dégradé.
        ui.painter().rect_stroke(rect.expand(8.0), 20.0, Stroke::new(8.0, PANEL), egui::StrokeKind::Inside);
        let caption = match &*self.bridge.lock().unwrap_or_else(PoisonError::into_inner) {
            Bridge::Ready { area, .. } => format!("Zone « {area} » · couleur du groove"),
            _ => "Couleur du groove".to_owned(),
        };
        ui.painter().text(
            pos2(rect.left() + 12.0, rect.bottom() - 12.0),
            egui::Align2::LEFT_BOTTOM,
            caption,
            theme::font(12.0, SEMIBOLD),
            theme::ink_on(primary),
        );

        ui.add_space(4.0);
        let current = PAGES.iter().position(|(page, _)| *page == self.page).unwrap_or(0);
        if let Some(i) = theme::segmented(ui, width, &PAGES.map(|(_, label)| label), current) {
            self.page = PAGES[i].0;
        }
        ui.add_space(4.0);
        self.light_page(ui, self.page, width, &SETUP_SKIN, None);
    }
}

/// Ligne qui déplie ou replie un groupe d'options, avec le résumé de ce qu'il contient ; rend vrai
/// au clic.
fn disclosure(ui: &mut Ui, width: f32, open: bool, title: &str, summary: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, 8.0, CARD);
    }
    // Chevron : vers la droite replié, vers le bas déplié.
    let c = pos2(rect.left() + 14.0, rect.center().y);
    let points = if open {
        vec![c + vec2(-5.0, -2.5), c + vec2(0.0, 2.5), c + vec2(5.0, -2.5)]
    } else {
        vec![c + vec2(-2.5, -5.0), c + vec2(2.5, 0.0), c + vec2(-2.5, 5.0)]
    };
    painter.add(egui::Shape::line(points, Stroke::new(2.0, MUTED)));
    let title = painter.layout_no_wrap(title.to_owned(), theme::font(14.0, SEMIBOLD), TEXT);
    let left = rect.left() + 30.0;
    let title_width = title.size().x;
    painter.galley(pos2(left, rect.center().y - title.size().y / 2.0), title, TEXT);
    if !open {
        let room = rect.right() - left - title_width - 16.0;
        let summary = painter.layout(summary.to_owned(), theme::regular(13.0), LABEL, room.max(0.0));
        if summary.rows.len() == 1 {
            painter.galley(pos2(left + title_width + 16.0, rect.center().y - summary.size().y / 2.0), summary, LABEL);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

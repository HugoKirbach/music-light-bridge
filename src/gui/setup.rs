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
/// En dessous de cette largeur de fenêtre, la lumière passe sous la session, en une colonne.
const TWO_COLUMNS: f32 = 1120.0;
const WINDOW_MODES: [&str; 2] = ["Simple", "Expert"];
const DRIVERS: [(Backend, &str); 3] =
    [(Backend::Auto, "Auto (ASIO si libre)"), (Backend::Asio, "ASIO"), (Backend::Wasapi, "WASAPI")];
const OPTION_GAP: f32 = 12.0;
const TEMPOS: [(TempoChoice, &str); 4] = [
    (TempoChoice::Auto, "Détection auto"),
    (TempoChoice::Manual, "BPM imposé"),
    (TempoChoice::Link, "Ableton Link"),
    (TempoChoice::Opus, "OPUS-QUAD"),
];

impl App {
    pub(super) fn setup(&mut self, ui: &mut Ui) {
        let two_columns = ui.max_rect().width() >= TWO_COLUMNS;
        self.two_columns = two_columns;
        egui::Panel::top("en-tête")
            .exact_size(60.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(24, 0)))
            .show(ui, |ui| self.header(ui));
        if two_columns {
            egui::Panel::right("allure")
                .exact_size(ASIDE_WIDTH)
                .resizable(false)
                .frame(Frame::new().fill(PANEL).inner_margin(Margin::same(24)))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.aside(ui));
                });
        }
        let margin = if two_columns { Margin::symmetric(40, 24) } else { Margin::symmetric(24, 20) };
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(margin)).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(780.0);
                self.session(ui);
                if !two_columns {
                    // Fenêtre étroite : la lumière sous la session, sur la même colonne.
                    ui.add_space(20.0);
                    Frame::new().fill(PANEL).corner_radius(14).inner_margin(Margin::same(20)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        self.aside(ui);
                    });
                }
            });
        });
    }

    fn header(&mut self, ui: &mut Ui) {
        let bridge = self.bridge.clone();
        let bridge = bridge.lock().unwrap_or_else(PoisonError::into_inner);
        let mut picked = None;
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
            ui.add_space(12.0);
            let current = usize::from(self.settings.expert);
            picked = ui.scope(|ui| theme::segmented(ui, 170.0, &WINDOW_MODES, current)).inner;
            let wide = ui.available_width() > 520.0;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &*bridge {
                _ if !wide => {}
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
        drop(bridge);
        if let Some(i) = picked {
            self.set_expert(i == 1);
        }
    }

    /// Colonne centrale : source, tempo, MIDI et lancement.
    fn session(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(theme::text("Préparer la session", theme::font(28.0, BOLD), TEXT));
        let side = if self.two_columns { "à droite" } else { "plus bas" };
        ui.label(theme::text(
            format!("Choisis la source et le tempo, puis lance. La lumière se règle {side}, et reste réglable pendant le live."),
            theme::regular(15.0),
            MUTED,
        ));
        let width = ui.available_width();

        ui.add_space(12.0);
        theme::section(ui, "1 · SOURCE");
        let opus = self.settings.opus_only;
        let output = !opus && self.settings.audio == Backend::Output;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let size = vec2((width - 24.0) / 3.0, 100.0);
            let card = theme::card(
                ui,
                size,
                &theme::SETUP_CARD,
                !output && !opus,
                Some(Icon::SoundCard),
                "Carte son",
                "Platines ou synthé sur la Focusrite",
            );
            if card.clicked() && (output || opus) {
                self.settings.opus_only = false;
                if output {
                    self.settings.audio = self.card_backend;
                    self.settings.input = None;
                }
            }
            let card = theme::card(
                ui,
                size,
                &theme::SETUP_CARD,
                output,
                Some(Icon::Speaker),
                "Sortie du PC",
                "Spotify, Ableton…",
            );
            if card.clicked() && !output {
                self.settings.opus_only = false;
                if self.settings.audio != Backend::Output {
                    self.card_backend = self.settings.audio;
                }
                self.settings.audio = Backend::Output;
                self.settings.input = None;
            }
            let card = theme::card(
                ui,
                size,
                &theme::SETUP_CARD,
                opus,
                Some(Icon::Wifi),
                "OPUS-QUAD en Wi-Fi",
                "Par le réseau, sans son",
            );
            if card.clicked() && !opus {
                self.settings.opus_only = true;
                // Faute de choix, la carte Wi-Fi du PC.
                if self.settings.opus_interface.is_none() {
                    self.settings.opus_interface =
                        self.networks.iter().map(|(label, _)| label.clone()).find(|label| label.to_lowercase().contains("wi-fi"));
                }
            }
        });
        ui.add_space(4.0);
        if opus {
            self.network_row(ui, width);
        } else {
            self.device_row(ui, width);
        }

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
        if opus {
            ui.label(theme::text(
                "Tempo, temps, phrases et kicks : ceux de la platine maître de l'OPUS. Ferme rekordbox pendant le live.",
                theme::regular(13.0),
                LABEL,
            ));
        } else {
            self.tempo_row(ui, width);
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if theme::big_button(ui, vec2(250.0, 56.0), ACCENT, BG, Icon::Play, "Lancer le live").clicked() {
                self.start();
            }
            ui.label(theme::text("L'état actuel des lampes sera rétabli à l'arrêt.", theme::regular(13.0), MUTED));
        });

        // Le reste, en mode expert, replié sous le lancement pour qu'il reste à portée de main.
        if self.settings.expert {
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

    /// Carte réseau par laquelle l'OPUS parle : la liaison s'y annonce dès le lancement.
    fn network_row(&mut self, ui: &mut Ui, width: f32) {
        ui.horizontal(|ui| {
            ui.set_width(width);
            ui.label(theme::text("Réseau de l'OPUS", theme::regular(13.0), MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let refresh = ui.add(egui::Label::new(theme::text("Actualiser", theme::regular(13.0), ACCENT)).sense(Sense::click()));
                if refresh.on_hover_text("Relire les cartes réseau du PC, les entrées audio et MIDI, et les lampes").clicked() {
                    self.refresh_devices();
                }
            });
        });
        let automatic = "Automatique (là où l'OPUS s'annonce)";
        let shown = |label: &str, networks: &[(String, std::net::Ipv4Addr)]| {
            networks.iter().find(|(l, _)| l == label).map_or_else(|| format!("{label} (absente)"), |(l, ip)| format!("{l} · {ip}"))
        };
        let selected = self.settings.opus_interface.as_deref().map_or_else(|| automatic.to_owned(), |label| shown(label, &self.networks));
        egui::ComboBox::from_id_salt("réseau")
            .width(width)
            .selected_text(theme::text(selected, theme::regular(14.0), TEXT))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.opus_interface, None, automatic);
                for (label, ip) in &self.networks {
                    ui.selectable_value(&mut self.settings.opus_interface, Some(label.clone()), format!("{label} · {ip}"));
                }
            });
        ui.label(theme::text(
            "L'OPUS et le PC sur le même Wi-Fi (ou l'OPUS branché en USB). Aucun son n'est capté : la lumière suit les platines.",
            theme::regular(12.0),
            LABEL,
        ));
    }

    /// Résumé des options repliées, pour voir d'un coup d'œil ce qui sort de l'ordinaire.
    fn options_summary(&self) -> String {
        let mut parts = Vec::new();
        if self.settings.audio != Backend::Output && !self.settings.opus_only {
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

        // Le plan de la pièce, replié et réservé au mode expert : il sert à la mise en place, pas à
        // chaque session.
        if !self.settings.expert {
            return;
        }
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
                    TempoChoice::Opus => ("Liaison", ""),
                };
                ui.label(theme::text(label, theme::regular(13.0), MUTED));
                if matches!(self.tempo, TempoChoice::Auto | TempoChoice::Opus) {
                    let (min, max) = DEFAULT_BPM_RANGE;
                    let (value, hover) = match self.tempo {
                        TempoChoice::Opus => ("Wi-Fi ou USB".to_owned(), "L'OPUS doit être sur le même réseau que le PC (Wi-Fi), ou branché en USB"),
                        _ => (format!("{min:.0} – {max:.0} BPM"), "Tempos cherchés par la détection automatique"),
                    };
                    Frame::new().fill(RAISED).corner_radius(8).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::text(value, egui::FontId::monospace(14.0), TEXT));
                    })
                    .response
                    .on_hover_text(hover);
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
            TempoChoice::Opus => {
                "Tempo, temps, phrases et kicks suivent la platine maître de l'OPUS : le son n'est plus nécessaire. \
                 Ferme rekordbox pendant le live."
            }
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
        if !self.settings.expert {
            // Mode simple : les scènes, les couleurs et l'intensité des flashs, d'un seul tenant.
            self.simple_light(ui, width, &SETUP_SKIN);
            return;
        }
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

//! Historique des sets joués : la liste des sets enregistrés pendant les lives avec l'OPUS-QUAD, et
//! pour celui choisi sa setlist avec les pochettes, à exporter en image ou en texte.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Frame, Layout, Margin, TextureHandle, Ui, vec2};

use super::App;
use super::theme::{self, ACCENT, BOLD, BORDER, CARD, Icon, LABEL, MONO_BOLD, MUTED, SEMIBOLD, TEXT};
use crate::setlist::{self, Set, poster};

const LIST_WIDTH: f32 = 300.0;
const THUMB: f32 = 52.0;

pub(super) struct History {
    /// Sets enregistrés, du plus récent au plus ancien, avec leur dossier ; celui affiché.
    sets: Vec<(PathBuf, Set)>,
    selected: usize,
    /// Pochettes déjà chargées (`None` : illisible).
    thumbs: HashMap<PathBuf, Option<TextureHandle>>,
}

impl History {
    pub(super) fn load() -> Self {
        Self { sets: setlist::history(), selected: 0, thumbs: HashMap::new() }
    }
}

/// Ce que demande un bouton de l'historique.
enum Action {
    Image,
    Text,
    Copy,
    Folder,
}

impl App {
    pub(super) fn history_view(&mut self, ui: &mut Ui) {
        let Some(history) = &mut self.history else { return };
        ui.label(theme::text("Sets joués", theme::font(26.0, BOLD), TEXT));
        ui.add_space(4.0);
        ui.label(theme::text(
            "Pendant un live avec l'OPUS-QUAD, chaque morceau entendu (en lecture, fader ouvert) est noté avec son heure d'entrée, le premier à 0:00.",
            theme::regular(14.0),
            MUTED,
        ));
        ui.add_space(16.0);
        if history.sets.is_empty() {
            ui.label(theme::text(
                "Aucun set enregistré pour l'instant. Lance un live avec la source « OPUS-QUAD en Wi-Fi » (ou le tempo OPUS-QUAD) et joue : la setlist apparaîtra ici.",
                theme::regular(14.0),
                LABEL,
            ));
            return;
        }
        let mut action = None;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 24.0;
            ui.allocate_ui(vec2(LIST_WIDTH, ui.available_height()), |ui| {
                egui::ScrollArea::vertical().id_salt("sets").show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for (i, (_, set)) in history.sets.iter().enumerate() {
                        let subtitle = format!("{} · {}", set.hour(), set.summary());
                        let card = theme::card(ui, vec2(LIST_WIDTH - 12.0, 64.0), &theme::SETUP_CARD, i == history.selected, None, &set.date(), &subtitle);
                        if card.clicked() {
                            history.selected = i;
                        }
                    }
                });
            });
            ui.vertical(|ui| {
                let Some((folder, set)) = history.sets.get(history.selected) else { return };
                ui.label(theme::text(set.date(), theme::font(22.0, BOLD), TEXT));
                ui.label(theme::text(format!("Début à {} · {}", set.hour(), set.summary()), theme::regular(14.0), MUTED));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    let buttons = [
                        (Icon::Image, "Exporter l'image", "Une image PNG avec les pochettes, à partager, dans Téléchargements", Action::Image),
                        (Icon::Text, "Exporter le texte", "Un fichier .txt, une ligne par morceau : « 0:00 Artiste - Titre », dans Téléchargements", Action::Text),
                        (Icon::Copy, "Copier la liste", "Copie la setlist en texte, à coller où tu veux", Action::Copy),
                        (Icon::Folder, "Ouvrir le dossier", "Le dossier du set : sa setlist et ses pochettes", Action::Folder),
                    ];
                    for (icon, label, hint, wanted) in buttons {
                        if theme::small_button(ui, icon, label).on_hover_text(hint).clicked() {
                            action = Some(wanted);
                        }
                    }
                });
                ui.add_space(12.0);
                let thumbs = &mut history.thumbs;
                egui::ScrollArea::vertical().id_salt("setlist").show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for entry in &set.tracks {
                        Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, BORDER)).corner_radius(10).inner_margin(Margin::same(8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 14.0;
                                ui.add_sized(vec2(72.0, THUMB), egui::Label::new(theme::text(setlist::timecode(entry.at), theme::font(17.0, MONO_BOLD), ACCENT)));
                                match entry.artwork.as_deref().and_then(|name| thumb(ui.ctx(), thumbs, folder, name)) {
                                    Some(texture) => {
                                        ui.add(egui::Image::new(&texture).fit_to_exact_size(vec2(THUMB, THUMB)).corner_radius(6));
                                    }
                                    None => {
                                        let (rect, _) = ui.allocate_exact_size(vec2(THUMB, THUMB), egui::Sense::hover());
                                        ui.painter().rect_filled(rect, 6.0, theme::RAISED);
                                        ui.painter().circle_filled(rect.center(), 5.0, ACCENT);
                                    }
                                }
                                ui.vertical(|ui| {
                                    ui.add_space(6.0);
                                    ui.label(theme::text(entry.shown_title(), theme::font(15.0, SEMIBOLD), TEXT));
                                    ui.label(theme::text(&entry.artist, theme::regular(13.0), MUTED));
                                });
                                if let Some(bpm) = entry.bpm {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(theme::text(format!("{bpm:.0} BPM · platine {}", entry.deck), theme::regular(12.0), LABEL));
                                    });
                                }
                            });
                        });
                    }
                });
            });
        });
        if let Some(action) = action {
            self.history_action(action, ui.ctx());
        }
    }

    fn history_action(&mut self, action: Action, ctx: &egui::Context) {
        let Some((folder, set)) = self.history.as_ref().and_then(|h| h.sets.get(h.selected)).cloned() else { return };
        let target = |extension: &str| {
            dirs::download_dir().unwrap_or_else(|| folder.clone()).join(format!("{}.{extension}", set.file_stem()))
        };
        match action {
            Action::Image => {
                let path = target("png");
                match poster::export(&set, &folder, &path) {
                    Ok(()) => {
                        reveal(&path);
                        self.show(format!("Image enregistrée : {}", path.display()));
                    }
                    Err(e) => self.show(format!("Export de l'image impossible : {e:#}")),
                }
            }
            Action::Text => {
                let path = target("txt");
                match std::fs::write(&path, set.text()) {
                    Ok(()) => {
                        reveal(&path);
                        self.show(format!("Texte enregistré : {}", path.display()));
                    }
                    Err(e) => self.show(format!("Export du texte impossible : {e}")),
                }
            }
            Action::Copy => {
                ctx.copy_text(set.text());
                self.show("Setlist copiée");
            }
            Action::Folder => open(&folder),
        }
    }
}

/// Pochette d'un morceau, chargée une fois.
fn thumb(ctx: &egui::Context, thumbs: &mut HashMap<PathBuf, Option<TextureHandle>>, folder: &Path, name: &str) -> Option<TextureHandle> {
    let path = folder.join(name);
    thumbs
        .entry(path.clone())
        .or_insert_with(|| {
            let image = image::open(&path).ok()?.thumbnail(128, 128).to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
            Some(ctx.load_texture(path.display().to_string(), pixels, egui::TextureOptions::LINEAR))
        })
        .clone()
}

/// Montre un fichier dans l'Explorateur.
fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// Ouvre un dossier dans l'Explorateur.
fn open(folder: &Path) {
    let _ = std::process::Command::new("explorer").arg(folder).spawn();
}

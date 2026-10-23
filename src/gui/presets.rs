//! Scènes : un clic (ou une note MIDI) rappelle couleurs, flashs, animation, disposition et
//! régisseur, avant comme pendant le live.

use eframe::egui::{self, Color32, Margin, Ui, vec2};

use super::App;
use super::theme::{self, CardColors, Icon};
use crate::config::Preset;
use crate::engine::Status;

impl App {
    /// Rappelle une scène : dans la fenêtre et, pendant le live, sur les lampes.
    fn apply_preset(&mut self, preset: &Preset) {
        self.take_preset(preset);
        if let Some(engine) = &self.engine {
            engine.apply_preset(preset);
        }
    }

    /// Reprend dans la fenêtre les réglages d'une scène.
    fn take_preset(&mut self, preset: &Preset) {
        self.palette = preset.palette;
        self.dynamics = preset.dynamics;
        self.settings.motion = preset.motion;
        self.settings.spread = preset.spread;
        self.settings.flash = preset.flash;
        self.settings.director = preset.director;
        self.active_preset = Some(preset.name.clone());
    }

    /// Une note MIDI ou la télécommande a changé les réglages pendant le live : la fenêtre les
    /// reprend.
    pub(super) fn follow_outside_changes(&mut self, status: &Status) {
        if status.outside_changes == self.outside_changes_seen {
            return;
        }
        self.outside_changes_seen = status.outside_changes;
        self.palette = status.palette;
        self.dynamics = status.dynamics;
        self.settings.motion = status.motion;
        self.settings.spread = status.spread;
        self.settings.flash = status.mode;
        self.settings.director = status.director;
        self.active_preset.clone_from(&status.preset);
    }

    /// Enregistre les réglages actuels sous le nom saisi (une scène du même nom est remplacée).
    fn save_preset(&mut self) {
        let name = self.preset_name.trim().to_owned();
        if name.is_empty() {
            return self.show("Donne un nom à la scène.");
        }
        let note = match self.preset_note.trim() {
            "" => None,
            text => match text.parse::<u8>() {
                Ok(note) if note <= 127 => Some(note),
                _ => return self.show("La note MIDI va de 0 à 127."),
            },
        };
        let preset = Preset {
            name: name.clone(),
            palette: self.palette,
            dynamics: self.dynamics,
            motion: self.settings.motion,
            spread: self.settings.spread,
            flash: self.settings.flash,
            director: self.settings.director,
            note,
        };
        // Une note ne rappelle qu'une scène.
        for other in self.cfg.presets.iter_mut().filter(|other| note.is_some() && other.note == note) {
            other.note = None;
        }
        match self.cfg.presets.iter_mut().find(|other| other.name == name) {
            Some(existing) => *existing = preset,
            None => self.cfg.presets.push(preset),
        }
        self.active_preset = Some(name);
        self.preset_name.clear();
        self.preset_note.clear();
        self.presets_changed();
    }

    fn presets_changed(&mut self) {
        if let Some(engine) = &self.engine {
            engine.set_presets(self.cfg.presets.clone());
        }
        self.save_config();
    }

    /// Scènes enregistrées (`per_row` par rangée), et de quoi enregistrer l'allure actuelle.
    pub(super) fn presets_panel(&mut self, ui: &mut Ui, width: f32, per_row: usize, colors: &CardColors, label: Color32) {
        let gap = ui.spacing().item_spacing.x;
        let size = vec2((width - gap * (per_row - 1) as f32) / per_row as f32, 54.0);
        let (mut recall, mut remove) = (None, None);
        for (row, presets) in self.cfg.presets.chunks(per_row).enumerate() {
            ui.horizontal(|ui| {
                for (i, preset) in presets.iter().enumerate() {
                    let subtitle = preset.note.map_or(String::new(), |note| format!("note MIDI {note}"));
                    let selected = self.active_preset.as_ref() == Some(&preset.name);
                    let card = theme::card(ui, size, colors, selected, None, &preset.name, &subtitle)
                        .on_hover_text("Clic : rappeler la scène · clic droit : la supprimer");
                    if card.clicked() {
                        recall = Some(row * per_row + i);
                    }
                    if card.secondary_clicked() {
                        remove = Some(row * per_row + i);
                    }
                }
            });
        }
        if let Some(index) = recall {
            let preset = self.cfg.presets[index].clone();
            self.apply_preset(&preset);
        }
        if let Some(index) = remove {
            let removed = self.cfg.presets.remove(index);
            if self.active_preset.as_ref() == Some(&removed.name) {
                self.active_preset = None;
            }
            self.presets_changed();
        }

        // Le nom, puis la note et le bouton : sur une ligne si la place le permet.
        const NOTE_WIDTH: f32 = 90.0;
        const NARROW: f32 = 480.0;
        let mut entered = false;
        let mut name_field = |ui: &mut Ui, width: f32| {
            let name = ui.add(field(&mut self.preset_name, width, "Nom de la scène"));
            entered = name.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        };
        let narrow = width < NARROW;
        if narrow {
            name_field(ui, width - 20.0);
        }
        let saved = ui
            .horizontal(|ui| {
                if !narrow {
                    name_field(ui, 260.0);
                }
                ui.add(field(&mut self.preset_note, NOTE_WIDTH, "Note MIDI"));
                theme::small_button(ui, Icon::Record, "Enregistrer la scène").clicked()
            })
            .inner;
        if saved || entered {
            self.save_preset();
        }
        let hint = if self.cfg.presets.is_empty() {
            "Règle couleurs, flashs et animation, puis enregistre-les comme scène. Une note MIDI peut la rappeler."
        } else {
            "Même nom : la scène est remplacée. Une note MIDI liée rappelle la scène au lieu de flasher."
        };
        ui.label(theme::text(hint, theme::regular(12.0), label));
    }
}

fn field<'a>(text: &'a mut String, width: f32, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).desired_width(width).margin(Margin::symmetric(10, 8)).hint_text(hint.to_owned())
}

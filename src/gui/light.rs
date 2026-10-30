//! Réglages de la lumière, rangés en quatre pages (mouvement, couleurs, flashs, scènes) : les
//! mêmes avant le lancement, dans la colonne de droite, et pendant le live, dans le dock de la scène.

use std::sync::PoisonError;

use eframe::egui::{self, Color32, Ui, vec2};

use super::theme::{self, ACCENT, CardColors, MUTED};
use super::{App, Bridge, motion_text, rotation_text, spread_text};
use crate::engine::Status;
use crate::look::{Motion, Rotation, Spread};

/// Page des réglages de la lumière.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    Motion,
    Colors,
    Flashes,
    Presets,
}

pub(super) const PAGES: [(Page, &str); 4] =
    [(Page::Motion, "Mouvement"), (Page::Colors, "Couleurs"), (Page::Flashes, "Flashs"), (Page::Presets, "Scènes")];

/// Habillage des réglages selon l'écran.
pub(super) struct Skin {
    pub card: &'static CardColors,
    /// Intitulés et valeurs des curseurs, explications.
    pub label: Color32,
    /// Contour de la pastille de couleur en cours de modification.
    pub ring: Color32,
    /// Les cartes ont une ligne d'explication (la scène a la place), sinon un simple titre.
    pub subtitles: bool,
}

pub(super) const SETUP_SKIN: Skin = Skin { card: &theme::SETUP_CARD, label: MUTED, ring: ACCENT, subtitles: false };
pub(super) const STAGE_SKIN: Skin =
    Skin { card: &theme::STAGE_CARD, label: Color32::from_rgb(0xB4, 0xB9, 0xC1), ring: Color32::WHITE, subtitles: true };

/// Réglage continu de l'allure de la lumière.
#[derive(Clone, Copy)]
enum Knob {
    Intensity,
    Decay,
    Floor,
    Curve,
    Gradient,
    Swell,
}

impl App {
    /// Une page de réglages, sur la largeur donnée ; `status` pendant le live (ce que joue l'Auto).
    pub(super) fn light_page(&mut self, ui: &mut Ui, page: Page, width: f32, skin: &Skin, status: Option<&Status>) {
        ui.spacing_mut().item_spacing.y = 8.0;
        match page {
            Page::Motion => self.motion_page(ui, width, skin, status),
            Page::Colors => self.colors_page(ui, width, skin),
            Page::Flashes => self.flashes_page(ui, width, skin),
            Page::Presets => self.presets_panel(ui, width, if skin.subtitles { 5 } else { 2 }, skin.card, skin.label),
        }
    }

    fn motion_page(&mut self, ui: &mut Ui, width: f32, skin: &Skin, status: Option<&Status>) {
        theme::section(ui, "ANIMATION");
        let height = if skin.subtitles { 64.0 } else { 44.0 };
        grid(ui, width, &Motion::ALL, 140.0, |ui, size, motion| {
            let (title, subtitle) = motion_text(motion);
            let mut subtitle = if skin.subtitles { subtitle.to_owned() } else { String::new() };
            if let Some(status) = status.filter(|_| motion == Motion::Auto && self.settings.motion == Motion::Auto) {
                subtitle = format!("En ce moment : {}", motion_text(status.active_motion).0.to_lowercase());
            }
            let selected = self.settings.motion == motion;
            let card = theme::card(ui, vec2(size, height), skin.card, selected, None, title, &subtitle);
            if card.on_hover_text(motion.description()).clicked() {
                self.set_motion(motion);
            }
        });
        ui.add_space(6.0);
        theme::section(ui, "DISPOSITION DES LAMPES");
        grid(ui, width, &self.spreads(), 140.0, |ui, size, spread| {
            let (title, subtitle) = spread_text(spread);
            let mut subtitle = if skin.subtitles { subtitle.to_owned() } else { String::new() };
            if let Some(status) = status.filter(|_| spread == Spread::Auto && self.settings.spread == Spread::Auto) {
                subtitle = format!("En ce moment : {}", spread_text(status.active_spread).0.to_lowercase());
            }
            let selected = self.settings.spread == spread;
            let card = theme::card(ui, vec2(size, height), skin.card, selected, None, title, &subtitle);
            if card.on_hover_text(spread.description()).clicked() {
                self.set_spread(spread);
            }
        });
        self.director_chip(ui);
        ui.add_space(6.0);
        knobs(ui, width, &[Knob::Swell], |ui, knob| self.knob(ui, knob, skin.label));
    }

    /// Dispositions que la pièce permet (et celle choisie, même si elle ne l'est plus).
    fn spreads(&self) -> Vec<Spread> {
        let mut possible = match &self.engine {
            Some(engine) => engine.info.movements.clone(),
            None => match &*self.bridge.lock().unwrap_or_else(PoisonError::into_inner) {
                Bridge::Ready { lamps, .. } => self.possible_spreads(lamps),
                _ => Spread::ALL.to_vec(),
            },
        };
        if !possible.contains(&self.settings.spread) {
            possible.push(self.settings.spread);
        }
        possible
    }

    fn colors_page(&mut self, ui: &mut Ui, width: f32, skin: &Skin) {
        let wide = width > 700.0;
        let swatch = if wide { vec2(120.0, 64.0) } else { vec2((width - 16.0) / 3.0, 64.0) };
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                theme::section(ui, "COULEURS");
                self.swatches(ui, swatch, skin.ring);
                if !wide {
                    self.color_picker(ui, width - 24.0, skin.label);
                }
            });
            if wide {
                ui.add_space(12.0);
                ui.vertical(|ui| self.color_picker(ui, 150.0, skin.label));
            }
        });
        ui.add_space(6.0);
        theme::section(ui, "ROTATION DES COULEURS");
        if !skin.subtitles {
            // Colonne étroite : un sélecteur à segments, où « 16 mesures » tient sur une ligne.
            let current = Rotation::ALL.iter().position(|&r| r == self.settings.rotation).unwrap_or(0);
            if let Some(i) = theme::segmented(ui, width, &Rotation::ALL.map(rotation_text), current) {
                self.set_rotation(Rotation::ALL[i]);
            }
        } else {
            self.rotation_cards(ui, width.min(560.0), skin);
        }
        ui.add_space(6.0);
        knobs(ui, width, &[Knob::Gradient], |ui, knob| self.knob(ui, knob, skin.label));
    }

    fn rotation_cards(&mut self, ui: &mut Ui, width: f32, skin: &Skin) {
        grid(ui, width, &Rotation::ALL, 90.0, |ui, size, rotation| {
            let subtitle = if rotation == Rotation::Off { "Couleurs choisies" } else { "Et à chaque drop" };
            let selected = self.settings.rotation == rotation;
            let card = theme::card(ui, vec2(size, 58.0), skin.card, selected, None, rotation_text(rotation), subtitle);
            if card.on_hover_text(rotation.description()).clicked() {
                self.set_rotation(rotation);
            }
        });
    }

    fn flashes_page(&mut self, ui: &mut Ui, width: f32, skin: &Skin) {
        theme::section(ui, "ALLURE DES FLASHS");
        let all = [Knob::Intensity, Knob::Decay, Knob::Floor, Knob::Curve];
        knobs(ui, width, &all, |ui, knob| self.knob(ui, knob, skin.label));
    }

    /// Curseur d'un réglage de l'allure, avec son explication au survol.
    fn knob(&mut self, ui: &mut Ui, knob: Knob, label: Color32) {
        let d = &mut self.dynamics;
        let response = ui.scope(|ui| match knob {
            Knob::Intensity => {
                let mut v = d.flash * 100.0;
                let changed = theme::slider_row(ui, "Intensité", format!("{v:.0} %"), egui::Slider::new(&mut v, 0.0..=100.0), label);
                d.flash = v / 100.0;
                (changed, "Jusqu'où monte un flash au-dessus de la lueur")
            }
            Knob::Decay => {
                let mut v = d.decay * 1000.0;
                let shown = format!("{v:.0} ms");
                let slider = egui::Slider::new(&mut v, 40.0..=1500.0).logarithmic(true);
                let changed = theme::slider_row(ui, "Redescente", shown, slider, label);
                d.decay = v / 1000.0;
                (changed, "Temps que met un flash à s'éteindre : sec à gauche, très doux à droite")
            }
            Knob::Floor => {
                let mut v = d.floor * 100.0;
                let changed = theme::slider_row(ui, "Lueur de fond", format!("{v:.0} %"), egui::Slider::new(&mut v, 0.0..=60.0), label);
                d.floor = v / 100.0;
                (changed, "Luminosité gardée entre les kicks, même sans son")
            }
            Knob::Curve => {
                let shown = format!("{:.1}", d.curve);
                let slider = egui::Slider::new(&mut d.curve, 1.0..=2.5).step_by(0.1);
                let changed = theme::slider_row(ui, "Contraste", shown, slider, label);
                (changed, "Courbe de luminosité : plus haut, les fondus s'attardent dans le sombre et les flashs ressortent davantage")
            }
            Knob::Gradient => {
                let shown = if d.gradient < 0.5 { "aucun".to_owned() } else { format!("±{:.0}°", d.gradient) };
                let slider = egui::Slider::new(&mut d.gradient, 0.0..=60.0).step_by(1.0);
                let changed = theme::slider_row(ui, "Dégradé entre les lampes", shown, slider, label);
                (changed, "Écart de teinte d'une lampe à l'autre ; le dégradé traverse lentement la pièce, au rythme des mesures")
            }
            Knob::Swell => {
                let mut v = d.swell * 100.0;
                let shown = if v < 0.5 { "aucune".to_owned() } else { format!("{v:.0} %") };
                let changed = theme::slider_row(ui, "Houle des breaks", shown, egui::Slider::new(&mut v, 0.0..=100.0), label);
                d.swell = v / 100.0;
                (changed, "Pendant les respirations, la lumière se creuse entre les temps et remonte sur chaque demi-mesure")
            }
        });
        let (changed, hint) = response.inner;
        response.response.on_hover_text(hint);
        if changed {
            self.dynamics_changed();
        }
    }
}

/// Cartes en grille : autant de colonnes que la largeur en permet, d'au moins `min` points.
fn grid<T: Copy>(ui: &mut Ui, width: f32, items: &[T], min: f32, mut card: impl FnMut(&mut Ui, f32, T)) {
    let gap = 8.0;
    let columns = (((width + gap) / (min + gap)).floor() as usize).clamp(1, items.len().max(1));
    let size = ((width - gap * (columns - 1) as f32) / columns as f32).floor();
    for row in items.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for &item in row {
                card(ui, size, item);
            }
        });
    }
}

/// Curseurs, sur deux colonnes quand la place le permet.
fn knobs(ui: &mut Ui, width: f32, all: &[Knob], mut knob: impl FnMut(&mut Ui, Knob)) {
    if width < 600.0 || all.len() == 1 {
        // Un curseur seul ne s'étire pas sur tout le dock.
        let row = if width < 600.0 { width } else { 480.0 };
        for &k in all {
            ui.allocate_ui(vec2(row, 0.0), |ui| knob(ui, k));
        }
        return;
    }
    for pair in all.chunks(2) {
        ui.columns(2, |columns| {
            for (column, &k) in columns.iter_mut().zip(pair) {
                knob(column, k);
            }
        });
    }
}

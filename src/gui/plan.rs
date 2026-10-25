//! Pièce et disposition des lampes : choix et gestion des pièces et de leurs dispositions, cotes,
//! plan vu de dessus où glisser chaque lampe et le DJ, hauteur de chaque lampe, et mouvements que
//! la disposition rend possibles.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::App;
use super::theme::{self, ACCENT, BG, BORDER, CARD, Icon, LABEL, MUTED, RAISED, SEMIBOLD, TEXT};
use crate::hue::api;
use crate::look::{Space, Spread};
use crate::room::{self, Level, Placement, Room};

/// Hauteur maximale du plan à l'écran (px), et pas de placement (m).
const PLAN_HEIGHT: f32 = 340.0;
const SNAP: f32 = 0.1;

/// Ce qu'on renomme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Renaming {
    Room,
    Layout,
}

/// Geste sur le plan.
enum Gesture {
    None,
    /// Lampe déplacée (m) ; `true` quand on la lâche.
    Lamp(String, [f32; 2], bool),
    Dj([f32; 2], bool),
    Select(String),
}

impl App {
    /// Pièce, disposition et plan des lampes pilotées (`lamps` : celles de la zone Hue).
    pub(super) fn room_panel(&mut self, ui: &mut Ui, lamps: &[api::Lamp]) {
        self.room_and_layout_rows(ui);
        ui.add_space(6.0);
        let piloted: Vec<&api::Lamp> = lamps.iter().filter(|lamp| !self.cfg.excluded().contains(&lamp.id)).collect();
        let room = self.cfg.room();
        let layout = self.cfg.layout();
        let placed: Vec<(String, String, Placement)> = piloted
            .iter()
            .map(|lamp| {
                let placement = layout.lamps.get(&lamp.id).copied().unwrap_or_else(|| room.default_placement(lamp.x));
                (lamp.id.clone(), lamp.name.clone(), placement)
            })
            .collect();
        match plan(ui, room, &placed, self.selected_lamp.as_deref()) {
            Gesture::Lamp(id, [x, y], done) => {
                let level = placed.iter().find(|(lamp, _, _)| *lamp == id).map_or(Level::Middle, |(_, _, p)| p.level);
                self.cfg.layout_mut().lamps.insert(id.clone(), Placement { x, y, level });
                self.selected_lamp = Some(id);
                if done {
                    self.save_config();
                }
            }
            Gesture::Dj(dj, done) => {
                self.cfg.room_mut().dj = dj;
                if done {
                    self.save_config();
                }
            }
            Gesture::Select(id) => self.selected_lamp = Some(id),
            Gesture::None => {}
        }
        self.level_row(ui, &placed);
        let space = Space::of_room(self.cfg.room(), self.cfg.active_layout, piloted.iter().map(|lamp| (lamp.id.as_str(), lamp.x)));
        movements_row(ui, &space);
    }

    /// Dispositions possibles avec les lampes cochées, dans la pièce active.
    pub(super) fn possible_spreads(&self, lamps: &[api::Lamp]) -> Vec<Spread> {
        let piloted = lamps.iter().filter(|lamp| !self.cfg.excluded().contains(&lamp.id));
        Space::of_room(self.cfg.room(), self.cfg.active_layout, piloted.map(|lamp| (lamp.id.as_str(), lamp.x))).possible()
    }

    /// Choix de la pièce et de la disposition, avec de quoi en ajouter, renommer, supprimer, et
    /// les cotes de la pièce.
    fn room_and_layout_rows(&mut self, ui: &mut Ui) {
        let rooms: Vec<String> = self.cfg.rooms.iter().map(|room| room.name.clone()).collect();
        let layouts: Vec<String> = self.cfg.room().layouts.iter().map(|layout| layout.name.clone()).collect();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(theme::text("Pièce", theme::regular(13.0), MUTED));
            if let Some(index) = pick(ui, "pièce", &rooms, self.cfg.active_room) {
                (self.cfg.active_room, self.cfg.active_layout) = (index, 0);
                changed = true;
            }
            if theme::small_button(ui, Icon::Play, "Nouvelle").on_hover_text("Ajoute une pièce de 5 × 4 m").clicked() {
                let name = room::unique_name("Pièce", rooms.iter().map(String::as_str));
                self.cfg.rooms.push(Room::new(name));
                (self.cfg.active_room, self.cfg.active_layout) = (self.cfg.rooms.len() - 1, 0);
                self.renaming = Some((Renaming::Room, self.cfg.room().name.clone()));
                changed = true;
            }
            changed |= self.manage(ui, Renaming::Room, rooms.len());
        });
        ui.horizontal(|ui| {
            ui.label(theme::text("Configuration", theme::regular(13.0), MUTED));
            if let Some(index) = pick(ui, "disposition", &layouts, self.cfg.active_layout) {
                self.cfg.active_layout = index;
                changed = true;
            }
            if theme::small_button(ui, Icon::Play, "Copier").on_hover_text("Nouvelle configuration, copie de celle-ci").clicked() {
                let mut copy = self.cfg.layout().clone();
                copy.name = room::unique_name(&copy.name, layouts.iter().map(String::as_str));
                let room = self.cfg.room_mut();
                room.layouts.push(copy);
                self.cfg.active_layout = room.layouts.len() - 1;
                self.renaming = Some((Renaming::Layout, self.cfg.layout().name.clone()));
                changed = true;
            }
            changed |= self.manage(ui, Renaming::Layout, layouts.len());
        });
        self.rename_row(ui);
        ui.horizontal(|ui| {
            let (mut width, mut depth) = (self.cfg.room().width, self.cfg.room().depth);
            ui.label(theme::text("Largeur", theme::regular(13.0), MUTED));
            let w = ui.add(size(&mut width));
            ui.label(theme::text("Profondeur", theme::regular(13.0), MUTED));
            let d = ui.add(size(&mut depth));
            if w.changed() || d.changed() {
                self.cfg.room_mut().resize(width, depth);
            }
            if w.drag_stopped() || d.drag_stopped() || w.lost_focus() || d.lost_focus() {
                self.save_config();
            }
        });
        if changed {
            self.selected_lamp = None;
            self.cfg = std::mem::take(&mut self.cfg).with_room();
            self.save_config();
        }
    }

    /// Boutons Renommer et Supprimer d'une pièce ou d'une disposition ; vrai si l'une a disparu.
    fn manage(&mut self, ui: &mut Ui, what: Renaming, count: usize) -> bool {
        if theme::small_button(ui, Icon::Refresh, "Renommer").clicked() {
            let name = match what {
                Renaming::Room => self.cfg.room().name.clone(),
                Renaming::Layout => self.cfg.layout().name.clone(),
            };
            self.renaming = Some((what, name));
        }
        let removable = count > 1;
        let hint = if removable { "Supprimer (sans retour)" } else { "Il en faut au moins une" };
        let remove = ui.add_enabled_ui(removable, |ui| theme::small_button(ui, Icon::Stop, "Supprimer")).inner;
        if !remove.on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
            return false;
        }
        match what {
            Renaming::Room => {
                self.cfg.rooms.remove(self.cfg.active_room);
                (self.cfg.active_room, self.cfg.active_layout) = (0, 0);
            }
            Renaming::Layout => {
                let layout = self.cfg.active_layout;
                self.cfg.room_mut().layouts.remove(layout);
                self.cfg.active_layout = 0;
            }
        }
        true
    }

    /// Champ du nouveau nom, pendant qu'on renomme.
    fn rename_row(&mut self, ui: &mut Ui) {
        let Some((what, mut name)) = self.renaming.take() else { return };
        let mut done = false;
        let mut cancelled = false;
        ui.horizontal(|ui| {
            let label = if what == Renaming::Room { "Nom de la pièce" } else { "Nom de la configuration" };
            ui.label(theme::text(label, theme::regular(13.0), MUTED));
            let field = ui.add(egui::TextEdit::singleline(&mut name).desired_width(220.0));
            if !field.has_focus() && !field.lost_focus() && name.is_empty() {
                field.request_focus();
            }
            done = theme::small_button(ui, Icon::Play, "OK").clicked()
                || (field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));
            cancelled = ui.input(|input| input.key_pressed(egui::Key::Escape));
        });
        let name = name.trim().to_owned();
        if cancelled {
            return;
        }
        if !done {
            self.renaming = Some((what, name));
            return;
        }
        if name.is_empty() {
            return self.show("Donne un nom.");
        }
        match what {
            Renaming::Room if self.cfg.rooms.iter().enumerate().any(|(i, r)| r.name == name && i != self.cfg.active_room) => {
                self.show("Une autre pièce porte déjà ce nom.");
            }
            Renaming::Layout
                if self.cfg.room().layouts.iter().enumerate().any(|(i, l)| l.name == name && i != self.cfg.active_layout) =>
            {
                self.show("Une autre configuration de cette pièce porte déjà ce nom.");
            }
            Renaming::Room => self.cfg.room_mut().name = name,
            Renaming::Layout => self.cfg.layout_mut().name = name,
        }
        self.save_config();
    }

    /// Hauteur de la lampe choisie sur le plan.
    fn level_row(&mut self, ui: &mut Ui, placed: &[(String, String, Placement)]) {
        let Some((id, name, placement)) = placed.iter().find(|(id, _, _)| Some(id) == self.selected_lamp.as_ref()) else {
            ui.label(theme::text("Clique une lampe pour régler sa hauteur ; glisse lampes et DJ sur le plan.", theme::regular(12.0), LABEL));
            return;
        };
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(theme::text(name, theme::font(14.0, SEMIBOLD), TEXT));
            for level in Level::ALL {
                if theme::chip(ui, level.label(), placement.level == level).clicked() {
                    chosen = Some(level);
                }
            }
        });
        if let Some(level) = chosen {
            self.cfg.layout_mut().lamps.insert(id.clone(), Placement { level, ..*placement });
            self.save_config();
        }
    }
}

/// Liste déroulante des noms ; rend le rang choisi s'il change.
fn pick(ui: &mut Ui, id: &str, names: &[String], selected: usize) -> Option<usize> {
    let mut chosen = selected;
    egui::ComboBox::from_id_salt(id)
        .width(180.0)
        .selected_text(theme::text(names.get(selected).cloned().unwrap_or_default(), theme::regular(14.0), TEXT))
        .show_ui(ui, |ui| {
            for (i, name) in names.iter().enumerate() {
                ui.selectable_value(&mut chosen, i, name);
            }
        });
    (chosen != selected).then_some(chosen)
}

/// Mouvements que la disposition permet, et pourquoi les autres ne le sont pas.
fn movements_row(ui: &mut Ui, space: &Space) {
    ui.label(theme::text("Mouvements possibles avec cette configuration", theme::regular(13.0), MUTED));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (spread, why) in space.movements() {
            if spread == Spread::Auto {
                continue;
            }
            let (fill, ink) = if why.is_none() { (theme::ACCENT_TINT, TEXT) } else { (RAISED, LABEL) };
            let pill = theme::pill(ui, theme::spaced(spread.label(), theme::regular(12.0), ink, 0.0), fill, None);
            match why {
                None => pill.on_hover_text(spread.description()),
                Some(why) => pill.on_hover_text(format!("Impossible : {why}")),
            };
        }
    });
}

/// Plan vu de dessus : la pièce à l'échelle, devant (côté DJ) en bas, une grille d'un mètre, les
/// lampes (plus grosses en hauteur, cerclées au plafond) et le DJ, à glisser.
fn plan(ui: &mut Ui, room: &Room, lamps: &[(String, String, Placement)], selected: Option<&str>) -> Gesture {
    let available = ui.available_width();
    let ratio = room.width / room.depth;
    let (mut w, mut h) = (available - 60.0, (available - 60.0) / ratio);
    if h > PLAN_HEIGHT {
        (w, h) = (PLAN_HEIGHT * ratio, PLAN_HEIGHT);
    }
    let (outer, _) = ui.allocate_exact_size(vec2(available, h + 44.0), Sense::hover());
    let rect = Rect::from_min_size(pos2(outer.center().x - w / 2.0, outer.top() + 18.0), vec2(w, h));
    let to_screen = |[x, y]: [f32; 2]| pos2(rect.left() + x / room.width * w, rect.bottom() - y / room.depth * h);
    let to_room = |p: Pos2| {
        let snap = |v: f32| (v / SNAP).round() * SNAP;
        [
            snap((p.x - rect.left()) / w * room.width).clamp(0.0, room.width),
            snap((rect.bottom() - p.y) / h * room.depth).clamp(0.0, room.depth),
        ]
    };

    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, CARD);
    let grid = Stroke::new(1.0, theme::white(0.04));
    for m in 1..room.width.ceil() as usize {
        let x = rect.left() + m as f32 / room.width * w;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid);
    }
    for m in 1..room.depth.ceil() as usize {
        let y = rect.bottom() - m as f32 / room.depth * h;
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], grid);
    }
    painter.rect_stroke(rect, 6.0, Stroke::new(1.5, BORDER), StrokeKind::Inside);
    let cote = |value: f32| format!("{value:.1} m").replace('.', ",");
    painter.text(pos2(rect.center().x, rect.top() - 4.0), Align2::CENTER_BOTTOM, cote(room.width), theme::regular(11.0), LABEL);
    painter.text(pos2(rect.right() + 6.0, rect.center().y), Align2::LEFT_CENTER, cote(room.depth), theme::regular(11.0), LABEL);
    painter.text(pos2(rect.center().x, rect.bottom() + 4.0), Align2::CENTER_TOP, "DEVANT", theme::regular(11.0), LABEL);
    painter.text(pos2(rect.center().x, rect.top() + 6.0), Align2::CENTER_TOP, "FOND", theme::regular(11.0), theme::white(0.18));

    let mut gesture = Gesture::None;
    // Le DJ.
    let dj = to_screen(room.dj);
    let dj_rect = Rect::from_center_size(dj, vec2(38.0, 22.0));
    let response = ui.interact(dj_rect, ui.scope_id().with("dj"), Sense::drag());
    let painter = ui.painter();
    painter.rect_filled(dj_rect, 5.0, if response.hovered() || response.dragged() { theme::white(0.3) } else { theme::white(0.18) });
    painter.text(dj, Align2::CENTER_CENTER, "DJ", theme::font(12.0, SEMIBOLD), TEXT);
    if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.dragged()) {
        gesture = Gesture::Dj(to_room(pointer), false);
    }
    if response.drag_stopped() {
        gesture = Gesture::Dj(room.dj, true);
    }
    response.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Le DJ : les ondes partent d'ici");

    // Les lampes.
    for (id, name, placement) in lamps {
        let center = to_screen([placement.x, placement.y]);
        let radius = 7.0 + 5.0 * placement.level.height();
        let handle = Rect::from_center_size(center, vec2(34.0, 34.0));
        let response = ui.interact(handle, ui.scope_id().with(("lampe", id)), Sense::click_and_drag());
        let active = response.hovered() || response.dragged();
        let painter = ui.painter();
        painter.circle_filled(center, radius + if active { 2.0 } else { 0.0 }, ACCENT);
        if placement.level == Level::Ceiling {
            painter.circle_stroke(center, radius + 5.0, Stroke::new(1.5, ACCENT));
        }
        let ring = if selected == Some(id.as_str()) { Stroke::new(2.0, Color32::WHITE) } else { Stroke::new(1.5, BG) };
        painter.circle_stroke(center, radius + 1.0, ring);
        painter.text(pos2(center.x, center.y + radius + 7.0), Align2::CENTER_TOP, name, theme::regular(11.0), TEXT);
        if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.dragged()) {
            gesture = Gesture::Lamp(id.clone(), to_room(pointer), false);
        }
        if response.drag_stopped() {
            gesture = Gesture::Lamp(id.clone(), [placement.x, placement.y], true);
        } else if response.clicked() {
            gesture = Gesture::Select(id.clone());
        }
        let hint = format!("{name} · {} · glisser pour déplacer, cliquer pour la hauteur", placement.level.label());
        response.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(hint);
    }
    gesture
}

/// Champ d'une cote de la pièce (m).
fn size(value: &mut f32) -> egui::DragValue<'_> {
    egui::DragValue::new(value).range(room::MIN_SIZE..=room::MAX_SIZE).speed(0.05).fixed_decimals(1).suffix(" m")
}

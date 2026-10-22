//! Fenêtre de DJ-Bridge : on y règle le mode live (entrée audio, tempo, MIDI, couleurs, mode de
//! flash), puis on le lance d'un clic ; pendant qu'il tourne, le tempo reconnu s'affiche au centre.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui::{self, Color32, RichText};
use tokio::runtime::Runtime;

use crate::audio::{self, Backend};
use crate::config::{Config, LiveSettings};
use crate::engine::{self, Engine, Status, THRESHOLD_RANGE};
use crate::flash::FlashMode;
use crate::look::Palette;
use crate::midi;
use crate::tempo::{DEFAULT_BPM_RANGE, TempoSource};

/// Le tempo affiché ne change qu'une fois par seconde, pour rester lisible.
const BPM_REFRESH: Duration = Duration::from_secs(1);
/// Rafraîchissement de l'aperçu de la lampe et des témoins.
const REPAINT: Duration = Duration::from_millis(33);
const MESSAGE_DURATION: Duration = Duration::from_secs(8);
const MODES: [FlashMode; 3] = [FlashMode::Kicks, FlashMode::Reactive, FlashMode::EveryBeat];
const BACKENDS: [(Backend, &str); 4] = [
    (Backend::Auto, "Auto (ASIO si libre)"),
    (Backend::Asio, "ASIO"),
    (Backend::Wasapi, "WASAPI"),
    (Backend::Output, "Sortie du PC"),
];
const BPM_SIZE: f32 = 96.0;

pub fn run(rt: Arc<Runtime>, cfg: Config) -> Result<()> {
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DJ-Bridge")
            .with_inner_size([460.0, 760.0])
            .with_min_inner_size([400.0, 620.0]),
        ..Default::default()
    };
    let app = App::new(rt, cfg);
    eframe::run_native("DJ-Bridge", native, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| anyhow!("fenêtre impossible à ouvrir : {e}"))
}

struct App {
    rt: Arc<Runtime>,
    /// Configuration où réglages et couleurs sont enregistrés.
    cfg: Config,
    settings: LiveSettings,
    palette: Palette,
    /// Mode live en cours, s'il est lancé.
    engine: Option<Engine>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    midi_ports: Vec<String>,
    /// BPM saisi : tempo imposé au lancement, puis pendant le live.
    bpm_input: String,
    shown_bpm: Option<f64>,
    shown_source: TempoSource,
    next_refresh: Instant,
    message: Option<(Instant, String)>,
    /// Bilan de la dernière session, affiché après l'arrêt.
    summary: Option<String>,
}

impl App {
    fn new(rt: Arc<Runtime>, cfg: Config) -> Self {
        let settings = cfg.live.clone();
        let mut app = Self {
            rt,
            palette: cfg.palette,
            bpm_input: settings.bpm.map(|bpm| format!("{bpm}")).unwrap_or_default(),
            settings,
            cfg,
            engine: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            midi_ports: Vec::new(),
            shown_bpm: None,
            shown_source: TempoSource::Auto,
            next_refresh: Instant::now(),
            message: None,
            summary: None,
        };
        app.refresh_devices();
        app
    }

    fn refresh_devices(&mut self) {
        self.inputs = audio::list_inputs().map(|list| list.into_iter().map(|i| i.name).collect()).unwrap_or_default();
        self.outputs = audio::list_outputs().unwrap_or_default();
        self.midi_ports = midi::list_inputs().unwrap_or_default();
    }

    fn show(&mut self, text: impl Into<String>) {
        self.message = Some((Instant::now() + MESSAGE_DURATION, text.into()));
    }

    fn report(&mut self, result: Result<()>) {
        if let Err(e) = result {
            self.show(format!("{e:#}"));
        }
    }

    fn save_config(&mut self) {
        self.cfg.live = self.settings.clone();
        self.cfg.palette = self.palette;
        if let Err(e) = self.cfg.save() {
            self.show(format!("Réglages non enregistrés : {e:#}"));
        }
    }

    fn start(&mut self) {
        let bpm = self.bpm_input.trim().replace(',', ".");
        self.settings.bpm = if bpm.is_empty() {
            None
        } else {
            match bpm.parse::<f64>() {
                Ok(bpm) => Some(bpm),
                Err(_) => return self.show(format!("BPM illisible : « {} »", self.bpm_input)),
            }
        };
        self.save_config();
        let options = engine::Options {
            input: self.settings.input.clone(),
            audio: self.settings.audio,
            threshold: self.settings.threshold,
            bpm: self.settings.bpm,
            bpm_range: DEFAULT_BPM_RANGE,
            flash: self.settings.flash,
            link: self.settings.link,
            midi: self.settings.midi.clone().map(Some),
            palette: self.palette,
        };
        match Engine::start(self.rt.clone(), &self.cfg, options) {
            Ok(engine) => {
                println!("{}", engine.info);
                self.next_refresh = Instant::now();
                self.summary = None;
                self.message = None;
                self.engine = Some(engine);
            }
            Err(e) => self.show(format!("Lancement impossible : {e:#}")),
        }
    }

    fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            match engine.stop() {
                Ok(summary) => {
                    println!("{summary}");
                    self.summary = Some(summary.to_string());
                }
                Err(e) => self.show(format!("Arrêt : {e:#}")),
            }
        }
        self.save_config();
    }

    fn launch_button(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            let (label, fill) = if self.engine.is_some() {
                ("■  Arrêter", Color32::from_rgb(150, 40, 40))
            } else {
                ("▶  Lancer", Color32::from_rgb(30, 120, 60))
            };
            let button = egui::Button::new(RichText::new(label).size(22.0).strong()).fill(fill);
            if ui.add_sized([220.0, 44.0], button).clicked() {
                if self.engine.is_some() { self.stop() } else { self.start() }
            }
            ui.add_space(6.0);
        });
    }

    fn tempo_and_lamp(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.vertical_centered(|ui| {
            lamp_preview(ui, status.color, status.kick_lit);
            ui.add_space(4.0);
            let bpm = self.shown_bpm.map_or("---".to_owned(), |bpm| format!("{bpm:.1}"));
            ui.label(RichText::new(bpm).size(BPM_SIZE).strong());
            let source = match status.link_peers {
                Some(peers) => format!("BPM · Ableton Link, {peers} autre(s) participant(s)"),
                None => format!("BPM · {}", self.shown_source.label()),
            };
            ui.label(RichText::new(source).size(16.0).weak());
            ui.add_space(6.0);
        });
    }

    /// Réglages qui ne changent qu'avant le lancement.
    fn launch_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Source audio");
        ui.horizontal_wrapped(|ui| {
            for (backend, label) in BACKENDS {
                let was_output = self.settings.audio == Backend::Output;
                if ui.radio_value(&mut self.settings.audio, backend, label).changed()
                    && was_output != (backend == Backend::Output)
                {
                    // Une entrée et une sortie ne portent pas les mêmes noms.
                    self.settings.input = None;
                }
            }
        });
        let output = self.settings.audio == Backend::Output;
        if output {
            ui.weak("Capte ce que le PC joue sur cette sortie (haut-parleurs, carte son…).");
        }
        ui.horizontal(|ui| {
            let (default, names) = if output {
                ("Sortie par défaut de Windows", &self.outputs)
            } else {
                ("Automatique (Focusrite)", &self.inputs)
            };
            let selected = self.settings.input.clone().unwrap_or_else(|| default.to_owned());
            egui::ComboBox::from_id_salt("entrée").width(280.0).selected_text(selected).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.input, None, default);
                for name in names {
                    ui.selectable_value(&mut self.settings.input, Some(name.clone()), name);
                }
            });
            if ui.button("Actualiser").on_hover_text("Relire les entrées audio et MIDI").clicked() {
                self.refresh_devices();
            }
        });

        ui.add_space(4.0);
        ui.heading("Tempo");
        ui.checkbox(&mut self.settings.link, "Suivre la session Ableton Link (tempo et phase)");
        ui.horizontal(|ui| {
            ui.label(if self.settings.link { "BPM si tu crées la session :" } else { "BPM imposé :" });
            ui.add(egui::TextEdit::singleline(&mut self.bpm_input).desired_width(70.0).hint_text("auto"));
            ui.weak("vide = détection automatique");
        });

        ui.add_space(4.0);
        ui.heading("MIDI");
        let mut enabled = self.settings.midi.is_some();
        if ui.checkbox(&mut enabled, "Les notes du synthé flashent à leur couleur").changed() {
            self.settings.midi = enabled.then(|| self.midi_ports.first().cloned().unwrap_or_default());
        }
        if let Some(port) = self.settings.midi.as_mut() {
            if self.midi_ports.is_empty() {
                ui.weak("Aucune entrée MIDI : branche le synthé en USB puis « Actualiser ».");
            } else {
                egui::ComboBox::from_id_salt("midi").width(280.0).selected_text(port.clone()).show_ui(ui, |ui| {
                    for name in &self.midi_ports {
                        ui.selectable_value(port, name.clone(), name);
                    }
                });
            }
        }
    }

    fn colors(&mut self, ui: &mut egui::Ui) {
        ui.heading("Couleurs");
        let mut changed = false;
        egui::Grid::new("couleurs").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            let rows = [
                ("Principale (groove)", &mut self.palette.primary),
                ("Temps creux", &mut self.palette.calm),
                ("Filtres (passe-haut / passe-bas)", &mut self.palette.filtered),
            ];
            for (label, color) in rows {
                ui.label(label);
                changed |= ui.color_edit_button_srgb(color).changed();
                ui.end_row();
            }
        });
        if changed && let Some(engine) = &self.engine {
            engine.set_palette(self.palette);
        }
    }

    fn flash_mode(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mode de flash");
        for mode in MODES {
            if ui.radio_value(&mut self.settings.flash, mode, mode.description()).changed()
                && let Some(engine) = &self.engine
            {
                engine.set_mode(mode);
            }
        }
    }

    fn detection(&mut self, ui: &mut egui::Ui) {
        let slider = egui::Slider::new(&mut self.settings.threshold, THRESHOLD_RANGE.0..=THRESHOLD_RANGE.1)
            .step_by(0.05)
            .text("seuil des kicks (plus haut = moins de kicks)");
        if ui.add(slider).changed()
            && let Some(engine) = &self.engine
        {
            engine.set_threshold(self.settings.threshold);
        }
    }

    /// Réglages du tempo et capture, pendant que le live tourne.
    fn live_controls(&mut self, ui: &mut egui::Ui, engine: &Engine) {
        ui.heading("Tempo");
        if engine.controls_tempo() {
            ui.horizontal(|ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut self.bpm_input).desired_width(70.0).hint_text("155"));
                let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if ui.button("Imposer ce BPM").clicked() || entered {
                    let result = match self.bpm_input.trim().replace(',', ".").parse::<f64>() {
                        Ok(bpm) => engine.set_bpm(bpm),
                        Err(_) => Err(anyhow!("BPM illisible : « {} »", self.bpm_input)),
                    };
                    self.report(result);
                }
                if ui.button("Détection auto").clicked() {
                    self.report(engine.set_auto());
                }
            });
        } else {
            ui.label("Piloté par la session Ableton Link.");
        }
        if ui.button("Enregistrer les 40 dernières secondes").clicked() {
            match engine.save_capture() {
                Ok(path) => self.show(format!("Capture : {}", path.display())),
                Err(e) => self.show(format!("Capture impossible : {e:#}")),
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(REPAINT);
        // Le moteur s'arrête seul si le bridge ne répond plus.
        if self.engine.as_ref().is_some_and(|engine| !engine.is_running()) {
            self.stop();
            self.show("Le mode live s'est arrêté : bridge injoignable ?");
        }
        let status = self.engine.as_ref().map(Engine::status);
        if let Some(status) = &status
            && Instant::now() >= self.next_refresh
        {
            self.shown_bpm = status.bpm;
            self.shown_source = status.source;
            self.next_refresh = Instant::now() + BPM_REFRESH;
        }

        if let (Some(engine), Some(status)) = (&self.engine, &status) {
            let line = status_line(engine, status);
            egui::Panel::bottom("état").show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(line);
                ui.add_space(4.0);
            });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.launch_button(ui);
                ui.separator();
                match &status {
                    Some(status) => {
                        self.tempo_and_lamp(ui, status);
                        // Sorti le temps du rendu, pour que la section puisse modifier l'application.
                        if let Some(engine) = self.engine.take() {
                            self.live_controls(ui, &engine);
                            self.engine = Some(engine);
                        }
                    }
                    None => self.launch_settings(ui),
                }
                ui.separator();
                self.colors(ui);
                ui.separator();
                self.flash_mode(ui);
                ui.separator();
                ui.heading("Détection");
                self.detection(ui);
                if let Some((until, text)) = &self.message
                    && Instant::now() < *until
                {
                    ui.add_space(6.0);
                    ui.colored_label(Color32::LIGHT_YELLOW, text);
                }
                if let Some(summary) = &self.summary
                    && self.engine.is_none()
                {
                    ui.separator();
                    ui.heading("Dernière session");
                    ui.label(summary);
                }
            });
        });
    }

    fn on_exit(&mut self) {
        self.stop();
    }
}

/// Aperçu de la couleur envoyée à la lampe, et témoin de kick à droite.
fn lamp_preview(ui: &mut egui::Ui, color: [u8; 3], kick: bool) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(260.0, 36.0), egui::Sense::hover());
    let painter = ui.painter();
    let swatch = egui::Rect::from_min_max(rect.min, rect.max - egui::vec2(30.0, 0.0));
    painter.rect_filled(swatch, 10.0, Color32::from_rgb(color[0], color[1], color[2]));
    let dot = if kick { Color32::WHITE } else { Color32::from_gray(60) };
    painter.circle_filled(egui::pos2(rect.max.x - 10.0, rect.center().y), 7.0, dot);
    response.on_hover_text("Couleur envoyée à la lampe ; le point s'allume à chaque kick détecté.");
}

fn status_line(engine: &Engine, status: &Status) -> String {
    let info = &engine.info;
    let mut parts = vec![
        format!("{} en {}", info.device, info.backend),
        format!("zone « {} »", info.area),
        format!("écart kicks/temps ±{:.0} ms", status.offset_ms),
    ];
    if let Some(port) = &info.midi_port {
        parts.push(format!("MIDI « {port} »"));
    }
    if status.input_lost {
        parts.push("⚠ entrée perdue".to_owned());
    } else if status.glitches > 0 {
        parts.push(format!("⚠ {} coupure(s) audio", status.glitches));
    }
    parts.join(" · ")
}

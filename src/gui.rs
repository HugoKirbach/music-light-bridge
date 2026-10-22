//! Fenêtre du mode live : tempo reconnu au centre, couleurs et mode de flash à portée de clic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui::{self, Color32, RichText};
use tokio::runtime::Runtime;

use crate::config::Config;
use crate::engine::{Engine, Options, Status, THRESHOLD_RANGE};
use crate::flash::FlashMode;
use crate::look::Palette;
use crate::tempo::TempoSource;

/// Le tempo affiché ne change qu'une fois par seconde, pour rester lisible.
const BPM_REFRESH: Duration = Duration::from_secs(1);
/// Rafraîchissement de l'aperçu de la lampe et des témoins.
const REPAINT: Duration = Duration::from_millis(33);
const MESSAGE_DURATION: Duration = Duration::from_secs(6);
const MODES: [FlashMode; 3] = [FlashMode::Kicks, FlashMode::Reactive, FlashMode::EveryBeat];
const BPM_SIZE: f32 = 96.0;

pub fn run(rt: Arc<Runtime>, cfg: Config, options: Options) -> Result<()> {
    let engine = Engine::start(rt, &cfg, options)?;
    println!("{}", engine.info);
    println!("Fenêtre ouverte : ferme-la pour arrêter.");
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DJ-Bridge")
            .with_inner_size([440.0, 660.0])
            .with_min_inner_size([380.0, 600.0]),
        ..Default::default()
    };
    let app = App::new(engine, cfg);
    eframe::run_native("DJ-Bridge", native, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| anyhow!("fenêtre impossible à ouvrir : {e}"))
}

struct App {
    engine: Option<Engine>,
    /// Configuration où les couleurs sont enregistrées à la fermeture.
    cfg: Config,
    palette: Palette,
    mode: FlashMode,
    threshold: f32,
    shown_bpm: Option<f64>,
    shown_source: TempoSource,
    next_refresh: Instant,
    bpm_input: String,
    message: Option<(Instant, String)>,
}

impl App {
    fn new(engine: Engine, cfg: Config) -> Self {
        let status = engine.status();
        Self {
            palette: engine.palette(),
            mode: status.mode,
            threshold: status.threshold,
            shown_bpm: status.bpm,
            shown_source: status.source,
            next_refresh: Instant::now() + BPM_REFRESH,
            bpm_input: String::new(),
            message: None,
            engine: Some(engine),
            cfg,
        }
    }

    fn show(&mut self, text: impl Into<String>) {
        self.message = Some((Instant::now() + MESSAGE_DURATION, text.into()));
    }

    fn report(&mut self, result: Result<()>) {
        if let Err(e) = result {
            self.show(format!("{e:#}"));
        }
    }

    fn tempo_and_lamp(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            lamp_preview(ui, status.color, status.kick_lit);
            ui.add_space(8.0);
            let bpm = self.shown_bpm.map_or("---".to_owned(), |bpm| format!("{bpm:.1}"));
            ui.label(RichText::new(bpm).size(BPM_SIZE).strong());
            let source = match status.link_peers {
                Some(peers) => format!("BPM · Ableton Link, {peers} autre(s) participant(s)"),
                None => format!("BPM · {}", self.shown_source.label()),
            };
            ui.label(RichText::new(source).size(16.0).weak());
            ui.add_space(8.0);
        });
    }

    fn colors(&mut self, ui: &mut egui::Ui, engine: &Engine) {
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
        if changed {
            engine.set_palette(self.palette);
        }
    }

    fn flash_mode(&mut self, ui: &mut egui::Ui, engine: &Engine) {
        ui.heading("Mode de flash");
        for mode in MODES {
            if ui.radio_value(&mut self.mode, mode, mode.description()).changed() {
                engine.set_mode(self.mode);
            }
        }
    }

    fn tempo_controls(&mut self, ui: &mut egui::Ui, engine: &Engine) {
        ui.heading("Tempo");
        if !engine.controls_tempo() {
            ui.label("Piloté par la session Ableton Link.");
            return;
        }
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
    }

    fn detection(&mut self, ui: &mut egui::Ui, engine: &Engine) {
        ui.heading("Détection");
        let slider = egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE.0..=THRESHOLD_RANGE.1)
            .step_by(0.05)
            .text("seuil des kicks (plus haut = moins de kicks)");
        if ui.add(slider).changed() {
            engine.set_threshold(self.threshold);
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
        // Le moteur est sorti le temps du rendu, pour que les sections puissent modifier l'application.
        let Some(engine) = self.engine.take() else { return };
        let status = engine.status();
        if Instant::now() >= self.next_refresh {
            self.shown_bpm = status.bpm;
            self.shown_source = status.source;
            self.next_refresh = Instant::now() + BPM_REFRESH;
        }

        egui::Panel::bottom("état").show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(status_line(&engine, &status));
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.tempo_and_lamp(ui, &status);
            ui.separator();
            self.colors(ui, &engine);
            ui.separator();
            self.flash_mode(ui, &engine);
            ui.separator();
            self.tempo_controls(ui, &engine);
            ui.separator();
            self.detection(ui, &engine);
            if !engine.is_running() {
                ui.colored_label(Color32::LIGHT_RED, "La lampe ne reçoit plus rien : bridge injoignable ?");
            }
            if let Some((until, text)) = &self.message
                && Instant::now() < *until
            {
                ui.add_space(6.0);
                ui.label(text);
            }
        });
        self.engine = Some(engine);
    }

    fn on_exit(&mut self) {
        if let Some(engine) = self.engine.take() {
            match engine.stop() {
                Ok(summary) => println!("{summary}"),
                Err(e) => eprintln!("Arrêt du mode live : {e:#}"),
            }
        }
        self.cfg.palette = self.palette;
        if let Err(e) = self.cfg.save() {
            eprintln!("Couleurs non enregistrées : {e:#}");
        }
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

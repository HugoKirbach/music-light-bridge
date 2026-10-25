//! Fenêtre de DJ-Bridge. Avant le lancement, un écran de réglages (source audio, tempo, MIDI,
//! couleurs, animation) ; pendant le live, une scène plein écran qui prend la couleur de la lampe,
//! le tempo en très grand et les réglages qui restent modifiables dans un dock en bas.

mod plan;
mod presets;
mod setup;
mod stage;
mod theme;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui;
use tokio::runtime::Runtime;

use crate::audio::{self, Backend};
use crate::config::{Config, LiveSettings};
use crate::engine::{self, Engine, THRESHOLD_RANGE};
use crate::flash::FlashMode;
use crate::hue::api;
use crate::look::{Dynamics, Motion, Palette, Spread};
use crate::midi;
use crate::tempo::{DEFAULT_BPM_RANGE, TempoSource};

/// Le tempo affiché ne change qu'une fois par seconde, pour rester lisible.
const BPM_REFRESH: Duration = Duration::from_secs(1);
/// Rafraîchissement de la scène (couleur de la lampe, temps de la mesure).
const LIVE_REPAINT: Duration = Duration::from_millis(33);
/// Les réglages n'ont besoin que de suivre la souris et la réponse du bridge.
const SETUP_REPAINT: Duration = Duration::from_millis(250);
const MESSAGE_DURATION: Duration = Duration::from_secs(8);
const MODES: [FlashMode; 3] = [FlashMode::Kicks, FlashMode::Reactive, FlashMode::EveryBeat];

pub fn run(rt: Arc<Runtime>, cfg: Config) -> Result<()> {
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DJ-Bridge")
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([940.0, 640.0]),
        ..Default::default()
    };
    let app = App::new(rt, cfg);
    eframe::run_native(
        "DJ-Bridge",
        native,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow!("fenêtre impossible à ouvrir : {e}"))
}

/// Ce que la fenêtre sait du bridge avant le lancement.
enum Bridge {
    Unpaired,
    Pending,
    Ready { area: String, lamps: Vec<api::Lamp> },
    Failed(String),
}

/// D'où vient le tempo au lancement.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TempoChoice {
    Auto,
    Manual,
    Link,
}

/// Onglets du dock de la scène.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Motion,
    Colors,
    Flashes,
    Presets,
    Tempo,
    Lamps,
    Source,
}

struct App {
    rt: Arc<Runtime>,
    /// Configuration où réglages et couleurs sont enregistrés.
    cfg: Config,
    settings: LiveSettings,
    palette: Palette,
    dynamics: Dynamics,
    tempo: TempoChoice,
    /// Pilote de la carte son, gardé quand on passe sur la sortie du PC.
    card_backend: Backend,
    /// Couleur de la palette en cours de modification.
    editing_color: Option<usize>,
    tab: Tab,
    bridge: Arc<Mutex<Bridge>>,
    /// Mode live en cours, s'il est lancé, et son heure de départ.
    engine: Option<Engine>,
    started: Instant,
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
    /// Captures automatiques déjà signalées pendant ce live.
    auto_captures_seen: u32,
    /// Scène rappelée en dernier, nom et note saisis pour en enregistrer une, et changements faits
    /// hors de la fenêtre déjà repris.
    active_preset: Option<String>,
    preset_name: String,
    preset_note: String,
    outside_changes_seen: u32,
    /// Lampe choisie sur le plan (pour sa hauteur), et pièce ou disposition qu'on renomme.
    selected_lamp: Option<String>,
    renaming: Option<(plan::Renaming, String)>,
}

impl App {
    fn new(rt: Arc<Runtime>, cfg: Config) -> Self {
        let settings = cfg.live.clone();
        let tempo = match (settings.link, settings.bpm) {
            (true, _) => TempoChoice::Link,
            (false, Some(_)) => TempoChoice::Manual,
            (false, None) => TempoChoice::Auto,
        };
        let card_backend = if settings.audio == Backend::Output { Backend::Auto } else { settings.audio };
        let bridge = Arc::new(Mutex::new(Bridge::Unpaired));
        let mut app = Self {
            rt,
            palette: cfg.palette,
            dynamics: cfg.dynamics,
            bpm_input: settings.bpm.map(|bpm| format!("{bpm}")).unwrap_or_default(),
            tempo,
            card_backend,
            editing_color: None,
            tab: Tab::Motion,
            bridge,
            settings,
            cfg,
            engine: None,
            started: Instant::now(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            midi_ports: Vec::new(),
            shown_bpm: None,
            shown_source: TempoSource::Auto,
            next_refresh: Instant::now(),
            message: None,
            summary: None,
            auto_captures_seen: 0,
            active_preset: None,
            preset_name: String::new(),
            preset_note: String::new(),
            outside_changes_seen: 0,
            selected_lamp: None,
            renaming: None,
        };
        app.refresh_devices();
        app
    }

    /// Relit les entrées audio et MIDI, et demande au bridge la zone et ses lampes.
    fn refresh_devices(&mut self) {
        self.refresh_bridge();
        self.inputs = audio::list_inputs().map(|list| list.into_iter().map(|i| i.name).collect()).unwrap_or_default();
        self.outputs = audio::list_outputs().unwrap_or_default();
        self.midi_ports = midi::list_inputs().unwrap_or_default();
    }

    /// Redemande au bridge la zone et ses lampes, par exemple après un changement dans l'app Hue.
    fn refresh_bridge(&mut self) {
        if let Some(credentials) = self.cfg.bridge.clone() {
            *self.bridge.lock().unwrap_or_else(PoisonError::into_inner) = Bridge::Pending;
            let (slot, wanted) = (self.bridge.clone(), self.cfg.area_id.clone());
            self.rt.spawn(async move {
                let state = match api::select_area(&credentials, wanted.as_deref()).await {
                    Ok(area) => match api::area_lamps(&credentials, &area).await {
                        Ok(lamps) => Bridge::Ready { area: area.metadata.name, lamps },
                        Err(e) => Bridge::Failed(format!("{e:#}")),
                    },
                    Err(e) => Bridge::Failed(format!("{e:#}")),
                };
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = state;
            });
        }
    }

    fn show(&mut self, text: impl Into<String>) {
        self.message = Some((Instant::now() + MESSAGE_DURATION, text.into()));
    }

    fn current_message(&self) -> Option<&str> {
        self.message.as_ref().filter(|(until, _)| Instant::now() < *until).map(|(_, text)| text.as_str())
    }

    fn report(&mut self, result: Result<()>) {
        if let Err(e) = result {
            self.show(format!("{e:#}"));
        }
    }

    fn save_config(&mut self) {
        self.cfg.live = self.settings.clone();
        self.cfg.palette = self.palette;
        self.cfg.dynamics = self.dynamics;
        if let Err(e) = self.cfg.save() {
            self.show(format!("Réglages non enregistrés : {e:#}"));
        }
    }

    fn parse_bpm(&self) -> Result<Option<f64>> {
        let bpm = self.bpm_input.trim().replace(',', ".");
        if bpm.is_empty() {
            return Ok(None);
        }
        bpm.parse::<f64>().map(Some).map_err(|_| anyhow!("BPM illisible : « {} »", self.bpm_input))
    }

    fn start(&mut self) {
        let bpm = match self.parse_bpm() {
            Ok(bpm) => bpm,
            Err(e) => return self.show(e.to_string()),
        };
        if self.tempo == TempoChoice::Manual && bpm.is_none() {
            return self.show("Indique le BPM à imposer, ou choisis la détection auto.");
        }
        self.settings.link = self.tempo == TempoChoice::Link;
        self.settings.bpm = if self.tempo == TempoChoice::Auto { None } else { bpm };
        self.save_config();
        let remote = match self.settings.remote {
            true => match self.cfg.remote_token() {
                Ok(token) => Some(token),
                Err(e) => return self.show(format!("Télécommande : {e:#}")),
            },
            false => None,
        };
        let options = engine::Options {
            input: self.settings.input.clone(),
            audio: self.settings.audio,
            threshold: self.settings.threshold,
            bpm: self.settings.bpm,
            bpm_range: DEFAULT_BPM_RANGE,
            flash: self.settings.flash,
            motion: self.settings.motion,
            spread: self.settings.spread,
            director: self.settings.director,
            remote,
            link: self.settings.link,
            midi: self.settings.midi.clone().map(Some),
            palette: self.palette,
            dynamics: self.dynamics,
        };
        match Engine::start(self.rt.clone(), &self.cfg, options) {
            Ok(engine) => {
                println!("{}", engine.info);
                self.started = Instant::now();
                self.next_refresh = Instant::now();
                self.shown_bpm = None;
                self.summary = None;
                self.auto_captures_seen = 0;
                self.outside_changes_seen = 0;
                self.message = None;
                if let Some(url) = &engine.info.remote_url {
                    self.show(format!("Télécommande : ouvre {url} sur ton téléphone (onglet Source)."));
                }
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

    fn set_motion(&mut self, motion: Motion) {
        self.settings.motion = motion;
        if let Some(engine) = &self.engine {
            engine.set_motion(motion);
        }
    }

    fn set_spread(&mut self, spread: Spread) {
        self.settings.spread = spread;
        if let Some(engine) = &self.engine {
            engine.set_spread(spread);
        }
    }

    fn set_director(&mut self, on: bool) {
        self.settings.director = on;
        if let Some(engine) = &self.engine {
            engine.set_director(on);
        }
    }

    /// Case du régisseur, sous les dispositions.
    fn director_chip(&mut self, ui: &mut egui::Ui) {
        let hint = "En disposition Auto, la mise en scène du groove et la teinte varient d'une section à l'autre selon l'énergie du set";
        if theme::chip(ui, "Régisseur auto", self.settings.director).on_hover_text(hint).clicked() {
            self.set_director(!self.settings.director);
        }
    }

    fn set_flash(&mut self, mode: FlashMode) {
        self.settings.flash = mode;
        if let Some(engine) = &self.engine {
            engine.set_mode(mode);
        }
    }

    fn set_threshold(&mut self, threshold: f32) {
        self.settings.threshold = threshold;
        if let Some(engine) = &self.engine {
            engine.set_threshold(threshold);
        }
    }

    fn palette_changed(&self) {
        if let Some(engine) = &self.engine {
            engine.set_palette(self.palette);
        }
    }

    fn dynamics_changed(&self) {
        if let Some(engine) = &self.engine {
            engine.set_dynamics(self.dynamics);
        }
    }

    fn color_mut(&mut self, index: usize) -> &mut [u8; 3] {
        match index {
            0 => &mut self.palette.primary,
            1 => &mut self.palette.calm,
            _ => &mut self.palette.filtered,
        }
    }

    /// Les trois pastilles de la palette ; un clic ouvre ou ferme le nuancier de l'une d'elles.
    fn swatches(&mut self, ui: &mut egui::Ui, size: egui::Vec2, ring: egui::Color32) {
        ui.horizontal(|ui| {
            for (i, (name, hint)) in COLOR_NAMES.into_iter().zip(COLOR_HINTS).enumerate() {
                let color = theme::rgb(*self.color_mut(i));
                let editing = self.editing_color == Some(i);
                if theme::swatch(ui, size, color, name, editing, ring).on_hover_text(hint).clicked() {
                    self.editing_color = if editing { None } else { Some(i) };
                }
            }
        });
    }

    /// Nuancier de la couleur ouverte, ou une invite s'il n'y en a pas.
    fn color_picker(&mut self, ui: &mut egui::Ui, size: f32, hint_color: egui::Color32) {
        let Some(i) = self.editing_color else {
            ui.label(theme::text("Clique une couleur pour la changer.", theme::regular(13.0), hint_color));
            return;
        };
        ui.label(theme::text(COLOR_HINTS[i], theme::regular(13.0), hint_color));
        let mut color = theme::rgb(*self.color_mut(i));
        let changed = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = size;
                egui::widgets::color_picker::color_picker_color32(ui, &mut color, egui::widgets::color_picker::Alpha::Opaque)
            })
            .inner;
        if changed {
            *self.color_mut(i) = [color.r(), color.g(), color.b()];
            self.palette_changed();
        }
    }

    /// Un des trois réglages de l'allure des flashs : 0 intensité, 1 redescente, 2 lueur de fond.
    fn flash_slider(&mut self, ui: &mut egui::Ui, which: usize, label: egui::Color32) {
        let d = &mut self.dynamics;
        let changed = match which {
            0 => {
                let mut v = d.flash * 100.0;
                let shown = format!("{v:.0} %");
                let changed = theme::slider_row(ui, "Intensité", shown, egui::Slider::new(&mut v, 0.0..=100.0), label);
                d.flash = v / 100.0;
                changed
            }
            1 => {
                let mut v = d.decay * 1000.0;
                let shown = format!("{v:.0} ms");
                let slider = egui::Slider::new(&mut v, 40.0..=1500.0).logarithmic(true);
                let changed = theme::slider_row(ui, "Redescente", shown, slider, label);
                d.decay = v / 1000.0;
                changed
            }
            _ => {
                let mut v = d.floor * 100.0;
                let shown = format!("{v:.0} %");
                let changed = theme::slider_row(ui, "Lueur de fond", shown, egui::Slider::new(&mut v, 0.0..=60.0), label);
                d.floor = v / 100.0;
                changed
            }
        };
        if changed {
            self.dynamics_changed();
        }
    }

    fn threshold_slider(&mut self, ui: &mut egui::Ui, label: egui::Color32) {
        let mut threshold = self.settings.threshold;
        let shown = format!("{threshold:.2}");
        let slider = egui::Slider::new(&mut threshold, THRESHOLD_RANGE.0..=THRESHOLD_RANGE.1).step_by(0.05);
        if theme::slider_row(ui, "Seuil des kicks", shown, slider, label) {
            self.set_threshold(threshold);
        }
        ui.label(theme::text("Plus haut : moins de kicks détectés.", theme::regular(12.0), label));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Le moteur s'arrête seul si le bridge ne répond plus.
        if self.engine.as_ref().is_some_and(|engine| !engine.is_running()) {
            self.stop();
            self.show("Le mode live s'est arrêté : bridge injoignable ?");
        }
        if let Some(Ok(())) = self.engine.as_mut().and_then(Engine::recover_audio) {
            self.show("Entrée audio retrouvée.");
        }
        let status = self.engine.as_ref().map(Engine::status);
        match status {
            Some(status) => {
                ui.ctx().request_repaint_after(LIVE_REPAINT);
                if Instant::now() >= self.next_refresh {
                    self.shown_bpm = status.bpm;
                    self.shown_source = status.source;
                    self.next_refresh = Instant::now() + BPM_REFRESH;
                }
                self.follow_outside_changes(&status);
                if status.auto_captures > self.auto_captures_seen {
                    self.auto_captures_seen = status.auto_captures;
                    self.show("Le groove a décroché : capture automatique enregistrée pour le banc d'essai.");
                }
                self.stage(ui, &status);
            }
            None => {
                ui.ctx().request_repaint_after(SETUP_REPAINT);
                self.setup(ui);
            }
        }
    }

    fn on_exit(&mut self) {
        self.stop();
    }
}

/// Titre court et explication d'une animation.
fn motion_text(motion: Motion) -> (&'static str, &'static str) {
    match motion {
        Motion::Auto => ("Auto", "Suit les phases"),
        Motion::Pulse => ("Pulse", "Un flash par kick"),
        Motion::Breath => ("Respiration", "Suit l'énergie"),
        Motion::BuildUp => ("Montée", "S'intensifie, blanchit"),
    }
}

/// Titre court et explication d'une disposition des lampes.
fn spread_text(spread: Spread) -> (&'static str, &'static str) {
    (spread.label(), spread.hint())
}

fn flash_text(mode: FlashMode) -> (&'static str, &'static str) {
    match mode {
        FlashMode::Kicks => ("Kicks", "Anticipés sur les temps"),
        FlashMode::Reactive => ("Réactif", "Chaque kick, sans avance"),
        FlashMode::EveryBeat => ("Chaque temps", "Même sans kick"),
    }
}

const COLOR_NAMES: [&str; 3] = ["Groove", "Creux", "Filtres"];
const COLOR_HINTS: [&str; 3] =
    ["Couleur principale : kicks réguliers", "Temps creux : break, passage sans kick", "Filtres passe-haut ou passe-bas"];

/// Tempo lisible : sans décimale quand il tombe juste.
fn format_bpm(bpm: Option<f64>) -> String {
    match bpm {
        Some(bpm) if (bpm - bpm.round()).abs() < 0.05 => format!("{bpm:.0}"),
        Some(bpm) => format!("{bpm:.1}"),
        None => "---".to_owned(),
    }
}

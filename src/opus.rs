//! Liaison OPUS-QUAD en direct, par le réseau (Wi-Fi ou carte réseau USB de l'OPUS) : DJ-Bridge
//! s'y présente comme rekordbox (mode Lighting), suit l'état des quatre platines et garde les
//! données de chaque morceau chargé (grille des temps, structure, waveform). Il en tire, temps
//! par temps, où en est la platine maître : le tempo joué, l'instant de chaque temps, la place
//! dans la mesure et dans la phrase, et les temps où tombe un kick.
//!
//! L'OPUS n'envoie pas l'instant exact des temps : seulement le numéro du temps en cours, cinq à
//! six fois par seconde. Chaque changement de numéro situe le début d'un temps entre deux paquets ;
//! l'estimation se resserre de temps en temps (une vingtaine de ms après 8 temps sur le Wi-Fi).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use netdev::ipnet::Ipv4Net;

use crate::audio::analysis::Levels;
use crate::look::{Accent, Phase};
use crate::prolink::{self, BeatGrid, Packet, Reassembler, SongStructure, Status, block};

/// Cadence de l'annonce de présence et de la demande d'état (celle de Beat Link).
const ANNOUNCE_INTERVAL: f64 = 1.5;
/// Cadence à laquelle on regarde si une carte réseau a reçu une adresse.
const RESCAN_INTERVAL: f64 = 5.0;
/// Une platine dont aucun état n'est arrivé depuis 1 s est perdue de vue.
const STALE: f64 = 1.0;
/// Données d'un morceau qui manquent encore : redemandées toutes les 3 s, quatre fois au plus.
const REQUEST_RETRY: f64 = 3.0;
const REQUEST_TRIES: u32 = 4;
/// L'estimation de la phase moyenne au plus les 8 derniers changements de temps ; elle est assez
/// sûre pour flasher après deux.
const PHASE_MEMORY: u32 = 8;
const SETTLED_AFTER: u32 = 2;
/// Changement de temps à plus de 30 % de période de l'instant prévu : aberrant.
const OUTLIER: f64 = 0.3;
/// Waveform détaillée : en-tête avant les points (16 octets : les graves y montent pile sur les
/// temps de la grille), et fenêtre où chercher le kick d'un temps (ms) : son corps culmine 30 à
/// 60 ms après le temps.
const WAVEFORM_HEADER: usize = 16;
const KICK_WINDOW_MS: (f64, f64) = (-10.0, 60.0);
/// Un temps porte un kick quand ses graves atteignent 55 % de ceux des temps forts du morceau
/// (80e centile).
const KICK_SHARE: f32 = 0.55;
const KICK_PERCENTILE: f32 = 0.8;
/// Le groove tient quand au moins deux des temps autour de l'instant (le précédent, celui-ci et
/// les deux suivants) portent un kick : un impact isolé n'en fait pas un, et le drop groove dès
/// son premier temps.
const GROOVE_WINDOW: (i64, i64) = (-1, 2);
const GROOVE_KICKS: usize = 2;
/// Appels d'une montée (roll, charleston, son acid) : une double croche dont l'attaque, le pic de
/// présence à son début (de 7 ms avant à 40 ms après) moins le creux d'avant, dépasse 0,3, avec un
/// pic d'au moins 0,35. Un riser continu n'a pas d'attaque : il ne flashe pas.
const CALL_PEAK_MS: (f64, f64) = (-7.0, 40.0);
const CALL_ATTACK: f32 = 0.3;
const CALL_PEAK: f32 = 0.35;
/// Têtes de phrase de la lumière : tous les 16 temps depuis le début de la phrase rekordbox.
const PHRASE_BEATS: u32 = 16;
/// Kicks cherchés au plus 8 temps devant.
const KICK_LOOKAHEAD: u32 = 8;

/// Carte réseau du PC, avec une de ses adresses IPv4.
#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub label: String,
    pub ip: Ipv4Addr,
    pub net: Ipv4Net,
    pub mac: [u8; 6],
}

pub fn label(i: &netdev::Interface) -> String {
    let name = i.friendly_name.clone().unwrap_or_else(|| i.name.clone());
    match &i.description {
        Some(description) if *description != name => format!("{name} ({description})"),
        _ => name,
    }
}

/// Cartes réseau actives qui ont une adresse IPv4.
pub fn interfaces() -> Vec<Interface> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !i.is_loopback())
        .flat_map(|i| {
            let label = label(&i);
            let mac = i.mac_addr.map(|m| m.octets()).unwrap_or_default();
            i.ipv4
                .iter()
                .map(|net| Interface { label: label.clone(), ip: net.addr(), net: *net, mac })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Un point de la waveform détaillée : couleur (rouge pour les graves, vert pour les médiums,
/// bleu pour les aigus) et hauteur, chacun de 0 à 1.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub height: f32,
}

impl Point {
    fn decode(raw: u16) -> Self {
        let channel = |shift: u16| f32::from((raw >> shift) & 7) / 7.0;
        Self { red: channel(13), green: channel(10), blue: channel(7), height: f32::from((raw >> 2) & 0x1f) / 31.0 }
    }

    /// Part des graves dans le point : de quoi reconnaître un kick.
    fn bass(self) -> f32 {
        self.red * self.height
    }

    /// Présence d'un coup, aigu (charleston, son acid) ou grave (roll de kicks).
    fn presence(self) -> f32 {
        self.height * (0.4 * self.green + 0.6 * self.blue).max(self.red)
    }
}

/// Waveform détaillée en couleur d'un morceau, à vitesse normale.
#[derive(Debug, Clone, PartialEq)]
pub struct Waveform {
    pub per_second: f64,
    pub points: Vec<Point>,
}

impl Waveform {
    /// Décode un bloc de waveform détaillée : nombre de points, octets, 2 octets par point,
    /// points par seconde, 4 octets inconnus, puis les points.
    pub fn parse(whole: &[u8]) -> Option<Self> {
        let header = prolink::Waveform::parse(whole)?;
        if header.point_len != 2 || header.per_second == 0 {
            return None;
        }
        let count = header.points as usize;
        let data = whole.get(WAVEFORM_HEADER..WAVEFORM_HEADER + 2 * count)?;
        let points = data.as_chunks::<2>().0.iter().map(|&p| Point::decode(u16::from_be_bytes(p))).collect();
        Some(Self { per_second: f64::from(header.per_second), points })
    }

    fn index(&self, ms: f64) -> usize {
        ((ms / 1000.0 * self.per_second).max(0.0) as usize).min(self.points.len().saturating_sub(1))
    }

    /// Point à `ms` du début.
    pub fn at(&self, ms: f64) -> Point {
        self.points.get(self.index(ms)).copied().unwrap_or_default()
    }

    /// Plus fort grave entre `from` et `to` (ms).
    fn bass_peak(&self, from: f64, to: f64) -> f32 {
        let (start, end) = (self.index(from), self.index(to));
        self.points.get(start..=end).map_or(0.0, |points| points.iter().map(|p| p.bass()).fold(0.0, f32::max))
    }

    /// Attaque d'une double croche qui commence à `at` (ms) et dure `step` : le pic de présence à
    /// son début moins le creux qui le précède ; 0 sous les seuils d'un appel.
    fn attack(&self, at: f64, step: f64) -> f32 {
        let range = |from: f64, to: f64| {
            let (start, end) = (self.index(from), self.index(to));
            self.points.get(start..=end).unwrap_or_default()
        };
        let peak = range(at + CALL_PEAK_MS.0, at + CALL_PEAK_MS.1).iter().map(|p| p.presence()).fold(0.0, f32::max);
        let valley = range(at - step / 2.0, at + CALL_PEAK_MS.0).iter().map(|p| p.presence()).fold(peak, f32::min);
        let attack = peak - valley;
        if attack >= CALL_ATTACK && peak >= CALL_PEAK { attack } else { 0.0 }
    }

    /// Couleur moyenne entre `from` et `to` (ms), pondérée par la hauteur et portée à son plein.
    fn color(&self, from: f64, to: f64) -> Option<[f32; 3]> {
        let (start, end) = (self.index(from), self.index(to));
        let points = self.points.get(start..=end)?;
        let mut sum = [0.0f32; 3];
        for p in points {
            sum[0] += p.red * p.height;
            sum[1] += p.green * p.height;
            sum[2] += p.blue * p.height;
        }
        let max = sum.iter().copied().fold(0.0, f32::max);
        (max > 1e-3).then(|| sum.map(|c| c / max))
    }
}

/// Ce que l'OPUS a envoyé d'un morceau.
#[derive(Debug, Clone, Default)]
pub struct Track {
    pub id: u32,
    pub grid: Option<BeatGrid>,
    pub song: Option<SongStructure>,
    pub waveform: Option<Waveform>,
    /// Temps de la grille (rang 0 : temps 1) qui portent un kick, une fois grille et waveform reçues.
    pub kicks: Vec<bool>,
    /// Attaque des appels de chaque double croche (0 : pas d'appel), temps par temps.
    pub calls: Vec<[f32; 4]>,
}

impl Track {
    /// Repère les kicks et les appels de la grille dans la waveform.
    fn analyze(&mut self) {
        let (Some(grid), Some(waveform)) = (&self.grid, &self.waveform) else {
            self.kicks.clear();
            self.calls.clear();
            return;
        };
        self.calls = grid
            .beats
            .iter()
            .enumerate()
            .map(|(i, beat)| {
                let start = f64::from(beat.time_ms);
                let next = grid.beats.get(i + 1).map_or(start + 60_000.0 / beat.bpm.max(1.0), |b| f64::from(b.time_ms));
                let step = (next - start) / 4.0;
                std::array::from_fn(|s| waveform.attack(start + s as f64 * step, step))
            })
            .collect();
        let peaks: Vec<f32> = grid
            .beats
            .iter()
            .map(|beat| {
                let t = f64::from(beat.time_ms);
                waveform.bass_peak(t + KICK_WINDOW_MS.0, t + KICK_WINDOW_MS.1)
            })
            .collect();
        let mut sorted = peaks.clone();
        sorted.sort_by(f32::total_cmp);
        let strong = sorted.get(((sorted.len() as f32 * KICK_PERCENTILE) as usize).min(sorted.len().saturating_sub(1)));
        let threshold = strong.map_or(f32::INFINITY, |s| (KICK_SHARE * s).max(0.05));
        self.kicks = peaks.iter().map(|&peak| peak >= threshold).collect();
    }

    /// Le temps `beat` (numéro de la grille, à partir de 1) porte un kick. Sans waveform, tous les
    /// temps en portent, sauf ceux des phrases Down.
    pub fn kick(&self, beat: i64) -> bool {
        let Some(index) = usize::try_from(beat - 1).ok() else { return false };
        if self.waveform.is_some() {
            return self.kicks.get(index).copied().unwrap_or(false);
        }
        let in_grid = self.grid.as_ref().is_some_and(|grid| index < grid.beats.len());
        in_grid && !self.phrase_at(beat as u32).is_some_and(|p| p.is_down)
    }

    /// Attaque de l'appel de la double croche `step` (0 à 3) du temps `beat` ; 0 s'il n'y en a pas.
    pub fn call(&self, beat: u32, step: u8) -> f32 {
        usize::try_from(beat).ok().and_then(|b| b.checked_sub(1)).and_then(|i| self.calls.get(i)).map_or(0.0, |c| c[usize::from(step.min(3))])
    }

    /// Le groove tient autour du temps `beat`.
    fn grooving(&self, beat: u32) -> bool {
        let beat = i64::from(beat);
        (GROOVE_WINDOW.0..=GROOVE_WINDOW.1).filter(|&d| self.kick(beat + d)).count() >= GROOVE_KICKS
    }

    /// Phrase rekordbox qui contient le temps `beat`.
    pub fn phrase_at(&self, beat: u32) -> Option<PhraseSpan> {
        let song = self.song.as_ref()?;
        let index = song.phrases.iter().rposition(|p| u32::from(p.beat) <= beat)?;
        let phrase = &song.phrases[index];
        let end = song.phrases.get(index + 1).map_or(u32::from(song.end_beat) + 1, |next| u32::from(next.beat));
        let is_up = |kind: u16| song.mood == 1 && kind == 2;
        // Une montée court sur les phrases Up qui se suivent ; son drop tombe au début de la suivante.
        let build_end = is_up(phrase.kind).then(|| {
            song.phrases[index..]
                .iter()
                .find(|p| !is_up(p.kind))
                .map_or(u32::from(song.end_beat) + 1, |p| u32::from(p.beat))
        });
        let build_start = song.phrases[..=index]
            .iter()
            .rev()
            .take_while(|p| is_up(p.kind))
            .last()
            .map_or(u32::from(phrase.beat), |p| u32::from(p.beat));
        Some(PhraseSpan {
            label: song.label(phrase.kind),
            start: u32::from(phrase.beat),
            end,
            next: song.phrases.get(index + 1).map(|next| song.label(next.kind)),
            is_down: song.mood == 1 && phrase.kind == 3,
            build: build_end.map(|end| (build_start, end)),
        })
    }
}

/// Une phrase rekordbox autour d'un temps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseSpan {
    pub label: String,
    /// Premier temps de la phrase, et premier temps de la suivante.
    pub start: u32,
    pub end: u32,
    pub next: Option<String>,
    pub is_down: bool,
    /// Phrase Up : début de la montée (première phrase Up de la suite) et temps du drop.
    pub build: Option<(u32, u32)>,
}

/// Instant des temps d'une platine, estimé à partir des changements de numéro.
#[derive(Debug, Default)]
struct BeatClock {
    /// Dernier numéro de temps vu, arrivée du dernier paquet qui le portait, et du premier.
    last: Option<(u32, f64)>,
    since: f64,
    /// Repère : numéro d'un temps et son instant estimé.
    anchor: Option<(u32, f64)>,
    observations: u32,
    /// Changements de temps de suite très loin de l'instant prévu.
    outliers: u32,
}

impl BeatClock {
    /// Un état est arrivé à `t` avec le temps `beat`, la période étant `period` (s) ; `playing` si la
    /// platine joue, `looping` si elle boucle.
    fn on_status(&mut self, t: f64, beat: u32, period: f64, playing: bool, looping: bool) {
        match self.last {
            Some((last, _)) if last == beat => {}
            Some((last, seen)) if playing && beat == last + 1 && self.anchor.is_some() => {
                // Le temps a commencé entre le dernier paquet qui portait le précédent et celui-ci.
                self.observe(beat, (seen + t) / 2.0, period);
            }
            Some((last, seen)) if playing && looping && self.anchor.is_some() => {
                // Retour au début d'une boucle : elle est calée sur les temps, le suivant tombe sur la
                // grille sous un autre numéro.
                if let Some((anchor, at)) = self.anchor {
                    let elapsed = f64::from(last) - f64::from(anchor);
                    self.anchor = Some((last, at + elapsed * period));
                }
                // Le temps d'après la fin de boucle peut s'afficher un instant avant le retour : le
                // début de boucle prend alors sa place.
                if t - self.since >= period / 2.0 {
                    self.observe(last + 1, (seen + t) / 2.0, period);
                }
                self.anchor = self.anchor.map(|(_, at)| (beat, at));
            }
            // Départ, saut ou lecture à l'envers : la place dans le temps est inconnue.
            _ => {
                self.anchor = Some((beat, t - period / 2.0));
                (self.observations, self.outliers) = (0, 0);
            }
        }
        if self.last.is_none_or(|(last, _)| last != beat) {
            self.since = t;
        }
        self.last = Some((beat, t));
    }

    fn observe(&mut self, beat: u32, observed: f64, period: f64) {
        let Some((anchor, at)) = self.anchor else { return };
        let predicted = at + f64::from(beat.saturating_sub(anchor)) * period;
        // Un changement isolé très loin de l'instant prévu (paquet retardé, numéro qui ne fait que
        // passer) ne déplace pas la phase ; deux de suite, si : elle a vraiment bougé.
        if self.settled() && (observed - predicted).abs() > OUTLIER * period {
            self.outliers += 1;
            self.anchor = Some((beat, if self.outliers >= 2 { observed } else { predicted }));
            if self.outliers >= 2 {
                (self.observations, self.outliers) = (1, 0);
            }
            return;
        }
        self.outliers = 0;
        self.observations = (self.observations + 1).min(PHASE_MEMORY);
        let gain = 1.0 / f64::from(self.observations);
        self.anchor = Some((beat, predicted + gain * (observed - predicted)));
    }

    /// La phase s'appuie sur assez de changements de temps pour flasher.
    fn settled(&self) -> bool {
        self.observations >= SETTLED_AFTER
    }
}

/// Ce qu'on sait d'une platine.
#[derive(Debug, Default)]
struct Deck {
    status: Option<Status>,
    at: f64,
    clock: BeatClock,
}

/// Données d'un morceau en attente : type de bloc, essais et prochain essai.
#[derive(Debug)]
struct Request {
    tries: u32,
    next: f64,
}

/// La platine qui mène, à un instant : de quoi caler la lumière.
#[derive(Debug, Clone)]
pub struct Playing {
    pub deck: u8,
    pub track: Arc<Track>,
    /// Durée d'un temps au tempo joué (s).
    pub period: f64,
    /// Numéro d'un temps et son instant estimé (s, horloge commune).
    pub beat: u32,
    pub beat_time: f64,
    /// La phase s'appuie sur assez de changements de temps pour flasher.
    pub settled: bool,
}

/// Ce que dit l'OPUS du morceau à l'instant `now`, pour la lumière.
#[derive(Debug, Clone)]
pub struct Guide {
    pub period: f64,
    /// Temps en cours (numéro de la grille) et son début.
    pub beat: u32,
    pub beat_time: f64,
    /// Place dans la mesure (0 à 3), et dans la phrase de la lumière (0 à 15).
    pub bar_beat: u8,
    pub phrase_beat: u8,
    pub phrase: Option<PhraseSpan>,
    pub phase: Phase,
    /// Avancement de la montée (0 à 1) et secondes avant son drop, s'il porte un kick.
    pub build: Option<f32>,
    pub drop_in: Option<f32>,
    /// Niveaux tirés de la waveform à cet instant.
    pub levels: Levels,
    /// Couleur de la waveform sur le temps en cours, portée à son plein.
    pub color: Option<[f32; 3]>,
    /// Prochain kick à partir de `now` : son numéro, son instant et sa place.
    pub next_kick: Option<(u32, f64, Accent)>,
    /// Pendant une montée, prochain appel (hors kick) : temps, double croche, instant et attaque.
    pub next_call: Option<(u32, u8, f64, f32)>,
    /// Position dans le morceau, à vitesse normale (ms).
    pub position_ms: f64,
}

impl Playing {
    /// Où en est le morceau à l'instant `now` (s, horloge commune), décalé de `offset` (s : les
    /// paquets arrivent après le son de tant).
    pub fn guide(&self, now: f64, offset: f64) -> Option<Guide> {
        let grid = self.track.grid.as_ref()?;
        let period = self.period;
        let origin = self.beat_time - offset;
        let elapsed = ((now - origin) / period).floor();
        let beat = u32::try_from(i64::from(self.beat) + elapsed as i64).ok().filter(|&b| b >= 1)?;
        let entry = grid.beats.get(beat as usize - 1)?;
        let beat_time = origin + elapsed * period;
        let fraction = ((now - beat_time) / period).clamp(0.0, 1.0);
        let phrase = self.track.phrase_at(beat);
        let phrase_start = phrase.as_ref().map_or(1, |p| p.start);
        let phase = match &phrase {
            Some(PhraseSpan { build: Some(_), .. }) => Phase::BuildUp,
            _ if self.track.grooving(beat) => Phase::Groove,
            _ => Phase::Calm,
        };
        let (build, drop_in) = match phrase.as_ref().and_then(|p| p.build) {
            Some((start, end)) => {
                let progress = (f64::from(beat - start) + fraction) / f64::from((end - start).max(1));
                let drop_in = self.track.kick(i64::from(end)).then(|| ((f64::from(end - beat) - fraction) * period) as f32);
                (Some(progress.clamp(0.0, 1.0) as f32), drop_in)
            }
            None => (None, None),
        };
        // Position dans le morceau à vitesse normale, entre ce temps et le suivant.
        let next_ms = grid.beats.get(beat as usize).map_or(f64::from(entry.time_ms) + 60_000.0 / entry.bpm, |b| f64::from(b.time_ms));
        let ms = f64::from(entry.time_ms) + fraction * (next_ms - f64::from(entry.time_ms));
        let (levels, color) = match &self.track.waveform {
            Some(waveform) => {
                let p = waveform.at(ms);
                let levels = Levels {
                    bass: p.bass(),
                    mid: p.green * p.height,
                    high: p.blue * p.height,
                    tilt: None,
                    presence: None,
                    raw_bass: p.bass(),
                };
                (levels, waveform.color(f64::from(entry.time_ms), next_ms))
            }
            None => (Levels::default(), None),
        };
        let accent = |k: u32| {
            if (k.saturating_sub(phrase_start)).is_multiple_of(PHRASE_BEATS) {
                Accent::Phrase
            } else if grid.beats.get(k as usize - 1).is_some_and(|b| b.bar_beat == 1) {
                Accent::Bar
            } else {
                Accent::Beat
            }
        };
        // Le prochain kick : celui du temps en cours s'il n'est pas encore passé, sinon un suivant.
        let first = if fraction <= 0.0 { beat } else { beat + 1 };
        let next_kick = (first..first + KICK_LOOKAHEAD)
            .find(|&k| self.track.kick(i64::from(k)))
            .map(|k| (k, origin + (elapsed + f64::from(k - beat)) * period, accent(k)));
        // Pendant une montée, la prochaine double croche à venir qui porte un appel (celle d'un kick
        // est déjà flashée par lui).
        let next_call = (phase == Phase::BuildUp)
            .then(|| {
                (beat..beat + 2)
                    .flat_map(|k| (0..4u8).map(move |s| (k, s)))
                    .map(|(k, s)| (k, s, beat_time + (f64::from(k - beat) + f64::from(s) / 4.0) * period))
                    .filter(|&(_, _, at)| at > now)
                    .find(|&(k, s, _)| self.track.call(k, s) > 0.0 && !(s == 0 && self.track.kick(i64::from(k))))
                    .map(|(k, s, at)| (k, s, at, self.track.call(k, s)))
            })
            .flatten();
        Some(Guide {
            period,
            beat,
            beat_time,
            bar_beat: entry.bar_beat.saturating_sub(1).min(3),
            phrase_beat: ((beat - phrase_start.min(beat)) % PHRASE_BEATS) as u8,
            phrase,
            phase,
            build,
            drop_in,
            levels,
            color,
            next_kick,
            next_call,
            position_ms: ms,
        })
    }
}

/// État de la liaison, pour l'affichage.
#[derive(Debug, Clone, Default)]
pub struct LinkState {
    pub opus: Option<Ipv4Addr>,
    /// Carte réseau du PC qui mène à l'OPUS.
    pub route: Option<String>,
    /// L'OPUS a répondu à rekordbox, et un état est arrivé il y a moins d'une seconde.
    pub answered: bool,
    pub live: bool,
}

/// Client de la liaison : il digère les paquets reçus, dit quoi envoyer, et tient l'état des
/// platines et des morceaux. Les instants sont en secondes de l'horloge commune.
#[derive(Default)]
pub struct Client {
    interfaces: Vec<Interface>,
    chosen: Option<Interface>,
    opus: Option<Ipv4Addr>,
    answered: bool,
    next_announce: f64,
    next_rescan: f64,
    decks: [Deck; 4],
    parts: Reassembler,
    /// Morceaux par platine et identifiant, et données redemandées (platine, morceau, type).
    tracks: HashMap<(u8, u32), Arc<Track>>,
    requests: HashMap<(u8, u32, u8), Request>,
}

/// Paquet à envoyer.
pub type Outgoing = (SocketAddrV4, Vec<u8>);

impl Client {
    pub fn new(interfaces: Vec<Interface>) -> Self {
        Self { interfaces, ..Self::default() }
    }

    /// Carte réseau choisie d'avance (son nom complet) : on s'y annonce dès le départ, sans
    /// attendre que l'OPUS s'y montre. S'il parle par une autre, la liaison le suit.
    pub fn prefer(&mut self, label: &str) {
        if let Some(iface) = self.interfaces.iter().find(|i| i.label == label) {
            self.chosen = Some(iface.clone());
            self.next_announce = f64::NEG_INFINITY;
        }
    }

    pub fn state(&self, now: f64) -> LinkState {
        LinkState {
            opus: self.opus,
            route: self.chosen.as_ref().map(|i| i.label.clone()),
            answered: self.answered,
            live: self.decks.iter().any(|d| d.status.is_some() && now - d.at < STALE),
        }
    }

    /// Adresses du PC : ses propres envois lui reviennent par la diffusion.
    pub fn is_own(&self, ip: Ipv4Addr) -> bool {
        self.interfaces.iter().any(|i| i.ip == ip)
    }

    /// Un paquet est arrivé de `from` à l'instant `t` sur le port `port`.
    pub fn on_packet(&mut self, t: f64, port: u16, from: Ipv4Addr, data: &[u8], out: &mut Vec<Outgoing>) {
        if self.is_own(from) {
            return;
        }
        match prolink::parse(port, data) {
            Some(Packet::KeepAlive(device)) if device.is_opus() => self.opus_seen(from),
            Some(Packet::Hello { .. }) => {
                self.opus_seen(from);
                if !self.answered {
                    self.answered = true;
                    out.push((SocketAddrV4::new(from, prolink::STATUS_PORT), prolink::lighting_request()));
                }
            }
            Some(Packet::Status(status)) => {
                self.opus_seen(from);
                self.on_status(t, status);
            }
            Some(Packet::Metadata(part)) => self.on_metadata(&part),
            _ => {}
        }
    }

    fn opus_seen(&mut self, ip: Ipv4Addr) {
        if self.opus.is_some() {
            return;
        }
        if !self.interfaces.iter().any(|i| i.net.contains(&ip)) {
            self.interfaces = interfaces();
        }
        self.opus = Some(ip);
        self.chosen = self.interfaces.iter().find(|i| i.net.contains(&ip)).cloned();
        self.next_announce = f64::NEG_INFINITY;
    }

    fn on_status(&mut self, t: f64, mut status: Status) {
        let number = status.deck();
        let Some(deck) = self.decks.get_mut(usize::from(number).wrapping_sub(1)) else { return };
        // L'OPUS envoie parfois des états aux drapeaux à zéro : on garde les précédents.
        if status.flags == 0
            && let Some(previous) = &deck.status
        {
            status.flags = previous.flags;
        }
        let track_changed = deck.status.as_ref().is_none_or(|previous| previous.track_id != status.track_id);
        if track_changed {
            deck.clock = BeatClock::default();
        }
        if let (Some(beat), Some(bpm)) = (status.beat, status.played_bpm().filter(|&bpm| bpm > 1.0)) {
            deck.clock.on_status(t, beat, 60.0 / bpm, status.playing(), status.looping());
        }
        let track = status.track_id;
        deck.status = Some(status);
        deck.at = t;
        if track != 0 {
            for kind in [block::BEAT_GRID, block::SONG_STRUCTURE, block::WAVEFORM_DETAIL] {
                self.requests.entry((number, track, kind)).or_insert(Request { tries: 0, next: t + REQUEST_RETRY });
            }
        }
        self.forget_unloaded();
    }

    /// Ne garde que les morceaux encore chargés sur une platine.
    fn forget_unloaded(&mut self) {
        let loaded: Vec<(u8, u32)> = self
            .decks
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.status.as_ref().map(|s| (i as u8 + 1, s.track_id)))
            .collect();
        self.tracks.retain(|key, _| loaded.contains(key));
        self.requests.retain(|(deck, track, _), _| loaded.contains(&(*deck, *track)));
    }

    fn on_metadata(&mut self, part: &prolink::Metadata) {
        let Some(whole) = self.parts.push(part) else { return };
        let key = (part.deck(), part.track_id);
        let mut track = self.tracks.get(&key).map_or_else(|| Track { id: part.track_id, ..Track::default() }, |t| (**t).clone());
        match part.kind {
            block::BEAT_GRID => track.grid = BeatGrid::parse(&whole),
            block::SONG_STRUCTURE => track.song = SongStructure::parse(&whole),
            block::WAVEFORM_DETAIL => track.waveform = Waveform::parse(&whole),
            _ => return,
        }
        self.requests.remove(&(key.0, key.1, part.kind));
        track.analyze();
        self.tracks.insert(key, Arc::new(track));
    }

    /// Envois réguliers : annonce de présence et demande d'état, données manquantes.
    pub fn tick(&mut self, t: f64, out: &mut Vec<Outgoing>) {
        if t >= self.next_rescan {
            self.next_rescan = t + RESCAN_INTERVAL;
            if self.chosen.is_none() {
                self.interfaces = interfaces();
                if let Some(ip) = self.opus.take() {
                    self.opus_seen(ip);
                }
            }
        }
        let Some(iface) = self.chosen.clone() else { return };
        if t >= self.next_announce {
            self.next_announce = t + ANNOUNCE_INTERVAL;
            let broadcast = iface.net.broadcast();
            out.push((SocketAddrV4::new(broadcast, prolink::ANNOUNCE_PORT), prolink::keep_alive(iface.mac, iface.ip)));
            out.push((SocketAddrV4::new(broadcast, prolink::STATUS_PORT), prolink::lighting_request()));
        }
        let Some(opus) = self.opus else { return };
        for ((deck, track, kind), request) in &mut self.requests {
            if t >= request.next && request.tries < REQUEST_TRIES {
                request.tries += 1;
                request.next = t + REQUEST_RETRY;
                let device = prolink::OPUS_FIRST_DEVICE - 1 + deck;
                out.push((SocketAddrV4::new(opus, prolink::STATUS_PORT), prolink::metadata_request(*track, *kind, device)));
            }
        }
    }

    /// La platine qui mène à l'instant `now` : la maître si elle joue, sinon une platine à
    /// l'antenne qui joue ; il faut sa grille.
    pub fn playing(&self, now: f64) -> Option<Playing> {
        let fresh = |d: &&Deck| d.status.as_ref().is_some_and(|s| s.playing() && s.track_id != 0) && now - d.at < STALE;
        let candidates: Vec<(usize, &Deck)> = self.decks.iter().enumerate().filter(|(_, d)| fresh(d)).collect();
        let (index, deck) = candidates
            .iter()
            .find(|(_, d)| d.status.as_ref().is_some_and(Status::master))
            .or_else(|| candidates.iter().find(|(_, d)| d.status.as_ref().is_some_and(Status::on_air)))
            .copied()?;
        let status = deck.status.as_ref()?;
        let number = index as u8 + 1;
        let track = self.tracks.get(&(number, status.track_id))?.clone();
        track.grid.as_ref()?;
        let (beat, beat_time) = deck.clock.anchor?;
        let bpm = status.played_bpm().filter(|&bpm| bpm > 1.0)?;
        Some(Playing {
            deck: number,
            track,
            period: 60.0 / bpm,
            beat,
            beat_time,
            settled: deck.clock.settled(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prolink::{GridBeat, Phrase};

    /// Morceau à 150 BPM : intro 1, Down (break) 33, Up 65 et 81, Chorus 97, fin 160 ; kicks partout
    /// sauf dans le break et la montée.
    fn track() -> Track {
        let period_ms = 400.0;
        let beats = (0..160)
            .map(|i| GridBeat { bar_beat: (i % 4) as u8 + 1, bpm: 150.0, time_ms: (i as f64 * period_ms) as u32 })
            .collect();
        let song = SongStructure {
            mood: 1,
            end_beat: 160,
            phrases: [(1, 1), (33, 3), (65, 2), (81, 2), (97, 5)].map(|(beat, kind)| Phrase { beat, kind }).to_vec(),
        };
        let per_second = 150.0;
        let mut points = vec![Point { red: 0.0, green: 0.4, blue: 0.8, height: 0.3 }; (160.0 * 0.4 * per_second) as usize];
        for beat in (1..=32).chain(97..=160) {
            let at = ((beat - 1) as f64 * 0.4 * per_second) as usize;
            for p in &mut points[at..at + 3] {
                *p = Point { red: 1.0, green: 0.3, blue: 0.1, height: 1.0 };
            }
        }
        // Montée (65 à 96) : un charleston bref sur la troisième double croche de chaque temps.
        for beat in 65..=96 {
            let at = (((beat - 1) as f64 + 0.5) * 0.4 * per_second) as usize;
            for p in &mut points[at..at + 3] {
                *p = Point { red: 0.0, green: 0.6, blue: 1.0, height: 0.8 };
            }
        }
        let mut track = Track {
            id: 1,
            grid: Some(BeatGrid { beats }),
            song: Some(song),
            waveform: Some(Waveform { per_second, points }),
            kicks: Vec::new(),
            calls: Vec::new(),
        };
        track.analyze();
        track
    }

    fn playing(track: Track) -> Playing {
        Playing { deck: 1, track: Arc::new(track), period: 0.4, beat: 1, beat_time: 10.0, settled: true }
    }

    #[test]
    fn decodes_waveform_points() {
        let p = Point::decode(0x13a4);
        assert_eq!((p.red, p.green, p.blue), (0.0, 4.0 / 7.0, 1.0));
        assert!((p.height - 9.0 / 31.0).abs() < 1e-6);
        let silent = Point::decode(0xff80);
        assert_eq!((silent.red, silent.height), (1.0, 0.0));
    }

    #[test]
    fn kicks_come_from_the_waveform() {
        let track = track();
        assert!(track.kick(1) && track.kick(32) && track.kick(97));
        assert!(!track.kick(33) && !track.kick(70) && !track.kick(96));
        assert!(!track.kick(0) && !track.kick(161));
    }

    #[test]
    fn phases_follow_the_phrases_and_the_kicks() {
        let playing = playing(track());
        let at = |beat: u32| 10.0 + f64::from(beat - 1) * 0.4 + 0.01;
        let guide = |beat| playing.guide(at(beat), 0.0).unwrap();
        assert_eq!(guide(5).phase, Phase::Groove);
        assert_eq!(guide(40).phase, Phase::Calm, "break sans kick");
        let build = guide(73);
        assert_eq!(build.phase, Phase::BuildUp);
        assert!((build.build.unwrap() - (8.0 + 0.025) / 32.0).abs() < 0.01, "{:?}", build.build);
        assert!((build.drop_in.unwrap() - (24.0 - 0.025) * 0.4).abs() < 0.01, "drop au Chorus 97");
        assert_eq!(guide(97).phase, Phase::Groove, "le drop groove dès son premier temps");
        assert_eq!(guide(32).phase, Phase::Groove, "le dernier kick avant le break groove encore");
        let span = guide(73).phrase.unwrap();
        assert_eq!((span.label.as_str(), span.start, span.build), ("Up", 65, Some((65, 97))));
    }

    #[test]
    fn the_next_kick_skips_the_break_and_carries_its_accent() {
        let playing = playing(track());
        let guide = playing.guide(10.0 + 31.0 * 0.4 + 0.05, 0.0).unwrap();
        assert_eq!(guide.beat, 32);
        assert_eq!(guide.next_kick, None, "rien à flasher au début du break");
        let guide = playing.guide(10.0 + 93.0 * 0.4 + 0.05, 0.0).unwrap();
        let (beat, at, accent) = guide.next_kick.unwrap();
        assert_eq!(beat, 97, "le kick du drop, après la montée sans kick");
        assert!((at - (10.0 + 96.0 * 0.4)).abs() < 1e-9);
        assert_eq!(accent, Accent::Phrase);
        let groove = playing.guide(10.0 + 4.0 * 0.4 - 0.01, 0.0).unwrap();
        assert_eq!(groove.next_kick.map(|(b, _, a)| (b, a)), Some((5, Accent::Bar)));
        // Un décalage avance les temps d'autant.
        let shifted = playing.guide(10.0 + 4.0 * 0.4 - 0.03, 0.02).unwrap();
        let (beat, at, _) = shifted.next_kick.unwrap();
        assert_eq!(beat, 5);
        assert!((at - (10.0 + 4.0 * 0.4 - 0.02)).abs() < 1e-9);
    }

    #[test]
    fn a_build_up_calls_on_its_hats_but_not_on_a_steady_pad() {
        let track = track();
        assert!(track.call(70, 2) > 0.3, "le charleston de la montée");
        assert_eq!(track.call(70, 1), 0.0);
        assert_eq!(track.call(40, 2), 0.0, "la nappe continue du break n'a pas d'attaque");
        let playing = playing(track);
        let build = playing.guide(10.0 + 69.0 * 0.4 + 0.05, 0.0).unwrap();
        assert_eq!(build.phase, Phase::BuildUp);
        let (beat, step, at, strength) = build.next_call.unwrap();
        assert_eq!((beat, step), (70, 2));
        assert!((at - (10.0 + 69.5 * 0.4)).abs() < 1e-9);
        assert!(strength > 0.3);
        assert_eq!(playing.guide(10.0 + 39.0 * 0.4 + 0.05, 0.0).unwrap().next_call, None, "pas d'appel hors montée");
    }

    #[test]
    fn the_waveform_gives_levels_and_color() {
        let playing = playing(track());
        let kick = playing.guide(10.0 + 0.001, 0.0).unwrap();
        assert!(kick.levels.bass > 0.9, "{:?}", kick.levels);
        let pad = playing.guide(10.0 + 40.0 * 0.4 + 0.2, 0.0).unwrap();
        assert_eq!(pad.levels.bass, 0.0);
        let color = pad.color.unwrap();
        assert!(color[2] > color[1] && color[0] == 0.0, "break bleuté : {color:?}");
    }

    #[test]
    fn the_beat_clock_narrows_with_each_beat_change() {
        // Temps réels toutes les 0,4 s à partir de 1,0 ; paquets toutes les 0,15 s, arrivés 20 ms tard.
        let mut clock = BeatClock::default();
        let mut t: f64 = 0.93;
        while t < 8.0 {
            let beat = ((t - 0.02 - 1.0) / 0.4).floor() as i64 + 1;
            if beat >= 1 {
                clock.on_status(t, beat as u32, 0.4, true, false);
            }
            t += 0.15;
        }
        let (beat, at) = clock.anchor.unwrap();
        let truth = 1.0 + f64::from(beat - 1) * 0.4 + 0.02;
        assert!((at - truth).abs() < 0.04, "écart {:.0} ms", (at - truth) * 1e3);
        assert!(clock.settled());
        // Un saut repart de zéro.
        clock.on_status(8.1, beat + 40, 0.4, true, false);
        assert!(!clock.settled());
    }

    #[test]
    fn a_loop_keeps_the_phase() {
        // Boucle de 4 temps (5 à 8) : temps réels toutes les 0,4 s à partir de 1,0.
        let mut clock = BeatClock::default();
        let mut t: f64 = 0.93;
        while t < 12.0 {
            let n = ((t - 0.02 - 1.0) / 0.4).floor() as i64;
            if n >= 0 {
                let beat = if n < 8 { n + 1 } else { 5 + (n - 8).rem_euclid(4) };
                clock.on_status(t, beat as u32, 0.4, true, n >= 8);
            }
            t += 0.15;
        }
        assert!(clock.settled(), "la boucle ne fait pas repartir le calage");
        let (beat, at) = clock.anchor.unwrap();
        // L'instant du repère tombe sur la grille des vrais temps.
        let phase = ((at - 1.02) / 0.4 - ((at - 1.02) / 0.4).round()).abs() * 0.4;
        assert!(phase < 0.04, "temps {beat} à {:.0} ms de la grille", phase * 1e3);
    }

    /// Graves moyens de la waveform autour des temps de la grille, selon la taille d'en-tête
    /// supposée, pour un journal de la sonde (`DJB_OPUS_JOURNAL`).
    #[test]
    #[ignore]
    fn waveform_alignment() {
        let path = std::env::var("DJB_OPUS_JOURNAL").expect("DJB_OPUS_JOURNAL");
        let text = std::fs::read_to_string(path).unwrap();
        let mut parts = Reassembler::default();
        let mut blocks: HashMap<(u8, u8), Vec<u8>> = HashMap::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let fields: Vec<&str> = line.split(' ').collect();
            if fields.len() < 5 || fields[1] != "<" || fields[2] != "50002" {
                continue;
            }
            let data = hex::decode(fields[4]).unwrap();
            if let Some(Packet::Metadata(part)) = prolink::parse(prolink::STATUS_PORT, &data)
                && let Some(whole) = parts.push(&part)
            {
                blocks.insert((part.deck(), part.kind), whole);
            }
        }
        for deck in [1, 2] {
            let grid = BeatGrid::parse(&blocks[&(deck, block::BEAT_GRID)]).unwrap();
            let raw = &blocks[&(deck, block::WAVEFORM_DETAIL)];
            for header in [12usize, 16] {
                let points: Vec<Point> =
                    raw[header..].as_chunks::<2>().0.iter().map(|&p| Point::decode(u16::from_be_bytes(p))).collect();
                let waveform = Waveform { per_second: 150.0, points };
                let profile: Vec<String> = (-6..=9)
                    .map(|lag| {
                        let ms = f64::from(lag) * 1000.0 / 150.0;
                        let mean = grid.beats.iter().map(|b| waveform.at(f64::from(b.time_ms) + ms).bass()).sum::<f32>()
                            / grid.beats.len() as f32;
                        format!("{ms:+.0}:{mean:.3}")
                    })
                    .collect();
                println!("platine {deck}, en-tête {header} : {}", profile.join(" "));
            }
        }
    }

    /// Pour chaque morceau d'un journal (`DJB_OPUS_JOURNAL`), la présence (médiums et aigus) de la
    /// waveform double croche par double croche dans les phrases Up, et son attaque.
    #[test]
    #[ignore]
    fn build_presence() {
        let path = std::env::var("DJB_OPUS_JOURNAL").expect("DJB_OPUS_JOURNAL");
        let text = std::fs::read_to_string(path).unwrap();
        let mut client = Client::new(Vec::new());
        let mut out = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() >= 5 && f[1] == "<" {
                let from: Ipv4Addr = f[3].split(':').next().unwrap().parse().unwrap();
                client.on_packet(f[0].parse().unwrap(), f[2].parse().unwrap(), from, &hex::decode(f[4]).unwrap(), &mut out);
            }
        }
        for track in client.tracks.values() {
            let (Some(grid), Some(waveform), Some(song)) = (&track.grid, &track.waveform, &track.song) else { continue };
            println!("morceau {}", track.id);
            for (index, phrase) in song.phrases.iter().enumerate() {
                let end = song.phrases.get(index + 1).map_or(u32::from(song.end_beat), |p| u32::from(p.beat));
                println!("  {} {}–{}", song.label(phrase.kind), phrase.beat, end - 1);
                if song.label(phrase.kind) != "Up" {
                    continue;
                }
                // Une mesure par ligne : K kick, x appel, · rien, double croche par double croche.
                let _ = (grid, waveform);
                for bar in (u32::from(phrase.beat)..end).step_by(4) {
                    let cells: String = (bar..(bar + 4).min(end))
                        .map(|beat| {
                            (0..4u8)
                                .map(|s| match (s == 0 && track.kick(i64::from(beat)), track.call(beat, s) > 0.0) {
                                    (true, _) => 'K',
                                    (false, true) => 'x',
                                    _ => '·',
                                })
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    println!("    {bar:4} {cells}");
                }
            }
        }
    }

    /// Rejoue un journal de la sonde (`DJB_OPUS_JOURNAL`) à 60 images par seconde comme la boucle
    /// d'éclairage, et compare l'instant visé par chaque flash aux temps réels d'un passage de
    /// lecture stable, retrouvés après coup sur tout le passage (`DJB_OPUS_SEGMENT` : platine,
    /// début et fin en s, par défaut « 1 112 148 »).
    #[test]
    #[ignore]
    fn replay_flashes() {
        let path = std::env::var("DJB_OPUS_JOURNAL").expect("DJB_OPUS_JOURNAL");
        let segment = std::env::var("DJB_OPUS_SEGMENT").unwrap_or_else(|_| "1 112 148".to_owned());
        let segment: Vec<f64> = segment.split(' ').map(|s| s.parse().unwrap()).collect();
        let (deck, from, until) = (segment[0] as u8, segment[1], segment[2]);
        let text = std::fs::read_to_string(path).unwrap();
        let packets: Vec<(f64, u16, Ipv4Addr, Vec<u8>)> = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|line| {
                let f: Vec<&str> = line.split(' ').collect();
                (f.len() >= 5 && f[1] == "<").then(|| {
                    (f[0].parse().unwrap(), f[2].parse().unwrap(), f[3].split(':').next().unwrap().parse().unwrap(), hex::decode(f[4]).unwrap())
                })
            })
            .collect();
        // Temps réels du passage : régression des milieux des changements de numéro, comptés un à un
        // (une boucle fait revenir le numéro, pas le temps).
        let mut changes = Vec::new();
        let mut last: Option<(u32, f64)> = None;
        let (mut count, mut since) = (0.0, 0.0);
        for (t, port, _, data) in &packets {
            if let Some(Packet::Status(s)) = prolink::parse(*port, data)
                && s.deck() == deck
                && let Some(beat) = s.beat
            {
                if let Some((b, seen)) = last
                    && beat != b
                {
                    // Un numéro qui n'a fait que passer (fin de boucle) ne compte pas pour un temps.
                    let transient = t - since < 0.2;
                    if (beat == b + 1 || s.looping()) && (from..until).contains(t) && !transient {
                        count += 1.0;
                        changes.push((count, (seen + t) / 2.0));
                    }
                    since = *t;
                }
                last = Some((beat, *t));
            }
        }
        // Grille des vrais temps autour de l'instant `at` : régression sur les changements à moins
        // de 3 s (une dérive ou un temps mal compté plus loin n'y joue pas).
        let fit = |at: f64| {
            let near: Vec<&(f64, f64)> = changes.iter().filter(|(_, m)| (m - at).abs() < 3.0).collect();
            let n = near.len() as f64;
            let (sx, sy) = near.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
            let sxx: f64 = near.iter().map(|(x, _)| x * x).sum();
            let sxy: f64 = near.iter().map(|(x, y)| x * y).sum();
            let period = (n * sxy - sx * sy) / (n * sxx - sx * sx);
            (period, (sy - period * sx) / n)
        };
        println!("passage : {} changements de temps", changes.len());

        let mut client = Client::new(Vec::new());
        let mut out = Vec::new();
        let mut flashed: Option<(u8, u32, u32)> = None;
        let (mut errors, mut kicks_missed, mut off_kick) = (Vec::new(), 0, 0);
        let mut frame = packets[0].0;
        let mut next = 0;
        let advance = 0.045;
        let mut beats_seen = std::collections::BTreeSet::new();
        while next < packets.len() {
            while next < packets.len() && packets[next].0 <= frame {
                let (t, port, from, data) = &packets[next];
                client.on_packet(*t, *port, *from, data, &mut out);
                next += 1;
            }
            if let Some(playing) = client.playing(frame).filter(|p| p.deck == deck && p.settled)
                && let Some(guide) = playing.guide(frame, 0.0)
                && (from..until).contains(&frame)
            {
                beats_seen.insert(guide.beat);
                if let Some((beat, at, _)) = guide.next_kick {
                    let key = (playing.deck, playing.track.id, beat);
                    if flashed != Some(key) && frame >= at - advance {
                        flashed = Some(key);
                        // Écart au vrai temps le plus proche.
                        let (period, origin) = fit(at);
                        let beats = (at - origin) / period;
                        errors.push((beats - beats.round()) * period * 1e3);
                        if std::env::var("DJB_OPUS_VERBOSE").is_ok() {
                            println!(
                                "  flash temps {beat} visé {at:.3} écart {:+.0} ms, repère {:?}",
                                (beats - beats.round()) * period * 1e3,
                                client.decks[usize::from(deck) - 1].clock.anchor
                            );
                        }
                        if !playing.track.kick(i64::from(beat)) {
                            off_kick += 1;
                        }
                    }
                }
            }
            frame += 1.0 / 60.0;
        }
        let kick_beats = beats_seen.iter().filter(|&&b| client.tracks.values().any(|t| t.kick(i64::from(b)))).count();
        kicks_missed += kick_beats.saturating_sub(errors.len());
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        let sd = (errors.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / errors.len() as f64).sqrt();
        let worst = errors.iter().fold(0.0f64, |w, e| w.max(e.abs()));
        let late = errors.iter().filter(|e| e.abs() > 40.0).count();
        println!(
            "{} flashs, {kicks_missed} kicks sans flash, {off_kick} hors kick ; écart au vrai temps : moyenne {mean:+.1} ms, écart-type {sd:.1} ms, pire {worst:.1} ms, {late} au-delà de 40 ms",
            errors.len()
        );
    }

    /// Rejoue un journal de la sonde (`DJB_OPUS_JOURNAL`) et imprime, temps par temps, ce que la
    /// lumière ferait : `cargo test opus::tests::replay -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn replay() {
        let path = std::env::var("DJB_OPUS_JOURNAL").expect("DJB_OPUS_JOURNAL");
        let text = std::fs::read_to_string(path).unwrap();
        let mut client = Client::new(Vec::new());
        let mut out = Vec::new();
        let mut shown = None;
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let fields: Vec<&str> = line.split(' ').collect();
            if fields.len() < 5 || fields[1] != "<" {
                continue;
            }
            let t: f64 = fields[0].parse().unwrap();
            let port: u16 = fields[2].parse().unwrap();
            let from: Ipv4Addr = fields[3].split(':').next().unwrap().parse().unwrap();
            client.on_packet(t, port, from, &hex::decode(fields[4]).unwrap(), &mut out);
            let Some(playing) = client.playing(t) else { continue };
            let Some(guide) = playing.guide(t, 0.0) else { continue };
            let key = (playing.deck, guide.beat);
            if shown == Some(key) {
                continue;
            }
            shown = Some(key);
            let phrase = guide.phrase.as_ref().map_or("-".to_owned(), |p| format!("{} {}", p.label, p.start));
            let kick = if playing.track.kick(i64::from(guide.beat)) { "●" } else { "·" };
            println!(
                "{t:8.3} platine {} temps {:4} ({}/4) {kick} {:?} {phrase} bass {:.2} {}",
                playing.deck,
                guide.beat,
                guide.bar_beat + 1,
                guide.phase,
                guide.levels.bass,
                if playing.settled { "" } else { "(phase à caler)" }
            );
        }
    }
}

//! Setlist d'un live avec l'OPUS-QUAD : chaque morceau entendu, dans l'ordre, avec son heure
//! d'entrée depuis le premier (0:00), son titre, son artiste et sa pochette.
//!
//! Un morceau entre dans la setlist quand il s'entend depuis quelques secondes (en lecture, fader
//! ouvert) ; son heure d'entrée est le début de ce passage. Chaque set s'enregistre au fil de l'eau
//! dans son dossier (`sets\<date>\set.json` et les pochettes), que l'historique de la fenêtre relit
//! et qui s'exporte en texte ou en image.

pub mod poster;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

use crate::config::Config;
use crate::opus::LoadedDeck;

/// Un morceau entre quand il s'entend depuis 4 s : un fader ouvert le temps d'un coup d'œil ne
/// compte pas.
const ENTRY_HOLD: f64 = 4.0;
/// Un morceau parmi les deux derniers entrés ne rentre pas à nouveau (fader de la platine sortante
/// rouvert en fin de transition).
const RECENT: usize = 2;
/// La durée du set s'enregistre au plus toutes les 30 s (et à l'arrêt).
const SAVE_EVERY: f64 = 30.0;
const SET_FILE: &str = "set.json";

/// Un morceau de la setlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Heure d'entrée depuis le début du set (s).
    pub at: f64,
    pub title: String,
    pub artist: String,
    pub deck: u8,
    /// Tempo joué à l'entrée, pitch compris.
    pub bpm: Option<f64>,
    pub track_id: u32,
    /// Pochette, fichier JPEG du dossier du set.
    pub artwork: Option<String>,
}

impl Entry {
    /// « Artiste - Titre », ou ce qu'on en sait.
    pub fn line(&self) -> String {
        match (self.artist.is_empty(), self.title.is_empty()) {
            (_, true) => format!("Morceau n° {}", self.track_id),
            (true, false) => self.title.clone(),
            (false, false) => format!("{} - {}", self.artist, self.title),
        }
    }

    /// Titre affiché : le titre, ou le numéro du morceau tant qu'il n'est pas connu.
    pub fn shown_title(&self) -> String {
        if self.title.is_empty() { format!("Morceau n° {}", self.track_id) } else { self.title.clone() }
    }
}

/// Un set joué.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Set {
    /// Début (le 0:00), en secondes Unix, et décalage de l'heure locale à ce moment (s).
    pub started: i64,
    pub utc_offset: i32,
    /// Du premier morceau au dernier instant où un morceau s'entendait (s).
    pub duration: f64,
    pub tracks: Vec<Entry>,
}

const DAYS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
const MONTHS: [&str; 12] =
    ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];

impl Set {
    pub fn load(folder: &Path) -> Result<Self> {
        let text = fs::read_to_string(folder.join(SET_FILE))?;
        Ok(serde_json::from_str(&text)?)
    }

    fn local(&self) -> OffsetDateTime {
        let offset = UtcOffset::from_whole_seconds(self.utc_offset).unwrap_or(UtcOffset::UTC);
        OffsetDateTime::from_unix_timestamp(self.started).unwrap_or(OffsetDateTime::UNIX_EPOCH).to_offset(offset)
    }

    /// « Jeudi 6 novembre 2026 ».
    pub fn date(&self) -> String {
        let t = self.local();
        let day = DAYS[usize::from(t.weekday().number_days_from_monday())];
        let mut text = format!("{day} {} {} {}", t.day(), MONTHS[usize::from(u8::from(t.month())) - 1], t.year());
        text[..1].make_ascii_uppercase();
        text
    }

    /// Heure du début, « 21:14 ».
    pub fn hour(&self) -> String {
        let t = self.local();
        format!("{:02}:{:02}", t.hour(), t.minute())
    }

    /// « 1:12:30 · 18 morceaux ».
    pub fn summary(&self) -> String {
        let count = match self.tracks.len() {
            1 => "1 morceau".to_owned(),
            n => format!("{n} morceaux"),
        };
        format!("{} · {count}", timecode(self.duration))
    }

    /// Nom des fichiers exportés : « Set 2026-11-06 21h14 ».
    pub fn file_stem(&self) -> String {
        let t = self.local();
        format!("Set {}-{:02}-{:02} {:02}h{:02}", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute())
    }

    /// Setlist en texte, réutilisable telle quelle : une ligne par morceau, « 0:00 Artiste - Titre »
    /// (le format des chapitres YouTube et des tracklists).
    pub fn text(&self) -> String {
        let mut text = format!("Set du {} à {}\n{}\n\n", self.date().to_lowercase(), self.hour(), self.summary());
        for entry in &self.tracks {
            text.push_str(&format!("{} {}\n", timecode(entry.at), entry.line()));
        }
        text
    }
}

/// Heure dans le set : « 4:32 », « 1:02:03 ».
pub fn timecode(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Dossier des sets enregistrés (`DJB_SETS_DIR` pour un autre, en développement).
pub fn sets_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DJB_SETS_DIR") {
        return Ok(dir.into());
    }
    Ok(Config::path()?.parent().context("dossier de configuration introuvable")?.join("sets"))
}

/// Les sets enregistrés, du plus récent au plus ancien, avec leur dossier.
pub fn history() -> Vec<(PathBuf, Set)> {
    let Ok(dir) = sets_dir() else { return Vec::new() };
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut sets: Vec<(PathBuf, Set)> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|path| Set::load(&path).ok().filter(|set| !set.tracks.is_empty()).map(|set| (path, set)))
        .collect();
    sets.sort_by_key(|(_, set)| std::cmp::Reverse(set.started));
    sets
}

/// Ce que le live montre du set en cours : l'instant du 0:00 (horloge commune, s) et les morceaux.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SetNow {
    pub origin: Option<f64>,
    pub tracks: Vec<Entry>,
}

/// Enregistre la setlist pendant le live, à partir des platines de l'OPUS.
pub struct Recorder {
    /// Dossier des sets, et celui du set en cours une fois le premier morceau entré.
    root: Option<PathBuf>,
    folder: Option<PathBuf>,
    set: Set,
    /// Instant du 0:00 (horloge commune, s).
    origin: Option<f64>,
    /// Depuis quand chaque morceau (platine, identifiant) s'entend sans interruption.
    heard_since: HashMap<(u8, u32), f64>,
    last_heard: f64,
    saved_at: f64,
    /// Le set a changé depuis son dernier enregistrement.
    dirty: bool,
}

impl Recorder {
    /// `root` : dossier des sets ; `None` pour ne rien écrire (essais).
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            folder: None,
            set: Set::default(),
            origin: None,
            heard_since: HashMap::new(),
            last_heard: 0.0,
            saved_at: 0.0,
            dirty: false,
        }
    }

    pub fn now(&self) -> SetNow {
        SetNow { origin: self.origin, tracks: self.set.tracks.clone() }
    }

    /// Les platines à l'instant `now` (horloge commune, s), `wall` l'heure qu'il est. Rend vrai si
    /// la setlist a changé (morceau entré, titre ou pochette arrivés).
    pub fn update(&mut self, now: f64, wall: SystemTime, decks: &[LoadedDeck]) -> bool {
        let mut changed = false;
        self.heard_since.retain(|key, _| decks.iter().any(|d| d.audible && (d.deck, d.track_id) == *key));
        for deck in decks.iter().filter(|d| d.audible) {
            let since = *self.heard_since.entry((deck.deck, deck.track_id)).or_insert(now);
            self.last_heard = now;
            let recent = self.set.tracks.iter().rev().take(RECENT).any(|e| e.track_id == deck.track_id);
            if now - since >= ENTRY_HOLD && !recent {
                self.enter(deck, since, wall - Duration::from_secs_f64((now - since).max(0.0)));
                changed = true;
            }
        }
        // Titre et pochette arrivent parfois après l'entrée du morceau.
        for i in 0..self.set.tracks.len() {
            let entry = &self.set.tracks[i];
            let Some(deck) = decks.iter().find(|d| d.track_id == entry.track_id) else { continue };
            if entry.title.is_empty()
                && let Some(info) = &deck.info
            {
                (self.set.tracks[i].title, self.set.tracks[i].artist) = (info.title.clone(), info.artist.clone());
                changed = true;
            }
            if self.set.tracks[i].artwork.is_none()
                && let Some(jpeg) = &deck.artwork
            {
                self.set.tracks[i].artwork = self.save_artwork(i, jpeg);
                changed |= self.set.tracks[i].artwork.is_some();
            }
        }
        if let Some(origin) = self.origin {
            let duration = self.last_heard - origin;
            self.dirty |= duration != self.set.duration;
            self.set.duration = duration;
        }
        self.dirty |= changed;
        if self.dirty && (changed || now - self.saved_at >= SAVE_EVERY) {
            self.save(now);
        }
        changed
    }

    fn enter(&mut self, deck: &LoadedDeck, since: f64, wall: SystemTime) {
        let origin = *self.origin.get_or_insert_with(|| {
            let unix = wall.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            self.set.started = unix;
            self.set.utc_offset = UtcOffset::current_local_offset().map_or(0, UtcOffset::whole_seconds);
            since
        });
        let info = deck.info.clone().unwrap_or_default();
        let index = self.set.tracks.len();
        self.set.tracks.push(Entry {
            at: since - origin,
            title: info.title,
            artist: info.artist,
            deck: deck.deck,
            bpm: deck.bpm,
            track_id: deck.track_id,
            artwork: None,
        });
        if let Some(jpeg) = &deck.artwork {
            self.set.tracks[index].artwork = self.save_artwork(index, jpeg);
        }
    }

    /// Dossier du set en cours, créé au premier morceau : sa date et son heure.
    fn folder(&mut self) -> Option<PathBuf> {
        if self.folder.is_none() {
            let root = self.root.as_ref()?;
            let stem = self.set.file_stem().trim_start_matches("Set ").to_owned();
            let mut folder = root.join(&stem);
            let mut n = 2;
            while folder.exists() {
                folder = root.join(format!("{stem} ({n})"));
                n += 1;
            }
            fs::create_dir_all(&folder).ok()?;
            self.folder = Some(folder);
        }
        self.folder.clone()
    }

    fn save_artwork(&mut self, index: usize, jpeg: &[u8]) -> Option<String> {
        let name = format!("{:02}.jpg", index + 1);
        match self.folder() {
            Some(folder) => fs::write(folder.join(&name), jpeg).ok().map(|()| name),
            // Sans dossier (essais), la pochette est seulement notée.
            None => self.root.is_none().then_some(name),
        }
    }

    fn save(&mut self, now: f64) {
        self.saved_at = now;
        let Some(folder) = self.folder() else { return };
        let Ok(text) = serde_json::to_string_pretty(&self.set) else { return };
        // Écrit à côté puis renommé : un arrêt brutal ne laisse jamais un fichier à moitié écrit.
        let temporary = folder.join("set.json.tmp");
        if fs::write(&temporary, text).is_ok() && fs::rename(&temporary, folder.join(SET_FILE)).is_ok() {
            self.dirty = false;
        }
    }

    /// Fin du live : la durée finale est enregistrée.
    pub fn finish(&mut self) {
        if self.dirty {
            self.save(self.saved_at);
        }
    }

    #[cfg(test)]
    fn set(&self) -> &Set {
        &self.set
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::prolink::TrackInfo;

    fn deck(deck: u8, track_id: u32, audible: bool, title: Option<&str>) -> LoadedDeck {
        LoadedDeck {
            deck,
            track_id,
            audible,
            bpm: Some(160.0),
            info: title.map(|t| TrackInfo { title: t.to_owned(), artist: "Artiste".to_owned() }),
            artwork: None,
        }
    }

    /// Joue `decks` de `from` à `to` (s), un état toutes les 160 ms comme l'OPUS.
    fn play(recorder: &mut Recorder, from: f64, to: f64, decks: &[LoadedDeck]) {
        let mut t = from;
        while t < to {
            recorder.update(t, UNIX_EPOCH + Duration::from_secs(1_800_000_000) + Duration::from_secs_f64(t), decks);
            t += 0.16;
        }
    }

    #[test]
    fn tracks_enter_in_order_from_zero_when_heard() {
        let mut recorder = Recorder::new(None);
        // Morceau chargé sur la 1, fader fermé : rien.
        play(&mut recorder, 0.0, 10.0, &[deck(1, 11, false, Some("A"))]);
        assert!(recorder.set().tracks.is_empty());
        // Il s'entend à 10 s : il entre à 0:00 après 4 s.
        play(&mut recorder, 10.0, 60.0, &[deck(1, 11, true, Some("A"))]);
        // Transition : la 2 s'entend à 60 s, la 1 se coupe à 75 s, puis la 1 est rouverte un
        // instant à 100 s (déjà jouée : elle ne rentre pas).
        play(&mut recorder, 60.0, 75.0, &[deck(1, 11, true, Some("A")), deck(2, 22, true, None)]);
        play(&mut recorder, 75.0, 100.0, &[deck(1, 11, false, Some("A")), deck(2, 22, true, Some("B"))]);
        play(&mut recorder, 100.0, 110.0, &[deck(1, 11, true, Some("A")), deck(2, 22, true, Some("B"))]);
        let set = recorder.set();
        let entries: Vec<(String, f64)> = set.tracks.iter().map(|e| (e.title.clone(), e.at)).collect();
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!((entries[0].0.as_str(), entries[0].1), ("A", 0.0));
        // Le titre de B est arrivé après son entrée ; elle est entrée à 60 s, soit 50 s du set.
        assert_eq!(entries[1].0, "B");
        assert!((entries[1].1 - 50.0).abs() < 0.2, "{entries:?}");
        assert!((set.duration - 100.0).abs() < 0.2, "{}", set.duration);
        // Sans dossier, rien n'est écrit, mais la durée reste à enregistrer jusqu'à l'arrêt.
        assert!(recorder.dirty);
    }

    #[test]
    fn a_short_listen_does_not_enter() {
        let mut recorder = Recorder::new(None);
        play(&mut recorder, 0.0, 3.0, &[deck(1, 11, true, Some("A"))]);
        play(&mut recorder, 3.0, 10.0, &[deck(1, 11, false, Some("A"))]);
        assert!(recorder.set().tracks.is_empty());
    }

    #[test]
    fn artwork_and_text_export() {
        let mut recorder = Recorder::new(None);
        let mut with_art = deck(1, 11, true, Some("On My Mind"));
        with_art.artwork = Some(Arc::from(&[0xff, 0xd8, 0xff][..]));
        play(&mut recorder, 0.0, 5.0, &[with_art]);
        play(&mut recorder, 5.0, 300.0, &[deck(2, 22, true, None)]);
        let set = recorder.set();
        assert_eq!(set.tracks[0].artwork.as_deref(), Some("01.jpg"));
        let text = set.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[3], "0:00 Artiste - On My Mind");
        assert!(lines[4].starts_with("0:05 Morceau n° 22") || lines[4].starts_with("0:04 Morceau n° 22"), "{text}");
        assert!(lines[1].ends_with("2 morceaux"), "{text}");
    }

    /// Rejoue un journal de la sonde (`DJB_OPUS_JOURNAL`) dans la liaison et l'enregistreur, comme
    /// pendant un live, puis écrit la setlist, ses pochettes, son texte et son image dans
    /// `DJB_SET_OUT`.
    #[test]
    #[ignore]
    fn replay_setlist() {
        use std::net::Ipv4Addr;

        use crate::opus::Client;

        let path = std::env::var("DJB_OPUS_JOURNAL").expect("DJB_OPUS_JOURNAL");
        let out = PathBuf::from(std::env::var("DJB_SET_OUT").expect("DJB_SET_OUT"));
        let text = fs::read_to_string(path).unwrap();
        let mut client = Client::new(Vec::new());
        let mut recorder = Recorder::new(Some(out.clone()));
        let mut sent = Vec::new();
        let mut next = 0.0;
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() < 5 || f[1] != "<" {
                continue;
            }
            let t: f64 = f[0].parse().unwrap();
            let from: Ipv4Addr = f[3].split(':').next().unwrap().parse().unwrap();
            client.on_packet(t, f[2].parse().unwrap(), from, &hex::decode(f[4]).unwrap(), &mut sent);
            if t >= next {
                next = t + 0.02;
                recorder.update(t, SystemTime::now(), &client.loaded(t));
            }
        }
        recorder.finish();
        let folder = recorder.folder.clone().expect("aucun morceau entendu");
        let set = Set::load(&folder).unwrap();
        print!("{}", set.text());
        fs::write(folder.join("setlist.txt"), set.text()).unwrap();
        poster::export(&set, &folder, &folder.join("setlist.png")).unwrap();
        println!("→ {}", folder.display());
    }

    #[test]
    fn timecodes_read_like_a_tracklist() {
        assert_eq!(timecode(0.0), "0:00");
        assert_eq!(timecode(272.9), "4:32");
        assert_eq!(timecode(3723.0), "1:02:03");
    }
}

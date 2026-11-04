//! Pro DJ Link, côté OPUS-QUAD. L'OPUS ne parle pas vraiment Pro DJ Link : il ne dialogue
//! qu'avec rekordbox, pour le mode Lighting. On se fait donc passer pour rekordbox, comme
//! Beat Link (Deep Symmetry) et l'analyse de Kyle Awayan (opus-quad-pro-dj-link-analysis) :
//! annonce de présence sur le port 50000, demande d'état « lighting » sur le port 50002, puis
//! l'OPUS envoie l'état de ses quatre platines (tempo, morceau, temps dans la mesure) et la
//! structure des morceaux chargés (analyse de phrases de rekordbox, bloc PSSI).
//!
//! Il n'envoie ni paquets de temps ni position précise : la phase reste l'affaire du son.

use std::collections::HashMap;
use std::net::Ipv4Addr;

pub const ANNOUNCE_PORT: u16 = 50000;
pub const BEAT_PORT: u16 = 50001;
pub const STATUS_PORT: u16 = 50002;

/// En-tête de tous les paquets Pro DJ Link.
const MAGIC: [u8; 10] = *b"Qspt1WmJOL";
const NAME_LEN: usize = 20;
/// Nom sous lequel on se présente : l'OPUS ne répond qu'à rekordbox.
const REKORDBOX: &str = "rekordbox";
/// Numéro de rekordbox sur le réseau (celui de Beat Link, dans la plage 0x13–0x27).
pub const DEVICE_NUMBER: u8 = 0x17;
pub const OPUS_NAME: &str = "OPUS-QUAD";
/// Nom de l'ordinateur dans la demande d'état.
const HOST_NAME: &str = "DJ-Bridge";
/// L'OPUS numérote ses platines 9 à 12 dans ses paquets.
pub const OPUS_FIRST_DEVICE: u8 = 9;

/// Types des blocs de métadonnées que l'OPUS envoie d'un morceau quand il est chargé.
pub mod block {
    /// Pochette, en JPEG.
    pub const ARTWORK: u8 = 0x02;
    /// Aperçu de la waveform en couleur (1200 points).
    pub const WAVEFORM_PREVIEW: u8 = 0x03;
    /// Waveform détaillée en couleur (150 points par seconde).
    pub const WAVEFORM_DETAIL: u8 = 0x04;
    /// Grille des temps.
    pub const BEAT_GRID: u8 = 0x06;
    /// Structure du morceau (bloc PSSI de l'analyse de phrases).
    pub const SONG_STRUCTURE: u8 = 0x0a;
}

/// Annonce de présence (port 50000, 54 octets), répétée toutes les 1,5 s.
pub fn keep_alive(mac: [u8; 6], ip: Ipv4Addr) -> Vec<u8> {
    let mut packet = header(0x06);
    packet.push(0x00);
    packet.extend(name(REKORDBOX));
    packet.extend([0x01, 0x03, 0x00, 0x36, DEVICE_NUMBER, 0x01]);
    packet.extend(mac);
    packet.extend(ip.octets());
    packet.extend([0x04, 0x01, 0x00, 0x00, 0x04, 0x08]);
    packet
}

/// Demande d'état du mode Lighting (port 50002) : en réponse, l'OPUS envoie l'état de ses platines.
pub fn lighting_request() -> Vec<u8> {
    let mut packet = header(0x11);
    packet.extend(name(REKORDBOX));
    packet.extend([0x01, 0x01, DEVICE_NUMBER, 0x01, 0x04, DEVICE_NUMBER, 0x01, 0x00, 0x00, 0x00]);
    // Nom de l'ordinateur, un octet sur deux, sur 255 octets.
    let mut host = [0u8; 255];
    for (i, byte) in HOST_NAME.bytes().enumerate() {
        host[2 * i] = byte;
    }
    packet.extend(host);
    packet
}

/// Demande le bloc de type `kind` (voir `block`) du morceau `track_id` chargé sur la platine
/// désignée par `deck_byte` (9 à 12). Vérifié pour la structure ; l'OPUS envoie de lui-même les
/// autres blocs au chargement.
pub fn metadata_request(track_id: u32, kind: u8, deck_byte: u8) -> Vec<u8> {
    let mut packet = header(0x55);
    packet.extend(name(REKORDBOX));
    packet.extend([0x01, 0x00, DEVICE_NUMBER, 0x00, 0x08]);
    packet.extend(track_id.to_le_bytes());
    packet.extend([kind, deck_byte, 0x03, 0x01]);
    packet
}

fn header(kind: u8) -> Vec<u8> {
    let mut packet = MAGIC.to_vec();
    packet.push(kind);
    packet
}

fn name(text: &str) -> [u8; NAME_LEN] {
    let mut out = [0u8; NAME_LEN];
    let len = text.len().min(NAME_LEN);
    out[..len].copy_from_slice(&text.as_bytes()[..len]);
    out
}

/// Paquet reçu, décodé selon son port et son type.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet<'a> {
    /// Annonce de présence d'un appareil (port 50000).
    KeepAlive(KeepAlive),
    /// Premier paquet de l'OPUS sur le port 50002 : il nous a vus.
    Hello { name: String },
    /// État d'une platine (port 50002).
    Status(Status),
    /// Bloc de métadonnées d'un morceau, en une ou plusieurs parties (port 50002).
    Metadata(Metadata<'a>),
    /// Temps joué par une platine (port 50001) ; l'OPUS n'en enverrait pas.
    Beat { device: u8, bpm: Option<f64>, beat_in_bar: u8 },
    /// Paquet Pro DJ Link d'un autre type.
    Other { kind: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAlive {
    pub name: String,
    pub device: u8,
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
}

impl KeepAlive {
    pub fn is_opus(&self) -> bool {
        self.name == OPUS_NAME
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    /// Numéro de la platine tel qu'envoyé (9 à 12 sur l'OPUS).
    pub device: u8,
    /// Identifiant rekordbox du morceau chargé, 0 si aucun.
    pub track_id: u32,
    pub source_player: u8,
    pub source_slot: u8,
    pub play_state: u8,
    pub flags: u8,
    /// Tempo du morceau, sans la variation de pitch.
    pub bpm: Option<f64>,
    /// Variation de pitch du fader (%).
    pub pitch: f64,
    /// Vitesse réellement jouée (%), coups de jog compris ; -100 à l'arrêt.
    pub speed: f64,
    /// Numéro du temps dans le morceau, à partir de 1 : il désigne un temps de la grille.
    pub beat: Option<u32>,
    /// Temps dans la mesure (1 à 4) ; toujours 0 sur l'OPUS, la grille le donne.
    pub beat_in_bar: u8,
}

impl Status {
    const PLAY_STATE_LOOPING: u8 = 0x04;
    const FLAG_ON_AIR: u8 = 0x08;
    const FLAG_SYNC: u8 = 0x10;
    const FLAG_MASTER: u8 = 0x20;
    const FLAG_PLAYING: u8 = 0x40;

    /// Platine de 1 à 4.
    pub fn deck(&self) -> u8 {
        if (OPUS_FIRST_DEVICE..OPUS_FIRST_DEVICE + 4).contains(&self.device) {
            self.device - OPUS_FIRST_DEVICE + 1
        } else {
            self.device
        }
    }

    pub fn playing(&self) -> bool {
        self.flags & Self::FLAG_PLAYING != 0
    }

    pub fn master(&self) -> bool {
        self.flags & Self::FLAG_MASTER != 0
    }

    pub fn on_air(&self) -> bool {
        self.flags & Self::FLAG_ON_AIR != 0
    }

    pub fn synced(&self) -> bool {
        self.flags & Self::FLAG_SYNC != 0
    }

    pub fn looping(&self) -> bool {
        self.play_state == Self::PLAY_STATE_LOOPING
    }

    /// Tempo réglé, pitch du fader compris.
    pub fn effective_bpm(&self) -> Option<f64> {
        self.bpm.map(|bpm| bpm * (1.0 + self.pitch / 100.0))
    }

    /// Tempo réellement joué à l'instant du paquet, coups de jog compris.
    pub fn played_bpm(&self) -> Option<f64> {
        self.bpm.map(|bpm| bpm * (1.0 + self.speed / 100.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata<'a> {
    pub device: u8,
    pub kind: u8,
    pub track_id: u32,
    /// Numéro de la partie, à partir de 0.
    pub index: u8,
    pub count: u8,
    pub payload: &'a [u8],
}

impl Metadata<'_> {
    pub fn deck(&self) -> u8 {
        self.device & 0x07
    }
}

/// Décode un paquet reçu sur `port` ; `None` si ce n'est pas du Pro DJ Link.
pub fn parse(port: u16, data: &[u8]) -> Option<Packet<'_>> {
    if data.len() < MAGIC.len() + 1 || data[..MAGIC.len()] != MAGIC {
        return None;
    }
    let kind = data[MAGIC.len()];
    let packet = match (port, kind) {
        (ANNOUNCE_PORT, 0x06) if data.len() >= 0x30 => Packet::KeepAlive(KeepAlive {
            name: text(&data[0x0c..0x0c + NAME_LEN]),
            device: data[0x24],
            mac: data[0x26..0x2c].try_into().ok()?,
            ip: Ipv4Addr::new(data[0x2c], data[0x2d], data[0x2e], data[0x2f]),
        }),
        (STATUS_PORT, 0x10) if data.len() >= 0x0b + NAME_LEN => {
            Packet::Hello { name: text(&data[0x0b..0x0b + NAME_LEN]) }
        }
        (STATUS_PORT, 0x0a) if data.len() >= 0xa7 => {
            let bpm = u16::from_be_bytes([data[0x92], data[0x93]]);
            let pitch = u32::from_be_bytes(data[0x8c..0x90].try_into().ok()?);
            let speed = u32::from_be_bytes(data[0x98..0x9c].try_into().ok()?);
            let beat = u32::from_be_bytes(data[0xa0..0xa4].try_into().ok()?);
            Packet::Status(Status {
                device: data[0x21],
                track_id: u32::from_be_bytes(data[0x2c..0x30].try_into().ok()?),
                source_player: data[0x28],
                source_slot: data[0x29],
                play_state: data[0x7b],
                flags: data[0x89],
                bpm: (bpm != 0xffff).then(|| f64::from(bpm) / 100.0),
                pitch: pitch_percent(pitch),
                speed: pitch_percent(speed),
                beat: (beat != u32::MAX).then_some(beat),
                beat_in_bar: data[0xa6],
            })
        }
        (STATUS_PORT, 0x56) if data.len() >= 0x34 => Packet::Metadata(Metadata {
            device: data[0x21],
            kind: data[0x25],
            track_id: u32::from_be_bytes(data[0x28..0x2c].try_into().ok()?),
            index: data[0x31],
            count: data[0x33],
            payload: &data[0x34..],
        }),
        (BEAT_PORT, 0x28) if data.len() >= 0x5d => {
            let bpm = u16::from_be_bytes([data[0x5a], data[0x5b]]);
            Packet::Beat {
                device: data[0x21],
                bpm: (bpm != 0xffff).then(|| f64::from(bpm) / 100.0),
                beat_in_bar: data[0x5c],
            }
        }
        _ => Packet::Other { kind },
    };
    Some(packet)
}

/// Pitch Pro DJ Link (0x100000 = vitesse normale) en pourcentage.
fn pitch_percent(raw: u32) -> f64 {
    const NORMAL: f64 = 0x10_0000 as f64;
    (f64::from(raw) - NORMAL) / NORMAL * 100.0
}

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

/// Platine, type de bloc et morceau.
type BlockKey = (u8, u8, u32);

/// Recolle les blocs de métadonnées envoyés en plusieurs parties.
#[derive(Default)]
pub struct Reassembler {
    parts: HashMap<BlockKey, Vec<Option<Vec<u8>>>>,
}

impl Reassembler {
    /// Ajoute une partie ; renvoie le bloc entier quand la dernière partie manquante arrive.
    pub fn push(&mut self, part: &Metadata) -> Option<Vec<u8>> {
        let count = usize::from(part.count.max(1));
        let index = usize::from(part.index);
        if index >= count {
            return None;
        }
        let key = (part.deck(), part.kind, part.track_id);
        let parts = self.parts.entry(key).or_default();
        if parts.len() != count {
            *parts = vec![None; count];
        }
        parts[index] = Some(part.payload.to_vec());
        if parts.iter().any(Option::is_none) {
            return None;
        }
        let parts = self.parts.remove(&key)?;
        Some(parts.into_iter().flatten().flatten().collect())
    }
}

/// Un temps de la grille rekordbox.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridBeat {
    /// Place dans la mesure, 1 à 4.
    pub bar_beat: u8,
    pub bpm: f64,
    /// Instant du temps dans le morceau, à vitesse normale (ms).
    pub time_ms: u32,
}

/// Grille des temps d'un morceau ; le numéro de temps des états y renvoie (le temps 1 est le premier).
#[derive(Debug, Clone, PartialEq)]
pub struct BeatGrid {
    pub beats: Vec<GridBeat>,
}

const GRID_HEADER: usize = 8;
const GRID_ENTRY_LEN: usize = 8;

impl BeatGrid {
    /// Décode un bloc de grille complet : 4 octets inconnus, le nombre de temps, puis 8 octets
    /// par temps (place dans la mesure, tempo × 100, instant en ms).
    pub fn parse(block: &[u8]) -> Option<Self> {
        let count = u32::from_be_bytes(block.get(4..GRID_HEADER)?.try_into().ok()?) as usize;
        let entries = block.get(GRID_HEADER..GRID_HEADER + count * GRID_ENTRY_LEN)?;
        let beats = entries
            .as_chunks::<GRID_ENTRY_LEN>()
            .0
            .iter()
            .map(|e| GridBeat {
                bar_beat: e[1],
                bpm: f64::from(u16::from_be_bytes([e[2], e[3]])) / 100.0,
                time_ms: u32::from_be_bytes([e[4], e[5], e[6], e[7]]),
            })
            .collect();
        Some(Self { beats })
    }
}

/// En-tête d'un bloc de waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waveform {
    pub points: u32,
    pub point_len: u16,
    /// Points par seconde ; 0 pour l'aperçu, qui couvre tout le morceau.
    pub per_second: u16,
}

impl Waveform {
    pub fn parse(block: &[u8]) -> Option<Self> {
        let header = block.get(..12)?;
        Some(Self {
            points: u32::from_be_bytes(header[0..4].try_into().ok()?),
            point_len: u16::from_be_bytes([header[8], header[9]]),
            per_second: u16::from_be_bytes([header[10], header[11]]),
        })
    }
}

/// Partie d'un morceau selon l'analyse de phrases de rekordbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phrase {
    /// Premier temps de la partie, à partir de 1.
    pub beat: u16,
    pub kind: u16,
}

/// Structure d'un morceau (bloc PSSI de l'analyse rekordbox).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongStructure {
    /// Humeur : 1 haute, 2 moyenne, 3 basse ; elle donne le sens des types de partie.
    pub mood: u16,
    pub end_beat: u16,
    pub phrases: Vec<Phrase>,
}

/// Masque appliqué par rekordbox 6 et suivants au bloc PSSI, à partir de l'octet 18.
const PSSI_MASK: [u8; 19] = [
    0xcb, 0xe1, 0xee, 0xfa, 0xe5, 0xee, 0xad, 0xee, 0xe9, 0xd2, 0xe9, 0xeb, 0xe1, 0xe9, 0xf3, 0xe8, 0xe9, 0xf4,
    0xe1,
];
const PSSI_BODY: usize = 18;
const PSSI_ENTRIES: usize = 32;
const PSSI_ENTRY_LEN: usize = 24;

impl SongStructure {
    /// Décode un bloc PSSI complet (en-tête « PSSI » compris), masqué ou non.
    pub fn parse(block: &[u8]) -> Option<Self> {
        if block.len() < PSSI_ENTRIES || &block[..4] != b"PSSI" {
            return None;
        }
        let len_tag = u32::from_be_bytes(block[8..12].try_into().ok()?) as usize;
        let block = &block[..len_tag.min(block.len())];
        let count = usize::from(u16::from_be_bytes([block[16], block[17]]));
        let mut body = block[PSSI_BODY..].to_vec();
        let mood = |body: &[u8]| u16::from_be_bytes([body[0], body[1]]);
        if !(1..=3).contains(&mood(&body)) {
            for (i, byte) in body.iter_mut().enumerate() {
                *byte ^= PSSI_MASK[i % PSSI_MASK.len()].wrapping_add(count as u8);
            }
            if !(1..=3).contains(&mood(&body)) {
                return None;
            }
        }
        let entries = &body[PSSI_ENTRIES - PSSI_BODY..];
        let phrases = entries
            .as_chunks::<PSSI_ENTRY_LEN>()
            .0
            .iter()
            .take(count)
            .map(|e| Phrase { beat: u16::from_be_bytes([e[2], e[3]]), kind: u16::from_be_bytes([e[4], e[5]]) })
            .collect();
        Some(Self { mood: mood(&body), end_beat: u16::from_be_bytes([body[8], body[9]]), phrases })
    }

    /// Nom rekordbox d'un type de partie.
    pub fn label(&self, kind: u16) -> String {
        let name = match (self.mood, kind) {
            (_, 1) => "Intro",
            (1, 2) => "Up",
            (1, 3) => "Down",
            (1, 5) => "Chorus",
            (1, 6) => "Outro",
            (2, 2..=7) => return format!("Verse {}", kind - 1),
            (3, 2..=4) => "Verse 1",
            (3, 5..=7) => "Verse 2",
            (2 | 3, 8) => "Bridge",
            (2 | 3, 9) => "Chorus",
            (2 | 3, 10) => "Outro",
            _ => return format!("type {kind}"),
        };
        name.to_owned()
    }

    pub fn mood_label(&self) -> &'static str {
        match self.mood {
            1 => "haute",
            2 => "moyenne",
            _ => "basse",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const IP: Ipv4Addr = Ipv4Addr::new(169, 254, 10, 20);

    #[test]
    fn keep_alive_has_beat_link_layout() {
        let packet = keep_alive(MAC, IP);
        assert_eq!(packet.len(), 0x36);
        assert_eq!(&packet[..10], b"Qspt1WmJOL");
        assert_eq!(packet[0x0a], 0x06);
        assert_eq!(&packet[0x0c..0x15], b"rekordbox");
        assert_eq!(packet[0x24], DEVICE_NUMBER);
        assert_eq!(packet[0x26..0x2c], MAC);
        assert_eq!(packet[0x2c..0x30], IP.octets());
        let Some(Packet::KeepAlive(parsed)) = parse(ANNOUNCE_PORT, &packet) else { panic!("annonce illisible") };
        assert_eq!(parsed, KeepAlive { name: "rekordbox".into(), device: DEVICE_NUMBER, mac: MAC, ip: IP });
        assert!(!parsed.is_opus());
    }

    #[test]
    fn requests_have_expected_lengths() {
        let lighting = lighting_request();
        assert_eq!(lighting.len(), 10 + 1 + 20 + 10 + 255);
        assert_eq!(lighting[0x0a], 0x11);
        assert_eq!(&lighting[0x29..0x2e], b"D\0J\0-");

        let pssi = metadata_request(0x0102_0304, block::SONG_STRUCTURE, 10);
        assert_eq!(pssi.len(), 44);
        assert_eq!(pssi[0x0a], 0x55);
        assert_eq!(pssi[0x24..0x28], [0x04, 0x03, 0x02, 0x01]);
        assert_eq!((pssi[0x28], pssi[0x29]), (0x0a, 10));
    }

    #[test]
    fn reads_opus_hello() {
        let hello = [
            0x51, 0x73, 0x70, 0x74, 0x31, 0x57, 0x6d, 0x4a, 0x4f, 0x4c, 0x10, 0x4f, 0x50, 0x55, 0x53, 0x2d, 0x51,
            0x55, 0x41, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x09,
            0x00, 0x00,
        ];
        assert_eq!(parse(STATUS_PORT, &hello), Some(Packet::Hello { name: OPUS_NAME.into() }));
        assert_eq!(parse(STATUS_PORT, b"pas du pro dj link"), None);
    }

    #[test]
    fn reads_status_fields() {
        let mut data = header(0x0a);
        data.resize(0xd4, 0);
        data[0x21] = 10;
        data[0x2c..0x30].copy_from_slice(&829u32.to_be_bytes());
        data[0x89] = 0x40 | 0x20 | 0x08;
        // +5 % de pitch.
        data[0x8c..0x90].copy_from_slice(&(0x10_0000u32 + 0x0_cccc).to_be_bytes());
        data[0x92..0x94].copy_from_slice(&16_000u16.to_be_bytes());
        data[0xa0..0xa4].copy_from_slice(&123u32.to_be_bytes());
        data[0xa6] = 3;
        let Some(Packet::Status(status)) = parse(STATUS_PORT, &data) else { panic!("état illisible") };
        assert_eq!(status.deck(), 2);
        assert_eq!(status.track_id, 829);
        assert!(status.playing() && status.master() && status.on_air() && !status.synced());
        assert_eq!(status.bpm, Some(160.0));
        assert!((status.pitch - 5.0).abs() < 0.01);
        assert!((status.effective_bpm().unwrap() - 168.0).abs() < 0.02);
        assert_eq!((status.beat, status.beat_in_bar), (Some(123), 3));
    }

    /// Bloc PSSI d'humeur haute : Intro, Up, Down, Chorus, Outro.
    fn pssi_block() -> Vec<u8> {
        let phrases = [(1u16, 1u16), (33, 2), (97, 3), (129, 5), (257, 6)];
        let mut block = b"PSSI".to_vec();
        let len = PSSI_ENTRIES + phrases.len() * PSSI_ENTRY_LEN;
        block.extend(32u32.to_be_bytes());
        block.extend((len as u32).to_be_bytes());
        block.extend((PSSI_ENTRY_LEN as u32).to_be_bytes());
        block.extend((phrases.len() as u16).to_be_bytes());
        block.extend(1u16.to_be_bytes());
        block.extend([0; 6]);
        block.extend(289u16.to_be_bytes());
        block.extend([0; 4]);
        for (index, (beat, kind)) in phrases.into_iter().enumerate() {
            let mut entry = [0u8; PSSI_ENTRY_LEN];
            entry[..2].copy_from_slice(&(index as u16 + 1).to_be_bytes());
            entry[2..4].copy_from_slice(&beat.to_be_bytes());
            entry[4..6].copy_from_slice(&kind.to_be_bytes());
            block.extend(entry);
        }
        block
    }

    #[test]
    fn decodes_song_structure_masked_or_not() {
        let plain = pssi_block();
        let mut masked = plain.clone();
        for (i, byte) in masked[PSSI_BODY..].iter_mut().enumerate() {
            *byte ^= PSSI_MASK[i % PSSI_MASK.len()].wrapping_add(5);
        }
        for block in [plain, masked] {
            let song = SongStructure::parse(&block).expect("PSSI illisible");
            assert_eq!((song.mood, song.end_beat), (1, 289));
            let labels: Vec<String> = song.phrases.iter().map(|p| song.label(p.kind)).collect();
            assert_eq!(labels, ["Intro", "Up", "Down", "Chorus", "Outro"]);
            assert_eq!(song.phrases[2].beat, 97);
        }
    }

    /// État de la platine 1 de l'OPUS (firmware 1.32) le 4 novembre : morceau 1297 à 147 BPM, pitch
    /// +0,7 %, en lecture, maître et à l'antenne, au temps 87.
    const REAL_STATUS: &str = "5173707431576d4a4f4c0a4f5055532d515541440000000000000000000000010509010109000100090001000000051100000001000000050000402500000000000000000000001700000000000000000000000000000000000000000000000000000000000000000100040400000004000000040000000000000003312e3332000000000000000500ecfffa00101cac8000396c7fffffff00101cac000001ff0000005701ff0000000000000000000000000000000001040000000000000000000000000000000000000000001ff30000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000";

    /// Structure masquée du morceau 1298, envoyée d'un bloc par l'OPUS (platine 2).
    const REAL_PSSI: &str = "5173707431576d4a4f4c564f5055532d51554144000000000000000000000001000a01c8000a00000000051209000100000000015053534900000020000001b8000000180011dcf3ff0bf6ffbefff8e6fafdf2fa04f8fa04f2ddf2fe0bf6ffbefffae3fafcf2fa04f9fa05f2dcf0ff2af6fabefefae3fafcf2fa04f9fa05f2dcf2fe0bcbffbdffbbe3fffcf3fa04f9fa05f2dcf2ff0bf6ffbefefabefaf8f29b04fbfa05f2dcf2ff0bf6ffbefffae3fbfcf3fa81f9ff057bdcf7ff0af6ffbefffae3fafcf2fa04f9fa04f27bf2f90b5fffbbfffbe3fafcf2fa04f9fa05f2dcf2ff0af63abef8fa2afaf9f2fb04f9fa05f2dcf2ff0bf6ffbefffbe31ffcfafaedf9ff05f3dcf2ff0bf6ffbefffae3fafcf2fa04f9fa0cf3d5f2fc0bf6ffbefffae3fafcf2fa04f9fa05f2dcf2ff01f7d6befdfae3fafcf2fa04f9fa05f2dcf2fe0bf7fef9fff1e2b3fcf0fa04f9fb05f3ddbbfe42f7aebefffae2fba9f2f605a0fa00f2ddf2ff0bf6ffbefffae3fafcf2fa05f88d05ffdd8bff0ef6febefffae3fafcf2fa04f9fa05f2ddf36a0bf8fe27ffffe3fbfcf2fa04f9fa05f2dcf2ff0bf6febf4afaecfb45f2ff04f8fa05f2dcf2ff0bf6ffbefffae3fbfd27fa14f82305f7dcf3ff0bf6ffbefffae3fafcf2fa04f8fbf2f2cdf3060bf0ffbffffae3fafcf2fa04f9fa05f2dcf2ff0b";

    /// Début de la première partie de la grille du morceau 1297 (784 temps à 147 BPM).
    const REAL_GRID_START: &str = "5173707431576d4a4f4c564f5055532d51554144000000000000000000000001000905680006000100000511090001000000000500080000000003100001396c000000300002396c000001c80003396c000003600004396c000004f90001396c00000691";

    #[test]
    fn reads_real_opus_status() {
        let data = hex::decode(REAL_STATUS).unwrap();
        let Some(Packet::Status(status)) = parse(STATUS_PORT, &data) else { panic!("état illisible") };
        assert_eq!((status.deck(), status.track_id, status.beat), (1, 1297, Some(87)));
        assert!(status.playing() && status.master() && status.on_air() && !status.looping());
        assert_eq!(status.bpm, Some(147.0));
        assert!((status.effective_bpm().unwrap() - 148.03).abs() < 0.01);
        assert!((status.played_bpm().unwrap() - 148.03).abs() < 0.01);
    }

    #[test]
    fn decodes_real_masked_song_structure() {
        let data = hex::decode(REAL_PSSI).unwrap();
        let Some(Packet::Metadata(part)) = parse(STATUS_PORT, &data) else { panic!("bloc illisible") };
        assert_eq!((part.deck(), part.kind, part.track_id, part.count), (2, block::SONG_STRUCTURE, 1298, 1));
        let song = SongStructure::parse(part.payload).expect("PSSI illisible");
        assert_eq!((song.mood, song.end_beat, song.phrases.len()), (1, 517, 17));
        let first: Vec<(String, u16)> = song.phrases[..4].iter().map(|p| (song.label(p.kind), p.beat)).collect();
        assert_eq!(first, [("Intro".into(), 1), ("Chorus".into(), 33), ("Chorus".into(), 65), ("Up".into(), 97)]);
        assert_eq!(song.phrases.last().map(|p| (song.label(p.kind), p.beat)), Some(("Outro".into(), 505)));
    }

    #[test]
    fn decodes_real_beat_grid() {
        let data = hex::decode(REAL_GRID_START).unwrap();
        let Some(Packet::Metadata(part)) = parse(STATUS_PORT, &data) else { panic!("bloc illisible") };
        assert_eq!((part.kind, part.count), (block::BEAT_GRID, 5));
        // Le début ne contient que 5 temps sur 784 : on ramène le nombre annoncé à 5.
        let mut start = part.payload.to_vec();
        start[4..8].copy_from_slice(&5u32.to_be_bytes());
        let grid = BeatGrid::parse(&start).expect("grille illisible");
        let first: Vec<(u8, u32)> = grid.beats.iter().map(|b| (b.bar_beat, b.time_ms)).collect();
        assert_eq!(first, [(1, 48), (2, 456), (3, 864), (4, 1273), (1, 1681)]);
        assert!(grid.beats.iter().all(|b| b.bpm == 147.0));
        assert_eq!(BeatGrid::parse(part.payload), None, "grille tronquée refusée");
    }

    #[test]
    fn reads_waveform_header() {
        let header = hex::decode("0000bb870001770e00020096").unwrap();
        assert_eq!(Waveform::parse(&header), Some(Waveform { points: 48007, point_len: 2, per_second: 150 }));
    }

    #[test]
    fn reassembles_parts_in_any_order() {
        let whole = pssi_block();
        let (first, second) = whole.split_at(50);
        let part =
            |index, payload| Metadata { device: 9, kind: block::SONG_STRUCTURE, track_id: 7, index, count: 2, payload };
        let mut parts = Reassembler::default();
        assert_eq!(parts.push(&part(1, second)), None);
        assert_eq!(parts.push(&part(0, first)), Some(whole));
    }
}

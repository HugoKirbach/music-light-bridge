//! Sonde OPUS-QUAD : écoute ce que l'OPUS envoie au PC (par sa carte réseau USB intégrée ou par le
//! Wi-Fi), se présente comme rekordbox pour qu'il envoie l'état de ses platines et les données des
//! morceaux chargés, et rend compte de ce qui arrive, par quel chemin, à quelle cadence, et des
//! coupures. Chaque paquet est aussi écrit dans un journal, dans le dossier des captures, pour être
//! relu et servir de cas de test.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufWriter, Stdout, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use crossterm::terminal::{self, ClearType};
use netdev::ipnet::Ipv4Net;

use super::tui::{self, RawMode};
use crate::config::Config;
use crate::prolink::{self, BeatGrid, Packet, Reassembler, SongStructure, Status, Waveform, block};

/// Cadence de l'annonce de présence et de la demande d'état (celle de Beat Link).
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);
/// Au-delà, un silence de l'OPUS compte comme une coupure de la liaison.
const SILENCE: Duration = Duration::from_secs(1);
/// Sans nouvelles de l'OPUS passé ce délai, on rappelle quoi vérifier.
const HINT_AFTER: Duration = Duration::from_secs(10);
/// Cadence à laquelle on regarde si une carte réseau a reçu (ou perdu) une adresse.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);
const PSSI_RETRY: Duration = Duration::from_secs(3);
const PSSI_TRIES: u32 = 4;
/// Écart de tempo et délai minimal entre deux lignes de tempo d'une platine.
const TEMPO_STEP: f64 = 0.5;
const TEMPO_EVERY: Duration = Duration::from_secs(1);
/// Durée de lecture minimale pour comparer le compteur de temps au tempo annoncé (s).
const MIN_TEMPO_CHECK: f64 = 8.0;
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const LOOP_PERIOD: Duration = Duration::from_millis(50);

pub struct Options {
    pub seconds: u64,
    /// Morceau du nom de la carte réseau à utiliser ; sinon celle où l'OPUS s'annonce.
    pub interface: Option<String>,
    /// Écoute seulement, sans se présenter comme rekordbox.
    pub passive: bool,
}

pub fn run(options: Options) -> Result<()> {
    println!("Sonde OPUS-QUAD : écoute de ce que l'OPUS envoie au PC.");
    println!();
    println!("Cartes réseau :");
    for line in adapters() {
        println!("  {line}");
    }
    let interfaces = interfaces();
    let forced = match &options.interface {
        Some(part) => {
            let needle = part.to_lowercase();
            let Some(iface) = interfaces.iter().find(|i| i.label.to_lowercase().contains(&needle)) else {
                bail!("aucune carte réseau avec une adresse IPv4 ne contient « {part} »");
            };
            Some(iface.clone())
        }
        None => None,
    };

    let status = bind(prolink::STATUS_PORT)?;
    let announce = bind(prolink::ANNOUNCE_PORT)?;
    let beat = bind(prolink::BEAT_PORT)
        .inspect_err(|e| println!("Port des temps non écouté : {e:#}"))
        .ok();
    let path = journal_path()?;
    let journal = Journal::create(&path, &interfaces)?;

    println!();
    if options.passive {
        println!("Écoute seule : la sonde n'envoie rien, l'OPUS ne parlera sans doute que de sa présence.");
    } else {
        println!("Dès que l'OPUS s'annonce, la sonde se présente comme rekordbox (mode Lighting).");
    }
    println!("Pendant l'écoute : charge un morceau sur une platine, lance-le, bouge le pitch,");
    println!("change de platine maître, puis charge un autre morceau sur une autre platine.");
    println!("Si Windows demande d'autoriser dj-bridge sur le réseau, accepte.");
    println!("Échap arrête avant la fin ({} s).", options.seconds);
    println!();

    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    let mut probe = Probe::new(interfaces, forced, options.passive, &status, journal);
    let deadline = Instant::now() + Duration::from_secs(options.seconds);
    let result = thread::scope(|scope| {
        for socket in [&status, &announce].into_iter().chain(beat.as_ref()) {
            let tx = tx.clone();
            let stop = &stop;
            scope.spawn(move || receive_loop(socket, &tx, stop));
        }
        drop(tx);
        let result = probe.run(&rx, deadline);
        stop.store(true, Ordering::Relaxed);
        result
    });
    probe.close();
    result?;
    probe.summary(&path);
    Ok(())
}

/// Carte réseau du PC, avec une de ses adresses IPv4.
#[derive(Clone, PartialEq)]
struct Interface {
    label: String,
    ip: Ipv4Addr,
    net: Ipv4Net,
    mac: [u8; 6],
}

fn label(i: &netdev::Interface) -> String {
    let name = i.friendly_name.clone().unwrap_or_else(|| i.name.clone());
    match &i.description {
        Some(description) if *description != name => format!("{name} ({description})"),
        _ => name,
    }
}

/// Cartes réseau actives qui ont une adresse IPv4.
fn interfaces() -> Vec<Interface> {
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

/// Toutes les cartes réseau physiques, avec ou sans adresse : la carte USB de l'OPUS peut
/// exister avant d'avoir une adresse.
fn adapters() -> Vec<String> {
    netdev::get_interfaces()
        .iter()
        .filter(|i| !i.is_loopback() && i.mac_addr.is_some_and(|m| m.octets() != [0; 6]))
        .map(|i| {
            let mac = i.mac_addr.map(|m| m.octets()).unwrap_or_default();
            let addresses: Vec<String> = i.ipv4.iter().map(|net| format!("{}/{}", net.addr(), net.prefix_len())).collect();
            let state = match (i.is_up(), addresses.is_empty()) {
                (false, _) => "inactive".to_owned(),
                (true, true) => "active, sans adresse IPv4".to_owned(),
                (true, false) => addresses.join(", "),
            };
            format!("{} : {state} (MAC {})", label(i), mac_text(mac))
        })
        .collect()
}

fn bind(port: u16) -> Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).with_context(|| {
        format!("port UDP {port} déjà pris : rekordbox, Beat Link ou une autre sonde est peut-être ouvert")
    })?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(READ_TIMEOUT))?;
    Ok(socket)
}

struct Received {
    at: Instant,
    port: u16,
    from: SocketAddr,
    data: Vec<u8>,
}

fn receive_loop(socket: &UdpSocket, tx: &mpsc::Sender<Received>, stop: &AtomicBool) {
    let port = socket.local_addr().map_or(0, |a| a.port());
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                let packet = Received { at: Instant::now(), port, from, data: buf[..len].to_vec() };
                if tx.send(packet).is_err() {
                    return;
                }
            }
            // Délai d'attente écoulé, ou écho ICMP d'un envoi précédent (Windows).
            Err(e) if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::ConnectionReset
            ) => {}
            Err(_) => return,
        }
    }
}

fn journal_path() -> Result<PathBuf> {
    let dir = Config::captures_dir()?;
    fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    Ok(dir.join(format!("opus-{stamp}.txt")))
}

/// Journal brut : un paquet par ligne, reçu (<) ou envoyé (>), en hexadécimal.
struct Journal {
    file: BufWriter<File>,
    started: Instant,
}

impl Journal {
    fn create(path: &Path, interfaces: &[Interface]) -> Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        writeln!(file, "# Sonde OPUS-QUAD, DJ-Bridge {}", env!("CARGO_PKG_VERSION"))?;
        for line in adapters() {
            writeln!(file, "# carte {line}")?;
        }
        for iface in interfaces {
            writeln!(file, "# adresse {} {}/{}", iface.label, iface.ip, iface.net.prefix_len())?;
        }
        writeln!(file, "# secondes < port_local source hex | secondes > destination hex | secondes # note")?;
        Ok(Self { file, started: Instant::now() })
    }

    fn seconds(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.started).as_secs_f64()
    }

    fn received(&mut self, at: Instant, port: u16, from: SocketAddr, data: &[u8]) {
        let _ = writeln!(self.file, "{:.3} < {port} {from} {}", self.seconds(at), hex::encode(data));
    }

    fn sent(&mut self, at: Instant, to: SocketAddrV4, data: &[u8]) {
        let _ = writeln!(self.file, "{:.3} > {to} {}", self.seconds(at), hex::encode(data));
    }

    fn note(&mut self, at: Instant, text: &str) {
        let _ = writeln!(self.file, "{:.3} # {text}", self.seconds(at));
    }

    fn flush(&mut self) {
        let _ = self.file.flush();
    }
}

/// Ce qu'on sait d'une platine.
#[derive(Default)]
struct Deck {
    last: Option<Status>,
    /// Dernière description affichée, sans le tempo.
    shown: Option<String>,
    /// Dernier tempo affiché et quand.
    shown_bpm: Option<(Instant, f64)>,
    /// Derniers drapeaux non nuls : l'OPUS envoie parfois des états aux drapeaux à zéro.
    flags: u8,
    count: u32,
    last_at: Option<Instant>,
    gap_sum: f64,
    gap_square_sum: f64,
    gap_max: Duration,
    /// Dernier changement du compteur de temps, pendant une lecture sans saut.
    last_beat: Option<(Instant, u32)>,
    /// Faux après un saut ou une marche arrière : l'intervalle suivant sert seulement à se recaler.
    settled: bool,
    /// Temps avancés, durée et tempo joué × durée, sur les intervalles sans saut ni boucle.
    checked_beats: u32,
    checked_secs: f64,
    checked_bpm_secs: f64,
}

impl Deck {
    fn record_gap(&mut self, at: Instant) {
        if let Some(last) = self.last_at {
            let gap = at.saturating_duration_since(last);
            self.gap_sum += gap.as_secs_f64();
            self.gap_square_sum += gap.as_secs_f64().powi(2);
            self.gap_max = self.gap_max.max(gap);
        }
        self.last_at = Some(at);
        self.count += 1;
    }

    /// Compare l'avance du compteur de temps au tempo joué, en écartant les sauts (recherche,
    /// repère), les boucles et la lecture à l'envers.
    fn check_beats(&mut self, at: Instant, status: &Status, same_track: bool) {
        let (true, Some(beat), Some(bpm)) =
            (status.playing() && !status.looping() && same_track, status.beat, status.played_bpm())
        else {
            self.last_beat = None;
            return;
        };
        match self.last_beat {
            Some((_, last)) if last == beat => return,
            Some((since, last)) => {
                let secs = at.saturating_duration_since(since).as_secs_f64();
                let expected = secs * bpm / 60.0;
                let steady = beat > last && f64::from(beat - last) <= expected * 1.5 + 1.0;
                if steady && self.settled {
                    self.checked_beats += beat - last;
                    self.checked_secs += secs;
                    self.checked_bpm_secs += bpm * secs;
                }
                self.settled = steady;
            }
            None => self.settled = false,
        }
        self.last_beat = Some((at, beat));
    }
}

/// Demande de structure en attente pour une platine.
struct PssiRequest {
    track: u32,
    device: u8,
    tries: u32,
    next: Instant,
}

/// Un bloc reçu pour un morceau : sa description et le nombre de fois qu'il est arrivé.
struct BlockSeen {
    text: String,
    times: u32,
}

/// Ce qu'on a reçu d'une adresse.
#[derive(Default)]
struct Source {
    packets: u32,
    opus: bool,
}

struct Probe<'a> {
    interfaces: Vec<Interface>,
    chosen: Option<Interface>,
    passive: bool,
    socket: &'a UdpSocket,
    journal: Journal,
    out: Stdout,
    started: Instant,
    opus: Option<Ipv4Addr>,
    hello: Option<Instant>,
    next_announce: Instant,
    next_rescan: Instant,
    send_failed: bool,
    hinted: bool,
    last_flush: Instant,
    counts: BTreeMap<(u16, u8), u32>,
    foreign: u32,
    sources: BTreeMap<Ipv4Addr, Source>,
    others: BTreeMap<Ipv4Addr, String>,
    decks: BTreeMap<u8, Deck>,
    zero_flags: u32,
    last_from_opus: Option<Instant>,
    longest_gap: Duration,
    silent_since: Option<Instant>,
    silences: Vec<Duration>,
    parts: Reassembler,
    pending: HashMap<u8, PssiRequest>,
    /// Blocs reçus, par platine et morceau, puis par type.
    blocks: BTreeMap<(u8, u32), BTreeMap<u8, BlockSeen>>,
    beats: u32,
}

impl<'a> Probe<'a> {
    fn new(
        interfaces: Vec<Interface>,
        chosen: Option<Interface>,
        passive: bool,
        socket: &'a UdpSocket,
        journal: Journal,
    ) -> Self {
        let now = Instant::now();
        Self {
            interfaces,
            chosen,
            passive,
            socket,
            journal,
            out: std::io::stdout(),
            started: now,
            opus: None,
            hello: None,
            next_announce: now,
            next_rescan: now + RESCAN_INTERVAL,
            send_failed: false,
            hinted: false,
            last_flush: now,
            counts: BTreeMap::new(),
            foreign: 0,
            sources: BTreeMap::new(),
            others: BTreeMap::new(),
            decks: BTreeMap::new(),
            zero_flags: 0,
            last_from_opus: None,
            longest_gap: Duration::ZERO,
            silent_since: None,
            silences: Vec::new(),
            parts: Reassembler::default(),
            pending: HashMap::new(),
            blocks: BTreeMap::new(),
            beats: 0,
        }
    }

    fn run(&mut self, rx: &mpsc::Receiver<Received>, deadline: Instant) -> Result<()> {
        let _raw = RawMode::enable()?;
        while Instant::now() < deadline {
            if let Some(key) = tui::poll_key(Duration::ZERO)?
                && tui::is_quit(&key)
            {
                break;
            }
            match rx.recv_timeout(LOOP_PERIOD) {
                Ok(packet) => self.handle(packet),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick(Instant::now())?;
        }
        print!("\r\n");
        Ok(())
    }

    /// Affiche un événement au-dessus de la ligne d'état, et le note dans le journal.
    fn say(&mut self, text: &str) {
        self.journal.note(Instant::now(), text);
        let _ = write!(self.out, "\r{text}");
        let _ = crossterm::queue!(self.out, terminal::Clear(ClearType::UntilNewLine));
        let _ = write!(self.out, "\r\n");
        let _ = self.out.flush();
    }

    /// Carte réseau du PC qui mène à `ip`.
    fn route(&self, ip: Ipv4Addr) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.net.contains(&ip))
    }

    fn route_label(&self, ip: Ipv4Addr) -> String {
        self.route(ip).map_or("carte inconnue".to_owned(), |i| format!("« {} »", i.label))
    }

    fn handle(&mut self, packet: Received) {
        let Received { at, port, from, data } = packet;
        let SocketAddr::V4(from) = from else { return };
        let ip = *from.ip();
        // Nos propres envois reviennent par la diffusion.
        if self.interfaces.iter().any(|i| i.ip == ip) {
            return;
        }
        self.journal.received(at, port, SocketAddr::V4(from), &data);
        let Some(parsed) = prolink::parse(port, &data) else {
            self.foreign += 1;
            return;
        };
        *self.counts.entry((port, data[10])).or_default() += 1;
        self.sources.entry(ip).or_default().packets += 1;
        if self.opus == Some(ip) {
            self.heard_opus(at);
        }
        match parsed {
            Packet::KeepAlive(device) if device.is_opus() => self.opus_seen(ip, at, &format!("numéro {}", device.device)),
            Packet::KeepAlive(device) => {
                if self.others.insert(ip, device.name.clone()).is_none() {
                    let warning = if device.name == "rekordbox" { " : ferme rekordbox pendant la sonde" } else { "" };
                    self.say(&format!("Autre appareil : « {} » (numéro {}) à {ip}{warning}.", device.name, device.device));
                }
            }
            Packet::Hello { name } => {
                self.opus_seen(ip, at, "bonjour");
                if self.hello.is_none() {
                    self.hello = Some(at);
                    self.say(&format!("L'OPUS a répondu (« {name} ») : il nous prend pour rekordbox."));
                    if !self.passive {
                        self.send(SocketAddrV4::new(ip, prolink::STATUS_PORT), &prolink::lighting_request());
                    }
                }
            }
            Packet::Status(status) => {
                self.opus_seen(ip, at, "état");
                self.status(at, status);
            }
            Packet::Metadata(part) => self.metadata(&part),
            Packet::Beat { device, bpm, beat_in_bar } => {
                self.beats += 1;
                if self.beats == 1 {
                    let bpm = bpm.map_or("?".to_owned(), |b| format!("{b:.2}"));
                    self.say(&format!(
                        "Paquet de temps reçu (appareil {device}, {bpm} BPM, temps {beat_in_bar}) : l'OPUS en envoie !"
                    ));
                }
            }
            Packet::Other { .. } => {}
        }
    }

    /// Un paquet de l'OPUS est arrivé de `ip` : on retient la première adresse et la carte réseau
    /// qui y mène, et on signale toute autre adresse (l'OPUS joignable par deux chemins).
    fn opus_seen(&mut self, ip: Ipv4Addr, at: Instant, what: &str) {
        let source = self.sources.entry(ip).or_default();
        let first_from_here = !source.opus;
        source.opus = true;
        if !first_from_here {
            return;
        }
        if self.route(ip).is_none() {
            // La carte de l'OPUS a pu recevoir son adresse après le démarrage de la sonde.
            self.interfaces = interfaces();
        }
        let route = self.route_label(ip);
        match self.opus {
            Some(first) => {
                self.say(&format!("L'OPUS parle aussi par {ip} ({route}) en plus de {first} : deux chemins à la fois."));
                return;
            }
            None => self.say(&format!("OPUS-QUAD vu à {ip} par {route} ({what}).")),
        }
        self.opus = Some(ip);
        self.last_from_opus = Some(at);
        if self.chosen.is_some() {
            return;
        }
        match self.route(ip).cloned() {
            Some(iface) => {
                if !self.passive {
                    self.say("On se présente comme rekordbox…");
                }
                self.chosen = Some(iface);
            }
            None => self.say(&format!("Aucune carte réseau du PC n'est dans le réseau de {ip} : rien ne sera envoyé.")),
        }
    }

    fn heard_opus(&mut self, at: Instant) {
        if let Some(last) = self.last_from_opus {
            self.longest_gap = self.longest_gap.max(at.saturating_duration_since(last));
        }
        if let Some(since) = self.silent_since.take() {
            let gap = at.saturating_duration_since(since);
            self.silences.push(gap);
            self.say(&format!("Liaison revenue après {:.1} s de silence.", gap.as_secs_f64()));
        }
        self.last_from_opus = Some(at);
    }

    fn status(&mut self, at: Instant, mut status: Status) {
        let deck_number = status.deck();
        let deck = self.decks.entry(deck_number).or_default();
        if status.flags == 0 {
            self.zero_flags += 1;
            status.flags = deck.flags;
        } else {
            deck.flags = status.flags;
        }
        deck.record_gap(at);
        let same_track = deck.last.as_ref().is_some_and(|last| last.track_id == status.track_id);
        deck.check_beats(at, &status, same_track);
        let state = describe(&status);
        let state_changed = deck.shown.as_deref() != Some(state.as_str());
        // Un balayage du pitch ne donne qu'une ligne par seconde au plus.
        let bpm = status.effective_bpm();
        let tempo_moved = match (deck.shown_bpm, bpm) {
            (Some((when, shown)), Some(bpm)) => {
                (bpm - shown).abs() >= TEMPO_STEP && at.saturating_duration_since(when) >= TEMPO_EVERY
            }
            (None, Some(_)) => true,
            _ => false,
        };
        let line = (state_changed || tempo_moved).then(|| format!("Platine {deck_number} : {}", describe_tempo(&status, &state)));
        if state_changed {
            deck.shown = Some(state);
        }
        if line.is_some() {
            deck.shown_bpm = bpm.map(|bpm| (at, bpm));
        }
        if !same_track && status.track_id != 0 {
            let request = PssiRequest { track: status.track_id, device: status.device, tries: 0, next: at };
            self.pending.insert(deck_number, request);
        }
        deck.last = Some(status);
        if let Some(line) = line {
            self.say(&line);
        }
    }

    fn metadata(&mut self, part: &prolink::Metadata) {
        let deck = part.deck();
        let Some(whole) = self.parts.push(part) else { return };
        if part.kind == block::SONG_STRUCTURE && self.pending.get(&deck).is_some_and(|r| r.track == part.track_id) {
            self.pending.remove(&deck);
        }
        let seen = self.blocks.entry((deck, part.track_id)).or_default();
        if let Some(seen) = seen.get_mut(&part.kind) {
            seen.times += 1;
            return;
        }
        let text = describe_block(part.kind, &whole);
        seen.insert(part.kind, BlockSeen { text: text.clone(), times: 1 });
        self.say(&format!("Platine {deck}, morceau {} : {text}", part.track_id));
    }

    fn tick(&mut self, now: Instant) -> Result<()> {
        if !self.passive
            && now >= self.next_announce
            && let Some(iface) = self.chosen.clone()
        {
            self.next_announce = now + ANNOUNCE_INTERVAL;
            let broadcast = iface.net.broadcast();
            self.send(SocketAddrV4::new(broadcast, prolink::ANNOUNCE_PORT), &prolink::keep_alive(iface.mac, iface.ip));
            self.send(SocketAddrV4::new(broadcast, prolink::STATUS_PORT), &prolink::lighting_request());
        }
        if !self.passive
            && let Some(opus) = self.opus
        {
            let due: Vec<u8> = self
                .pending
                .iter()
                .filter(|(_, r)| now >= r.next && r.tries < PSSI_TRIES)
                .map(|(&deck, _)| deck)
                .collect();
            for deck in due {
                let Some(request) = self.pending.get_mut(&deck) else { continue };
                request.tries += 1;
                request.next = now + PSSI_RETRY;
                let packet = prolink::pssi_request(request.track, request.device);
                self.send(SocketAddrV4::new(opus, prolink::STATUS_PORT), &packet);
            }
        }
        if now >= self.next_rescan {
            self.next_rescan = now + RESCAN_INTERVAL;
            self.rescan();
        }
        if let Some(last) = self.last_from_opus
            && self.silent_since.is_none()
            && now.saturating_duration_since(last) > SILENCE
        {
            self.silent_since = Some(last);
            self.say(&format!("⚠ Plus rien de l'OPUS depuis {} s.", SILENCE.as_secs()));
        }
        if self.opus.is_none() && !self.hinted && now.saturating_duration_since(self.started) > HINT_AFTER {
            self.hinted = true;
            self.say("Rien de l'OPUS pour l'instant. Vérifie qu'il est allumé et branché en USB. Sa carte réseau");
            self.say("USB peut mettre une minute à recevoir une adresse : la sonde le signalera.");
            self.say("Le pare-feu Windows peut aussi bloquer : dj-bridge doit être autorisé sur ce réseau.");
        }
        if now.saturating_duration_since(self.last_flush) >= Duration::from_secs(1) {
            self.last_flush = now;
            self.journal.flush();
        }
        let line = self.status_text(now);
        tui::status_line(&mut self.out, &line)
    }

    /// Signale les adresses apparues ou disparues sur les cartes réseau du PC.
    fn rescan(&mut self) {
        let fresh = interfaces();
        let appeared: Vec<String> = fresh
            .iter()
            .filter(|i| !self.interfaces.contains(i))
            .map(|i| format!("Nouvelle adresse sur « {} » : {}/{}.", i.label, i.ip, i.net.prefix_len()))
            .collect();
        let gone: Vec<String> = self
            .interfaces
            .iter()
            .filter(|i| !fresh.contains(i))
            .map(|i| format!("Adresse perdue sur « {} » : {}.", i.label, i.ip))
            .collect();
        self.interfaces = fresh;
        for line in appeared.iter().chain(&gone) {
            self.say(line);
        }
    }

    fn send(&mut self, to: SocketAddrV4, data: &[u8]) {
        match self.socket.send_to(data, to) {
            Ok(_) => self.journal.sent(Instant::now(), to, data),
            Err(e) if !self.send_failed => {
                self.send_failed = true;
                self.say(&format!("Envoi impossible vers {to} : {e}"));
            }
            Err(_) => {}
        }
    }

    fn status_text(&self, now: Instant) -> String {
        let elapsed = now.saturating_duration_since(self.started).as_secs();
        let opus = self.opus.map_or("OPUS non vu".to_owned(), |ip| format!("OPUS {ip}"));
        let per_port = |port| self.counts.iter().filter(|((p, _), _)| *p == port).map(|(_, n)| n).sum::<u32>();
        format!(
            "{elapsed} s │ {opus} │ paquets 50000:{} 50001:{} 50002:{} │ coupures {} │ Échap = arrêter",
            per_port(prolink::ANNOUNCE_PORT),
            per_port(prolink::BEAT_PORT),
            per_port(prolink::STATUS_PORT),
            self.silences.len() + usize::from(self.silent_since.is_some()),
        )
    }

    fn close(&mut self) {
        if let Some(since) = self.silent_since.take() {
            self.silences.push(Instant::now().saturating_duration_since(since));
        }
        self.journal.flush();
    }

    fn summary(&self, journal: &Path) {
        let elapsed = self.started.elapsed().as_secs_f64();
        println!();
        println!("Bilan après {elapsed:.0} s :");
        match &self.chosen {
            Some(iface) => println!("  Carte réseau utilisée : « {} » ({})", iface.label, iface.ip),
            None => println!("  Carte réseau utilisée : aucune trouvée vers l'OPUS"),
        }
        match (self.opus, self.hello) {
            (None, _) => println!("  OPUS-QUAD : jamais vu"),
            (Some(ip), Some(hello)) => println!(
                "  OPUS-QUAD : {ip}, il a répondu à rekordbox après {:.1} s",
                hello.saturating_duration_since(self.started).as_secs_f64()
            ),
            (Some(ip), None) => println!("  OPUS-QUAD : {ip}, sans réponse à rekordbox"),
        }
        for (ip, source) in self.sources.iter().filter(|(_, s)| s.opus) {
            println!("  Chemin : {ip} par {}, {} paquets", self.route_label(*ip), source.packets);
        }
        for (ip, name) in &self.others {
            println!("  Autre appareil : « {name} » à {ip}");
        }
        let counts: Vec<String> =
            self.counts.iter().map(|((port, kind), n)| format!("{port}/0x{kind:02x} ×{n}")).collect();
        println!("  Paquets reçus (port/type) : {}", if counts.is_empty() { "aucun".to_owned() } else { counts.join(", ") });
        if self.foreign > 0 {
            println!("  Paquets étrangers au Pro DJ Link : {}", self.foreign);
        }
        for (number, deck) in &self.decks {
            let gaps = f64::from(deck.count.saturating_sub(1).max(1));
            let mean = deck.gap_sum / gaps;
            let spread = (deck.gap_square_sum / gaps - mean * mean).max(0.0).sqrt();
            print!(
                "  Platine {number} : {} états, un tous les {:.0} ms (écart-type {:.0} ms, au plus {:.0} ms)",
                deck.count,
                mean * 1e3,
                spread * 1e3,
                deck.gap_max.as_secs_f64() * 1e3
            );
            if deck.checked_secs >= MIN_TEMPO_CHECK {
                println!(
                    " ; compteur de temps : {:.2} BPM mesurés sur {:.0} s de lecture, {:.2} joués",
                    f64::from(deck.checked_beats) / deck.checked_secs * 60.0,
                    deck.checked_secs,
                    deck.checked_bpm_secs / deck.checked_secs
                );
            } else {
                println!();
            }
        }
        if self.zero_flags > 0 {
            println!("  États aux drapeaux à zéro (remplacés par les précédents) : {}", self.zero_flags);
        }
        if self.blocks.is_empty() {
            println!("  Données des morceaux : aucune reçue");
        }
        for ((deck, track), blocks) in &self.blocks {
            println!("  Platine {deck}, morceau {track} :");
            for seen in blocks.values() {
                let times = if seen.times > 1 { format!(" (reçu {} fois)", seen.times) } else { String::new() };
                println!("    {}{times}", seen.text);
            }
        }
        for (deck, request) in &self.pending {
            println!("  Structure demandée {} fois sans réponse : platine {deck}, morceau {}", request.tries, request.track);
        }
        println!("  Paquets de temps (port 50001) : {}", self.beats);
        let silences: Vec<String> = self.silences.iter().map(|s| format!("{:.1} s", s.as_secs_f64())).collect();
        println!(
            "  Coupures de plus de {} s : {}{} ; plus long écart entre deux paquets : {:.0} ms",
            SILENCE.as_secs(),
            self.silences.len(),
            if silences.is_empty() { String::new() } else { format!(" ({})", silences.join(", ")) },
            self.longest_gap.as_secs_f64() * 1e3
        );
        println!("  Journal : {}", journal.display());
    }
}

/// Ce qui, dans l'état d'une platine hors tempo, mérite d'être signalé quand ça change.
fn describe(status: &Status) -> String {
    let mut state = vec![match (status.playing(), status.looping()) {
        (_, true) => "boucle",
        (true, false) => "lecture",
        (false, false) => "arrêt",
    }];
    if status.master() {
        state.push("maître");
    }
    if status.on_air() {
        state.push("à l'antenne");
    }
    if status.synced() {
        state.push("sync");
    }
    format!(
        "morceau {} (source {}/{}) │ {} (0x{:02x})",
        status.track_id,
        status.source_player,
        status.source_slot,
        state.join(", "),
        status.play_state
    )
}

/// Description de l'état avec le tempo, glissé après le morceau.
fn describe_tempo(status: &Status, state: &str) -> String {
    let tempo = match (status.bpm, status.effective_bpm()) {
        (Some(bpm), Some(played)) => format!("{played:.1} BPM (morceau {bpm:.2}, pitch {:+.1} %)", status.pitch),
        _ => "tempo inconnu".to_owned(),
    };
    match state.split_once(" │ ") {
        Some((track, rest)) => format!("{track} │ {tempo} │ {rest}"),
        None => format!("{state} │ {tempo}"),
    }
}

/// Résumé d'un bloc de données d'un morceau.
fn describe_block(kind: u8, whole: &[u8]) -> String {
    match kind {
        block::SONG_STRUCTURE => match SongStructure::parse(whole) {
            Some(song) => {
                let phrases: Vec<String> =
                    song.phrases.iter().map(|p| format!("{} {}", song.label(p.kind), p.beat)).collect();
                format!(
                    "structure, humeur {}, fin au temps {}, {} parties : {}",
                    song.mood_label(),
                    song.end_beat,
                    song.phrases.len(),
                    phrases.join(" · ")
                )
            }
            None => format!("structure illisible ({} octets)", whole.len()),
        },
        block::BEAT_GRID => match BeatGrid::parse(whole) {
            Some(grid) => {
                let tempos: BTreeSet<u32> = grid.beats.iter().map(|b| (b.bpm * 100.0).round() as u32).collect();
                let tempo = match (tempos.first(), tempos.last()) {
                    (Some(low), Some(high)) if low == high => format!("{:.2} BPM", f64::from(*low) / 100.0),
                    (Some(low), Some(high)) => {
                        format!("de {:.2} à {:.2} BPM", f64::from(*low) / 100.0, f64::from(*high) / 100.0)
                    }
                    _ => "sans tempo".to_owned(),
                };
                let first = grid.beats.first().map_or(0, |b| b.time_ms);
                format!("grille de {} temps, {tempo}, premier temps à {first} ms", grid.beats.len())
            }
            None => format!("grille illisible ({} octets)", whole.len()),
        },
        block::WAVEFORM_PREVIEW | block::WAVEFORM_DETAIL => {
            let name = if kind == block::WAVEFORM_DETAIL { "waveform détaillée" } else { "aperçu de waveform" };
            match Waveform::parse(whole) {
                Some(w) if w.per_second > 0 => format!(
                    "{name} : {} points de {} octets, {} par seconde ({:.0} s)",
                    w.points,
                    w.point_len,
                    w.per_second,
                    f64::from(w.points) / f64::from(w.per_second)
                ),
                Some(w) => format!("{name} : {} points de {} octets", w.points, w.point_len),
                None => format!("{name} illisible ({} octets)", whole.len()),
            }
        }
        block::ARTWORK => format!("pochette de {} Ko", whole.len().div_ceil(1024)),
        _ => format!("bloc de type 0x{kind:02x}, {} octets", whole.len()),
    }
}

fn mac_text(mac: [u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

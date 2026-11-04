//! Sonde OPUS-QUAD : écoute ce que l'OPUS envoie par le câble USB (sa carte réseau intégrée),
//! se présente comme rekordbox pour qu'il envoie l'état de ses platines, et rend compte de ce
//! qui arrive, à quelle cadence, et des coupures de la liaison. Chaque paquet est aussi écrit
//! dans un journal, dans le dossier des captures, pour être relu et servir de cas de test.

use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::prolink::{self, Packet, Reassembler, SongStructure, Status};

/// Cadence de l'annonce de présence et de la demande d'état (celle de Beat Link).
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);
/// Au-delà, un silence de l'OPUS compte comme une coupure de la liaison.
const SILENCE: Duration = Duration::from_secs(1);
/// Sans nouvelles de l'OPUS passé ce délai, on rappelle quoi vérifier.
const HINT_AFTER: Duration = Duration::from_secs(10);
const PSSI_RETRY: Duration = Duration::from_secs(3);
const PSSI_TRIES: u32 = 4;
/// Octet de platine de la demande de structure chez Beat Link, essayé en alternance avec 9–12.
const PSSI_ANY_DECK: u8 = 0x02;
/// Durée de lecture minimale pour comparer le compteur de temps au tempo annoncé.
const MIN_TEMPO_RUN: f64 = 8.0;
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
    let interfaces = interfaces();
    println!("Cartes réseau actives :");
    for iface in &interfaces {
        println!("  {} : {}/{} (MAC {})", iface.label, iface.ip, iface.net.prefix_len(), mac_text(iface.mac));
    }
    if interfaces.is_empty() {
        println!("  aucune avec une adresse IPv4");
    }
    let forced = match &options.interface {
        Some(part) => {
            let needle = part.to_lowercase();
            let Some(iface) = interfaces.iter().find(|i| i.label.to_lowercase().contains(&needle)) else {
                bail!("aucune carte réseau active ne contient « {part} »");
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
#[derive(Clone)]
struct Interface {
    label: String,
    ip: Ipv4Addr,
    net: Ipv4Net,
    mac: [u8; 6],
}

fn interfaces() -> Vec<Interface> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !i.is_loopback())
        .flat_map(|i| {
            let name = i.friendly_name.clone().unwrap_or_else(|| i.name.clone());
            let label = match &i.description {
                Some(description) if *description != name => format!("{name} ({description})"),
                _ => name,
            };
            let mac = i.mac_addr.map(|m| m.octets()).unwrap_or_default();
            i.ipv4
                .iter()
                .map(|net| Interface { label: label.clone(), ip: net.addr(), net: *net, mac })
                .collect::<Vec<_>>()
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
        for iface in interfaces {
            writeln!(file, "# carte {} {}/{} {}", iface.label, iface.ip, iface.net.prefix_len(), mac_text(iface.mac))?;
        }
        writeln!(file, "# secondes < port_local source hex | secondes > destination hex")?;
        Ok(Self { file, started: Instant::now() })
    }

    fn received(&mut self, at: Instant, port: u16, from: SocketAddr, data: &[u8]) {
        let t = at.saturating_duration_since(self.started).as_secs_f64();
        let _ = writeln!(self.file, "{t:.3} < {port} {from} {}", hex::encode(data));
    }

    fn sent(&mut self, at: Instant, to: SocketAddrV4, data: &[u8]) {
        let t = at.saturating_duration_since(self.started).as_secs_f64();
        let _ = writeln!(self.file, "{t:.3} > {to} {}", hex::encode(data));
    }

    fn flush(&mut self) {
        let _ = self.file.flush();
    }
}

/// Ce qu'on sait d'une platine.
#[derive(Default)]
struct Deck {
    last: Option<Status>,
    /// Dernière description affichée.
    shown: Option<String>,
    /// Derniers drapeaux non nuls : l'OPUS envoie parfois des états aux drapeaux à zéro.
    flags: u8,
    count: u32,
    last_at: Option<Instant>,
    gap_sum: Duration,
    gap_max: Duration,
    /// Premier et dernier changement du compteur de temps pendant la lecture en cours.
    run_start: Option<(Instant, u32)>,
    run_last: Option<(Instant, u32)>,
    run_bpm: Option<f64>,
    best_run: Option<TempoCheck>,
}

/// Tempo mesuré par le compteur de temps, comparé au tempo annoncé.
struct TempoCheck {
    secs: f64,
    measured: f64,
    reported: f64,
}

impl Deck {
    fn update_run(&mut self, at: Instant, status: &Status, same_track: bool) {
        match (status.playing() && same_track, status.beat, status.effective_bpm()) {
            (true, Some(beat), Some(bpm)) => {
                self.run_start.get_or_insert((at, beat));
                if self.run_last.is_none_or(|(_, last)| last != beat) {
                    self.run_last = Some((at, beat));
                }
                self.run_bpm = Some(bpm);
            }
            _ => self.close_run(),
        }
    }

    fn close_run(&mut self) {
        let (Some((t0, b0)), Some((t1, b1)), Some(reported)) =
            (self.run_start.take(), self.run_last.take(), self.run_bpm.take())
        else {
            return;
        };
        let secs = t1.saturating_duration_since(t0).as_secs_f64();
        if secs >= MIN_TEMPO_RUN && b1 > b0 && self.best_run.as_ref().is_none_or(|best| secs > best.secs) {
            self.best_run = Some(TempoCheck { secs, measured: f64::from(b1 - b0) / secs * 60.0, reported });
        }
    }
}

/// Demande de structure en attente pour une platine.
struct PssiRequest {
    track: u32,
    device: u8,
    tries: u32,
    next: Instant,
    first: Option<Instant>,
    last_byte: u8,
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
    send_failed: bool,
    hinted: bool,
    last_flush: Instant,
    counts: BTreeMap<(u16, u8), u32>,
    foreign: u32,
    others: BTreeMap<Ipv4Addr, String>,
    decks: BTreeMap<u8, Deck>,
    zero_flags: u32,
    last_from_opus: Option<Instant>,
    longest_gap: Duration,
    silent_since: Option<Instant>,
    silences: Vec<Duration>,
    pssi: Reassembler,
    pending: HashMap<u8, PssiRequest>,
    structures: Vec<String>,
    other_metadata: HashSet<(u8, u8, u32)>,
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
            send_failed: false,
            hinted: false,
            last_flush: now,
            counts: BTreeMap::new(),
            foreign: 0,
            others: BTreeMap::new(),
            decks: BTreeMap::new(),
            zero_flags: 0,
            last_from_opus: None,
            longest_gap: Duration::ZERO,
            silent_since: None,
            silences: Vec::new(),
            pssi: Reassembler::default(),
            pending: HashMap::new(),
            structures: Vec::new(),
            other_metadata: HashSet::new(),
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

    /// Affiche un événement au-dessus de la ligne d'état.
    fn say(&mut self, text: &str) {
        let _ = write!(self.out, "\r{text}");
        let _ = crossterm::queue!(self.out, terminal::Clear(ClearType::UntilNewLine));
        let _ = write!(self.out, "\r\n");
        let _ = self.out.flush();
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
        if self.opus == Some(ip) {
            self.heard_opus(at);
        }
        match parsed {
            Packet::KeepAlive(device) if device.is_opus() => {
                if self.opus.is_none() {
                    self.say(&format!(
                        "OPUS-QUAD vu à {ip} (numéro {}, MAC {}).",
                        device.device,
                        mac_text(device.mac)
                    ));
                    self.set_opus(ip, at);
                }
            }
            Packet::KeepAlive(device) => {
                if self.others.insert(ip, device.name.clone()).is_none() {
                    let warning = if device.name == "rekordbox" { " : ferme rekordbox pendant la sonde" } else { "" };
                    self.say(&format!("Autre appareil : « {} » (numéro {}) à {ip}{warning}.", device.name, device.device));
                }
            }
            Packet::Hello { name } => {
                if self.hello.is_none() {
                    self.hello = Some(at);
                    self.say(&format!("L'OPUS a répondu (« {name} ») : il nous prend pour rekordbox."));
                    self.set_opus(ip, at);
                    if !self.passive {
                        self.send(SocketAddrV4::new(ip, prolink::STATUS_PORT), &prolink::lighting_request());
                    }
                }
            }
            Packet::Status(status) => self.status(at, ip, status),
            Packet::Metadata(part) => self.metadata(at, &part),
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

    /// Retient l'adresse de l'OPUS et la carte réseau qui mène à lui.
    fn set_opus(&mut self, ip: Ipv4Addr, at: Instant) {
        if self.opus.is_some() {
            return;
        }
        self.opus = Some(ip);
        self.last_from_opus = Some(at);
        if self.chosen.is_some() {
            return;
        }
        if !self.interfaces.iter().any(|i| i.net.contains(&ip)) {
            // La carte de l'OPUS a pu recevoir son adresse après le démarrage de la sonde.
            self.interfaces = interfaces();
        }
        match self.interfaces.iter().find(|i| i.net.contains(&ip)).cloned() {
            Some(iface) => {
                self.say(&format!("Liaison par la carte « {} » ({}/{}).", iface.label, iface.ip, iface.net.prefix_len()));
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

    fn status(&mut self, at: Instant, ip: Ipv4Addr, mut status: Status) {
        self.set_opus(ip, at);
        let deck_number = status.deck();
        let deck = self.decks.entry(deck_number).or_default();
        if status.flags == 0 {
            self.zero_flags += 1;
            status.flags = deck.flags;
        } else {
            deck.flags = status.flags;
        }
        deck.count += 1;
        if let Some(last) = deck.last_at {
            let gap = at.saturating_duration_since(last);
            deck.gap_sum += gap;
            deck.gap_max = deck.gap_max.max(gap);
        }
        deck.last_at = Some(at);
        let same_track = deck.last.as_ref().is_some_and(|last| last.track_id == status.track_id);
        deck.update_run(at, &status, same_track);
        let text = describe(&status);
        let changed = deck.shown.as_deref() != Some(text.as_str());
        if changed {
            deck.shown = Some(text.clone());
        }
        let new_track = !same_track && status.track_id != 0;
        if new_track {
            self.pending.insert(
                deck_number,
                PssiRequest { track: status.track_id, device: status.device, tries: 0, next: at, first: None, last_byte: 0 },
            );
        }
        deck.last = Some(status);
        if changed {
            self.say(&format!("Platine {deck_number} : {text}"));
        }
    }

    fn metadata(&mut self, at: Instant, part: &prolink::Metadata) {
        let deck = part.deck();
        if part.kind != prolink::METADATA_PSSI {
            if part.index.saturating_add(1) >= part.count && self.other_metadata.insert((deck, part.kind, part.track_id)) {
                self.say(&format!(
                    "Platine {deck} : métadonnées de type {} reçues pour le morceau {}.",
                    part.kind, part.track_id
                ));
            }
            return;
        }
        let Some(block) = self.pssi.push(part) else { return };
        let request = match self.pending.get(&deck) {
            Some(request) if request.track == part.track_id => self.pending.remove(&deck),
            _ => None,
        };
        let how = match request.and_then(|r| r.first.map(|first| (first, r.last_byte))) {
            Some((first, byte)) => format!(
                "après demande, octet de platine 0x{byte:02x}, {:.1} s",
                at.saturating_duration_since(first).as_secs_f64()
            ),
            None => "spontanément".to_owned(),
        };
        let line = match SongStructure::parse(&block) {
            Some(song) => {
                let phrases: Vec<String> =
                    song.phrases.iter().map(|p| format!("{} {}", song.label(p.kind), p.beat)).collect();
                format!(
                    "morceau {} : humeur {}, fin au temps {}, {} parties ({how}) : {}",
                    part.track_id,
                    song.mood_label(),
                    song.end_beat,
                    song.phrases.len(),
                    phrases.join(" · ")
                )
            }
            None => format!("morceau {} : structure de {} octets, illisible ({how})", part.track_id, block.len()),
        };
        self.structures.push(format!("platine {deck}, {line}"));
        self.say(&format!("Platine {deck}, structure du {line}"));
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
                let byte = if request.tries % 2 == 0 { request.device } else { PSSI_ANY_DECK };
                request.tries += 1;
                request.next = now + PSSI_RETRY;
                request.last_byte = byte;
                request.first.get_or_insert(now);
                let packet = prolink::pssi_request(request.track, byte);
                self.send(SocketAddrV4::new(opus, prolink::STATUS_PORT), &packet);
            }
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
            self.say("Rien de l'OPUS pour l'instant. Vérifie qu'il est allumé et branché en USB, et qu'une carte");
            self.say("réseau de l'OPUS figure dans la liste du début (sinon relance la sonde dans une minute).");
            self.say("Le pare-feu Windows peut aussi bloquer : dj-bridge doit être autorisé sur ce réseau.");
        }
        if now.saturating_duration_since(self.last_flush) >= Duration::from_secs(1) {
            self.last_flush = now;
            self.journal.flush();
        }
        let line = self.status_text(now);
        tui::status_line(&mut self.out, &line)
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
        for deck in self.decks.values_mut() {
            deck.close_run();
        }
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
            Some(iface) => println!("  Carte réseau : « {} » ({})", iface.label, iface.ip),
            None => println!("  Carte réseau : aucune trouvée vers l'OPUS"),
        }
        match (self.opus, self.hello) {
            (None, _) => println!("  OPUS-QUAD : jamais vu"),
            (Some(ip), Some(hello)) => println!(
                "  OPUS-QUAD : {ip}, il a répondu à rekordbox après {:.1} s",
                hello.saturating_duration_since(self.started).as_secs_f64()
            ),
            (Some(ip), None) => println!("  OPUS-QUAD : {ip}, sans réponse à rekordbox"),
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
            let mean = deck.gap_sum.as_secs_f64() * 1e3 / f64::from(deck.count.saturating_sub(1).max(1));
            print!(
                "  Platine {number} : {} états, un tous les {mean:.0} ms en moyenne (au plus {:.0} ms)",
                deck.count,
                deck.gap_max.as_secs_f64() * 1e3
            );
            match &deck.best_run {
                Some(run) => println!(
                    " ; compteur de temps : {:.2} BPM mesurés sur {:.0} s, {:.2} annoncés",
                    run.measured, run.secs, run.reported
                ),
                None => println!(),
            }
        }
        if self.zero_flags > 0 {
            println!("  États aux drapeaux à zéro (remplacés par les précédents) : {}", self.zero_flags);
        }
        if self.structures.is_empty() {
            println!("  Structures de morceaux : aucune reçue");
        }
        for line in &self.structures {
            println!("  Structure : {line}");
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

/// Ce qui, dans l'état d'une platine, mérite d'être signalé quand ça change.
fn describe(status: &Status) -> String {
    let tempo = match (status.bpm, status.effective_bpm()) {
        (Some(bpm), Some(played)) => format!("{played:.1} BPM (morceau {bpm:.2}, pitch {:+.1} %)", status.pitch),
        _ => "tempo inconnu".to_owned(),
    };
    let mut state = vec![if status.playing() { "lecture" } else { "arrêt" }];
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
        "morceau {} (source {}/{}) │ {tempo} │ {} (0x{:02x})",
        status.track_id,
        status.source_player,
        status.source_slot,
        state.join(", "),
        status.play_state
    )
}

fn mac_text(mac: [u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

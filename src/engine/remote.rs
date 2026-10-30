//! Télécommande mobile : un petit serveur HTTP sur le réseau local sert une page pour téléphone
//! (tempo, phase, couleur) et reçoit ses commandes (scène, animation, disposition, régisseur,
//! drop). L'adresse porte un jeton secret : seul qui l'a reçue pilote.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// Port de la télécommande (le suivant s'il est pris).
pub const REMOTE_PORT: u16 = 8484;
const PAGE: &str = include_str!("remote.html");
/// Une requête tient en 4 Kio et arrive en 5 s, ou la connexion est fermée.
const MAX_REQUEST: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Commande reçue de la télécommande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    Motion(Motion),
    Spread(Spread),
    Flash(FlashMode),
    Director(bool),
    Preset(usize),
    Drop,
}

impl Command {
    fn parse(query: &HashMap<&str, &str>) -> Option<Self> {
        let index = |key: &str| query.get(key)?.parse::<usize>().ok();
        Some(match *query.get("action")? {
            "motion" => Self::Motion(*Motion::ALL.get(index("value")?)?),
            "spread" => Self::Spread(*Spread::ALL.get(index("value")?)?),
            "flash" => Self::Flash(*FLASH_MODES.get(index("value")?)?),
            "director" => Self::Director(index("value")? != 0),
            "preset" => Self::Preset(index("value")?),
            "drop" => Self::Drop,
            _ => return None,
        })
    }
}

const FLASH_MODES: [FlashMode; 3] = [FlashMode::Kicks, FlashMode::Reactive, FlashMode::EveryBeat];

/// Ce que la télécommande lit et pilote.
pub(super) trait Remote: Send + Sync + 'static {
    fn state(&self) -> Value;
    fn command(&self, command: Command);
}

impl Remote for (Arc<Shared>, Arc<Features>) {
    fn state(&self) -> Value {
        let (shared, features) = self;
        let now = features.seconds(Instant::now());
        let (bpm, position) = {
            let tracker = shared.tracker();
            (tracker.clock().bpm(), tracker.clock().nearest_beat(now).and_then(|beat| tracker.position(beat)))
        };
        let build = shared.build.load(Ordering::Relaxed);
        let drop_beats = shared.drop_beats.load(Ordering::Relaxed);
        json!({
            "bpm": bpm,
            "phase": Phase::from_index(shared.phase.load(Ordering::Relaxed)).label(),
            "build": (build != NO_BUILD).then_some(build),
            "drop_beats": (drop_beats != NO_BUILD).then_some(drop_beats),
            "bar": position.map(|p| p.phrase / 4 + 1),
            "color": unpack(shared.color.load(Ordering::Relaxed)),
            "lamps": shared.lamp_colors.iter().map(|c| unpack(c.load(Ordering::Relaxed))).collect::<Vec<_>>(),
            "motion": shared.motion.load(Ordering::Relaxed),
            "spread": shared.spread.load(Ordering::Relaxed),
            "spreads": shared.movements.iter().map(|&s| (s.index(), s.label())).collect::<Vec<_>>(),
            "flash": FLASH_MODES.iter().position(|&mode| mode == shared.mode()),
            "director": shared.director.load(Ordering::Relaxed),
            "preset": *lock(&shared.preset),
            "presets": lock(&shared.presets).iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "bridge_lost": shared.bridge_lost.load(Ordering::Relaxed),
            "input_lost": features.input_lost(),
        })
    }

    fn command(&self, command: Command) {
        let shared = &self.0;
        match command {
            Command::Motion(motion) => shared.motion.store(motion.index(), Ordering::Relaxed),
            Command::Spread(spread) => shared.spread.store(spread.index(), Ordering::Relaxed),
            Command::Flash(mode) => shared.mode.store(mode.index(), Ordering::Relaxed),
            Command::Director(on) => shared.director.store(on, Ordering::Relaxed),
            Command::Preset(index) => {
                let preset = lock(&shared.presets).get(index).cloned();
                if let Some(preset) = preset {
                    shared.apply(&preset);
                }
                return;
            }
            Command::Drop => {
                shared.drop_now.store(true, Ordering::Relaxed);
                return;
            }
        }
        // La fenêtre reprend les réglages changés d'ici ; ils ne sont plus ceux d'une scène.
        *lock(&shared.preset) = None;
        shared.outside_changes.fetch_add(1, Ordering::Relaxed);
    }
}

/// Ouvre la télécommande sur le port habituel (ou le suivant) ; rend l'écoute et son port.
pub(super) fn bind(rt: &Runtime) -> Result<(TcpListener, u16)> {
    let listener = rt
        .block_on(TcpListener::bind((Ipv4Addr::UNSPECIFIED, REMOTE_PORT)))
        .or_else(|_| rt.block_on(TcpListener::bind((Ipv4Addr::UNSPECIFIED, REMOTE_PORT + 1))))
        .context("télécommande : aucun port libre")?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Adresse du PC sur le réseau du bridge (aucun paquet n'est envoyé pour la trouver).
pub(super) fn lan_address(bridge_ip: &str) -> IpAddr {
    UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            socket.connect((bridge_ip, 80))?;
            socket.local_addr()
        })
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |addr| addr.ip())
}

/// Sert la télécommande jusqu'à ce que la tâche soit annulée.
pub(super) async fn serve(listener: TcpListener, remote: Arc<impl Remote>, token: String) {
    let token: Arc<str> = token.into();
    loop {
        let Ok((socket, _)) = listener.accept().await else { continue };
        let (remote, token) = (remote.clone(), token.clone());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(REQUEST_TIMEOUT, answer(socket, &*remote, &token)).await;
        });
    }
}

/// Lit une requête et y répond.
async fn answer(mut socket: impl AsyncRead + AsyncWrite + Unpin, remote: &impl Remote, token: &str) -> std::io::Result<()> {
    let mut request = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < MAX_REQUEST {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let request = String::from_utf8_lossy(&request);
    let target = request.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("");
    let (status, kind, body) = route(target, remote, token);
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.shutdown().await
}

/// Réponse à `target` (chemin et requête) : statut, type et corps.
fn route(target: &str, remote: &impl Remote, token: &str) -> (&'static str, &'static str, String) {
    const JSON: &str = "application/json";
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let Some(rest) = path.strip_prefix('/').and_then(|p| p.strip_prefix(token)) else {
        return ("404 Not Found", "text/plain; charset=utf-8", "introuvable".to_owned());
    };
    match rest {
        "" | "/" => ("200 OK", "text/html; charset=utf-8", PAGE.to_owned()),
        "/state" => ("200 OK", JSON, remote.state().to_string()),
        "/do" => {
            let query: HashMap<&str, &str> = query.split('&').filter_map(|pair| pair.split_once('=')).collect();
            match Command::parse(&query) {
                Some(command) => {
                    remote.command(command);
                    ("200 OK", JSON, remote.state().to_string())
                }
                None => ("400 Bad Request", JSON, json!({ "error": "commande inconnue" }).to_string()),
            }
        }
        _ => ("404 Not Found", "text/plain; charset=utf-8", "introuvable".to_owned()),
    }
}

/// Jeton de l'adresse : 64 bits au hasard, en hexadécimal.
pub fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Télécommande factice : garde les commandes reçues.
    #[derive(Default)]
    struct Fake(Mutex<Vec<Command>>);

    impl Remote for Fake {
        fn state(&self) -> Value {
            json!({ "commands": lock(&self.0).len() })
        }

        fn command(&self, command: Command) {
            lock(&self.0).push(command);
        }
    }

    #[test]
    fn the_page_and_its_commands_need_the_token() {
        let fake = Fake::default();
        assert_eq!(route("/", &fake, "abc").0, "404 Not Found");
        assert_eq!(route("/abd/state", &fake, "abc").0, "404 Not Found");
        let (status, kind, page) = route("/abc/", &fake, "abc");
        assert_eq!((status, kind), ("200 OK", "text/html; charset=utf-8"));
        assert!(page.contains("<html"));
        assert_eq!(route("/abc/state", &fake, "abc").2, r#"{"commands":0}"#);
    }

    #[test]
    fn commands_are_parsed_and_unknown_ones_refused() {
        let fake = Fake::default();
        route("/abc/do?action=spread&value=1", &fake, "abc");
        route("/abc/do?action=motion&value=2", &fake, "abc");
        route("/abc/do?action=director&value=0", &fake, "abc");
        route("/abc/do?action=preset&value=3", &fake, "abc");
        route("/abc/do?action=flash&value=2", &fake, "abc");
        let (status, _, state) = route("/abc/do?action=drop", &fake, "abc");
        assert_eq!((status, state.as_str()), ("200 OK", r#"{"commands":6}"#));
        assert_eq!(
            *lock(&fake.0),
            [
                Command::Spread(Spread::Together),
                Command::Motion(Motion::Breath),
                Command::Director(false),
                Command::Preset(3),
                Command::Flash(FlashMode::EveryBeat),
                Command::Drop,
            ]
        );
        assert_eq!(route("/abc/do?action=spread&value=99", &fake, "abc").0, "400 Bad Request");
        assert_eq!(route("/abc/do?action=reboot", &fake, "abc").0, "400 Bad Request");
    }

    #[test]
    fn a_request_over_tcp_gets_an_http_answer() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(serve(listener, Arc::new(Fake::default()), "abc".to_owned()));
            let mut client = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
            client.write_all(b"GET /abc/do?action=drop HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
            let mut answer = String::new();
            client.read_to_string(&mut answer).await.unwrap();
            server.abort();
            assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
            assert!(answer.ends_with(r#"{"commands":1}"#), "{answer}");
        });
    }

    #[test]
    fn tokens_differ() {
        assert_ne!(new_token(), new_token());
        assert_eq!(new_token().len(), 16);
    }
}

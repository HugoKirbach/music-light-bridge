//! Boucles des fils du moteur : éclairage (60 Hz), tempo, Ableton Link, OPUS-QUAD.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::{self, RecvTimeoutError};

use super::delays::LampDelays;
use super::*;
use crate::look::Guide;
use crate::opus::{self, Client};
use crate::prolink;

/// Durée d'un flash du test de synchro, et sa cadence sans tempo connu.
const SYNC_FLASH: f64 = 0.08;
const SYNC_PERIOD: f64 = 0.5;
const WHITE: Rgb = Rgb { r: u16::MAX, g: u16::MAX, b: u16::MAX };
/// Un kick de l'OPUS encore dû avec ce retard (réveil tardif) flashe quand même.
const OPUS_LATE: f64 = 0.1;

/// Flashe selon le planificateur (vrais kicks, anticipés quand ils sont attendus) et sur chaque
/// note MIDI, et entretient le flux à 60 Hz entre deux flashs. Quand l'OPUS mène, les flashs
/// partent sur les kicks de sa waveform, en avance de la latence des lampes, et la phase, les
/// niveaux et la couleur de la trame viennent de lui.
pub(super) fn light_loop(
    rt: &Runtime,
    stream: &mut HueStream,
    rig: &Rig,
    features: &Features,
    notes: &Notes,
    shared: &Shared,
    advance: Duration,
) -> Result<()> {
    let mut planner = FlashPlanner::new(shared.mode(), advance.as_secs_f64(), CONFIRM_RANGE.1);
    let mut look = ReactiveLook::new(shared.palette());
    let mut frame: Vec<(u8, Rgb)> = Vec::with_capacity(rig.channels.len());
    // Couleur de chaque canal sur la photo d'avant le live : celle d'une lampe qu'on désactive en
    // cours de route. Une lampe décochée dès le départ n'est pas touchée du tout.
    let before_live: Vec<Rgb> = rig
        .owners
        .iter()
        .map(|owner| owner.as_deref().and_then(|id| stream.before_color(id)).unwrap_or(Rgb::OFF))
        .collect();
    let mut streamed = vec![false; rig.channels.len()];
    // Canaux que le live allume (rangs), relus quand on en active ou désactive un.
    let mut active: Vec<usize> = Vec::new();
    let mut lamps_seen = None;
    let mut seen = features.kicks();
    let mut seen_notes = notes.count();
    let mut last = Instant::now();
    let mut next_frame = last;
    let mut delays = LampDelays::default();
    let mut lags: Vec<f32> = Vec::new();
    // L'OPUS menait la lumière à la trame précédente ; dernier kick flashé d'après lui (platine,
    // morceau, temps).
    let mut guided = false;
    let mut opus_flashed: Option<(u8, u32, u32)> = None;
    // Dernier appel de montée flashé : platine, morceau, temps, double croche.
    let mut opus_called: Option<(u8, u32, u32, u8)> = None;
    while !shared.stopped() {
        let changes = shared.lamps_changed.load(Ordering::Acquire);
        if lamps_seen != Some(changes) {
            lamps_seen = Some(changes);
            active = (0..rig.channels.len()).filter(|&i| shared.lamp_enabled[i].load(Ordering::Relaxed)).collect();
            // Les mouvements se répartissent entre les seules lampes allumées.
            look.set_space(&rig.space.subset(&active));
        }
        planner.mode = shared.mode();
        planner.hold_off_pattern = look.phase() != Phase::BuildUp;
        // La lampe la plus lente reçoit ses flashs d'autant plus tôt ; les autres l'attendent.
        lags.clear();
        lags.extend(active.iter().map(|&i| f32::from_bits(shared.lamp_lags[i].load(Ordering::Relaxed))));
        let head_start = Duration::from_secs_f32(lags.iter().copied().fold(0.0, f32::max) / 1000.0);
        let total_advance = advance + head_start;
        planner.advance = total_advance.as_secs_f64();
        // Le kick d'un flash anticipé arrive après l'âge d'un bloc audio : on l'attend jusque-là.
        planner.confirm_window = (features.input_latency().as_secs_f64() + CONFIRM_MARGIN)
            .clamp(CONFIRM_RANGE.0, CONFIRM_RANGE.1);
        let rhythm = shared.tracker().rhythm();
        let before = features.seconds(Instant::now());
        // Réveil au prochain flash : anticipé par le planificateur, ou kick (ou appel de montée)
        // prévu par l'OPUS.
        let opus_flash = shared.opus_guide(before).and_then(|(_, guide)| {
            let kick = guide.next_kick.map(|(_, at, _)| at);
            let call = guide.next_call.map(|(_, _, at, _)| at);
            kick.into_iter().chain(call).reduce(f64::min).map(|at| at - total_advance.as_secs_f64())
        });
        let event = match (planner.next_event(rhythm.as_ref(), before), opus_flash) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let deadline = match event {
            Some(event) => next_frame.min(features.instant(event)),
            None => next_frame,
        };
        timing::sleep_until_or(deadline, || features.kicks() != seen || notes.count() != seen_notes);

        let now = Instant::now();
        let opus = shared.opus_guide(features.seconds(now));
        if guided && opus.is_none() {
            // L'OPUS ne mène plus (liaison coupée, platine arrêtée) : l'écoute reprend la main.
            shared.tracker().set_auto();
            *lock(&shared.opus_now) = None;
        }
        guided = opus.is_some();

        let kicks = features.kicks();
        // Hors groove, un coup sans grave (charleston, clap, mélodie) n'est pas un kick : il ne flashe
        // pas et ne compte ni pour le tempo ni pour le motif. Quand l'OPUS mène, le son n'y est pour rien.
        let kick = (kicks != seen && !guided && look.carries_bass(features.levels().raw_bass))
            .then(|| features.seconds(features.last_kick()) - KICK_DETECTION_DELAY);
        seen = kicks;
        if let Some(at) = kick {
            shared.tracker().on_kick(at);
            look.kick_detected(features.levels().raw_bass);
        }
        if let Some((_, guide)) = &opus {
            shared.tracker().set_opus(guide.period, guide.beat_time);
        }
        let note_count = notes.count();
        let note = (note_count != seen_notes).then(|| notes.last());
        seen_notes = note_count;

        let (rhythm, locked, structure, beat_time) = {
            let tracker = shared.tracker();
            let clock = tracker.clock();
            // Place sur la grille, recalée sur la phrase quand la structure est reconnue : les
            // mouvements lents de la lumière s'y calent, même pendant un break.
            let beat_time = clock.period().map(|period| {
                let beats = (features.seconds(now) - clock.anchor()) / period;
                let last = clock.anchor() + beats.floor() * period;
                let shift = tracker.position(last).map_or(0.0, |p| f64::from(p.phrase) - beats.floor().rem_euclid(16.0));
                BeatTime { beats: beats + shift, period: period as f32 }
            });
            (tracker.rhythm(), tracker.locked(features.seconds(now)), tracker.structure(features.seconds(now)), beat_time)
        };
        let (action, grooving, beat_time) = match &opus {
            Some((playing, guide)) => {
                let (action, accent) = opus_action(playing, guide, features.seconds(now), total_advance.as_secs_f64(), &mut opus_flashed);
                if let Some(accent) = accent {
                    look.kick_at(accent);
                }
                if let Some(attack) = opus_call(playing, guide, features.seconds(now), total_advance.as_secs_f64(), &mut opus_called) {
                    look.call(attack);
                }
                // Les mouvements lents se calent sur la phrase rekordbox : un multiple de 16 en tête.
                let start = guide.phrase.as_ref().map_or(1, |p| p.start);
                let fraction = ((features.seconds(now) - guide.beat_time) / guide.period).clamp(0.0, 1.0);
                let beats = f64::from(guide.beat.saturating_sub(start)) + fraction;
                let beat_time = Some(BeatTime { beats, period: guide.period as f32 });
                publish_opus(shared, playing, guide, now);
                (action, guide.phase == Phase::Groove, beat_time)
            }
            None => {
                let action = planner.poll(rhythm.as_ref(), features.seconds(now), kick);
                // La détection cherche les kicks là où le motif les attend dès que le tempo est sûr, et
                // se méfie de ce qui tombe ailleurs tant que le groove tient.
                let grooving = planner.in_groove(rhythm.as_ref(), features.seconds(now));
                features.set_grid(Grid::of(rhythm.as_ref(), locked, grooving));
                match action {
                    Action::Flash { reactive } => {
                        // Place du temps flashé dans la structure : tête de phrase ou de mesure.
                        let at = if reactive { kick.unwrap_or(features.seconds(now)) } else { features.seconds(now) + total_advance.as_secs_f64() };
                        match shared.tracker().position(at) {
                            Some(p) if p.phrase == 0 => look.kick_at(Accent::Phrase),
                            Some(p) if p.beat == 0 => look.kick_at(Accent::Bar),
                            Some(_) => look.kick_at(Accent::Beat),
                            None => look.kick(),
                        }
                    }
                    Action::Cancel => look.cancel_flash(),
                    Action::None => {}
                }
                (action, grooving, beat_time)
            }
        };
        shared.grooving.store(grooving, Ordering::Relaxed);
        look.set_grid_groove(rhythm.map(|_| grooving));
        look.set_structure(structure);
        look.set_drop_advance(total_advance.as_secs_f32());
        let trame = shared.trame.load(Ordering::Relaxed);
        look.set_guide(opus.as_ref().map(|(_, guide)| Guide {
            phase: guide.phase,
            build: guide.build.unwrap_or(0.0),
            drop_in: guide.drop_in,
            color: guide.color.filter(|_| trame),
        }));
        if shared.drop_now.swap(false, Ordering::Relaxed) {
            look.drop_now();
        }
        if let Some(preset) = note.and_then(|(pitch, _)| shared.preset_for(pitch)) {
            shared.apply(&preset);
        } else if let Some((pitch, velocity)) = note {
            look.note(note_hue(pitch), velocity);
            shared.note_flashes.fetch_add(1, Ordering::Relaxed);
            shared.last_note.store(features.seconds(now).to_bits(), Ordering::Relaxed);
        }
        look.palette = shared.palette();
        look.dynamics = shared.dynamics();
        look.motion = Motion::from_index(shared.motion.load(Ordering::Relaxed));
        look.spread = Spread::from_index(shared.spread.load(Ordering::Relaxed));
        look.set_director(shared.director.load(Ordering::Relaxed));
        look.rotation = Rotation::from_index(shared.rotation.load(Ordering::Relaxed));
        look.set_beat_time(beat_time);
        let levels = opus.as_ref().map_or_else(|| features.levels(), |(_, guide)| guide.levels);
        let mut colors = look.render_lamps(now - last, levels).to_vec();
        if look.take_drop() {
            shared.tracker().on_drop();
        }
        if shared.sync_test.load(Ordering::Relaxed) {
            // Blanc sur chaque temps, tel qu'il sonnera quand les lampes montreront l'image.
            let at = features.seconds(now) + total_advance.as_secs_f64();
            let (period, anchor) = rhythm.map_or((SYNC_PERIOD, 0.0), |r| (r.period, r.bar));
            let lit = (at - anchor).rem_euclid(period) < SYNC_FLASH;
            colors.iter_mut().for_each(|c| *c = if lit { WHITE } else { Rgb::OFF });
        }
        let colors = delays.delay(now, colors, &lags);
        frame.clear();
        let mut lit = active.iter().zip(&colors).peekable();
        for (i, &channel) in rig.channels.iter().enumerate() {
            let shown = match lit.next_if(|&(&rank, _)| rank == i) {
                Some((_, &color)) => {
                    streamed[i] = true;
                    Some(color)
                }
                // Lampe désactivée après avoir été allumée : elle retrouve son état d'avant le live.
                None if streamed[i] => Some(before_live[i]),
                None => None,
            };
            if let Some(color) = shown {
                frame.push((channel, color));
            }
            shared.lamp_colors[i].store(pack(shown.unwrap_or(before_live[i])), Ordering::Relaxed);
        }
        // Couleur résumée (aperçu, fond de la scène) : la plus lumineuse des lampes.
        let brightest = colors.iter().copied().max_by_key(|c| c.r.max(c.g).max(c.b)).unwrap_or(Rgb::OFF);
        shared.color.store(pack(brightest), Ordering::Relaxed);
        shared.phase.store(look.phase().index(), Ordering::Relaxed);
        shared.build.store(look.build_progress().map_or(NO_BUILD, |p| (p * 100.0).round() as u8), Ordering::Relaxed);
        let drop_beats = look.drop_in().zip(structure).map(|(seconds, (period, _))| (seconds / period).ceil().clamp(0.0, 254.0) as u8);
        shared.drop_beats.store(drop_beats.unwrap_or(NO_BUILD), Ordering::Relaxed);
        shared.active_motion.store(look.active_motion().index(), Ordering::Relaxed);
        shared.active_spread.store(look.active_spread().index(), Ordering::Relaxed);
        last = now;
        if rt.block_on(stream.send(&frame)).is_err() {
            reconnect(rt, stream, shared);
        }
        if let Action::Flash { reactive } = action {
            shared.flashes.fetch_add(1, Ordering::Relaxed);
            if reactive {
                shared.record_reaction(Instant::now().saturating_duration_since(features.last_kick()));
            }
        }
        next_frame = now + FRAME_PERIOD;
    }
    Ok(())
}

/// Transmet la force d'attaque au suivi du tempo et réestime le tempo une fois par seconde, hors
/// de la boucle d'éclairage.
pub(super) fn tempo_loop(shared: &Shared, features: &Features) {
    let mut next = Instant::now() + TEMPO_UPDATE;
    let mut seen = 0;
    while !shared.stopped() {
        thread::sleep(Duration::from_millis(50));
        {
            let mut tracker = shared.tracker();
            features.read_onsets(&mut seen, |t, strength| tracker.on_onset(t, strength));
        }
        let now = Instant::now();
        if now >= next {
            shared.tracker().update(features.seconds(now));
            next = now + TEMPO_UPDATE;
        }
    }
}

/// Suit la session Ableton Link et impose son tempo et sa phase à l'horloge.
pub(super) fn link_loop(shared: &Shared, features: &Features, bpm: Option<f64>) {
    let mut session = LinkSession::join(bpm);
    while !shared.stopped() {
        let beat = session.read(features.seconds(Instant::now()));
        shared.tracker().set_link(beat.bpm, beat.beat_time);
        shared.link_peers.store(beat.peers, Ordering::Relaxed);
        thread::sleep(LINK_POLL);
    }
}

/// Les lampes du live : tous les canaux de la zone, la lampe de chacun, et leur place dans la pièce
/// (dans le même ordre).
pub(super) struct Rig {
    pub(super) channels: Vec<u8>,
    pub(super) owners: Vec<Option<String>>,
    pub(super) space: Space,
}

/// Flash du prochain kick prévu par l'OPUS, quand son heure moins l'avance des lampes est venue :
/// une seule fois par temps, et seulement une fois la phase assez sûre (jamais à côté du kick).
fn opus_action(
    playing: &Playing,
    guide: &opus::Guide,
    now: f64,
    advance: f64,
    flashed: &mut Option<(u8, u32, u32)>,
) -> (Action, Option<Accent>) {
    let Some((beat, at, accent)) = guide.next_kick else { return (Action::None, None) };
    let key = (playing.deck, playing.track.id, beat);
    if !playing.settled || *flashed == Some(key) || now < at - advance || now > at + OPUS_LATE {
        return (Action::None, None);
    }
    *flashed = Some(key);
    (Action::Flash { reactive: false }, Some(accent))
}

/// Appel de montée prévu par l'OPUS (roll, charleston, son acid) dont l'heure moins l'avance des
/// lampes est venue : son attaque, une seule fois par double croche, phase calée.
fn opus_call(
    playing: &Playing,
    guide: &opus::Guide,
    now: f64,
    advance: f64,
    called: &mut Option<(u8, u32, u32, u8)>,
) -> Option<f32> {
    let (beat, step, at, attack) = guide.next_call?;
    let key = (playing.deck, playing.track.id, beat, step);
    if !playing.settled || *called == Some(key) || now < at - advance || now > at + OPUS_LATE {
        return None;
    }
    *called = Some(key);
    Some(attack)
}

/// Publie, pour l'affichage, où en est la platine qui mène.
fn publish_opus(shared: &Shared, playing: &Playing, guide: &opus::Guide, now: Instant) {
    let phrase = guide.phrase.as_ref();
    *lock(&shared.opus_now) = Some(OpusNow {
        deck: playing.deck,
        track: playing.track.id,
        data: playing.track.clone(),
        position_ms: guide.position_ms,
        beat: guide.beat,
        bar_beat: guide.bar_beat,
        phrase_beat: guide.phrase_beat,
        phrase: phrase.map(|p| p.label.clone()),
        next: phrase.and_then(|p| p.next.clone()),
        beats_left: phrase.map(|p| p.end.saturating_sub(guide.beat)),
        settled: playing.settled,
        at: now,
    });
}

/// Suit l'OPUS-QUAD par le réseau : se présente comme rekordbox (dès le départ sur `interface`, la
/// carte réseau choisie d'avance), digère ses paquets, et publie la platine qui mène à chaque
/// paquet (au moins toutes les 20 ms).
pub(super) fn opus_loop(shared: &Shared, features: &Features, interface: Option<&str>) {
    let sockets = [prolink::STATUS_PORT, prolink::ANNOUNCE_PORT].map(bind_opus_port);
    let [Ok(status), Ok(announce)] = sockets else {
        let reason = sockets.into_iter().find_map(Result::err).unwrap_or_default();
        *lock(&shared.opus_error) = Some(reason);
        return;
    };
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for socket in [&status, &announce] {
            let tx = tx.clone();
            scope.spawn(move || receive_opus(socket, &tx, shared));
        }
        drop(tx);
        let mut client = Client::new(opus::interfaces());
        if let Some(label) = interface {
            client.prefer(label);
        }
        let mut out = Vec::new();
        while !shared.stopped() {
            match rx.recv_timeout(OPUS_POLL) {
                Ok((at, port, from, data)) => client.on_packet(features.seconds(at), port, from, &data, &mut out),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let now = features.seconds(Instant::now());
            client.tick(now, &mut out);
            for (to, data) in out.drain(..) {
                let _ = status.send_to(&data, to);
            }
            *lock(&shared.opus_playing) = client.playing(now);
            *lock(&shared.opus_link) = client.state(now);
        }
    });
}

/// Ouvre un port de la liaison sur toutes les cartes réseau.
fn bind_opus_port(port: u16) -> std::result::Result<UdpSocket, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))
        .map_err(|e| format!("port UDP {port} déjà pris (rekordbox ou la sonde ouverts ?) : {e}"))?;
    socket.set_broadcast(true).map_err(|e| e.to_string())?;
    socket.set_read_timeout(Some(OPUS_POLL)).map_err(|e| e.to_string())?;
    Ok(socket)
}

/// Paquet reçu de l'OPUS : arrivée, port local, adresse d'origine, contenu.
type OpusPacket = (Instant, u16, Ipv4Addr, Vec<u8>);

fn receive_opus(socket: &UdpSocket, tx: &mpsc::Sender<OpusPacket>, shared: &Shared) {
    let port = socket.local_addr().map_or(0, |a| a.port());
    let mut buf = [0u8; 2048];
    while !shared.stopped() {
        match socket.recv_from(&mut buf) {
            Ok((len, SocketAddr::V4(from))) => {
                if tx.send((Instant::now(), port, *from.ip(), buf[..len].to_vec())).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            // Délai d'attente écoulé, ou écho ICMP d'un envoi précédent (Windows).
            Err(e) if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::ConnectionReset
            ) => {}
            Err(_) => return,
        }
    }
}

/// Couleur `0x00RRGGBB` en sRGB 8 bits.
pub(super) fn unpack(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

fn pack(color: Rgb) -> u32 {
    u32::from(color.r >> 8) << 16 | u32::from(color.g >> 8) << 8 | u32::from(color.b >> 8)
}


/// Le bridge ne reçoit plus le flux : rouvre-le, de plus en plus patiemment (de 0,5 à 5 s
/// entre deux tentatives), jusqu'à ce qu'il réponde ou que le moteur s'arrête.
fn reconnect(rt: &Runtime, stream: &mut HueStream, shared: &Shared) {
    shared.bridge_lost.store(true, Ordering::Relaxed);
    let mut wait = BRIDGE_RETRY.0;
    while !shared.stopped() {
        if rt.block_on(stream.reopen()).is_ok() {
            shared.bridge_restarts.fetch_add(1, Ordering::Relaxed);
            shared.bridge_lost.store(false, Ordering::Relaxed);
            return;
        }
        let until = Instant::now() + wait;
        while !shared.stopped() && Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
        wait = (wait * 2).min(BRIDGE_RETRY.1);
    }
}

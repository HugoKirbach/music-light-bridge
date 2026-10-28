//! Boucles des fils du moteur : éclairage (60 Hz), tempo, Ableton Link.

use super::delays::LampDelays;
use super::*;

/// Durée d'un flash du test de synchro, et sa cadence sans tempo connu.
const SYNC_FLASH: f64 = 0.08;
const SYNC_PERIOD: f64 = 0.5;
const WHITE: Rgb = Rgb { r: u16::MAX, g: u16::MAX, b: u16::MAX };

/// Flashe selon le planificateur (vrais kicks, anticipés quand ils sont attendus) et sur chaque
/// note MIDI, et entretient le flux à 60 Hz entre deux flashs.
pub(super) fn light_loop(
    rt: &Runtime,
    stream: &mut HueStream,
    // Canaux pilotés, et leur place dans la pièce (dans le même ordre).
    (channels, space): (&[u8], &Space),
    features: &Features,
    notes: &Notes,
    shared: &Shared,
    advance: Duration,
) -> Result<()> {
    let mut planner = FlashPlanner::new(shared.mode(), advance.as_secs_f64(), CONFIRM_RANGE.1);
    let mut look = ReactiveLook::new(shared.palette());
    look.set_space(space);
    let mut frame: Vec<(u8, Rgb)> = channels.iter().map(|&id| (id, Rgb::OFF)).collect();
    let mut seen = features.kicks();
    let mut seen_notes = notes.count();
    let mut last = Instant::now();
    let mut next_frame = last;
    let mut delays = LampDelays::default();
    let mut lags: Vec<f32> = Vec::new();
    while !shared.stopped() {
        planner.mode = shared.mode();
        // La lampe la plus lente reçoit ses flashs d'autant plus tôt ; les autres l'attendent.
        lags.clear();
        lags.extend(shared.lamp_lags.iter().map(|lag| f32::from_bits(lag.load(Ordering::Relaxed))));
        let head_start = Duration::from_secs_f32(lags.iter().copied().fold(0.0, f32::max) / 1000.0);
        let total_advance = advance + head_start;
        planner.advance = total_advance.as_secs_f64();
        // Le kick d'un flash anticipé arrive après l'âge d'un bloc audio : on l'attend jusque-là.
        planner.confirm_window = (features.input_latency().as_secs_f64() + CONFIRM_MARGIN)
            .clamp(CONFIRM_RANGE.0, CONFIRM_RANGE.1);
        let rhythm = shared.tracker().rhythm();
        let deadline = match planner.next_event(rhythm.as_ref(), features.seconds(Instant::now())) {
            Some(event) => next_frame.min(features.instant(event)),
            None => next_frame,
        };
        timing::sleep_until_or(deadline, || features.kicks() != seen || notes.count() != seen_notes);

        let kicks = features.kicks();
        // Hors groove, un coup sans grave (charleston, clap, mélodie) n'est pas un kick : il ne flashe
        // pas et ne compte ni pour le tempo ni pour le motif.
        let kick = (kicks != seen && look.carries_bass(features.levels().raw_bass))
            .then(|| features.seconds(features.last_kick()) - KICK_DETECTION_DELAY);
        seen = kicks;
        if let Some(at) = kick {
            shared.tracker().on_kick(at);
            look.kick_detected(features.levels().raw_bass);
        }
        let note_count = notes.count();
        let note = (note_count != seen_notes).then(|| notes.last());
        seen_notes = note_count;

        let now = Instant::now();
        let (rhythm, locked, structure) = {
            let tracker = shared.tracker();
            (tracker.rhythm(), tracker.locked(features.seconds(now)), tracker.structure(features.seconds(now)))
        };
        let action = planner.poll(rhythm.as_ref(), features.seconds(now), kick);
        // La détection cherche les kicks là où le motif les attend dès que le tempo est sûr, et se
        // méfie de ce qui tombe ailleurs tant que le groove tient.
        let grooving = planner.in_groove(rhythm.as_ref(), features.seconds(now));
        features.set_grid(Grid::of(rhythm.as_ref(), locked, grooving));
        shared.grooving.store(grooving, Ordering::Relaxed);
        look.set_grid_groove(rhythm.map(|_| grooving));
        look.set_structure(structure);
        look.set_drop_advance(total_advance.as_secs_f32());
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
        let mut colors = look.render_lamps(now - last, features.levels()).to_vec();
        if shared.sync_test.load(Ordering::Relaxed) {
            // Blanc sur chaque temps, tel qu'il sonnera quand les lampes montreront l'image.
            let at = features.seconds(now) + total_advance.as_secs_f64();
            let (period, anchor) = rhythm.map_or((SYNC_PERIOD, 0.0), |r| (r.period, r.bar));
            let lit = (at - anchor).rem_euclid(period) < SYNC_FLASH;
            colors.iter_mut().for_each(|c| *c = if lit { WHITE } else { Rgb::OFF });
        }
        let colors = delays.delay(now, colors, &lags);
        for ((_, c), (color, published)) in frame.iter_mut().zip(colors.iter().zip(&shared.lamp_colors)) {
            *c = *color;
            published.store(pack(*color), Ordering::Relaxed);
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

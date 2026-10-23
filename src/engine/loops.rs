//! Boucles des fils du moteur : éclairage (60 Hz), tempo, Ableton Link.

use super::*;

/// Flashe selon le planificateur (vrais kicks, anticipés quand ils sont attendus) et sur chaque
/// note MIDI, et entretient le flux à 60 Hz entre deux flashs.
pub(super) fn light_loop(
    rt: &Runtime,
    stream: &mut HueStream,
    lamps: &[(u8, f32)],
    features: &Features,
    notes: &Notes,
    shared: &Shared,
    advance: Duration,
) -> Result<()> {
    let mut planner = FlashPlanner::new(shared.mode(), advance.as_secs_f64(), CONFIRM_RANGE.1);
    let mut look = ReactiveLook::new(shared.palette());
    look.set_lamps(&lamps.iter().map(|&(_, x)| x).collect::<Vec<f32>>());
    let mut frame: Vec<(u8, Rgb)> = lamps.iter().map(|&(id, _)| (id, Rgb::OFF)).collect();
    let mut seen = features.kicks();
    let mut seen_notes = notes.count();
    let mut last = Instant::now();
    let mut next_frame = last;
    while !shared.stopped() {
        planner.mode = shared.mode();
        // Le kick d'un flash anticipé arrive après l'âge d'un bloc audio : on l'attend jusque-là.
        planner.confirm_window = (features.input_latency().as_secs_f64() + CONFIRM_MARGIN)
            .clamp(CONFIRM_RANGE.0, CONFIRM_RANGE.1);
        let clock = shared.tracker().clock();
        let deadline = match planner.next_event(&clock, features.seconds(Instant::now())) {
            Some(event) => next_frame.min(features.instant(event)),
            None => next_frame,
        };
        timing::sleep_until_or(deadline, || features.kicks() != seen || notes.count() != seen_notes);

        let kicks = features.kicks();
        let kick = (kicks != seen).then(|| features.seconds(features.last_kick()) - KICK_DETECTION_DELAY);
        seen = kicks;
        if let Some(at) = kick {
            shared.tracker().on_kick(at);
            look.kick_detected();
        }
        let note_count = notes.count();
        let note = (note_count != seen_notes).then(|| notes.last());
        seen_notes = note_count;

        let now = Instant::now();
        let (clock, locked) = {
            let tracker = shared.tracker();
            (tracker.clock(), tracker.locked(features.seconds(now)))
        };
        let action = planner.poll(&clock, features.seconds(now), kick);
        // La détection cherche les kicks sur les temps dès que le tempo est sûr, et se méfie de
        // ce qui tombe entre eux tant que le groove tient.
        features.set_grid(Grid {
            period: clock.period().unwrap_or(0.0),
            anchor: clock.anchor(),
            locked,
            groove: planner.in_groove(&clock, features.seconds(now)),
        });
        shared.grooving.store(planner.in_groove(&clock, features.seconds(now)), Ordering::Relaxed);
        match action {
            Action::Flash { reactive } => {
                // Place du temps flashé dans la structure : tête de phrase ou de mesure.
                let at = if reactive { kick.unwrap_or(features.seconds(now)) } else { features.seconds(now) + advance.as_secs_f64() };
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
        if let Some((pitch, velocity)) = note {
            look.note(note_hue(pitch), velocity);
            shared.note_flashes.fetch_add(1, Ordering::Relaxed);
            shared.last_note.store(features.seconds(now).to_bits(), Ordering::Relaxed);
        }
        look.palette = shared.palette();
        look.dynamics = shared.dynamics();
        look.motion = Motion::from_index(shared.motion.load(Ordering::Relaxed));
        look.spread = Spread::from_index(shared.spread.load(Ordering::Relaxed));
        let colors = look.render_lamps(now - last, features.levels());
        for ((_, c), (color, published)) in frame.iter_mut().zip(colors.iter().zip(&shared.lamp_colors)) {
            *c = *color;
            published.store(pack(*color), Ordering::Relaxed);
        }
        // Couleur résumée (aperçu, fond de la scène) : la plus lumineuse des lampes.
        let brightest = colors.iter().copied().max_by_key(|c| c.r.max(c.g).max(c.b)).unwrap_or(Rgb::OFF);
        shared.color.store(pack(brightest), Ordering::Relaxed);
        shared.phase.store(look.phase().index(), Ordering::Relaxed);
        shared.active_motion.store(look.active_motion().index(), Ordering::Relaxed);
        shared.active_spread.store(look.active_spread().index(), Ordering::Relaxed);
        last = now;
        rt.block_on(stream.send(&frame))?;
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


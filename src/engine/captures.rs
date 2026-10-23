//! Captures audio : enregistrement à la demande, et captures automatiques quand le groove
//! décroche dans un passage fort.

use super::*;

/// Écrit les dernières secondes de l'entrée audio dans le dossier des captures, avec une fiche
/// JSON du même nom (contexte de la capture, pour l'annoter au banc d'essai).
pub(super) fn write_capture(capture: &Mutex<Capture>, sheet: &serde_json::Value) -> Result<PathBuf> {
    let (rate, samples) = {
        let capture = capture.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (capture.rate(), capture.snapshot())
    };
    let dir = Config::captures_dir()?;
    fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let path = (0..)
        .map(|n| dir.join(if n == 0 { format!("capture-{stamp}.wav") } else { format!("capture-{stamp}-{n}.wav") }))
        .find(|path| !path.exists())
        .context("nom de capture introuvable")?;
    wav::write_mono_f32(&path, rate, &samples)?;
    fs::write(path.with_extension("json"), serde_json::to_string_pretty(sheet)?)?;
    Ok(path)
}

/// Décide des captures automatiques : quand le groove décroche alors que les basses restent aussi
/// fortes qu'avant, ce n'est pas un break, la détection a perdu les kicks. Les instants sont en
/// secondes.
#[derive(Debug, Default)]
struct AutoTrigger {
    grooving_since: Option<f64>,
    /// Basses brutes pendant le groove (moyenne glissante).
    groove_bass: f32,
    /// Perte du groove : instant, puis cumul et nombre des niveaux de basse depuis.
    lost: Option<(f64, f32, u32)>,
    last_saved: Option<f64>,
    saved: u32,
}

impl AutoTrigger {
    /// Une mesure ; rend le temps écoulé depuis la perte du groove quand il faut capturer.
    fn step(&mut self, now: f64, dt: f32, grooving: bool, bass: f32) -> Option<f64> {
        if grooving {
            self.grooving_since.get_or_insert(now);
            self.groove_bass += (bass - self.groove_bass) * (dt / AUTO_BASS_MEMORY).min(1.0);
            self.lost = None;
            return None;
        }
        if let Some(since) = self.grooving_since.take()
            && now - since >= AUTO_MIN_GROOVE.as_secs_f64()
        {
            self.lost = Some((now, 0.0, 0));
        }
        let (at, sum, count) = self.lost.as_mut()?;
        *sum += bass;
        *count += 1;
        let after = now - *at;
        if after < AUTO_AFTER.as_secs_f64() {
            return None;
        }
        let loud = *sum / *count as f32 >= AUTO_LOUD * self.groove_bass;
        let spaced = self.last_saved.is_none_or(|saved| now - saved >= AUTO_SPACING.as_secs_f64());
        self.lost = None;
        (loud && spaced && self.saved < AUTO_MAX).then(|| {
            self.last_saved = Some(now);
            self.saved += 1;
            after
        })
    }
}

/// Enregistre de lui-même les dernières secondes quand le groove décroche dans un passage fort
/// (voir `AutoTrigger`). La capture, prise 8 s après, couvre l'avant et l'après.
pub(super) fn auto_capture_loop(shared: &Shared, features: &Features, capture: &Mutex<Capture>) {
    let mut trigger = AutoTrigger::default();
    let start = Instant::now();
    let dt = AUTO_POLL.as_secs_f32();
    while !shared.stopped() {
        thread::sleep(AUTO_POLL);
        let now = start.elapsed().as_secs_f64();
        let grooving = shared.grooving.load(Ordering::Relaxed);
        let Some(after) = trigger.step(now, dt, grooving, features.levels().raw_bass) else { continue };
        let sheet = json!({
            "auto": true,
            "raison": "groove perdu alors que les basses restent fortes",
            "groove_perdu_a": f64::from(CAPTURE_SECONDS) - after,
            "bpm": shared.tracker().clock().bpm(),
        });
        if write_capture(capture, &sheet).is_ok() {
            shared.auto_captures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fait tourner le déclencheur toutes les 250 ms : (durée, groove, basses) par étape.
    fn run(steps: &[(f64, bool, f32)]) -> Vec<f64> {
        let mut trigger = AutoTrigger::default();
        let (mut now, mut fired) = (0.0, Vec::new());
        for &(duration, grooving, bass) in steps {
            let end = now + duration;
            while now < end {
                now += 0.25;
                if trigger.step(now, 0.25, grooving, bass).is_some() {
                    fired.push(now);
                }
            }
        }
        fired
    }

    #[test]
    fn a_lost_groove_in_a_loud_passage_is_captured_after_eight_seconds() {
        let fired = run(&[(20.0, true, 1.0), (12.0, false, 0.9)]);
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert!((fired[0] - 28.0).abs() < 0.3, "{fired:?}");
    }

    #[test]
    fn a_break_a_short_groove_or_a_quick_recovery_is_not_captured() {
        assert!(run(&[(20.0, true, 1.0), (12.0, false, 0.1)]).is_empty(), "break : les basses tombent");
        assert!(run(&[(5.0, true, 1.0), (12.0, false, 0.9)]).is_empty(), "groove trop court");
        assert!(run(&[(20.0, true, 1.0), (4.0, false, 0.9), (10.0, true, 1.0)]).is_empty(), "groove revenu");
    }

    #[test]
    fn captures_are_spaced_by_two_minutes() {
        let lose = [(20.0, true, 1.0), (12.0, false, 0.9)];
        let fired = run(&[lose, lose, lose, lose, lose].concat());
        assert_eq!(fired.len(), 2, "{fired:?}");
        assert!(fired[1] - fired[0] >= 120.0);
    }
}

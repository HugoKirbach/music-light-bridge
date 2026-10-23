//! Retard propre de chaque lampe : toutes les images partent avec l'avance de la plus lente, et
//! les plus rapides rejouent l'image d'il y a un instant pour s'allumer en même temps qu'elle.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::hue::stream::Rgb;

/// Images gardées au plus : de quoi rattraper le plus grand écart de retard.
const MEMORY: Duration = Duration::from_millis(400);

#[derive(Default)]
pub(super) struct LampDelays {
    history: VecDeque<(Instant, Vec<Rgb>)>,
}

impl LampDelays {
    /// Enregistre l'image du moment et rend celle que chaque canal doit montrer : son image d'il
    /// y a `max - lag` (le canal le plus lent montre l'image courante).
    pub(super) fn delay(&mut self, now: Instant, colors: Vec<Rgb>, lags_ms: &[f32]) -> Vec<Rgb> {
        let max = lags_ms.iter().copied().fold(0.0f32, f32::max);
        if max <= 0.0 {
            self.history.clear();
            return colors;
        }
        self.history.push_back((now, colors));
        while self.history.front().is_some_and(|&(t, _)| now.saturating_duration_since(t) > MEMORY) {
            self.history.pop_front();
        }
        let current = &self.history.back().expect("vient d'être ajoutée").1;
        (0..current.len())
            .map(|i| {
                let wait = Duration::from_micros(((max - lags_ms.get(i).copied().unwrap_or(0.0)).max(0.0) * 1000.0).round() as u64);
                // La plus récente des images assez anciennes, ou la plus vieille gardée.
                let shown = self
                    .history
                    .iter()
                    .rev()
                    .find(|(t, _)| now.saturating_duration_since(*t) >= wait)
                    .or(self.history.front())
                    .map(|(_, image)| image);
                shown.and_then(|image| image.get(i).copied()).unwrap_or(current[i])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb = Rgb { r: u16::MAX, g: u16::MAX, b: u16::MAX };

    #[test]
    fn without_lags_frames_pass_untouched() {
        let mut delays = LampDelays::default();
        let now = Instant::now();
        assert_eq!(delays.delay(now, vec![WHITE, Rgb::OFF], &[0.0, 0.0]), vec![WHITE, Rgb::OFF]);
    }

    #[test]
    fn the_fast_lamp_lights_up_when_the_slow_one_has_had_its_head_start() {
        let mut delays = LampDelays::default();
        let start = Instant::now();
        // La lampe 0 est lente de 50 ms : la lampe 1 attend 50 ms avant de montrer le flash.
        let lags = [50.0, 0.0];
        let frame = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(delays.delay(frame(0), vec![Rgb::OFF, Rgb::OFF], &lags), vec![Rgb::OFF, Rgb::OFF]);
        assert_eq!(delays.delay(frame(16), vec![WHITE, WHITE], &lags), vec![WHITE, Rgb::OFF]);
        assert_eq!(delays.delay(frame(48), vec![WHITE, WHITE], &lags), vec![WHITE, Rgb::OFF]);
        assert_eq!(delays.delay(frame(66), vec![Rgb::OFF, Rgb::OFF], &lags), vec![Rgb::OFF, WHITE]);
        assert_eq!(delays.delay(frame(120), vec![Rgb::OFF, Rgb::OFF], &lags), vec![Rgb::OFF, Rgb::OFF]);
    }
}

//! Attente précise : Windows ne réveille un thread qu'à la granularité de son minuteur,
//! donc on dort jusqu'à un peu avant l'échéance puis on finit en attente active.

use std::time::{Duration, Instant};

const SPIN_MARGIN: Duration = Duration::from_micros(1500);

pub fn sleep_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > SPIN_MARGIN {
            std::thread::sleep(left - SPIN_MARGIN);
        } else {
            std::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mesure de précision sur la machine : `cargo test --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn wake_up_lateness_at_60_hz() {
        let period = Duration::from_micros(16_667);
        let mut next = Instant::now() + period;
        let (mut max, mut total) = (Duration::ZERO, Duration::ZERO);
        let frames = 300;
        for _ in 0..frames {
            sleep_until(next);
            let late = Instant::now() - next;
            max = max.max(late);
            total += late;
            next += period;
        }
        println!("retard de réveil : moyen {:?}, max {:?}", total / frames, max);
        assert!(max < Duration::from_millis(2), "retard max {max:?}");
    }
}

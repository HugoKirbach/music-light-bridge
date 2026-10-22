//! Attente précise : Windows ne réveille un thread qu'à la granularité de son minuteur,
//! donc on dort jusqu'à un peu avant l'échéance puis on finit en attente active.

use std::time::{Duration, Instant};

const SPIN_MARGIN: Duration = Duration::from_micros(1500);
/// Intervalle de vérification de `sleep_until_or` : borne le délai de réaction à un
/// événement (un kick) sans occuper un cœur du processeur en continu.
const POLL: Duration = Duration::from_micros(500);

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

/// Comme `sleep_until`, mais rend la main dès que `ready()` devient vrai.
pub fn sleep_until_or(deadline: Instant, mut ready: impl FnMut() -> bool) {
    loop {
        if ready() {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > SPIN_MARGIN {
            std::thread::sleep(POLL.min(left - SPIN_MARGIN));
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

    /// Délai entre un événement signalé par un autre thread et le réveil de l'attente.
    #[test]
    #[ignore]
    fn reacts_quickly_to_an_event() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = AtomicBool::new(false);
        let rounds = 50;
        let (mut max, mut total) = (Duration::ZERO, Duration::ZERO);
        for _ in 0..rounds {
            flag.store(false, Ordering::SeqCst);
            let (set_at, woke) = std::thread::scope(|scope| {
                let setter = scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(7));
                    let at = Instant::now();
                    flag.store(true, Ordering::SeqCst);
                    at
                });
                sleep_until_or(Instant::now() + Duration::from_millis(50), || flag.load(Ordering::SeqCst));
                let woke = Instant::now();
                (setter.join().unwrap(), woke)
            });
            let reaction = woke.saturating_duration_since(set_at);
            max = max.max(reaction);
            total += reaction;
        }
        println!("réaction à un événement : moyenne {:?}, max {:?}", total / rounds, max);
        assert!(max < Duration::from_millis(2), "réaction max {max:?}");
    }
}

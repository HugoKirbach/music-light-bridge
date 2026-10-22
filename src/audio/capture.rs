//! Mémoire tournante des dernières secondes de l'entrée audio (mono), pour enregistrer un
//! passage après coup et le rejouer avec `dj-bridge analyze`.

pub struct Capture {
    samples: Vec<f32>,
    next: usize,
    full: bool,
    rate: u32,
}

impl Capture {
    /// Alloue toute la mémoire d'emblée : le fil audio ne doit jamais allouer.
    pub fn new(rate: u32, seconds: u32) -> Self {
        Self { samples: vec![0.0; (rate * seconds) as usize], next: 0, full: false, rate }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    #[inline]
    pub fn push(&mut self, sample: f32) {
        self.samples[self.next] = sample;
        self.next += 1;
        if self.next == self.samples.len() {
            self.next = 0;
            self.full = true;
        }
    }

    /// Échantillons enregistrés, du plus ancien au plus récent.
    pub fn snapshot(&self) -> Vec<f32> {
        if self.full {
            [&self.samples[self.next..], &self.samples[..self.next]].concat()
        } else {
            self.samples[..self.next].to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_most_recent_samples_in_order() {
        let mut capture = Capture::new(4, 1);
        for x in 0..3 {
            capture.push(x as f32);
        }
        assert_eq!(capture.snapshot(), [0.0, 1.0, 2.0]);
        for x in 3..7 {
            capture.push(x as f32);
        }
        assert_eq!(capture.snapshot(), [3.0, 4.0, 5.0, 6.0]);
    }
}

use std::time::Instant;

/// An exponentially decaying event rate: `add(amount)` counts events (or tokens), `per_sec` reads the rate.
#[derive(Clone, Copy, Debug)]
pub struct DecayingRate {
    per_sec: f64,
    at: Instant,
}

impl DecayingRate {
    pub fn new(now: Instant) -> Self {
        Self { per_sec: 0.0, at: now }
    }

    pub fn per_sec(&self, now: Instant, tau_secs: f64) -> f64 {
        self.per_sec * (-now.saturating_duration_since(self.at).as_secs_f64() / tau_secs).exp()
    }

    pub fn add(&mut self, amount: f64, now: Instant, tau_secs: f64) {
        self.per_sec = self.per_sec(now, tau_secs) + amount / tau_secs;
        self.at = now;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn converges_to_the_steady_rate_and_decays() {
        let start = Instant::now();
        let mut rate = DecayingRate::new(start);
        for i in 0..1_000u64 {
            rate.add(10.0, start + Duration::from_millis(i * 100), 5.0);
        }
        let end = start + Duration::from_millis(99_900);
        assert!((rate.per_sec(end, 5.0) - 100.0).abs() < 5.0, "10 units every 100 ms");
        assert!(rate.per_sec(end + Duration::from_secs(20), 5.0) < 5.0);
    }
}

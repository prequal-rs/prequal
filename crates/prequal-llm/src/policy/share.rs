use std::sync::Mutex;

use super::Candidate;

/// Moving average over picks of this router's share of the fleet's requests in flight: its own active requests
/// over the engines' running + waiting counts. Near 1 when this router is the fleet's only one.
#[derive(Debug, Default)]
pub struct RouterShare(Mutex<(f64, u32)>);

/// Weight of each pick (with any load) in the moving average, once the first `1 / ALPHA` have set a plain mean.
const ALPHA: f64 = 0.01;

impl RouterShare {
    /// Folds in the fleet's current state and returns the updated share (1 until any load is seen).
    pub fn observe(&self, candidates: &[Candidate]) -> f64 {
        let own: f64 = candidates.iter().map(|c| f64::from(c.replica.active)).sum();
        let total: f64 = candidates.iter().map(|c| c.replica.in_flight()).sum();
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let (share, samples) = &mut *guard;
        if total >= 1.0 {
            *samples = samples.saturating_add(1);
            *share += (1.0 / f64::from(*samples)).max(ALPHA) * (own / total - *share);
        }
        if *samples == 0 { 1.0 } else { *share }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::EngineStats, fleet::Replica};

    fn observe(share: &RouterShare, own: u32, engine_in_flight: f64) -> f64 {
        let mut replica = Replica::new("10.0.0.1:8000".parse().unwrap());
        replica.active = own;
        replica.stats = Some(EngineStats { running: engine_in_flight, ..EngineStats::default() });
        share.observe(&[Candidate { replica: &replica, matched_blocks: 0 }])
    }

    #[test]
    fn tracks_the_fraction_of_fleet_load_this_router_sent() {
        let sole = RouterShare::default();
        assert_eq!(observe(&sole, 0, 0.0), 1.0, "no load yet: assume the only router");
        (0..50).for_each(|_| _ = observe(&sole, 4, 4.0));
        assert!(observe(&sole, 4, 3.0) > 0.99, "stale scrapes below own count still read as sole");
        let one_of_two = RouterShare::default();
        (0..50).for_each(|_| _ = observe(&one_of_two, 4, 8.0));
        assert!((observe(&one_of_two, 4, 8.0) - 0.5).abs() < 0.01);
    }
}

use std::{collections::HashMap, net::SocketAddr, time::Instant};

use crate::rate::DecayingRate;

/// Decaying request rates per prompt key (see [`Request::key`](crate::policy::Request::key)) and for the whole
/// fleet, so a policy can tell a prefix that carries more than one replica's share of traffic from an ordinary one;
/// and where each key's latest request went, which outlives the prefix index's memory of it.
#[derive(Debug)]
pub struct HeatTracker {
    tau_secs: f64,
    heads: HashMap<u64, Head>,
    total: DecayingRate,
}

#[derive(Debug)]
struct Head {
    rate: DecayingRate,
    home: Option<SocketAddr>,
}

/// A prompt key's traffic as of its latest request.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeadStats {
    /// Its share of all recent requests (0 to 1).
    pub share: f64,
    /// Where its previous request went.
    pub home: Option<SocketAddr>,
}

/// Hard cap on tracked heads. A prune leaves at most half of it, so prunes (O(cap)) come at most once per
/// `MAX_HEADS / 2` new heads: amortized O(1) per request whatever heads clients send.
const MAX_HEADS: usize = 65_536;

impl HeatTracker {
    pub fn new(tau_secs: f64) -> Self {
        Self { tau_secs, heads: HashMap::new(), total: DecayingRate::new(Instant::now()) }
    }

    /// Counts one request for `head`: its current rate as a fraction of the whole fleet's, and its last home.
    pub fn record(&mut self, head: u64, now: Instant) -> HeadStats {
        let tau = self.tau_secs;
        self.total.add(1.0, now, tau);
        if self.heads.len() >= MAX_HEADS && !self.heads.contains_key(&head) {
            self.prune(now);
        }
        let entry = self.heads.entry(head).or_insert_with(|| Head { rate: DecayingRate::new(now), home: None });
        entry.rate.add(1.0, now, tau);
        let share = entry.rate.per_sec(now, tau) / self.total.per_sec(now, tau).max(f64::MIN_POSITIVE);
        HeadStats { share, home: entry.home }
    }

    /// Records where `head`'s latest request went (ignored if it isn't tracked).
    pub fn set_home(&mut self, head: u64, addr: SocketAddr) {
        if let Some(entry) = self.heads.get_mut(&head) {
            entry.home = Some(addr);
        }
    }

    /// Drops heads that have gone cold, then, if more than half the cap remain, all but the hottest half.
    fn prune(&mut self, now: Instant) {
        let tau = self.tau_secs;
        self.heads.retain(|_, head| head.rate.per_sec(now, tau) > 0.01 / tau);
        let keep = MAX_HEADS / 2;
        if self.heads.len() <= keep {
            return;
        }
        let mut rates: Vec<f64> = self.heads.values().map(|h| h.rate.per_sec(now, tau)).collect();
        let cut = rates.len() - keep;
        let (_, &mut cutoff, _) = rates.select_nth_unstable_by(cut, f64::total_cmp);
        // Heads exactly at the cutoff fill the remaining room, so ties neither overfill nor empty the map.
        let mut at_cutoff = keep - self.heads.values().filter(|h| h.rate.per_sec(now, tau) > cutoff).count();
        self.heads.retain(|_, head| {
            let r = head.rate.per_sec(now, tau);
            if r == cutoff && at_cutoff > 0 {
                at_cutoff -= 1;
                return true;
            }
            r > cutoff
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_dominant_prefix_has_a_large_share() {
        let mut heat = HeatTracker::new(10.0);
        let start = Instant::now();
        let mut last = 0.0;
        for i in 0..400u64 {
            let now = start + Duration::from_millis(i * 25);
            let head = if i % 2 == 0 { 7 } else { 1000 + i };
            let share = heat.record(head, now).share;
            if head == 7 {
                last = share;
            }
        }
        assert!((0.4..0.6).contains(&last), "half the traffic: {last}");
        assert!(heat.record(99_999, start + Duration::from_secs(10)).share < 0.05, "a one-off prefix is cold");
    }

    #[test]
    fn remembers_where_a_key_last_went() {
        let mut heat = HeatTracker::new(10.0);
        let (now, home) = (Instant::now(), "10.0.0.1:8000".parse().unwrap());
        assert_eq!(heat.record(7, now).home, None);
        heat.set_home(7, home);
        heat.set_home(8, home);
        assert_eq!(heat.record(7, now).home, Some(home));
        assert_eq!(heat.record(8, now).home, None, "untracked keys keep no home");
    }

    #[test]
    fn distinct_hot_heads_stay_capped_and_keep_the_hottest() {
        let mut heat = HeatTracker::new(10.0);
        let now = Instant::now();
        for _ in 0..10 {
            heat.record(7, now);
        }
        let mut prunes = 0;
        for head in 1_000..1_000 + 4 * MAX_HEADS as u64 {
            let before = heat.heads.len();
            heat.record(head, now);
            prunes += usize::from(heat.heads.len() < before);
            assert!(heat.heads.len() <= MAX_HEADS);
        }
        assert!(prunes <= 8, "pruning is amortized: {prunes}");
        assert!(heat.heads.contains_key(&7), "the hottest head survives every prune");
    }
}

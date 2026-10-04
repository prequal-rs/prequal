use std::sync::Mutex;

use crate::LatencyEstimator;

const PER_BUCKET: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
struct Bucket {
    at_us: [u64; PER_BUCKET],
    latency_us: [u64; PER_BUCKET],
    len: usize,
    next: usize,
}

impl Bucket {
    fn push(&mut self, at_us: u64, latency_us: u64) {
        self.at_us[self.next] = at_us;
        self.latency_us[self.next] = latency_us;
        self.next = (self.next + 1) % PER_BUCKET;
        self.len = (self.len + 1).min(PER_BUCKET);
    }
}

/// The paper's estimator: median latency of recent requests that arrived at RIF within ±1.
/// Samples are bucketed per RIF (the last 16 each; RIFs above `max_rif` share the top bucket),
/// so an estimate reads at most 48 samples and never allocates.
#[derive(Debug)]
pub struct RecentMedian {
    window_us: u64,
    buckets: Mutex<Vec<Bucket>>,
}

impl RecentMedian {
    /// Medians over samples younger than `window_us`, with one bucket per RIF up to `max_rif`.
    #[must_use]
    pub fn new(window_us: u64, max_rif: u32) -> Self {
        Self { window_us, buckets: Mutex::new(vec![Bucket::default(); max_rif as usize + 1]) }
    }
}

impl Default for RecentMedian {
    fn default() -> Self {
        Self::new(1_000_000, 255)
    }
}

impl LatencyEstimator for RecentMedian {
    fn record(&self, rif_at_arrival: u32, latency_us: u64, now_us: u64) {
        let mut buckets = self.buckets.lock().unwrap();
        let top = buckets.len() - 1;
        buckets[(rif_at_arrival as usize).min(top)].push(now_us, latency_us);
    }

    // No fresh samples must report 0, not a stale value: stale highs starve idle replicas forever.
    fn estimate(&self, rif: u32, now_us: u64) -> u64 {
        let mut near = [0u64; 3 * PER_BUCKET];
        let mut n = 0;
        {
            let buckets = self.buckets.lock().unwrap();
            let top = buckets.len() - 1;
            let center = (rif as usize).min(top);
            for bucket in &buckets[center.saturating_sub(1)..=(center + 1).min(top)] {
                for i in 0..bucket.len {
                    if now_us.saturating_sub(bucket.at_us[i]) <= self.window_us {
                        near[n] = bucket.latency_us[i];
                        n += 1;
                    }
                }
            }
        }
        if n == 0 {
            return 0;
        }
        *near[..n].select_nth_unstable(n / 2).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_uses_nearby_fresh_samples_only() {
        let m = RecentMedian::new(100, 16);
        m.record(5, 1_000, 0);
        for (rif, lat) in [(4, 10), (5, 30), (6, 20), (9, 999)] {
            m.record(rif, lat, 150);
        }
        assert_eq!(m.estimate(5, 200), 20);
        assert_eq!(m.estimate(20, 200), 0);
        assert_eq!(m.estimate(5, 10_000), 0);
    }

    #[test]
    fn high_rifs_share_top_bucket() {
        let m = RecentMedian::new(1_000, 4);
        m.record(40, 700, 0);
        assert_eq!(m.estimate(99, 10), 700);
        for i in 0..40 {
            m.record(2, i, 10);
        }
        assert_eq!(m.estimate(2, 10), 32);
    }
}

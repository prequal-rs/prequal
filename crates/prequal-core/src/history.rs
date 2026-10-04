use std::collections::VecDeque;

/// Sliding window of recent probe RIFs, kept sorted alongside so a quantile is a single lookup.
#[derive(Clone, Debug)]
pub(crate) struct RifHistory {
    recent: VecDeque<u32>,
    sorted: Vec<u32>,
    capacity: usize,
}

impl RifHistory {
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self { recent: VecDeque::with_capacity(capacity), sorted: Vec::with_capacity(capacity), capacity }
    }

    pub(crate) fn push(&mut self, rif: u32) {
        if self.recent.len() == self.capacity {
            let evicted = self.recent.pop_front().expect("full window");
            let at = self.sorted.partition_point(|&v| v < evicted);
            self.sorted.remove(at);
        }
        self.recent.push_back(rif);
        let at = self.sorted.partition_point(|&v| v <= rif);
        self.sorted.insert(at, rif);
    }

    pub(crate) fn threshold(&self, q: f64) -> Option<u32> {
        let last = self.sorted.len().checked_sub(1)?;
        Some(self.sorted[(last as f64 * q.clamp(0.0, 1.0)).floor() as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_and_eviction() {
        let mut h = RifHistory::new(5);
        assert_eq!(h.threshold(0.5), None);
        for rif in [9, 1, 2, 3, 4, 5] {
            h.push(rif);
        }
        assert_eq!(h.threshold(0.0), Some(1));
        assert_eq!(h.threshold(0.5), Some(3));
        assert_eq!(h.threshold(1.0), Some(5));
    }

    #[test]
    fn duplicates_evict_one_copy() {
        let mut h = RifHistory::new(3);
        for rif in [2, 2, 7, 2] {
            h.push(rif);
        }
        assert_eq!(h.sorted, [2, 2, 7]);
    }
}

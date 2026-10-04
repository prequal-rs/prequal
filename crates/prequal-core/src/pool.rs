#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub(crate) replica: usize,
    pub(crate) recv_us: u64,
    pub(crate) rif: u32,
    pub(crate) latency_us: u64,
    pub(crate) uses_left: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Pool {
    entries: Vec<Entry>,
    capacity: usize,
}

fn is_hot(e: &Entry, threshold: Option<u32>) -> bool {
    threshold.is_some_and(|t| e.rif > t)
}

impl Pool {
    pub(crate) fn new(capacity: usize) -> Self {
        Self { entries: Vec::with_capacity(capacity + 1), capacity: capacity.max(1) }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Keeps only the newest probe per replica, then evicts the oldest entry if over capacity.
    pub(crate) fn insert(&mut self, entry: Entry) {
        self.entries.retain(|e| e.replica != entry.replica);
        self.entries.push(entry);
        if self.entries.len() > self.capacity {
            self.remove_oldest();
        }
    }

    pub(crate) fn expire(&mut self, now_us: u64, max_age_us: u64) {
        self.entries.retain(|e| now_us.saturating_sub(e.recv_us) <= max_age_us);
    }

    pub(crate) fn remove_replica(&mut self, replica: usize) {
        self.entries.retain(|e| e.replica != replica);
    }

    pub(crate) fn rename_replica(&mut self, from: usize, to: usize) {
        self.entries.iter_mut().filter(|e| e.replica == from).for_each(|e| e.replica = to);
    }

    /// HCL over eligible entries: lowest-latency cold entry, or lowest-RIF entry when all are hot.
    pub(crate) fn select(&self, threshold: Option<u32>, eligible: impl Fn(usize) -> bool) -> Option<usize> {
        let indexed = self.entries.iter().enumerate().filter(|(_, e)| eligible(e.replica));
        let cold = indexed.clone().filter(|(_, e)| !is_hot(e, threshold)).min_by_key(|(_, e)| (e.latency_us, e.rif));
        cold.or_else(|| indexed.min_by_key(|(_, e)| (e.rif, e.latency_us))).map(|(i, _)| i)
    }

    /// Marks an entry as used, bumping its RIF locally so concurrent picks spread out.
    pub(crate) fn consume(&mut self, index: usize) -> usize {
        let e = &mut self.entries[index];
        e.rif = e.rif.saturating_add(1);
        e.uses_left = e.uses_left.saturating_sub(1);
        let replica = e.replica;
        if e.uses_left == 0 {
            self.entries.swap_remove(index);
        }
        replica
    }

    pub(crate) fn remove_oldest(&mut self) {
        if let Some(i) = (0..self.entries.len()).min_by_key(|&i| self.entries[i].recv_us) {
            self.entries.swap_remove(i);
        }
    }

    pub(crate) fn remove_worst(&mut self, threshold: Option<u32>) {
        let hot = (0..self.entries.len())
            .filter(|&i| is_hot(&self.entries[i], threshold))
            .max_by_key(|&i| self.entries[i].rif);
        let worst = hot.or_else(|| (0..self.entries.len()).max_by_key(|&i| self.entries[i].latency_us));
        if let Some(i) = worst {
            self.entries.swap_remove(i);
        }
    }

    #[cfg(test)]
    pub(crate) fn replicas(&self) -> Vec<usize> {
        let mut r: Vec<_> = self.entries.iter().map(|e| e.replica).collect();
        r.sort_unstable();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(replica: usize, recv_us: u64, rif: u32, latency_us: u64) -> Entry {
        Entry { replica, recv_us, rif, latency_us, uses_left: 2 }
    }

    fn pool(entries: &[Entry]) -> Pool {
        let mut p = Pool::new(16);
        entries.iter().for_each(|&e| p.insert(e));
        p
    }

    #[test]
    fn prefers_cold_low_latency_over_hot_low_latency() {
        let p = pool(&[entry(0, 0, 10, 1), entry(1, 0, 2, 50), entry(2, 0, 3, 20)]);
        let i = p.select(Some(5), |_| true).unwrap();
        assert_eq!(p.entries[i].replica, 2);
        let i = p.select(Some(5), |r| r != 2).unwrap();
        assert_eq!(p.entries[i].replica, 1);
    }

    #[test]
    fn all_hot_picks_min_rif() {
        let p = pool(&[entry(0, 0, 10, 1), entry(1, 0, 8, 90)]);
        let i = p.select(Some(5), |_| true).unwrap();
        assert_eq!(p.entries[i].replica, 1);
    }

    #[test]
    fn remove_and_rename() {
        let mut p = pool(&[entry(0, 0, 1, 1), entry(3, 0, 1, 1)]);
        p.remove_replica(0);
        p.rename_replica(3, 0);
        assert_eq!(p.replicas(), [0]);
    }

    #[test]
    fn insert_dedupes_and_caps() {
        let mut p = Pool::new(2);
        p.insert(entry(0, 1, 1, 1));
        p.insert(entry(0, 2, 1, 1));
        p.insert(entry(1, 3, 1, 1));
        p.insert(entry(2, 4, 1, 1));
        assert_eq!(p.replicas(), [1, 2]);
    }

    #[test]
    fn consume_compensates_and_exhausts() {
        let mut p = pool(&[entry(4, 0, 1, 1)]);
        assert_eq!(p.consume(0), 4);
        assert_eq!(p.entries[0].rif, 2);
        p.consume(0);
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn expire_and_evictions() {
        let mut p = pool(&[entry(0, 0, 9, 1), entry(1, 500, 1, 99), entry(2, 900, 1, 5)]);
        p.expire(1_200, 1_000);
        assert_eq!(p.replicas(), [1, 2]);
        p.remove_worst(Some(5));
        assert_eq!(p.replicas(), [2]);
    }
}

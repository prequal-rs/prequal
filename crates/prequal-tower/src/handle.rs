use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::Hash,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use prequal_core::{Config, Counters, Prequal, ProbeResponse};
use rand::{SeedableRng, rngs::SmallRng};

/// Prequal state plus the key ↔ index mapping; indices follow `Prequal`'s swap-remove order.
pub(crate) struct Shared<K> {
    pub(crate) prequal: Prequal,
    pub(crate) rng: SmallRng,
    pub(crate) keys: Vec<K>,
    index: HashMap<K, usize>,
    scratch: Vec<usize>,
    origin: Instant,
}

impl<K: Clone + Eq + Hash> Shared<K> {
    pub(crate) fn now_us(&self) -> u64 {
        self.origin.elapsed().as_micros() as u64
    }

    pub(crate) fn index_of(&self, key: &K) -> Option<usize> {
        self.index.get(key).copied()
    }

    pub(crate) fn insert(&mut self, key: K) -> Option<usize> {
        if self.index.contains_key(&key) {
            return None;
        }
        let at = self.prequal.add_replica();
        self.keys.push(key.clone());
        self.index.insert(key, at);
        Some(at)
    }

    /// Swap-removes `key`, returning the index it held.
    pub(crate) fn remove(&mut self, key: &K) -> Option<usize> {
        let at = self.index.remove(key)?;
        self.prequal.swap_remove_replica(at);
        self.keys.swap_remove(at);
        if let Some(moved) = self.keys.get(at) {
            self.index.insert(moved.clone(), at);
        }
        Some(at)
    }
}

/// Shared Prequal state for one balancer, addressed by discovery key. Clone it into endpoints to
/// feed load reports piggybacked on responses; unknown (e.g. removed) keys are ignored.
pub struct PrequalHandle<K> {
    shared: Arc<Mutex<Shared<K>>>,
}

impl<K> Clone for PrequalHandle<K> {
    fn clone(&self) -> Self {
        Self { shared: Arc::clone(&self.shared) }
    }
}

impl<K> fmt::Debug for PrequalHandle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrequalHandle").finish_non_exhaustive()
    }
}

impl<K: Clone + Eq + Hash> PrequalHandle<K> {
    /// Empty state; keys are added by a balancer's discovery or by [`PrequalHandle::sync`].
    #[must_use]
    pub fn new(config: Config) -> Self {
        let shared = Shared {
            prequal: Prequal::new(config, 0),
            rng: SmallRng::from_rng(&mut rand::rng()),
            keys: Vec::new(),
            index: HashMap::new(),
            scratch: Vec::new(),
            origin: Instant::now(),
        };
        Self { shared: Arc::new(Mutex::new(shared)) }
    }

    /// Pools a load report from `key` (probed, or piggybacked on a response).
    pub fn record(&self, key: &K, response: ProbeResponse) {
        let mut guard = self.lock();
        let s = &mut *guard;
        if let Some(at) = s.index_of(key) {
            let now = s.now_us();
            s.prequal.record_probe(at, response, now, &mut s.rng);
        }
    }

    /// A request to `key` succeeded.
    pub fn record_success(&self, key: &K) {
        let mut s = self.lock();
        if let Some(at) = s.index_of(key) {
            s.prequal.record_success(at);
        }
    }

    /// A request to `key` failed: drops its pooled reports and may eject it.
    pub fn record_failure(&self, key: &K) {
        let mut s = self.lock();
        if let Some(at) = s.index_of(key) {
            let now = s.now_us();
            s.prequal.record_failure(at, now);
        }
    }

    /// Picks a replica for one query and appends the keys to probe for it to `probe_targets`.
    /// Returns `None` while no replicas are registered.
    pub fn choose(&self, probe_targets: &mut Vec<K>) -> Option<K> {
        self.choose_where(probe_targets, |_| true)
    }

    /// Like [`PrequalHandle::choose`], restricted to keys for which `allowed` holds.
    /// Returns `None` when no registered key is allowed.
    pub fn choose_where(&self, probe_targets: &mut Vec<K>, allowed: impl Fn(&K) -> bool) -> Option<K> {
        let mut guard = self.lock();
        let s = &mut *guard;
        if s.keys.is_empty() {
            return None;
        }
        s.prequal.probe_targets_into(&mut s.rng, &mut s.scratch);
        probe_targets.extend(s.scratch.iter().map(|&t| s.keys[t].clone()));
        let now = s.now_us();
        let keys = &s.keys;
        let index = s.prequal.select_where(now, &mut s.rng, |i| allowed(&keys[i]))?;
        Some(s.keys[index].clone())
    }

    /// Up to `n` allowed keys other than `chosen`, in random order: fallbacks for a retrying proxy.
    pub fn fallbacks(&self, chosen: &K, n: usize, allowed: impl Fn(&K) -> bool) -> Vec<K> {
        use rand::seq::SliceRandom;
        let mut guard = self.lock();
        let s = &mut *guard;
        let mut others: Vec<K> = s.keys.iter().filter(|k| *k != chosen && allowed(k)).cloned().collect();
        others.shuffle(&mut s.rng);
        others.truncate(n);
        others
    }

    /// Makes the registered replicas exactly `keys`: stale ones are removed, new ones added.
    /// Replicas present in both keep their pooled probes and health. For callers that drive
    /// [`PrequalHandle::choose`] themselves (e.g. `prequal-pingora`); a [`PrequalBalance`](crate::PrequalBalance)
    /// owns its handle's membership and restores it from discovery on its next request.
    pub fn sync<'a>(&self, keys: impl IntoIterator<Item = &'a K>)
    where
        K: 'a,
    {
        let wanted: HashSet<&K> = keys.into_iter().collect();
        let mut s = self.lock();
        let stale: Vec<K> = s.keys.iter().filter(|k| !wanted.contains(k)).cloned().collect();
        for key in &stale {
            s.remove(key);
        }
        for key in wanted {
            s.insert(key.clone());
        }
    }

    /// Selection and ejection totals since creation.
    pub fn counters(&self) -> Counters {
        self.lock().prequal.counters()
    }

    /// Registered replicas.
    pub fn len(&self) -> usize {
        self.lock().keys.len()
    }

    /// Whether no replica is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Shared<K>> {
        self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

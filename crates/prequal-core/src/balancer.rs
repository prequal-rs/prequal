use rand::{Rng, RngExt, seq::index};

use crate::{
    Config, ProbeResponse,
    frac::FracCounter,
    health::HealthTable,
    history::RifHistory,
    pool::{Entry, Pool},
};

/// Totals since creation, for export as metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Counters {
    /// Replicas selected.
    pub selections: u64,
    /// Selections made uniformly at random because fewer than two usable probes were pooled.
    pub random_fallbacks: u64,
    /// Replicas ejected after consecutive failures.
    pub ejections: u64,
}

/// Per-client Prequal state over replicas `0..replicas()`. Each query: call
/// [`Prequal::probe_targets_into`] and probe those replicas asynchronously, feed answers to
/// [`Prequal::record_probe`], route to [`Prequal::select`], and report the outcome with
/// [`Prequal::record_success`] / [`Prequal::record_failure`].
#[derive(Clone, Debug)]
pub struct Prequal {
    config: Config,
    pool: Pool,
    history: RifHistory,
    health: HealthTable,
    replicas: usize,
    probe_acc: FracCounter,
    remove_acc: FracCounter,
    remove_oldest_next: bool,
    counters: Counters,
}

impl Prequal {
    /// State for `replicas` replicas, numbered `0..replicas`.
    #[must_use]
    pub fn new(config: Config, replicas: usize) -> Self {
        let mut health = HealthTable::default();
        (0..replicas).for_each(|_| health.push());
        Self {
            pool: Pool::new(config.pool_capacity),
            history: RifHistory::new(config.rif_history),
            config,
            health,
            replicas,
            probe_acc: FracCounter::default(),
            remove_acc: FracCounter::default(),
            remove_oldest_next: true,
            counters: Counters::default(),
        }
    }

    /// The parameters this state was created with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Number of replicas.
    pub fn replicas(&self) -> usize {
        self.replicas
    }

    /// Load reports currently pooled.
    pub fn pool_len(&self) -> usize {
        self.pool.len()
    }

    /// Totals since creation.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Appends a replica and returns its index.
    pub fn add_replica(&mut self) -> usize {
        self.health.push();
        self.replicas += 1;
        self.replicas - 1
    }

    /// Removes `replica`; the last replica takes over its index (like `Vec::swap_remove`).
    pub fn swap_remove_replica(&mut self, replica: usize) {
        assert!(replica < self.replicas, "replica {replica} out of range");
        let last = self.replicas - 1;
        self.pool.remove_replica(replica);
        self.pool.rename_replica(last, replica);
        self.health.swap_remove(replica);
        self.replicas = last;
    }

    /// Whether `replica` is ejected at `now_us`.
    ///
    /// # Panics
    /// If `replica` is out of range.
    pub fn is_ejected(&self, replica: usize, now_us: u64) -> bool {
        self.health.is_ejected(replica, now_us)
    }

    /// Replicas to probe for one query ([`Config::probes_per_query`] on average, distinct).
    #[must_use]
    pub fn probe_targets<R: Rng + ?Sized>(&mut self, rng: &mut R) -> Vec<usize> {
        let mut targets = Vec::new();
        self.probe_targets_into(rng, &mut targets);
        targets
    }

    /// Like [`Prequal::probe_targets`] but reuses `out` (cleared first) to avoid allocating.
    pub fn probe_targets_into<R: Rng + ?Sized>(&mut self, rng: &mut R, out: &mut Vec<usize>) {
        out.clear();
        let wanted = self.probe_acc.take(self.config.probes_per_query).min(self.replicas);
        if wanted * 4 <= self.replicas {
            while out.len() < wanted {
                let r = rng.random_range(0..self.replicas);
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        } else {
            out.extend(index::sample(rng, self.replicas, wanted));
        }
    }

    /// Pools a load report from `replica` (a probe answer or one piggybacked on a response). Out-of-range replicas
    /// are ignored.
    pub fn record_probe<R: Rng + ?Sized>(&mut self, replica: usize, response: ProbeResponse, now_us: u64, rng: &mut R) {
        if replica >= self.replicas {
            return;
        }
        self.history.push(response.rif);
        let uses_left = self.reuse_budget(rng);
        self.pool.insert(Entry {
            replica,
            recv_us: now_us,
            rif: response.rif,
            latency_us: response.latency_us,
            uses_left,
        });
    }

    /// A query to `replica` succeeded: resets its consecutive-failure count.
    pub fn record_success(&mut self, replica: usize) {
        if replica < self.replicas {
            self.health.record_success(replica);
        }
    }

    /// Drops the replica's pooled probes (a fast-failing replica looks idle) and may eject it.
    pub fn record_failure(&mut self, replica: usize, now_us: u64) {
        if replica >= self.replicas {
            return;
        }
        self.pool.remove_replica(replica);
        if self.health.record_failure(replica, now_us, &self.config) {
            self.counters.ejections += 1;
        }
    }

    /// Picks a replica for one query, skipping ejected replicas. Falls back to a uniformly random
    /// un-ejected replica while fewer than two usable probes are pooled. If membership shrinks until
    /// every replica is ejected, it still routes ("panic routing") rather than failing the query.
    ///
    /// # Panics
    /// If the replica count is zero.
    pub fn select<R: Rng + ?Sized>(&mut self, now_us: u64, rng: &mut R) -> usize {
        assert!(self.replicas > 0, "Prequal::select with no replicas");
        self.select_where(now_us, rng, |_| true).expect("some replica is allowed")
    }

    /// Like [`Prequal::select`], restricted to replicas for which `allowed` holds (e.g. a gateway's
    /// subset hint). `None` if no replica is allowed.
    pub fn select_where<R: Rng + ?Sized>(
        &mut self,
        now_us: u64,
        rng: &mut R,
        allowed: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        if !(0..self.replicas).any(&allowed) {
            return None;
        }
        self.counters.selections += 1;
        self.pool.expire(now_us, self.config.max_age_us);
        let threshold = self.history.threshold(self.config.q_rif);
        let health = &self.health;
        let eligible = |r: usize| allowed(r) && !health.is_ejected(r, now_us);
        let replica = match self.pool.select(threshold, eligible) {
            Some(i) if self.pool.len() >= 2 => self.pool.consume(i),
            _ => {
                self.counters.random_fallbacks += 1;
                self.random_allowed(now_us, rng, &allowed)
            }
        };
        for _ in 0..self.remove_acc.take(self.config.removes_per_query) {
            if self.remove_oldest_next {
                self.pool.remove_oldest();
            } else {
                self.pool.remove_worst(threshold);
            }
            self.remove_oldest_next = !self.remove_oldest_next;
        }
        Some(replica)
    }

    /// A random allowed, un-ejected replica; an allowed ejected one if that's all there is.
    fn random_allowed<R: Rng + ?Sized>(&self, now_us: u64, rng: &mut R, allowed: &impl Fn(usize) -> bool) -> usize {
        let start = rng.random_range(0..self.replicas);
        let mut ring = (0..self.replicas).map(|offset| (start + offset) % self.replicas).filter(|&r| allowed(r));
        let first_allowed = ring.clone().next().expect("caller checked some replica is allowed");
        ring.find(|&r| !self.health.is_ejected(r, now_us)).unwrap_or(first_allowed)
    }

    /// `max(1, 1 + δ / ((1 - m/n)·r_probe - r_remove))`, randomly rounded. The paper leaves a
    /// non-positive denominator undefined; that means probes can't keep up, so allow `max_reuse`.
    fn reuse_budget<R: Rng + ?Sized>(&self, rng: &mut R) -> u32 {
        let c = &self.config;
        let fill = c.pool_capacity as f64 / self.replicas as f64;
        let denom = (1.0 - fill) * c.probes_per_query - c.removes_per_query;
        let max = f64::from(c.max_reuse.max(1));
        let budget = if denom > 0.0 { (1.0 + c.reuse_delta / denom).clamp(1.0, max) } else { max };
        let whole = budget.floor();
        whole as u32 + u32::from(rng.random::<f64>() < budget - whole)
    }
}

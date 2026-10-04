use std::{
    collections::HashSet,
    fmt,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use tokio::sync::Notify;

use crate::{
    engine::EngineStats,
    fleet::Replica,
    heat::HeatTracker,
    policy::{Policy, PolicyRng},
    prompt::Prompt,
    ticket::{PrefillSignal, Ticket},
};

/// Routes LLM requests across replicas with a [`Policy`], keeping per-replica state current from scrapes
/// ([`Scheduler::observe`]) and from the lifecycle of each routed request ([`Ticket`]).
///
/// With an admission limit ([`Scheduler::with_admission_limit`]), [`Scheduler::acquire`] binds late: a request
/// waits in one router-wide FIFO queue until some replica has fewer than `limit` requests awaiting their first
/// token, and is then placed among the replicas with room. One shared queue instead of a queue per replica means
/// no request waits behind a slow replica while another is free (M/M/c rather than c × M/M/1), and a burst
/// cannot herd onto one replica between scrapes.
///
/// Clones share all state, and the `with_*` settings apply to every clone; tickets keep the prefill signal they
/// were routed under.
#[derive(Clone)]
pub struct Scheduler {
    shared: Arc<Shared>,
}

/// The scheduler's time source: `Instant::now` in production, a virtual clock in simulation.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub(crate) struct Shared {
    policy: Box<dyn Policy>,
    state: Mutex<State>,
    /// Signalled whenever a replica may have gained room; waiters are woken in FIFO order.
    room: Notify,
}

struct State {
    replicas: Vec<Replica>,
    rng: PolicyRng,
    admission_limit: Option<f64>,
    signal: PrefillSignal,
    clock: Clock,
    /// EWMA of admission demand (uncached prompt + output tokens) per routed request.
    typical_demand_tokens: f64,
    /// Requests queued in [`Scheduler::acquire`]; newcomers queue behind them rather than barging in.
    queued: usize,
    heat: HeatTracker,
    next_ticket: u64,
}

/// Seconds over which prefix heat decays.
const HEAT_TAU_SECS: f64 = 10.0;

enum Placement {
    Routed(Ticket),
    /// Eligible replicas exist but none has room.
    Full,
    Unavailable,
}

impl fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        f.debug_struct("Scheduler")
            .field("policy", &self.shared.policy.name())
            .field("replicas", &state.replicas.len())
            .field("signal", &state.signal)
            .field("admission_limit", &state.admission_limit)
            .finish_non_exhaustive()
    }
}

impl Scheduler {
    /// No replicas yet ([`Scheduler::sync`] adds them); routes with `policy`.
    #[must_use]
    pub fn new(policy: Box<dyn Policy>) -> Self {
        let state = State {
            replicas: Vec::new(),
            rng: PolicyRng::from_entropy(),
            admission_limit: None,
            signal: PrefillSignal::FirstChunk,
            clock: Arc::new(Instant::now),
            typical_demand_tokens: 1024.0,
            queued: 0,
            heat: HeatTracker::new(HEAT_TAU_SECS),
            next_ticket: 0,
        };
        let shared = Shared { policy, state: Mutex::new(state), room: Notify::new() };
        Self { shared: Arc::new(shared) }
    }

    /// Late binding for [`Scheduler::acquire`]: at most `limit` requests awaiting first token per replica (this
    /// router's plus other routers' queued requests seen in scrapes). A few keeps engines' prefill busy.
    #[must_use]
    pub fn with_admission_limit(self, limit: u32) -> Self {
        self.state().admission_limit = Some(f64::from(limit.max(1)));
        self
    }

    /// What ends a routed request's prefill reservation (default: its first response body chunk).
    #[must_use]
    pub fn with_prefill_signal(self, signal: PrefillSignal) -> Self {
        self.state().signal = signal;
        self
    }

    /// Reads time from `clock` instead of `Instant::now` (for virtual-time simulation).
    #[must_use]
    pub fn with_clock(self, clock: Clock) -> Self {
        self.state().clock = clock;
        self
    }

    /// Seeds the policy's random tie-breaking (for reproducible simulation).
    #[must_use]
    pub fn with_seed(self, seed: u64) -> Self {
        self.state().rng = PolicyRng::seed_from_u64(seed);
        self
    }

    /// The policy's [`Policy::name`].
    pub fn policy_name(&self) -> &'static str {
        self.shared.policy.name()
    }

    /// Replaces the replica set, keeping state for replicas that remain.
    pub fn sync(&self, addrs: impl IntoIterator<Item = SocketAddr>) {
        let wanted: HashSet<SocketAddr> = addrs.into_iter().collect();
        let mut state = self.state();
        state.replicas.retain(|r| wanted.contains(&r.addr));
        let known: HashSet<SocketAddr> = state.replicas.iter().map(|r| r.addr).collect();
        let mut added: Vec<_> = wanted.difference(&known).copied().collect();
        added.sort();
        state.replicas.extend(added.into_iter().map(Replica::new));
        drop(state);
        self.shared.room.notify_one();
    }

    /// Every replica's address.
    pub fn addrs(&self) -> Vec<SocketAddr> {
        self.state().replicas.iter().map(|r| r.addr).collect()
    }

    /// Replicas not marked down (by a failed scrape or request): what a readiness check wants to be non-zero.
    pub fn up_count(&self) -> usize {
        self.state().replicas.iter().filter(|r| !r.down).count()
    }

    /// Applies a good scrape of `addr` (clearing any down mark); unknown addresses are ignored.
    pub fn observe(&self, addr: SocketAddr, stats: EngineStats) {
        self.shared.update_replica(addr, |replica, now| replica.observe(stats, now));
    }

    /// Marks `addr` down until its next good scrape (a failed scrape).
    pub fn mark_down(&self, addr: SocketAddr) {
        self.shared.update_replica(addr, |replica, _| replica.down = true);
    }

    /// Picks a replica now among those `allowed` (down replicas only if nothing else is left) and reserves the
    /// request's load there until the returned ticket ends. Ignores the admission limit.
    #[must_use = "dropping the ticket ends the request's reservation"]
    pub fn route(&self, prompt: &Prompt, output_tokens: u64, allowed: impl Fn(&SocketAddr) -> bool) -> Option<Ticket> {
        let mut state = self.state();
        match self.place(&mut state, prompt, output_tokens, &allowed, None, &mut None) {
            Placement::Routed(ticket) => Some(ticket),
            Placement::Full | Placement::Unavailable => None,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared.state()
    }
}

impl State {
    fn now(&self) -> Instant {
        (self.clock)()
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Applies `update` to one replica (with the scheduler's current time), then wakes the head of the admission
    /// queue (room may have appeared).
    pub(crate) fn update_replica(&self, addr: SocketAddr, update: impl FnOnce(&mut Replica, Instant)) {
        let mut state = self.state();
        let now = state.now();
        if let Some(replica) = state.replicas.iter_mut().find(|r| r.addr == addr) {
            update(replica, now);
        }
        let admission = state.admission_limit.is_some();
        drop(state);
        if admission {
            self.room.notify_one();
        }
    }
}

#[path = "admission.rs"]
mod admission;
#[path = "placement.rs"]
mod placement;
#[path = "status.rs"]
mod status;

pub use status::ReplicaStatus;

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;

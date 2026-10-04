//! The routers of a virtual-time run: building them, replacing one mid-run (`--router-restart-s`), their scrapes, and
//! what they tell each other (`--gossip-ms`).

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use prequal_llm::{Clock, ExactIndex, PrefillSignal, Prompt, Scheduler, policy};
use rand::{RngExt, SeedableRng, rngs::SmallRng};

use super::{Event, SCRAPE_US, Sim};
use crate::diagnose::Diagnosis;

pub(super) struct RouterSpec {
    pub policy: String,
    pub clock: Clock,
    pub seed: u64,
    pub signal: PrefillSignal,
    pub exact: Option<Arc<dyn ExactIndex>>,
}

impl RouterSpec {
    pub fn build(&self, r: usize, addrs: &[SocketAddr], diagnosis: Option<&Diagnosis>) -> Scheduler {
        let policy = policy::by_name(&self.policy).unwrap_or_else(|| panic!("unknown policy {}", self.policy));
        let policy = match diagnosis {
            Some(d) => d.wrap(policy),
            None => policy,
        };
        let scheduler = Scheduler::new(policy)
            .with_clock(Arc::clone(&self.clock))
            .with_seed(self.seed ^ r as u64)
            .with_prefill_signal(self.signal);
        let scheduler = match &self.exact {
            Some(exact) => scheduler.with_exact_index(Arc::clone(exact)),
            None => scheduler,
        };
        scheduler.sync(addrs.iter().copied());
        scheduler
    }
}

/// `--gossip-ms`: placements on their way to the other routers.
pub(super) struct Gossip {
    delay_us: u64,
    /// Chance that a peer never hears of a placement (`--gossip-loss`).
    loss: f64,
    rng: SmallRng,
    /// Sending router, engine and prompt of each request not yet announced.
    pending: HashMap<usize, (usize, usize, Prompt)>,
}

impl Gossip {
    pub fn new(delay: Duration, loss: f64, seed: u64) -> Self {
        Self {
            delay_us: delay.as_micros() as u64,
            loss: loss.clamp(0.0, 1.0),
            rng: SmallRng::seed_from_u64(seed),
            pending: HashMap::new(),
        }
    }
}

impl Sim {
    /// Schedules telling the other routers that `router` sent `prompt` to `engine`.
    pub(super) fn announce(&mut self, id: usize, router: usize, engine: usize, prompt: Prompt) {
        let now = self.now();
        let Some(gossip) = &mut self.gossip else { return };
        gossip.pending.insert(id, (router, engine, prompt));
        let at = now + gossip.delay_us;
        self.push(at, Event::Gossip(id));
    }

    pub(super) fn gossip(&mut self, id: usize) {
        let Some(gossip) = &mut self.gossip else { return };
        let Some((from, engine, prompt)) = gossip.pending.remove(&id) else { return };
        for (r, peer) in self.routers.iter().enumerate() {
            if r != from && !gossip.rng.random_bool(gossip.loss) {
                peer.observe_peer_route(self.addrs[engine], &prompt);
            }
        }
    }

    /// Replaces router `r` with one that has seen nothing, as a restarted process; its requests in flight carry on.
    pub(super) fn restart(&mut self, r: usize) {
        self.routers[r] = self.router_spec.build(r, &self.addrs, self.diagnosis.as_ref());
    }

    pub(super) fn scrape(&mut self) {
        for (engine, addr) in self.engines.iter().zip(&self.addrs) {
            let stats = engine.stats(self.hidden);
            self.routers.iter().for_each(|r| r.observe(*addr, stats));
        }
        if self.pending > 0 {
            let at = self.now() + SCRAPE_US;
            self.push(at, Event::Scrape);
        }
    }
}

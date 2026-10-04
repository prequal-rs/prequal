//! `--diagnose` (virtual time only): why requests miss. At each routing decision it compares which engines really
//! hold most of the prompt (their caches) with which replicas the router believed did (the candidates its policy
//! saw) and where the request went, tallied per popularity tier and printed to stderr after the run.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use prequal_llm::policy::{Candidate, Policy, PolicyRng, Request};

use crate::engine::EngineCore;

/// A replica holds a prompt when at least this fraction of its blocks is cached there (or believed to be).
const HELD: f64 = 0.5;

type View = Arc<Mutex<Vec<(SocketAddr, f64)>>>;

/// Delegates to `inner`, recording each pick's believed match ratio per replica.
struct Spy {
    inner: Box<dyn Policy>,
    view: View,
}

impl Policy for Spy {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], rng: &mut PolicyRng) -> usize {
        *self.view.lock().unwrap() = candidates.iter().map(|c| (c.addr(), c.match_ratio(request))).collect();
        self.inner.pick(request, candidates, rng)
    }
}

/// Outcome classes, in report order.
const CLASSES: [&str; 6] = ["to-holder", "spread", "forgotten", "cold", "stale-belief", "believed-fresh"];

pub struct Diagnosis {
    view: View,
    addrs: Vec<SocketAddr>,
    /// Exclusive end group of each tier.
    tier_ends: Vec<usize>,
    counts: Vec<[u64; CLASSES.len()]>,
    requests: Vec<u64>,
    /// Sampled `(blocks cached across engines, distinct blocks)`: how much of the fleet's KV holds copies.
    copies: (u64, u64),
}

/// Requests between duplication samples.
const COPY_SAMPLE_EVERY: u64 = 500;

impl Diagnosis {
    pub fn new(addrs: Vec<SocketAddr>, tier_ends: Vec<usize>) -> Self {
        let tiers = tier_ends.len().max(1);
        Self {
            view: View::default(),
            addrs,
            tier_ends,
            counts: vec![[0; CLASSES.len()]; tiers],
            requests: vec![0; tiers],
            copies: (0, 0),
        }
    }

    pub fn wrap(&self, policy: Box<dyn Policy>) -> Box<dyn Policy> {
        Box::new(Spy { inner: policy, view: Arc::clone(&self.view) })
    }

    /// Classifies the request just routed to `chosen` (an engine index), before it reaches the engine.
    pub fn observe<T>(&mut self, group: Option<usize>, text: &[u8], chosen: usize, engines: &[EngineCore<T>]) {
        let truth: Vec<bool> = engines
            .iter()
            .map(|e| {
                let hashes = e.tokenize(text).0;
                !hashes.is_empty() && e.cached_prefix(&hashes) as f64 >= HELD * hashes.len() as f64
            })
            .collect();
        let view = std::mem::take(&mut *self.view.lock().unwrap());
        let believed: Vec<bool> =
            self.addrs.iter().map(|a| view.iter().any(|(addr, ratio)| addr == a && *ratio >= HELD)).collect();
        let tier = group.map_or(0, |g| self.tier_ends.iter().position(|&end| g < end).unwrap_or(0));
        let counts = &mut self.counts[tier];
        self.requests[tier] += 1;
        let holders = (0..truth.len()).filter(|&i| truth[i]);
        match (truth[chosen], holders.clone().count(), holders.clone().any(|i| believed[i])) {
            (true, _, _) => counts[0] += 1,
            (false, 0, _) => counts[3] += 1,
            (false, _, true) => counts[1] += 1,
            (false, _, false) => counts[2] += 1,
        }
        counts[4] += u64::from(believed[chosen] && !truth[chosen]);
        counts[5] += u64::from(!believed.iter().any(|&b| b) && holders.count() > 0);
        if self.requests.iter().sum::<u64>() % COPY_SAMPLE_EVERY == 0 {
            let all: Vec<u64> = engines.iter().flat_map(EngineCore::cached_block_ids).collect();
            self.copies.0 += all.len() as u64;
            self.copies.1 += all.into_iter().collect::<HashSet<_>>().len() as u64;
        }
    }

    pub fn print(&self) {
        eprintln!("diagnose: per tier, requests then {}", CLASSES.join(" / "));
        for (tier, (n, counts)) in self.requests.iter().zip(&self.counts).enumerate() {
            let shares: Vec<String> = counts.iter().map(|&c| format!("{:.3}", c as f64 / (*n).max(1) as f64)).collect();
            eprintln!("diagnose: tier {tier}: {n} / {}", shares.join(" / "));
        }
        eprintln!(
            "diagnose: cached blocks per distinct block {:.3}",
            self.copies.0 as f64 / self.copies.1.max(1) as f64
        );
    }
}

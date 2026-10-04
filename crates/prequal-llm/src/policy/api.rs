//! What a [`Policy`] sees: the request, a read-only view of each routable replica, and an RNG for tie-breaking.

use std::{fmt, net::SocketAddr, time::Instant};

use rand::{Rng as _, RngExt, SeedableRng, rngs::SmallRng};

use crate::{
    engine::EngineStats,
    fleet::Replica,
    prompt::{BLOCK_TOKENS, Prompt},
};

/// Picks a replica for each request. [`by_name`](super::by_name) builds the bundled ones.
pub trait Policy: Send + Sync {
    /// Short name, as [`by_name`](super::by_name) accepts it.
    fn name(&self) -> &'static str;

    /// Index into `candidates` (never empty) of the replica to use; an out-of-range index is clamped.
    fn pick(&self, request: &Request, candidates: &[Candidate], rng: &mut PolicyRng) -> usize;
}

/// The request being routed.
#[derive(Debug)]
pub struct Request<'a> {
    pub(crate) prompt: &'a Prompt,
    pub(crate) output_tokens: u64,
    pub(crate) typical_demand_tokens: f64,
    pub(crate) heat_share: f64,
    pub(crate) home: Option<SocketAddr>,
    pub(crate) key: u64,
    pub(crate) now: Instant,
}

impl Request<'_> {
    /// The prompt's blocks and token estimate.
    pub fn prompt(&self) -> &Prompt {
        self.prompt
    }

    /// Output tokens the request may generate.
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Recent mean admission demand (uncached prompt + output tokens) per request, to price other routers' queued
    /// requests, whose size is unknown.
    pub fn typical_demand_tokens(&self) -> f64 {
        self.typical_demand_tokens
    }

    /// The block identifying this prompt for routing: the first past the prefix every candidate holds (a system
    /// prompt common to all traffic says nothing about where to send it), or its last if all hold it whole.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// This prompt key's recent share of all routed requests (0 to 1).
    pub fn heat_share(&self) -> f64 {
        self.heat_share
    }

    /// Where this router sent this prompt key's previous request, if it remembers.
    pub fn home(&self) -> Option<SocketAddr> {
        self.home
    }

    /// The scheduler's clock at routing time.
    pub fn now(&self) -> Instant {
        self.now
    }
}

/// A routable replica, read-only, and how much of this request's prompt it is believed to hold.
pub struct Candidate<'a> {
    pub(crate) replica: &'a Replica,
    pub(crate) matched_blocks: usize,
}

impl Candidate<'_> {
    /// The replica's address.
    pub fn addr(&self) -> SocketAddr {
        self.replica.addr
    }

    /// Leading prompt blocks the replica is believed to have cached.
    pub fn matched_blocks(&self) -> usize {
        self.matched_blocks
    }

    /// [`Candidate::matched_blocks`] in tokens, at most the prompt's.
    pub fn matched_tokens(&self, request: &Request) -> u64 {
        (self.matched_blocks as u64 * BLOCK_TOKENS).min(request.prompt.tokens)
    }

    /// Prompt tokens not covered by the matched prefix, taking the match at face value.
    pub fn uncached_tokens(&self, request: &Request) -> u64 {
        request.prompt.tokens - self.matched_tokens(request)
    }

    /// Fraction of the prompt's blocks matched (0 for an empty prompt).
    pub fn match_ratio(&self, request: &Request) -> f64 {
        let blocks = request.prompt.blocks.len();
        if blocks == 0 { 0.0 } else { self.matched_blocks as f64 / blocks as f64 }
    }

    /// The replica's latest scrape (non-finite values zeroed); `None` until the first.
    pub fn stats(&self) -> Option<EngineStats> {
        self.replica.stats
    }

    /// Scraped waiting requests.
    pub fn waiting(&self) -> Option<f64> {
        self.replica.stats.map(|s| s.waiting)
    }

    /// Scraped KV-cache usage (0 to 1).
    pub fn kv_usage(&self) -> Option<f64> {
        self.replica.stats.map(|s| s.kv_usage)
    }

    /// Requests running or queued: the engine's last count, or this router's own if more have arrived since.
    pub fn in_flight(&self) -> f64 {
        self.replica.in_flight()
    }

    /// [`Candidate::in_flight`] averaged over the last few seconds.
    pub fn smoothed_load(&self) -> f64 {
        self.replica.smoothed_load
    }

    /// This router's requests here that have not reached their first token.
    pub fn prefilling(&self) -> u32 {
        self.replica.prefilling
    }

    /// Uncached prompt tokens of [`Candidate::prefilling`] requests.
    pub fn pending_prefill_tokens(&self) -> u64 {
        self.replica.pending_prefill_tokens
    }

    /// This router's requests here that have not ended.
    pub fn active(&self) -> u32 {
        self.replica.active
    }

    /// Prompt + output tokens of [`Candidate::active`] requests.
    pub fn active_tokens(&self) -> u64 {
        self.replica.active_tokens
    }
}

impl fmt::Debug for Candidate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Candidate")
            .field("addr", &self.replica.addr)
            .field("matched_blocks", &self.matched_blocks)
            .finish_non_exhaustive()
    }
}

/// Randomness for a policy's tie-breaking, owned by the scheduler and seeded by
/// [`Scheduler::with_seed`](crate::Scheduler::with_seed).
#[derive(Clone, Debug)]
pub struct PolicyRng(SmallRng);

impl PolicyRng {
    /// A generator seeded from `seed` (for tests and simulation).
    #[must_use]
    pub fn seed_from_u64(seed: u64) -> Self {
        Self(SmallRng::seed_from_u64(seed))
    }

    pub(crate) fn from_entropy() -> Self {
        Self(SmallRng::from_rng(&mut rand::rng()))
    }

    /// A uniformly random index in `0..n`.
    ///
    /// # Panics
    /// If `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        self.0.random_range(0..n)
    }

    /// A uniformly random `u64`.
    pub fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }
}

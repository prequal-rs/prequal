//! Per-replica routing state: the latest scrape, this router's own reservations, and the approximate prefix cache
//! with its self-correction against the engine's hit counters.

use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::{
    engine::EngineStats,
    index::BlockLru,
    prompt::{BLOCK_BYTES, BYTES_PER_TOKEN, Prompt},
    ticket::PrefillSignal,
};

/// Approximate-index capacity (in blocks) before an engine reports its KV size; llm-d's default.
const DEFAULT_CAPACITY_BLOCKS: usize = 31_250;
/// Predicted hit tokens a correction window needs before its observed hit rate is trusted.
const MIN_PREDICTED_FOR_CORRECTION: f64 = 4_096.0;
/// Prefilled prompt bytes a calibration window needs, so that requests straddling its ends barely skew it.
const MIN_BYTES_FOR_CALIBRATION: f64 = 1_048_576.0;
/// Weight of each calibration window once the first `1 / CALIBRATION_WEIGHT` have set a plain mean.
const CALIBRATION_WEIGHT: f64 = 0.1;
/// Plausible prompt bytes per engine token, a router's share of the engine included.
const BYTES_PER_TOKEN_RANGE: (f64, f64) = (0.1, 64.0);
/// Matches are never discounted below this: some prefix knowledge beats none, and hits need affinity to recover.
const MIN_CONFIDENCE: f64 = 0.25;
/// Seconds over which [`Replica::smoothed_load`] averages.
const LOAD_TAU_SECS: f64 = 5.0;

/// A request's prefill reservation that ends at an estimated time rather than at its first token.
#[derive(Clone, Copy)]
pub struct Estimate {
    pub id: u64,
    pub done_at: Instant,
    pub uncached: u64,
    pub output: u64,
    pub hit_tokens: u64,
}

pub struct Replica {
    pub addr: SocketAddr,
    pub stats: Option<EngineStats>,
    pub scraped_at: Option<Instant>,
    /// Scrape failing: excluded from routing unless nothing else is left.
    pub down: bool,
    pub cache: BlockLru,
    /// This router's requests not yet at first token, their uncached prompt tokens, and their admission demand
    /// (uncached prompt + output tokens: the KV cache they must claim before prefill can start).
    pub prefilling: u32,
    pub pending_prefill_tokens: u64,
    pub pending_demand_tokens: u64,
    /// This router's requests in flight (until the response ends), and their prompt + output tokens.
    pub active: u32,
    pub active_tokens: u64,
    /// [`Replica::in_flight`] averaged over the last [`LOAD_TAU_SECS`]: sustained load, blind to arrival noise.
    pub smoothed_load: f64,
    /// Estimated prefill completions of this router's requests (`PrefillSignal::Estimate`), in completion order.
    estimates: VecDeque<Estimate>,
    /// When this router's estimated prefill backlog here clears.
    prefill_free_at: Option<Instant>,
    load_sampled_at: Option<Instant>,
    /// Observed / predicted prefix-cache hits, in [MIN_CONFIDENCE, 1]; scales how much a match is believed.
    pub confidence: f64,
    /// Hit tokens predicted for requests that started prefill since the engine's counter read `window_hits`.
    predicted_hit_tokens: f64,
    window_hits: Option<f64>,
    last_counters: Option<(f64, f64)>,
    /// Prompt bytes per engine token, which sizes [`Replica::cache`] from the engine's KV capacity (see
    /// [`Replica::calibrate`]).
    bytes_per_token: f64,
    calibrations: u32,
    /// Prompt bytes of this router's requests that started prefill since the engine's query counter read
    /// `window_queries`.
    prefilled_bytes: f64,
    window_queries: Option<f64>,
}

impl Replica {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            stats: None,
            scraped_at: None,
            down: false,
            cache: BlockLru::new(DEFAULT_CAPACITY_BLOCKS),
            prefilling: 0,
            pending_prefill_tokens: 0,
            pending_demand_tokens: 0,
            active: 0,
            active_tokens: 0,
            smoothed_load: 0.0,
            estimates: VecDeque::new(),
            prefill_free_at: None,
            load_sampled_at: None,
            confidence: 1.0,
            predicted_hit_tokens: 0.0,
            window_hits: None,
            last_counters: None,
            bytes_per_token: BYTES_PER_TOKEN as f64,
            calibrations: 0,
            prefilled_bytes: 0.0,
            window_queries: None,
        }
    }

    /// Whether the cache model's size has been measured (see [`Replica::calibrate`]).
    pub fn calibrated(&self) -> bool {
        self.calibrations > 0
    }

    pub fn matched_blocks(&self, prompt: &Prompt) -> usize {
        self.cache.matched(&prompt.blocks)
    }

    /// Queued requests the engine reports beyond this router's own: other routers' work, in requests.
    pub fn foreign_waiting(&self) -> f64 {
        self.stats.map_or(0.0, |s| (s.waiting - f64::from(self.prefilling)).max(0.0))
    }

    /// Requests awaiting first token here: this router's in prefill or queued, plus other routers' queued ones.
    pub fn admission_backlog(&self) -> f64 {
        f64::from(self.prefilling) + self.foreign_waiting()
    }

    /// Requests running or queued here: the engine's last count, or this router's own if more have arrived since.
    pub fn in_flight(&self) -> f64 {
        self.stats.map_or(0.0, |s| s.running + s.waiting).max(f64::from(self.active))
    }

    /// Folds the current [`Replica::in_flight`] into [`Replica::smoothed_load`], weighted by the time since the last
    /// sample.
    pub fn sample_load(&mut self, now: Instant) {
        let alpha = self
            .load_sampled_at
            .map_or(1.0, |at| 1.0 - (-now.saturating_duration_since(at).as_secs_f64() / LOAD_TAU_SECS).exp());
        self.smoothed_load += alpha * (self.in_flight() - self.smoothed_load);
        self.load_sampled_at = Some(now);
    }

    pub fn record_route(&mut self, prompt: &Prompt, uncached: u64, output: u64) {
        self.cache.touch(&prompt.blocks);
        self.prefilling += 1;
        self.pending_prefill_tokens += uncached;
        self.pending_demand_tokens += uncached + output;
        self.active += 1;
        self.active_tokens += prompt.tokens + output;
    }

    /// Another router sent `prompt` here: the engine caches it and counts its tokens like this router's own, so it
    /// enters the cache model and its calibration ([`Replica::calibrate`]).
    pub fn record_peer_route(&mut self, prompt: &Prompt) {
        self.cache.touch(&prompt.blocks);
        self.prefilled_bytes += (prompt.tokens * BYTES_PER_TOKEN as u64) as f64;
    }

    /// A request routed here finished prefill, which is when the engine counts its prompt and cache hits (both in
    /// this router's estimated tokens).
    pub fn count_prefilled(&mut self, prompt_tokens: u64, hit_tokens: u64) {
        self.predicted_hit_tokens += hit_tokens as f64;
        self.prefilled_bytes += (prompt_tokens * BYTES_PER_TOKEN as u64) as f64;
    }

    /// A request routed here reached its first token (or ended before it).
    pub fn end_prefill(&mut self, uncached: u64, output: u64) {
        self.prefilling = self.prefilling.saturating_sub(1);
        self.pending_prefill_tokens = self.pending_prefill_tokens.saturating_sub(uncached);
        self.pending_demand_tokens = self.pending_demand_tokens.saturating_sub(uncached + output);
    }

    /// For signals the router cannot observe, schedules when a just-routed request's prefill reservation ends
    /// ([`Replica::expire_estimates`]): for [`PrefillSignal::Estimate`] after this router's estimated backlog here plus
    /// its own uncached tokens; for [`PrefillSignal::Scrape`] at the next scrape (`estimate.done_at` is the route time).
    pub fn schedule_prefill_end(&mut self, signal: PrefillSignal, estimate: Estimate) {
        let done_at = match signal {
            PrefillSignal::Estimate { tokens_per_sec } => {
                let start = self.prefill_free_at.map_or(estimate.done_at, |free| free.max(estimate.done_at));
                let done_at = start + Duration::from_secs_f64(estimate.uncached as f64 / tokens_per_sec);
                self.prefill_free_at = Some(done_at);
                done_at
            }
            PrefillSignal::Scrape => estimate.done_at,
            _ => return,
        };
        self.estimates.push_back(Estimate { done_at, ..estimate });
    }

    /// Ends the prefill reservations whose estimated completion has passed.
    pub fn expire_estimates(&mut self, now: Instant) {
        while let Some(e) = self.estimates.front().filter(|e| e.done_at <= now).copied() {
            self.estimates.pop_front();
            self.end_prefill(e.uncached, e.output);
            self.count_prefilled(e.uncached + e.hit_tokens, e.hit_tokens);
        }
    }

    /// Drops a request's estimated reservation; whether it was still pending.
    pub fn forget_estimate(&mut self, id: u64) -> bool {
        let found = self.estimates.iter().position(|e| e.id == id);
        found.map(|i| self.estimates.remove(i)).is_some()
    }

    /// Applies a scrape, which now counts any request whose estimated prefill is due (or that waited for a scrape)
    /// in its own queue. A counter that went backwards means the engine restarted: its cache is gone.
    pub fn observe(&mut self, stats: EngineStats, now: Instant) {
        let stats = stats.finite();
        self.expire_estimates(now);
        if let (Some(queries), Some(hits)) = (stats.prefix_queries, stats.prefix_hits) {
            if self.last_counters.is_some_and(|(q0, h0)| queries < q0 || hits < h0) {
                self.cache.clear();
                self.confidence = 1.0;
                self.predicted_hit_tokens = 0.0;
                self.window_hits = None;
                self.window_queries = None;
            }
            self.calibrate(queries);
            let window_start = *self.window_hits.get_or_insert(hits);
            if self.predicted_hit_tokens >= MIN_PREDICTED_FOR_CORRECTION {
                // Other routers' hits land in the same counter, so the ratio is capped at 1.
                let ratio = ((hits - window_start) / self.predicted_hit_tokens).min(1.0);
                self.confidence = (0.8 * self.confidence + 0.2 * ratio).max(MIN_CONFIDENCE);
                self.predicted_hit_tokens = 0.0;
                self.window_hits = Some(hits);
            }
            self.last_counters = Some((queries, hits));
        }
        if let Some(tokens) = stats.cache_tokens {
            self.cache.set_capacity((tokens * self.bytes_per_token / BLOCK_BYTES as f64).max(1.0) as usize);
        }
        self.stats = Some(stats);
        self.scraped_at = Some(now);
        self.down = false;
    }

    /// Measures prompt bytes per engine token: this router's prefilled prompt bytes over the growth of the engine's
    /// prompt-token counter (prefix-cache queries). Engines report KV capacity in their own tokens, and bytes per token
    /// vary several-fold with tokenizer, language and JSON escaping (~12 for inference-perf's random-token prompts on
    /// llm-d-inference-sim), so assuming 4 sizes the cache model too small: it forgets prefixes the engine still
    /// holds, and the policy deals them out again as cold prompts, to replicas that miss. Other routers' prompts land
    /// in the same counter, which scales the estimate, and so the model's capacity, to this router's share of the
    /// engine's cache: about what its own sends, all the model records, occupy there.
    fn calibrate(&mut self, queries: f64) {
        let Some(start) = self.window_queries else {
            self.window_queries = Some(queries);
            self.prefilled_bytes = 0.0;
            return;
        };
        let counted = queries - start;
        if self.prefilled_bytes < MIN_BYTES_FOR_CALIBRATION || counted <= 0.0 {
            return;
        }
        self.calibrations = self.calibrations.saturating_add(1);
        let weight = (1.0 / f64::from(self.calibrations)).max(CALIBRATION_WEIGHT);
        let (lo, hi) = BYTES_PER_TOKEN_RANGE;
        self.bytes_per_token += weight * ((self.prefilled_bytes / counted).clamp(lo, hi) - self.bytes_per_token);
        self.prefilled_bytes = 0.0;
        self.window_queries = Some(queries);
    }
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;

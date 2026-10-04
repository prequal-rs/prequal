//! Incumbent routers' scoring as published in their source,
//! fed the same scrapes and approximate prefix index as [`super::Prequal`]. Deterministic tie-breaking and
//! thresholds follow the originals.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Candidate, Policy, PolicyRng, Request, cold_lru::ColdLru, first_max};
use crate::prompt::BLOCK_TOKENS;

/// Each replica in turn.
#[derive(Debug, Default)]
pub struct RoundRobin(AtomicUsize);

impl Policy for RoundRobin {
    fn name(&self) -> &'static str {
        "round-robin"
    }

    fn pick(&self, _: &Request, candidates: &[Candidate], _: &mut PolicyRng) -> usize {
        self.0.fetch_add(1, Ordering::Relaxed) % candidates.len()
    }
}

/// Fewest of this router's requests in flight, random among ties.
#[derive(Debug)]
pub struct LeastRequest;

impl Policy for LeastRequest {
    fn name(&self) -> &'static str {
        "least-request"
    }

    fn pick(&self, _: &Request, candidates: &[Candidate], rng: &mut PolicyRng) -> usize {
        min_random(candidates.iter().map(|c| f64::from(c.replica.active)), rng)
    }
}

/// llm-d's built-in default profile: queue ×2 + KV utilisation ×2 + prefix match ratio ×3, max-score picker.
#[derive(Debug)]
pub struct LlmdDefault;

impl Policy for LlmdDefault {
    fn name(&self) -> &'static str {
        "llmd-default"
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], _: &mut PolicyRng) -> usize {
        first_max(llmd_default_scores(request, candidates))
    }
}

fn llmd_default_scores<'a>(request: &'a Request, candidates: &'a [Candidate]) -> impl Iterator<Item = f64> + 'a {
    let queues: Vec<f64> = candidates.iter().filter_map(Candidate::waiting).collect();
    let (min_q, max_q) = queues.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &q| (lo.min(q), hi.max(q)));
    candidates.iter().map(move |c| {
        let queue = c.waiting().map_or(0.0, |q| if max_q > min_q { (max_q - q) / (max_q - min_q) } else { 1.0 });
        let kv = c.kv_usage().map_or(0.0, |kv| 1.0 - kv);
        2.0 * queue + 2.0 * kv + 3.0 * c.match_ratio(request)
    })
}

/// llm-d's optimized-baseline profile as `tools/kind-llmd.sh` deploys it: [`LlmdDefault`] plus no-hit-lru ×2, which
/// sends requests no replica has a prefix for to the replica that least recently took such a request.
#[derive(Debug, Default)]
pub struct LlmdOptimized(ColdLru);

impl Policy for LlmdOptimized {
    fn name(&self) -> &'static str {
        "llmd-optimized"
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], _: &mut PolicyRng) -> usize {
        let base = llmd_default_scores(request, candidates);
        if !ColdLru::is_cold(candidates) {
            return first_max(base);
        }
        let chosen = first_max(base.zip(self.0.scores(candidates)).map(|(s, lru)| s + 2.0 * lru));
        self.0.record(candidates[chosen].replica.addr);
        chosen
    }
}

/// llm-d's "optimized baseline" guide: prefix-cache-affinity-filter (match ratio ≥ 0.8 sticky, unless the best
/// sticky replica's TTFT estimate is > 18 s worse) then token-load-scorer on router-local in-flight uncached tokens.
#[derive(Debug)]
pub struct LlmdGuide;

impl LlmdGuide {
    const AFFINITY_THRESHOLD: f64 = 0.8;
    const MAX_TTFT_PENALTY_MS: f64 = 18_000.0;
    const PEAK_PREFILL_TOKENS_PER_SEC: f64 = 15_928.0;
    const TOKEN_LOAD_THRESHOLD: f64 = 4_194_304.0;

    fn ttft_ms(c: &Candidate) -> f64 {
        c.replica.pending_prefill_tokens as f64 / Self::PEAK_PREFILL_TOKENS_PER_SEC * 1000.0
    }
}

impl Policy for LlmdGuide {
    fn name(&self) -> &'static str {
        "llmd-guide"
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], _: &mut PolicyRng) -> usize {
        let sticky: Vec<bool> = candidates.iter().map(|c| c.match_ratio(request) >= Self::AFFINITY_THRESHOLD).collect();
        let best = |want: bool| {
            (0..candidates.len()).filter(|&i| sticky[i] == want).map(|i| Self::ttft_ms(&candidates[i])).reduce(f64::min)
        };
        let keep_sticky_only = match (best(true), best(false)) {
            (None, _) => false,
            (Some(s), Some(n)) => s - n <= Self::MAX_TTFT_PENALTY_MS,
            (Some(_), None) => true,
        };
        first_max(candidates.iter().enumerate().map(|(i, c)| {
            if keep_sticky_only && !sticky[i] {
                return f64::NEG_INFINITY;
            }
            let load = (c.replica.pending_prefill_tokens + c.uncached_tokens(request)) as f64;
            1.0 - load.min(Self::TOKEN_LOAD_THRESHOLD) / Self::TOKEN_LOAD_THRESHOLD
        }))
    }
}

/// SGLang router `cache_aware` (CLI defaults): shortest queue when load is imbalanced, else the deepest prefix
/// match if more than 30% of the prompt matches, else least load. Load is router-local in-flight requests.
#[derive(Debug)]
pub struct SglangCacheAware;

impl Policy for SglangCacheAware {
    fn name(&self) -> &'static str {
        "sglang-cache-aware"
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], rng: &mut PolicyRng) -> usize {
        let loads: Vec<u32> = candidates.iter().map(|c| c.replica.active).collect();
        let (min, max) = (*loads.iter().min().unwrap_or(&0), *loads.iter().max().unwrap_or(&0));
        let imbalanced = max.saturating_sub(min) > 64 && max as f32 > min as f32 * 1.5;
        let deepest = first_max(candidates.iter().map(|c| c.matched_blocks as f64));
        if !imbalanced && candidates[deepest].match_ratio(request) > 0.3 {
            return deepest;
        }
        min_random(loads.iter().map(|&l| f64::from(l)), rng)
    }
}

/// NVIDIA Dynamo KV router default cost (lower is better): uncached + active prefill blocks, plus active decode
/// blocks; argmin with temperature 0. Dynamo counts in engine blocks; token counts are converted at 64 per block.
#[derive(Debug)]
pub struct Dynamo;

impl Policy for Dynamo {
    fn name(&self) -> &'static str {
        "dynamo"
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], _: &mut PolicyRng) -> usize {
        let block = BLOCK_TOKENS as f64;
        first_max(candidates.iter().map(|c| {
            let prefill = (c.replica.pending_prefill_tokens + c.uncached_tokens(request)) as f64 / block;
            let decode = c.replica.active_tokens as f64 / block;
            -(prefill + decode)
        }))
    }
}

fn min_random(values: impl Iterator<Item = f64>, rng: &mut PolicyRng) -> usize {
    let values: Vec<f64> = values.collect();
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let ties: Vec<usize> = (0..values.len()).filter(|&i| values[i] <= min).collect();
    // Empty only if every value is NaN.
    ties.get(rng.below(ties.len().max(1))).copied().unwrap_or(0)
}

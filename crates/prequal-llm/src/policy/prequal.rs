use super::{Candidate, Policy, PolicyRng, Request, cold_lru::ColdLru, rendezvous, share::RouterShare};

/// Prefix affinity balanced against load and memory pressure: llm-d's optimized-baseline scoring plus prefix heat,
/// fresh load on warm prefixes, overload shedding and multi-router-safe cold placement.
///
/// Scores each replica `prefix × 3 / heat + queue × 2 + free KV × 2 − overload + cold` and takes the best:
/// - **prefix**: fraction of the prompt's blocks believed cached there.
/// - **queue**: scraped `waiting`, min-max normalised across the fleet (llm-d's queue scorer). For a prompt some
///   replica already holds it is `max(waiting, this router's own requests awaiting first token)`: scrapes are tens of
///   ms stale, so without it every request between two scrapes herds onto the same holder of a shared or hot prefix
///   (burst, zipf). Cold prompts keep the scraped queue: on them fresh load flips on single in-flight prefills and
///   outweighs the cold term's rank steps, re-creating the uneven placement the cold term exists to prevent — that,
///   not spilling at light load, is why applying it everywhere lost to llm-d's optimized baseline.
/// - **free KV**: 1 − reported KV-cache usage. Pure stickiness overfills a busy replica's KV cache, and the engine's
///   evictions then cost more hits than stickiness saves; this term relieves that pressure.
/// - **heat**: a prompt key ([`Request::key`]) carrying more than its holders' fair share of traffic
///   (`share × replicas / holders`, at least 1) has its prefix weight divided by that ratio, so a popular prefix
///   spreads by load onto more replicas while an ordinary one keeps full affinity. The key is the first block past
///   what every replica holds, not the prompt's first block: behind a system prompt common to all traffic (Mooncake's
///   conversation and tool traces) a first-block key reads as 100% of traffic and erased affinity for every
///   conversation (hits 20% → 10%), and tied every new conversation to the same rendezvous replica.
/// - **overload**: `− 6 × max(0, load / mean load − 1.5)`, where load is requests in flight (running + queued)
///   averaged over seconds (`Replica::smoothed_load`). Nothing else above makes an ordinary prefix
///   leave its holder: queue and KV terms are bounded below the prefix weight. So when engines slow with batch size
///   (or the host slows them all) the replica whose prefixes hash heaviest crosses its knee first and keeps receiving
///   its traffic until it saturates — a bistable collapse. This term sheds only from a replica well past the fleet's
///   sustained load, so ordinary imbalance and arrival noise keep full affinity.
/// - **cold**: a placement score when no replica holds any of the prompt. As the fleet's only router: `+ 2 ×` llm-d's
///   no-hit-lru (`ColdLru`), which deals new prefixes out evenly; hash placement leaves replicas holding unequal
///   numbers of prefixes, and since an ordinary prefix never moves, that imbalance persists. Once other routers carry
///   a real share of the load (`RouterShare`), each one's LRU would place every prefix again on a different
///   replica (duplicated KV, hits ~70%), so cold placement switches to `+ 4 ×` the key's rendezvous ranking, which
///   all routers share. Weighted 2 like the LRU, queue and KV terms overrode it often enough that routers
///   disagreed, and each missed the prefixes the other had placed off their hash home.
/// - **home** (opt-in, `prequal-home`): on replicas whose cache model was never calibrated (engines without prefix
///   counters, such as SGLang), a cold prompt goes back to where its key last went. The model is then sized at an
///   assumed 4 bytes per token and forgets prefixes the engine still holds when prompts run larger; where the
///   assumption holds it costs ~1 point of hits and some tail, so it is not the default.
///
/// Remaining ties go by rendezvous hashing of the prompt's key. (Measured alternatives that lost: per-request TTFT
/// minimisation, sticky holders with escape rules, confidence-weighted matches, router-side late binding, overload on
/// smoothed queue instead of in-flight load, restricting cold LRU to a key's top rendezvous choices; pricing the
/// expected reuse of what a placement evicts from a replica's LRU tail, with or without crediting a recurring
/// prompt's own retention; a token-weighted queue. Results: `docs/benchmarks.md`.)
///
/// [`Prequal::default`] is the tuned policy; [`by_name`](super::by_name) builds the experimental variants.
#[derive(Debug)]
pub struct Prequal {
    pub(crate) prefix_weight: f64,
    pub(crate) queue_weight: f64,
    pub(crate) kv_weight: f64,
    /// Queue term counts requests in flight (running + waiting, or this router's active requests) instead of only
    /// waiting ones: engines slow down with batch size before anything queues.
    pub(crate) rif_queue: bool,
    /// Which prompts' queue term also counts this router's own requests awaiting first token (see above).
    pub(crate) fresh: Fresh,
    pub(crate) overload_weight: f64,
    /// Load above the fleet mean, as a fraction of it, that costs nothing.
    pub(crate) overload_margin: f64,
    pub(crate) cold_weight: f64,
    /// The cold weight once placement goes by hash.
    pub(crate) hash_weight: f64,
    pub(crate) cold: ColdLru,
    pub(crate) share: RouterShare,
    /// Cold placement goes by hash once this router's share of the fleet's load drops below this (0: never).
    pub(crate) hash_below_share: f64,
    pub(crate) home: bool,
}

impl Default for Prequal {
    fn default() -> Self {
        Self {
            prefix_weight: 3.0,
            queue_weight: 2.0,
            kv_weight: 2.0,
            rif_queue: false,
            fresh: Fresh::Warm,
            overload_weight: 6.0,
            overload_margin: 0.5,
            cold_weight: 2.0,
            hash_weight: 4.0,
            cold: ColdLru::default(),
            share: RouterShare::default(),
            hash_below_share: 0.75,
            home: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fresh {
    Never,
    /// Prompts some replica holds part of.
    Warm,
    Always,
}

/// Replicas matching at least this fraction of the best match hold the prefix.
const HOLDER_FRACTION: f64 = 0.9;
/// Scores within this of the best are ties.
const TIE: f64 = 1e-6;

impl Prequal {
    fn queues(&self, candidates: &[Candidate], fresh: bool) -> Vec<f64> {
        candidates
            .iter()
            .map(|c| match (self.rif_queue, fresh) {
                (true, _) => c.replica.in_flight(),
                (false, true) => c.waiting().unwrap_or(0.0).max(f64::from(c.replica.prefilling)),
                (false, false) => c.waiting().unwrap_or(0.0),
            })
            .collect()
    }

    /// Per candidate, how strongly a cold prompt belongs there (0 to 1), weighted.
    fn placement(&self, request: &Request, candidates: &[Candidate], shared_fleet: bool) -> Vec<f64> {
        let home = (self.home)
            .then_some(request.home)
            .flatten()
            .and_then(|home| candidates.iter().position(|c| c.addr() == home && !c.replica.calibrated()));
        match (home, shared_fleet) {
            (Some(home), _) => (0..candidates.len()).map(|i| if i == home { self.cold_weight } else { 0.0 }).collect(),
            (None, true) => {
                ColdLru::hash_scores(request.key, candidates).iter().map(|s| s * self.hash_weight).collect()
            }
            (None, false) => self.cold.scores(candidates).iter().map(|s| s * self.cold_weight).collect(),
        }
    }
}

impl Policy for Prequal {
    fn name(&self) -> &'static str {
        "prequal"
    }

    fn pick(&self, request: &Request, candidates: &[Candidate], _: &mut PolicyRng) -> usize {
        let shared_fleet = self.hash_below_share > 0.0 && self.share.observe(candidates) < self.hash_below_share;
        let ratios: Vec<f64> = candidates.iter().map(|c| c.match_ratio(request)).collect();
        let best_ratio = ratios.iter().copied().fold(0.0, f64::max);
        let holders = ratios.iter().filter(|&&r| best_ratio > 0.0 && r >= HOLDER_FRACTION * best_ratio).count().max(1);
        let heat = (request.heat_share * candidates.len() as f64 / holders as f64).max(1.0);
        let is_cold = ColdLru::is_cold(candidates);
        let fresh = match self.fresh {
            Fresh::Never => false,
            Fresh::Warm => !is_cold,
            Fresh::Always => true,
        };
        let queues = self.queues(candidates, fresh);
        let (min_q, max_q) =
            queues.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &q| (lo.min(q), hi.max(q)));
        let mean_load =
            (candidates.iter().map(|c| c.replica.smoothed_load).sum::<f64>() / candidates.len() as f64).max(1.0);
        let cold = self.cold_weight > 0.0 && is_cold;
        let placement =
            if cold { self.placement(request, candidates, shared_fleet) } else { vec![0.0; candidates.len()] };
        let scores: Vec<f64> = candidates
            .iter()
            .zip(&queues)
            .zip(&ratios)
            .zip(&placement)
            .map(|(((c, &q), &ratio), &placement)| {
                let queue = if max_q > min_q { (max_q - q) / (max_q - min_q) } else { 1.0 };
                let kv = 1.0 - c.kv_usage().unwrap_or(0.0);
                let overload = (c.replica.smoothed_load / mean_load - 1.0 - self.overload_margin).max(0.0);
                let score = self.prefix_weight / heat * ratio + self.queue_weight * queue + self.kv_weight * kv
                    - self.overload_weight * overload
                    + placement;
                if score.is_nan() { f64::NEG_INFINITY } else { score }
            })
            .collect();
        let best = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // Non-empty: `best` is some score, and `-inf - TIE` is `-inf` when every score is.
        let chosen = (0..scores.len())
            .filter(|&i| scores[i] >= best - TIE)
            .max_by_key(|&i| rendezvous(request.key, &candidates[i].replica.addr))
            .unwrap_or(0);
        if cold {
            self.cold.record(candidates[chosen].replica.addr);
        }
        chosen
    }
}

#[cfg(test)]
#[path = "prequal_tests.rs"]
mod tests;

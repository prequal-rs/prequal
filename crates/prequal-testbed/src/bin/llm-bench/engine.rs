//! A simulated engine's scheduling state, shared by the HTTP engine (`sim`) and virtual-time simulation (`vsim`) so
//! both run identical logic: admission in (priority, arrival) order, as vLLM V1's `--scheduling-policy priority`
//! (FCFS when priorities are equal), bounded by sequence slots, KV blocks and the step's token budget, prefix-cache
//! reuse, chunked prefill, and decode steps that slow down with batch size.
//!
//! KV for the output is reserved at admission unless the spec sets `preempt`; then, as vLLM V1, a sequence gets
//! blocks as it decodes, and when none are left the lowest-priority (latest-arrived among equals) running sequence is
//! preempted: its blocks are freed and it is requeued to recompute its prompt and the tokens it already emitted. No
//! waiting job is admitted in a step that preempted.

use std::collections::VecDeque;

use prequal_llm::EngineStats;

use crate::{
    batch,
    cache::{BLOCK_TOKENS, BlockCache, Lease, block_hashes, blocks_for, token_count},
    preset::EngineSpec,
};

/// Metrics a simulated engine's scrapes withhold, as some engines don't export them: KV capacity (routers keep
/// their default index size) and prefix-cache counters (SGLang; routers can't calibrate their index).
#[derive(Clone, Copy, Debug, Default)]
pub struct Hidden {
    pub kv_capacity: bool,
    pub prefix_counters: bool,
}

/// A request as the engine sees it; `tag` identifies it to the caller.
pub struct Job<T> {
    hashes: Vec<u64>,
    prompt: u64,
    output: u64,
    /// Lower is admitted first and preempted last.
    priority: u64,
    tag: T,
    /// Submission order, kept across preemptions.
    arrival: u64,
    /// Tokens emitted before a preemption.
    emitted: u64,
}

impl<T> Job<T> {
    pub fn new(hashes: Vec<u64>, prompt: u64, output: u64, priority: u64, tag: T) -> Self {
        Self { hashes, prompt, output, priority, tag, arrival: 0, emitted: 0 }
    }

    pub fn tag(&self) -> &T {
        &self.tag
    }

    fn rank(&self) -> (u64, u64) {
        (self.priority, self.arrival)
    }
}

struct Running<T> {
    job: Job<T>,
    lease: Lease,
    emitted: u64,
    /// Uncached context tokens not yet prefilled; 0 once decoding (a prefill always has ≥ 1 uncached token).
    prefill_left: u64,
    blocks: u64,
}

pub struct EngineCore<T> {
    spec: EngineSpec,
    cache: BlockCache,
    queue: VecDeque<Job<T>>,
    running: Vec<Running<T>>,
    /// Prefill chunks of the step in progress, one per running sequence.
    chunks: Vec<u64>,
    submitted: u64,
    pub prefix_queries: u64,
    pub prefix_hits: u64,
    pub preemptions: u64,
}

impl<T> EngineCore<T> {
    pub fn new(spec: EngineSpec) -> Self {
        let cache = BlockCache::new(spec.kv_tokens / BLOCK_TOKENS);
        Self {
            spec,
            cache,
            queue: VecDeque::new(),
            running: Vec::new(),
            chunks: Vec::new(),
            submitted: 0,
            prefix_queries: 0,
            prefix_hits: 0,
            preemptions: 0,
        }
    }

    /// `(block hashes, token count)` of prompt text under this engine's tokenizer.
    pub fn tokenize(&self, text: &[u8]) -> (Vec<u64>, u64) {
        (block_hashes(text, self.spec.token_bytes), token_count(text, self.spec.token_bytes))
    }

    pub fn cached_prefix(&self, hashes: &[u64]) -> usize {
        self.cache.cached_prefix(hashes)
    }

    pub fn cached_block_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.cache.block_ids()
    }

    pub fn num_blocks(&self) -> u64 {
        self.spec.kv_tokens / BLOCK_TOKENS
    }

    pub fn submit(&mut self, mut job: Job<T>) {
        job.arrival = self.submitted;
        self.submitted += 1;
        self.enqueue(job);
    }

    fn enqueue(&mut self, job: Job<T>) {
        let at = self.queue.partition_point(|queued| queued.rank() < job.rank());
        self.queue.insert(at, job);
    }

    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    pub fn running(&self) -> usize {
        self.running.len()
    }

    /// Blocks held by running sequences (vLLM's `kv_cache_usage_perc` numerator).
    pub fn active_blocks(&self) -> u64 {
        self.cache.active_blocks()
    }

    /// What a scrape of this engine's `/metrics` reports, minus what `hidden` withholds.
    pub fn stats(&self, hidden: Hidden) -> EngineStats {
        let mut stats = EngineStats::new(
            self.running() as f64,
            self.waiting() as f64,
            self.active_blocks() as f64 / self.num_blocks() as f64,
        );
        stats.prefix_queries = (!hidden.prefix_counters).then_some(self.prefix_queries as f64);
        stats.prefix_hits = (!hidden.prefix_counters).then_some(self.prefix_hits as f64);
        stats.cache_tokens = (!hidden.kv_capacity).then(|| (self.num_blocks() * BLOCK_TOKENS) as f64);
        stats
    }

    /// Admits what fits, calling `admitted(tag, cached_tokens)` for each first admission, and plans the next step.
    /// Returns its duration in µs (scaled by the spec's time scale), or `None` when there is nothing to run.
    pub fn start_step(&mut self, mut admitted: impl FnMut(&T, u64)) -> Option<f64> {
        let preempted = self.spec.preempt && self.grow_running();
        while !preempted
            && self.running.len() < self.spec.max_seqs
            && batch::has_spare_budget(self.running.iter().map(|r| r.prefill_left), self.spec.max_batched_tokens)
        {
            let Some(next) = self.queue.front_mut() else { break };
            let context = next.prompt + next.emitted;
            let reserve = blocks_for(if self.spec.preempt { context + 1 } else { next.prompt + next.output });
            let hashes = std::mem::take(&mut next.hashes);
            let lease = match self.cache.admit(hashes, context, reserve, self.running.is_empty()) {
                Ok(lease) => lease,
                Err(hashes) => {
                    next.hashes = hashes;
                    break;
                }
            };
            let job = self.queue.pop_front().expect("front exists");
            if job.emitted == 0 {
                self.prefix_queries += job.prompt;
                self.prefix_hits += lease.hit_tokens;
                admitted(&job.tag, lease.hit_tokens);
            }
            let prefill_left = context - lease.hit_tokens;
            self.running.push(Running { emitted: job.emitted, job, lease, prefill_left, blocks: reserve });
        }
        if self.running.is_empty() {
            return None;
        }
        let left: Vec<u64> = self.running.iter().map(|r| r.prefill_left).collect();
        self.chunks = batch::prefill_chunks(&left, self.spec.max_batched_tokens);
        let decoding = left.iter().filter(|&&l| l == 0).count();
        Some(self.spec.step_us(decoding, self.chunks.iter().sum()))
    }

    /// Gives each decoding sequence the block its next token needs, preempting while KV is exhausted. Returns whether
    /// anything was preempted.
    fn grow_running(&mut self) -> bool {
        let mut preempted = false;
        let mut i = 0;
        while i < self.running.len() {
            let r = &mut self.running[i];
            if r.prefill_left > 0 || blocks_for(r.job.prompt + r.emitted + 1) <= r.blocks {
                i += 1;
            } else if self.cache.grow(&mut r.lease, 1) {
                r.blocks += 1;
                i += 1;
            } else {
                let victim = (0..self.running.len()).max_by_key(|&v| self.running[v].job.rank()).expect("non-empty");
                self.preempt(victim);
                preempted = true;
                // Retry the same sequence (or, if it was the victim, the one now at `i`).
                i -= usize::from(victim < i);
            }
        }
        preempted
    }

    fn preempt(&mut self, index: usize) {
        let running = self.running.remove(index);
        let mut job = running.job;
        job.hashes = self.cache.release(running.lease);
        job.emitted = running.emitted;
        self.preemptions += 1;
        self.enqueue(job);
    }

    /// Completes the planned step: `token(tag, is_first)` for each sequence that emits a token, then releases and
    /// returns the jobs that are done or for which `cancelled(tag)` holds.
    pub fn finish_step(&mut self, mut token: impl FnMut(&T, bool), cancelled: impl Fn(&T) -> bool) -> Vec<Job<T>> {
        for (r, chunk) in self.running.iter_mut().zip(std::mem::take(&mut self.chunks)) {
            let was_decoding = r.prefill_left == 0;
            r.prefill_left -= chunk;
            if r.prefill_left > 0 {
                continue;
            }
            if !was_decoding {
                self.cache.commit(&mut r.lease);
            }
            r.emitted += 1;
            token(&r.job.tag, r.emitted == 1);
        }
        let cache = &mut self.cache;
        self.running
            .extract_if(.., |r| r.emitted >= r.job.output || cancelled(&r.job.tag))
            .map(|done| {
                cache.release(done.lease);
                done.job
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::Preset;

    fn job(priority: u64, output: u64, tag: i32) -> Job<i32> {
        Job::new(Vec::new(), BLOCK_TOKENS, output, priority, tag)
    }

    #[test]
    fn queue_orders_by_priority_then_arrival() {
        let mut engine = EngineCore::new(Preset::Default.spec());
        for (tag, priority) in [(0, 5), (1, 1), (2, 5), (3, 0), (4, 1)] {
            engine.submit(job(priority, 1, tag));
        }
        let tags: Vec<i32> = engine.queue.iter().map(|j| j.tag).collect();
        assert_eq!(tags, [3, 1, 4, 0, 2]);
    }

    /// Runs to completion; returns tags in finishing order and each tag's emitted-token count.
    fn drain(engine: &mut EngineCore<i32>) -> (Vec<i32>, Vec<u64>) {
        let (mut done, mut emitted) = (Vec::new(), vec![0; 3]);
        while engine.start_step(|_, _| {}).is_some() {
            let finished = engine.finish_step(|&tag, _| emitted[tag as usize] += 1, |_| false);
            done.extend(finished.iter().map(|j| j.tag));
        }
        (done, emitted)
    }

    #[test]
    fn preemption_evicts_lowest_priority_and_recomputes_it() {
        // 8 blocks: three 1-block prompts admit, but their outputs (40 tokens = 3 blocks each) can't all grow.
        let spec = EngineSpec { kv_tokens: 8 * BLOCK_TOKENS, preempt: true, ..Preset::Default.spec() };
        let mut engine = EngineCore::new(spec);
        for (tag, priority) in [(0, 2), (1, 0), (2, 1)] {
            engine.submit(job(priority, 40, tag));
        }
        let (done, emitted) = drain(&mut engine);
        assert!(engine.preemptions > 0);
        assert_eq!(done[0], 1, "highest priority is never preempted");
        assert_eq!(emitted, [40, 40, 40], "recompute doesn't re-emit tokens");
        assert_eq!(engine.active_blocks(), 0);
    }

    #[test]
    fn reservation_mode_never_preempts() {
        let spec = EngineSpec { kv_tokens: 8 * BLOCK_TOKENS, ..Preset::Default.spec() };
        let mut engine = EngineCore::new(spec);
        for (tag, priority) in [(0, 2), (1, 0), (2, 1)] {
            engine.submit(job(priority, 40, tag));
        }
        assert_eq!(drain(&mut engine).1, [40, 40, 40]);
        assert_eq!(engine.preemptions, 0);
    }
}

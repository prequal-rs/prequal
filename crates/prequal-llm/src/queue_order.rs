//! Request priorities for engines that schedule by them (vLLM `--scheduling-policy priority`: waiting requests run
//! in `(priority, arrival)` order, and KV exhaustion preempts the largest). Shorter jobs first cuts queueing, and in
//! KV-bound engines "shorter" means less KV held over the decode, so the cost is [`kv_cost`] of a predicted output
//! length ([`OutputHistory`]). A [`Deadline`] turns cost into a priority that a later arrival can only beat by a
//! bounded margin, so long jobs can't starve. Evidence: `research/prequal/queue-order-findings.md` (rust_stuff).

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use crate::prompt::Prompt;

/// Prefix depths (in [`crate::BLOCK_BYTES`] blocks) that key output history: 2 KB and 4 KB, about a system prompt
/// and its first turn.
const KEY_DEPTHS: [usize; 2] = [8, 16];
/// Recent costs a [`Deadline`] ranks against.
const WINDOW: usize = 1024;

/// A prompt's history keys, shallowest first; `None` where the prompt is shorter than the depth.
pub type HistoryKeys = [Option<u64>; KEY_DEPTHS.len()];

/// KV a request holds while decoding, in token × output-token units: its context grows from `prompt_tokens` and it
/// lives about `output_tokens` steps.
pub fn kv_cost(prompt_tokens: u64, output_tokens: f64) -> f64 {
    output_tokens * (prompt_tokens as f64 + output_tokens)
}

#[derive(Clone, Copy, Debug, Default)]
struct LogMean {
    sum: f64,
    count: f64,
}

impl LogMean {
    fn add(&mut self, tokens: u64) {
        self.sum += (tokens.max(1) as f64).ln();
        self.count += 1.0;
    }

    fn geometric(&self) -> Option<f64> {
        (self.count > 0.0).then(|| (self.sum / self.count).exp())
    }
}

/// Predicts output length as the geometric mean of past outputs under the deepest known shared prefix. Requests
/// behind one system prompt or tool schema tend to answer alike; on Mooncake's tool-agent trace this ranks outputs
/// at Spearman 0.75. Unbounded: one entry per distinct prefix seen.
#[derive(Debug, Default)]
pub struct OutputHistory {
    levels: [HashMap<u64, LogMean>; KEY_DEPTHS.len()],
    global: LogMean,
}

impl OutputHistory {
    /// The prefix hashes `prompt` is predicted and later observed under.
    pub fn keys(prompt: &Prompt) -> HistoryKeys {
        KEY_DEPTHS.map(|depth| prompt.blocks.get(depth - 1).copied())
    }

    /// `None` until anything has completed.
    pub fn predict(&self, keys: &HistoryKeys) -> Option<f64> {
        self.levels
            .iter()
            .zip(keys)
            .rev()
            .find_map(|(level, key)| level.get(key.as_ref()?)?.geometric())
            .or_else(|| self.global.geometric())
    }

    /// Learns a completed request's output length.
    pub fn observe(&mut self, keys: &HistoryKeys, output_tokens: u64) {
        for (level, key) in self.levels.iter_mut().zip(keys) {
            if let Some(key) = key {
                level.entry(*key).or_default().add(output_tokens);
            }
        }
        self.global.add(output_tokens);
    }
}

/// Maps a cost to a delay of up to `handicap`, by its rank among recent costs: priority = arrival + delay, so a
/// request is overtaken only by cheaper ones arriving less than `handicap` after it, whatever the cost's scale.
#[derive(Debug)]
pub struct Deadline {
    handicap: Duration,
    recent: VecDeque<f64>,
    sorted: Vec<f64>,
}

impl Deadline {
    /// Delays of up to `handicap`.
    pub fn new(handicap: Duration) -> Self {
        Self { handicap, recent: VecDeque::with_capacity(WINDOW), sorted: Vec::with_capacity(WINDOW) }
    }

    /// The delay for `cost` (`None`: unknown, ranked median), recording the cost in the window.
    pub fn delay(&mut self, cost: Option<f64>) -> Duration {
        let Some(cost) = cost.filter(|c| c.is_finite()) else { return self.handicap / 2 };
        let below = self.sorted.partition_point(|&c| c < cost);
        let equal = self.sorted[below..].partition_point(|&c| c <= cost);
        let rank = match self.sorted.len() {
            0 => 0.5,
            n => (below as f64 + equal as f64 / 2.0) / n as f64,
        };
        if self.recent.len() == WINDOW {
            let oldest = self.recent.pop_front().expect("window is full");
            let at = self.sorted.partition_point(|&c| c < oldest);
            self.sorted.remove(at);
        }
        self.recent.push_back(cost);
        let at = self.sorted.partition_point(|&c| c < cost);
        self.sorted.insert(at, cost);
        self.handicap.mul_f64(rank)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(system: u8, turn: u8) -> Prompt {
        Prompt::from_text(&[[system; 8 * crate::BLOCK_BYTES], [turn; 8 * crate::BLOCK_BYTES]].concat())
    }

    #[test]
    fn history_prefers_deepest_known_prefix() {
        let mut history = OutputHistory::default();
        let (short, long, fresh) = (prompt(b'a', b'x'), prompt(b'a', b'y'), prompt(b'a', b'z'));
        let keys = |p: &Prompt| OutputHistory::keys(p);
        assert_eq!(history.predict(&keys(&short)), None);
        history.observe(&keys(&short), 10);
        history.observe(&keys(&long), 1000);
        let predict = |p: &Prompt| history.predict(&keys(p)).unwrap().round();
        assert_eq!(predict(&short), 10.0);
        assert_eq!(predict(&long), 1000.0);
        assert_eq!(predict(&fresh), 100.0, "shares only the system prompt: geometric mean of both");
        assert_eq!(predict(&prompt(b'b', b'x')), 100.0, "unknown prefix: global mean");
    }

    #[test]
    fn short_prompts_have_no_deep_keys() {
        let keys = OutputHistory::keys(&Prompt::from_text(&[b'q'; 3 * crate::BLOCK_BYTES]));
        assert_eq!(keys, [None, None]);
    }

    #[test]
    fn deadline_delays_by_rank_within_handicap() {
        let mut deadline = Deadline::new(Duration::from_secs(10));
        assert_eq!(deadline.delay(Some(5.0)), Duration::from_secs(5), "first cost ranks median");
        for cost in 1..=99 {
            deadline.delay(Some(cost as f64 * 100.0));
        }
        assert!(deadline.delay(Some(1.0)) < Duration::from_millis(200));
        assert!(deadline.delay(Some(1e9)) > Duration::from_millis(9_800));
        assert_eq!(deadline.delay(None), Duration::from_secs(5));
        assert!(deadline.delay(Some(1e12)) <= Duration::from_secs(10));
    }

    #[test]
    fn deadline_window_forgets_old_costs() {
        let mut deadline = Deadline::new(Duration::from_secs(1));
        (0..WINDOW).for_each(|_| _ = deadline.delay(Some(1e6)));
        (0..WINDOW).for_each(|_| _ = deadline.delay(Some(1.0)));
        assert_eq!(deadline.sorted.len(), WINDOW);
        assert!(deadline.delay(Some(10.0)) > Duration::from_millis(990), "the old large costs are gone");
    }
}

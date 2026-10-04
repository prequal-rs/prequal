//! `--engine-priority-handicap`: stamps vLLM's `priority` request field so engines started with
//! `--scheduling-policy priority` run cheap requests first (`prequal_llm::queue_order`). Off by default: vLLM rejects
//! a non-zero priority from any other policy. Lower runs first, so the stamp is
//! `-objective × OBJECTIVE_BAND + unix ms + delay(predicted KV cost)`: InferenceObjective priority dominates, and within
//! one objective a request is overtaken only by cheaper ones arriving less than the handicap after it.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use prequal_llm::{
    Prompt, completion_tokens,
    queue_order::{Deadline, HistoryKeys, OutputHistory, kv_cost},
};

/// Milliseconds between objective levels, far above any arrival spread (~317 years).
const OBJECTIVE_BAND: i64 = 10_000_000_000_000;
/// Objective priorities beyond this share the extreme band, keeping stamps within `i64`.
const MAX_OBJECTIVE: i32 = 100_000;

pub struct EnginePriority {
    state: Mutex<State>,
}

struct State {
    history: OutputHistory,
    deadline: Deadline,
}

impl EnginePriority {
    pub fn new(handicap: Duration) -> Self {
        Self { state: Mutex::new(State { history: OutputHistory::default(), deadline: Deadline::new(handicap) }) }
    }

    /// The vLLM priority for a request with InferenceObjective priority `objective`.
    pub fn stamp(&self, objective: i32, prompt: &Prompt) -> i64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        self.stamp_at(objective, prompt, now)
    }

    fn stamp_at(&self, objective: i32, prompt: &Prompt, now: Duration) -> i64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let predicted = state.history.predict(&OutputHistory::keys(prompt));
        let delay = state.deadline.delay(predicted.map(|output| kv_cost(prompt.tokens, output)));
        let band = -i64::from(objective.clamp(-MAX_OBJECTIVE, MAX_OBJECTIVE)) * OBJECTIVE_BAND;
        band + (now + delay).as_millis() as i64
    }

    pub fn observe(&self, keys: &HistoryKeys, output_tokens: u64) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).history.observe(keys, output_tokens);
    }

    /// The stamped request body, and what its response teaches; `None` if the body isn't a JSON object.
    pub fn stamp_body(self: &Arc<Self>, objective: i32, prompt: &Prompt, body: &[u8]) -> Option<(Vec<u8>, Learning)> {
        let stamped = with_priority(body, self.stamp(objective, prompt))?;
        let learning = Learning {
            priority: Arc::clone(self),
            keys: OutputHistory::keys(prompt),
            output: OutputCounter::default(),
        };
        Some((stamped, learning))
    }
}

/// A stamped request's pending lesson: its output length, learned once a 200 response completes.
pub struct Learning {
    priority: Arc<EnginePriority>,
    keys: HistoryKeys,
    output: OutputCounter,
}

impl Learning {
    pub fn feed(&mut self, chunk: &[u8]) {
        self.output.feed(chunk);
    }

    pub fn finish(self) {
        if let Some(tokens) = self.output.tokens() {
            self.priority.observe(&self.keys, tokens);
        }
    }
}

/// `body` with `"priority":<priority>` as its last field; `None` unless it is a JSON object. vLLM keeps the last
/// duplicate key, so this overrides a client's own `priority`, as llm-d's `propagatePriority` does.
pub fn with_priority(body: &[u8], priority: i64) -> Option<Vec<u8>> {
    let open = body.iter().position(|b| !b.is_ascii_whitespace()).filter(|&at| body[at] == b'{')?;
    let close = body.iter().rposition(|b| !b.is_ascii_whitespace()).filter(|&at| body[at] == b'}')?;
    let empty = body[open + 1..close].iter().all(u8::is_ascii_whitespace);
    let field = format!("{}\"priority\":{priority}", if empty { "" } else { "," });
    Some([&body[..close], field.as_bytes(), &body[close..]].concat())
}

/// Counts a response's output tokens: `usage.completion_tokens` when the body carries it (non-streaming, or streaming
/// with `include_usage`), else one per SSE `data:` event, as vLLM streams a token per event. A key split across
/// chunks is missed; the event count stands in.
#[derive(Default)]
pub struct OutputCounter {
    events: u64,
    usage: Option<u64>,
}

impl OutputCounter {
    pub fn feed(&mut self, chunk: &[u8]) {
        self.usage = completion_tokens(chunk).or(self.usage);
        let events = chunk.windows(5).filter(|w| w == b"data:").count();
        let done = chunk.windows(12).filter(|w| w == b"data: [DONE]").count();
        self.events += (events - done) as u64;
    }

    pub fn tokens(&self) -> Option<u64> {
        self.usage.or((self.events > 0).then_some(self.events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_priority_as_last_field() {
        assert_eq!(with_priority(b" {\"model\":\"m\"}\n", -7).unwrap(), b" {\"model\":\"m\",\"priority\":-7}\n");
        assert_eq!(with_priority(br#"{"priority":-9}"#, 3).unwrap(), br#"{"priority":-9,"priority":3}"#);
        assert_eq!(with_priority(b"{ }", 3).unwrap(), br#"{ "priority":3}"#);
        assert_eq!(with_priority(b"{", 3), None);
        assert_eq!(with_priority(b"[1]", 3), None);
        assert_eq!(with_priority(b"", 3), None);
    }

    #[test]
    fn objective_bands_dominate_arrival_and_cost() {
        let priority = EnginePriority::new(Duration::from_secs(30));
        let prompt = Prompt::from_text(b"hi");
        let now = Duration::from_secs(1_800_000_000);
        let later = now + Duration::from_secs(3600);
        assert!(priority.stamp_at(5, &prompt, later) < priority.stamp_at(0, &prompt, now));
        assert!(priority.stamp_at(0, &prompt, later) < priority.stamp_at(-1, &prompt, now));
        let extreme = priority.stamp_at(i32::MIN, &prompt, now);
        assert!(extreme > 0 && priority.stamp_at(i32::MAX, &prompt, now) < 0);
    }

    #[test]
    fn learned_short_outputs_run_first() {
        let priority = EnginePriority::new(Duration::from_secs(30));
        let prompt = |system: u8, turn: &[u8]| {
            Prompt::from_text(&[&vec![system; 8 * prequal_llm::BLOCK_BYTES][..], turn].concat())
        };
        priority.observe(&OutputHistory::keys(&prompt(b't', b"call")), 5);
        priority.observe(&OutputHistory::keys(&prompt(b'e', b"essay")), 2000);
        let now = Duration::from_secs(1_800_000_000);
        // Same arrival, unseen turns: the tool prompt's prefix answered with 5 tokens, the essay prompt's with 2000.
        let (tool, essay) = (prompt(b't', b"next"), prompt(b'e', b"next"));
        assert!(priority.stamp_at(0, &tool, now) < priority.stamp_at(0, &essay, now));
        assert!(priority.stamp_at(0, &essay, now) <= now.as_millis() as i64 + 30_000);
    }

    #[test]
    fn counts_usage_else_events() {
        let mut streamed = OutputCounter::default();
        streamed.feed(b"data: {\"choices\":[{\"text\":\"a\"}]}\n\ndata: {\"choices\":[{\"text\":\"b\"}]}\n\n");
        streamed.feed(b"data: [DONE]\n\n");
        assert_eq!(streamed.tokens(), Some(2));
        let mut usage = OutputCounter::default();
        usage.feed(b"data: {\"x\":1}\n\ndata: {\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":41}}\n\n");
        assert_eq!(usage.tokens(), Some(41));
        assert_eq!(OutputCounter::default().tokens(), None);
    }
}

//! Replays a Mooncake-format request trace (`--trace`): JSONL lines of `timestamp` (ms from the start),
//! `input_length` and `output_length` (tokens) and `hash_ids`, prefix-chained ids of the prompt's 512-token blocks
//! (equal id at position i means equal first i+1 blocks). The public traces are Kimi's production samples published
//! with Mooncake (FAST'25; github.com/kvcache-ai/Mooncake `FAST25-release/traces`, Apache-2.0); fetch with
//! `tools/fetch-mooncake-traces.sh`. Each block id becomes a fixed pseudo-text, so prompts sharing leading ids
//! share leading text and both the router's and the engines' prefix hashing see the trace's sharing.

use std::{fs, io, path::Path, time::Duration};

use serde::Deserialize;

use crate::workload::{Stage, ascii, derive};

const TRACE_BLOCK_TOKENS: u64 = 512;

#[derive(Deserialize)]
struct Line {
    timestamp: f64,
    input_length: u64,
    output_length: u64,
    hash_ids: Vec<u64>,
}

pub struct Trace {
    lines: Vec<Line>,
    /// Trace seconds per simulated second.
    speed: f64,
}

impl Trace {
    /// Requests whose prompt plus output exceed `max_context` tokens are dropped, for engines with a smaller window.
    pub fn load(path: &Path, speed: f64, max_context: Option<u64>) -> io::Result<Self> {
        let mut lines = fs::read_to_string(path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<Line>, _>>()
            .map_err(io::Error::other)?;
        if let Some(max) = max_context {
            let all = lines.len();
            lines.retain(|l| l.input_length + l.output_length <= max);
            eprintln!("trace: dropped {} of {all} requests over {max} tokens", all - lines.len());
        }
        if lines.is_empty() || speed.is_nan() || speed <= 0.0 {
            return Err(io::Error::other("empty trace or non-positive --trace-speed"));
        }
        Ok(Self { lines, speed })
    }

    pub fn mean_output(&self) -> f64 {
        self.lines.iter().map(|l| l.output_length as f64).sum::<f64>() / self.lines.len() as f64
    }

    /// Prompt text and output tokens of the `index`-th request (wrapping).
    pub fn request(&self, index: u64) -> (String, u64) {
        let line = &self.lines[index as usize % self.lines.len()];
        let text = line
            .hash_ids
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let tokens = line.input_length.saturating_sub(i as u64 * TRACE_BLOCK_TOKENS).min(TRACE_BLOCK_TOKENS);
                ascii(derive(0, 5, id), tokens as usize)
            })
            .collect();
        (text, line.output_length.max(1))
    }

    /// Arrival times in simulated µs.
    pub fn arrivals_us(&self) -> Vec<u64> {
        self.lines.iter().map(|l| (l.timestamp * 1e3 / self.speed) as u64).collect()
    }

    /// `windows` equal stages over the replay, each at its realised mean rate.
    pub fn stages(&self, windows: usize) -> Vec<Stage> {
        let arrivals = self.arrivals_us();
        let windows = windows.max(1);
        let span = arrivals.last().copied().unwrap_or(0) + 1;
        let width = span.div_ceil(windows as u64);
        (0..windows as u64)
            .map(|w| {
                let count = arrivals.iter().filter(|&&at| at / width == w).count();
                let duration = Duration::from_micros(width);
                Stage { rate: (count as f64 / duration.as_secs_f64()).max(f64::MIN_POSITIVE), duration }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(lines: &str, speed: f64, max_context: Option<u64>) -> Trace {
        let name = format!("llm-bench-trace-{}-{max_context:?}.jsonl", std::process::id());
        let path = std::env::temp_dir().join(name);
        fs::write(&path, lines).unwrap();
        let trace = Trace::load(&path, speed, max_context).unwrap();
        fs::remove_file(path).unwrap();
        trace
    }

    #[test]
    fn max_context_drops_requests_that_do_not_fit() {
        let lines = "{\"timestamp\": 0, \"input_length\": 600, \"output_length\": 5, \"hash_ids\": [1, 2]}\n\
                     {\"timestamp\": 9, \"input_length\": 90, \"output_length\": 10, \"hash_ids\": [3]}\n";
        assert_eq!(trace(lines, 1.0, Some(100)).request(0).1, 10);
    }

    #[test]
    fn shared_block_ids_share_text_and_time_scales() {
        let t = trace(
            "{\"timestamp\": 0, \"input_length\": 600, \"output_length\": 5, \"hash_ids\": [1, 2]}\n\
             {\"timestamp\": 2000, \"input_length\": 700, \"output_length\": 7, \"hash_ids\": [1, 3]}\n",
            2.0,
            None,
        );
        let ((a, out_a), (b, out_b)) = (t.request(0), t.request(1));
        assert_eq!((out_a, out_b), (5, 7));
        let block = ascii(0, TRACE_BLOCK_TOKENS as usize).len();
        assert_eq!(a.len() * 700, b.len() * 600, "text length follows input tokens");
        assert_eq!(a[..block], b[..block]);
        assert_ne!(a[block..block + 8], b[block..block + 8]);
        assert_eq!(t.arrivals_us(), [0, 1_000_000]);
        let stages = t.stages(2);
        assert_eq!(stages.len(), 2);
        assert!(stages.iter().all(|s| s.rate > 1.9 && s.rate < 2.1), "one request per ~0.5 s window");
    }
}

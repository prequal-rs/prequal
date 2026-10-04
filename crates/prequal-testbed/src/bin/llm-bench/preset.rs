//! Engine timing/capacity models.

use std::str::FromStr;

use clap::ValueEnum;

/// Host contention in virtual time (`factor:start_s:duration_s`): every engine step starting in the window lasts
/// `factor` times longer, like timer oversleep on a starved box.
#[derive(Clone, Copy, Debug)]
pub struct Slowdown {
    pub factor: f64,
    pub start_us: u64,
    pub end_us: u64,
}

impl Slowdown {
    pub fn at(slowdown: Option<Self>, now_us: u64) -> f64 {
        slowdown.filter(|s| (s.start_us..s.end_us).contains(&now_us)).map_or(1.0, |s| s.factor)
    }
}

impl FromStr for Slowdown {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let parts: Vec<f64> =
            s.split(':').map(str::parse).collect::<Result<_, _>>().map_err(|e| format!("{s:?}: {e}"))?;
        let [factor, start, duration] = parts[..] else {
            return Err(format!("{s:?} is not factor:start_s:duration_s"));
        };
        Ok(Self { factor, start_us: (start * 1e6) as u64, end_us: ((start + duration) * 1e6) as u64 })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EngineSpec {
    pub max_seqs: usize,
    pub kv_tokens: u64,
    pub step_us: f64,
    pub per_seq_us: f64,
    pub prefill_us_per_token: f64,
    pub speed: f64,
    /// vLLM's `max_num_batched_tokens` per step; `None` prefills whole prompts in one step (no chunking).
    pub max_batched_tokens: Option<u64>,
    /// Wall-clock seconds per simulated second.
    pub time_scale: f64,
    /// Steps last a whole number (≥ 1) of these, as llm-d-inference-sim truncates every sleep to whole ms.
    pub step_quantum_us: Option<f64>,
    /// Prompt bytes per engine token: routers estimate 4, which a real tokenizer only approximates.
    pub token_bytes: usize,
    /// vLLM V1 KV allocation: output blocks as decoded, preempting on exhaustion (see `engine`); else reserved upfront.
    pub preempt: bool,
}

impl EngineSpec {
    /// Simulated decode tokens per second with a full batch.
    pub fn decode_throughput(&self) -> f64 {
        let step = (self.step_us + self.per_seq_us * self.max_seqs as f64) / self.speed;
        self.max_seqs as f64 * 1e6 / step
    }

    /// Wall-clock duration of one engine step.
    pub fn step_us(&self, decoding: usize, uncached_prefill: u64) -> f64 {
        let mut simulated =
            self.step_us + self.per_seq_us * decoding as f64 + self.prefill_us_per_token * uncached_prefill as f64;
        if let Some(q) = self.step_quantum_us {
            simulated = (simulated / q).floor().max(1.0) * q;
        }
        simulated / self.speed * self.time_scale
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Preset {
    Default,
    H100Qwen32b,
    /// llm-d-inference-sim as run by `tools/kind-llmd.sh ... loaded`.
    LlmdSimLoaded,
    /// llm-d-inference-sim as run by `tools/kind-cache.sh ... cache`.
    LlmdSimCache,
}

impl Preset {
    pub fn spec(self) -> EngineSpec {
        let base = EngineSpec {
            max_seqs: 32,
            kv_tokens: 40_000,
            step_us: 12_000.0,
            per_seq_us: 400.0,
            prefill_us_per_token: 60.0,
            speed: 1.0,
            max_batched_tokens: Some(2048),
            time_scale: 1.0,
            step_quantum_us: None,
            token_bytes: crate::cache::TOKEN_BYTES,
            preempt: false,
        };
        let loaded = EngineSpec {
            max_seqs: 32,
            kv_tokens: 160_000_000,
            step_us: 1_000.0 - 3_000.0 / 31.0,
            per_seq_us: 3_000.0 / 31.0,
            prefill_us_per_token: 4.0,
            max_batched_tokens: None,
            step_quantum_us: Some(1_000.0),
            ..base
        };
        match self {
            Self::Default => base,
            // TP=2: 32 GB/GPU at 3 TB/s ≈ 11 ms → 15 ms floor +200 µs/seq = 27.8 ms @64; 1e6/15.9k ≈ 63 µs/tok.
            Self::H100Qwen32b => EngineSpec {
                max_seqs: 256,
                kv_tokens: 307_328,
                step_us: 15_000.0,
                per_seq_us: 200.0,
                prefill_us_per_token: 63.0,
                max_batched_tokens: Some(8192),
                ..base
            },
            // 1 ms inter-token latency × a load factor rising linearly from 1 (one running) to 4 (32 running), i.e.
            // 903 µs + 96.8 µs/seq, truncated to whole ms; 4 µs/prefill token. KV (10M blocks) never binds.
            Self::LlmdSimLoaded => loaded,
            // `--max-num-seqs 8` (load factor 1 to 4 over 8 running), `--kv-cache-size 6000` prompt blocks plus room
            // for running outputs (the sim keeps only prompt blocks; this model reserves output too), and its dummy
            // tokenizer's ~6 bytes per token (7,420 tokens for the 45 KB prompts).
            Self::LlmdSimCache => EngineSpec {
                max_seqs: 8,
                kv_tokens: (6_000 + 8 * 63) * 16,
                step_us: 1_000.0 - 3_000.0 / 7.0,
                per_seq_us: 3_000.0 / 7.0,
                token_bytes: 6,
                ..loaded
            },
        }
    }
}

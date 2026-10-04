//! Command-line flags and what they resolve to: engine specs, the fleet child's arguments, the load schedule.

use std::{io, net::SocketAddr, path::PathBuf, time::Duration};

use clap::{Parser, ValueEnum};
use prequal_llm::PrefillSignal;

use crate::{
    order::QueueOrder,
    preset::{EngineSpec, Preset, Slowdown},
    trace::Trace,
    workload::{Generator, LogNormal, Popularity, SharedPrefixParams, Stage, Stages},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum WorkloadKind {
    Random,
    SharedPrefix,
}

#[derive(Parser)]
pub struct Args {
    /// Path to the `prequal-router` binary.
    #[arg(long)]
    pub router_bin: Option<PathBuf>,
    /// Router policy, passed through (e.g. prequal, round-robin, llmd-guide, sglang-cache-aware, dynamo).
    #[arg(long, default_value = "prequal")]
    pub policy: String,
    #[arg(long, default_value_t = 2)]
    pub routers: usize,
    /// Extra argument for every router process (repeatable).
    #[arg(long, allow_hyphen_values = true)]
    pub router_arg: Vec<String>,
    /// Skip routers and round-robin requests directly over the engines.
    #[arg(long)]
    pub direct: bool,
    /// Simulate in virtual time: same engines, workload and routing library, no processes or sockets, seconds per
    /// run. `--policy` is resolved by `prequal_llm::policy::by_name`; `--time-scale` and router flags are ignored.
    #[arg(long = "virtual")]
    pub virtual_time: bool,

    #[arg(long, default_value_t = 8)]
    pub engines: usize,
    /// Share of engines running at 0.6x speed (older GPUs).
    #[arg(long, default_value_t = 0.5)]
    pub slow_fraction: f64,
    #[arg(long, value_enum, default_value = "default")]
    pub engine_preset: Preset,
    /// Overrides the preset's sequence slots.
    #[arg(long)]
    pub max_seqs: Option<usize>,
    /// Overrides the preset's KV capacity (tokens, shared by running sequences and the prefix cache).
    #[arg(long)]
    pub kv_tokens: Option<u64>,
    /// Overrides the preset's per-step token budget for chunked prefill (vLLM `max_num_batched_tokens`).
    #[arg(long)]
    pub max_batched_tokens: Option<u64>,
    /// Overrides the preset's prompt bytes per engine token (the router always assumes 4).
    #[arg(long)]
    pub token_bytes: Option<usize>,
    /// Prefill each admitted prompt in a single step, ignoring the token budget.
    #[arg(long, conflicts_with = "max_batched_tokens")]
    pub no_chunked_prefill: bool,
    /// Allocate output KV as decoded and preempt on exhaustion, as vLLM V1, instead of reserving it at admission.
    #[arg(long)]
    pub preempt: bool,
    /// Wall-clock seconds per simulated second; scales engine steps, stage durations, rates and the timeout.
    #[arg(long, default_value_t = 1.0)]
    pub time_scale: f64,
    /// Virtual time only: `factor:start_s:duration_s` host contention stretching every engine step.
    #[arg(long)]
    pub host_slowdown: Option<Slowdown>,
    /// Virtual time only: what ends a request's prefill reservation (first-chunk, headers, estimate:<tok/s>, end).
    #[arg(long, default_value = "first-chunk")]
    pub prefill_signal: PrefillSignal,
    /// Virtual time only: routing tickets end at the first token instead of the response's end.
    #[arg(long)]
    pub end_at_first_token: bool,
    /// Virtual time only: print why requests missed, per popularity tier (see `diagnose`).
    #[arg(long)]
    pub diagnose: bool,
    /// Virtual time only: engines don't publish their KV capacity, so routers keep their default index size.
    #[arg(long)]
    pub hide_kv_capacity: bool,
    /// Virtual time only: engines don't publish prefix-cache counters (as SGLang), so routers can't calibrate.
    #[arg(long)]
    pub hide_prefix_counters: bool,
    /// Engines admit by a stamped priority instead of FCFS (see `order`): `oracle`, `noisy:<sigma>` or `history`,
    /// optionally `+kv`, then `@<seconds>` of aging. Over HTTP the load generator stamps the request body.
    #[arg(long)]
    pub queue_order: Option<QueueOrder>,

    /// Drive these running OpenAI-compatible servers (e.g. vLLM with `--scheduling-policy priority`) instead of
    /// spawning simulated engines; engine flags are then ignored.
    #[arg(long, value_delimiter = ',', conflicts_with = "virtual_time")]
    pub targets: Vec<SocketAddr>,
    /// The `model` each request names, which real servers require.
    #[arg(long)]
    pub model: Option<String>,
    /// Send prompts as token ids below this vocabulary size, one per pseudo-token, so a real tokenizer sees the
    /// workload's prompt lengths and shared prefixes exactly.
    #[arg(long)]
    pub token_vocab: Option<u32>,
    /// Drop `--trace` requests whose prompt plus output exceed this many tokens (the model's context window).
    #[arg(long)]
    pub max_context: Option<u64>,

    #[arg(long, value_enum, default_value = "random")]
    pub workload: WorkloadKind,
    /// Replay a Mooncake-format JSONL trace instead of `--workload` (see `trace`), in `--trace-windows` equal
    /// stages.
    #[arg(long, conflicts_with_all = ["rate", "stages", "closed_loop"])]
    pub trace: Option<PathBuf>,
    /// Trace seconds replayed per simulated second.
    #[arg(long, default_value_t = 1.0)]
    pub trace_speed: f64,
    #[arg(long, default_value_t = 6)]
    pub trace_windows: usize,
    /// Offered load as a multiple of the fleet's decode capacity (used when neither --rate nor --stages is given).
    #[arg(long, default_value_t = 0.8)]
    pub load: f64,
    /// Fixed Poisson rate (requests/s).
    #[arg(long, conflicts_with = "stages")]
    pub rate: Option<f64>,
    /// Poisson rate stages as `rate:seconds,...`.
    #[arg(long)]
    pub stages: Option<Stages>,
    /// Leading --stages excluded from the report.
    #[arg(long, default_value_t = 0)]
    pub warmup_stages: usize,
    /// Closed loop: this many users each send their next request when the previous one finishes.
    #[arg(long, conflicts_with_all = ["rate", "stages"])]
    pub closed_loop: Option<usize>,
    /// Measured duration for fixed-rate and closed-loop runs.
    #[arg(long, default_value_t = 30)]
    pub duration_s: u64,
    /// Unreported warmup before a fixed-rate or closed-loop run.
    #[arg(long, default_value_t = 5)]
    pub warmup_s: u64,
    #[arg(long, default_value_t = 300)]
    pub timeout_s: u64,

    #[arg(long, default_value_t = 150)]
    pub groups: usize,
    #[arg(long, default_value_t = 5)]
    pub prompts_per_group: usize,
    /// System prompt length in tokens.
    #[arg(long, default_value_t = 6000)]
    pub system_len: usize,
    #[arg(long, default_value_t = 1200)]
    pub question_len: usize,
    #[arg(long, default_value_t = 1000)]
    pub output_len: u64,
    /// Group popularity: `uniform` (inference-perf cyclic replay), `zipf:<s>`, or
    /// `tiers:<groups>=<share>,...` (consecutive group runs taking those shares of requests).
    #[arg(long, default_value = "uniform")]
    pub popularity: Popularity,
    /// A fresh random question per request instead of cycling the fixed per-group prompts.
    #[arg(long)]
    pub unique_questions: bool,

    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    #[arg(long)]
    pub csv: Option<PathBuf>,
    /// Internal: run only the engine fleet (spawned by the parent process).
    #[arg(long, hide = true)]
    pub serve: bool,
}

/// Load schedule in simulated time.
pub struct Schedule {
    pub stages: Vec<Stage>,
    pub warmup_stages: usize,
    pub closed_users: Option<usize>,
}

impl Args {
    pub fn engine_specs(&self) -> Vec<EngineSpec> {
        let mut base = self.engine_preset.spec();
        base.max_seqs = self.max_seqs.unwrap_or(base.max_seqs);
        base.kv_tokens = self.kv_tokens.unwrap_or(base.kv_tokens);
        base.token_bytes = self.token_bytes.unwrap_or(base.token_bytes).max(1);
        base.max_batched_tokens = match self.no_chunked_prefill {
            true => None,
            false => self.max_batched_tokens.or(base.max_batched_tokens),
        };
        base.time_scale = self.time_scale;
        base.preempt = self.preempt;
        let slow = (self.engines as f64 * self.slow_fraction).round() as usize;
        (0..self.engines).map(|i| EngineSpec { speed: if i < slow { 0.6 } else { 1.0 }, ..base }).collect()
    }

    pub fn fleet_args(&self) -> Vec<String> {
        let preset = self.engine_preset.to_possible_value().expect("presets are named").get_name().to_owned();
        let mut args = vec![
            ("--engines", self.engines.to_string()),
            ("--slow-fraction", self.slow_fraction.to_string()),
            ("--engine-preset", preset),
            ("--time-scale", self.time_scale.to_string()),
        ];
        args.extend(self.max_seqs.map(|n| ("--max-seqs", n.to_string())));
        args.extend(self.kv_tokens.map(|n| ("--kv-tokens", n.to_string())));
        args.extend(self.token_bytes.map(|n| ("--token-bytes", n.to_string())));
        args.extend(self.max_batched_tokens.map(|n| ("--max-batched-tokens", n.to_string())));
        let mut args: Vec<String> = args.into_iter().flat_map(|(flag, value)| [flag.to_owned(), value]).collect();
        if self.no_chunked_prefill {
            args.push("--no-chunked-prefill".to_owned());
        }
        if self.preempt {
            args.push("--preempt".to_owned());
        }
        args
    }

    /// Exclusive end group of each `--popularity` tier (one tier unless `tiers:`).
    pub fn tier_ends(&self) -> Vec<usize> {
        match &self.popularity {
            Popularity::Tiers(tiers) => tiers
                .iter()
                .scan(0, |end, t| {
                    *end += t.0;
                    Some(*end)
                })
                .collect(),
            _ => vec![self.groups],
        }
    }

    pub fn generator(&self) -> io::Result<Generator> {
        if let Some(path) = &self.trace {
            return Trace::load(path, self.trace_speed, self.max_context).map(|t| Generator::Trace(Box::new(t)));
        }
        Ok(match self.workload {
            WorkloadKind::Random => Generator::Random {
                prompt: LogNormal { median: 400.0, sigma: 1.0, max: 6_000.0 },
                output: LogNormal { median: 120.0, sigma: 0.8, max: 1_500.0 },
            },
            WorkloadKind::SharedPrefix => Generator::shared_prefix(
                SharedPrefixParams {
                    groups: self.groups,
                    prompts_per_group: self.prompts_per_group,
                    system_len: self.system_len,
                    question_len: self.question_len,
                    output_len: self.output_len,
                    popularity: self.popularity.clone(),
                    unique_questions: self.unique_questions,
                },
                self.seed,
            ),
        })
    }

    pub fn schedule(&self, generator: &Generator, capacity_rps: f64) -> Schedule {
        if let Generator::Trace(trace) = generator {
            let stages = trace.stages(self.trace_windows);
            return Schedule { stages, warmup_stages: self.warmup_stages, closed_users: None };
        }
        if let Some(Stages(stages)) = &self.stages {
            return Schedule { stages: stages.clone(), warmup_stages: self.warmup_stages, closed_users: None };
        }
        let rate = match self.closed_loop {
            Some(_) => 0.0,
            None => self.rate.unwrap_or(self.load * capacity_rps),
        };
        let stage = |secs| Stage { rate, duration: Duration::from_secs(secs) };
        let warmup = (self.warmup_s > 0).then(|| stage(self.warmup_s));
        Schedule {
            stages: warmup.into_iter().chain([stage(self.duration_s)]).collect(),
            warmup_stages: usize::from(self.warmup_s > 0),
            closed_users: self.closed_loop,
        }
    }
}

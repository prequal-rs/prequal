use std::{net::SocketAddr, time::Duration};

use clap::{ArgAction, Parser};
use prequal_llm::{Engine, PrefillSignal};

use crate::{compat, shutdown::parse_duration};

#[derive(Parser)]
#[command(version, about)]
pub struct Args {
    /// InferencePool to serve.
    #[arg(long, required_unless_present = "endpoints")]
    pub pool_name: Option<String>,
    /// Namespace of the InferencePool (and its Pods and InferenceObjectives).
    #[arg(long, default_value = "default")]
    pub pool_namespace: String,
    /// API group of the InferencePool.
    #[arg(long, default_value = "inference.networking.k8s.io")]
    pub pool_group: String,
    /// Fixed endpoints (`ip:port,...`) instead of an InferencePool, for local testing.
    #[arg(long, value_delimiter = ',', conflicts_with = "pool_name")]
    pub endpoints: Vec<SocketAddr>,
    /// ext_proc gRPC port (also serves gRPC health).
    #[arg(long, default_value_t = 9002)]
    pub grpc_port: u16,
    /// gRPC health port for Kubernetes probes.
    #[arg(long, default_value_t = 9003)]
    pub grpc_health_port: u16,
    /// Prometheus `/metrics` port.
    #[arg(long, default_value_t = 9090)]
    pub metrics_port: u16,
    /// Serve ext_proc over TLS with a self-signed certificate (`--secure-serving=false` for plaintext).
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub secure_serving: bool,
    /// `vllm` or `sglang`: which metric names to read from model servers.
    #[arg(long, default_value = "vllm")]
    pub engine: Engine,
    /// Path of the model servers' Prometheus metrics.
    #[arg(long, default_value = "/metrics")]
    pub metrics_path: String,
    /// Routing policy: `prequal`, or a baseline (see `prequal_llm::policy::NAMES`).
    #[arg(long, default_value = "prequal")]
    pub policy: String,
    /// Model-server metrics scrape interval.
    #[arg(long, default_value_t = 50)]
    pub scrape_ms: u64,
    /// Experimental late binding: defer each pick until a pod has fewer than this many requests awaiting their
    /// first token, queueing in one EPP-wide FIFO. 0 (default) picks immediately; see prequal-router's flag.
    #[arg(long, default_value_t = 0)]
    pub admission_limit: u32,
    /// What ends a request's prefill reservation: `first-chunk` (needs response bodies streamed), `headers`,
    /// `scrape` (the replica's next metrics scrape), `estimate:<prefill tokens/s>` or `end`. All but `first-chunk`
    /// work with response_body_mode NONE; use `scrape` there on vLLM, `headers` on SGLang.
    #[arg(long, default_value = "first-chunk")]
    pub prefill_signal: PrefillSignal,
    /// Alternative endpoints listed after the pick, for gateways that retry down the list. 0 (the default, as
    /// llm-d's picker) keeps the destination header a single `ip:port`, which some data planes require.
    #[arg(long, default_value_t = 0)]
    pub fallbacks: usize,
    /// How often the InferencePool, its Pods and InferenceObjectives are re-read, in milliseconds.
    #[arg(long, default_value_t = 1000)]
    pub refresh_ms: u64,
    /// Threads serving ext_proc connections. One batches the per-token message stream best (see `shards`); raise it
    /// only if that thread saturates.
    #[arg(long, default_value_t = 1)]
    pub ext_proc_threads: usize,
    /// Microseconds an ext_proc thread lingers before idling, to answer the messages arriving meanwhile in one pass:
    /// fewer syscalls per streamed token at up to this much added latency per message (see `shards`). 0 = off.
    #[arg(long, default_value_t = 100)]
    pub ext_proc_coalesce_us: u64,
    /// Most ext_proc streams (one per in-flight HTTP request) open at once, in total and per gateway connection.
    /// Streams beyond it fail with gRPC RESOURCE_EXHAUSTED, so the gateway's failure mode applies. 0 = unlimited.
    #[arg(long, default_value_t = 20_000)]
    pub max_concurrent_streams: u32,
    /// MiB of request bodies buffered across all streams while awaiting their pick (each body is also capped at
    /// 10 MiB, as in llm-d). Requests that would exceed it get 503. 0 = unlimited.
    #[arg(long, default_value_t = 1024)]
    pub max_buffered_body_mib: usize,
    /// On SIGTERM/SIGINT readiness turns NOT_SERVING, new ext_proc streams are refused, and in-flight ones get this
    /// long to finish before exit (e.g. `25s`, `1m`). Keep it below the pod's terminationGracePeriodSeconds.
    #[arg(long, default_value = "25s", value_parser = parse_duration)]
    pub drain_timeout: Duration,
    /// Experimental. Stamp each request body with a vLLM `priority` so cheap requests run first, overtaking a
    /// costlier one only if they arrive within this long after it (e.g. `30s`). Every engine must run
    /// `--scheduling-policy priority`: without it vLLM 0.31 accepts the stamp and ignores it.
    #[arg(long, value_parser = parse_duration)]
    pub engine_priority_handicap: Option<Duration>,
    /// Experimental. `host:port` resolving to every picker of this pool (a headless Service on `--gossip-port`): each
    /// tells the others where it sent each prompt, so their prefix indexes agree. The messages are unauthenticated UDP;
    /// expose the port to the pickers only.
    #[arg(long)]
    pub gossip_peers: Option<String>,
    /// UDP port this picker's gossip listens on, when `--gossip-peers` is set.
    #[arg(long, default_value_t = 9004)]
    pub gossip_port: u16,
    /// Gateway API Inference Extension conformance hooks, as in the reference lwepp: the `test-epp-endpoint-selection`
    /// request header restricts candidates and responses report `x-conformance-test-served-endpoint`. Test only:
    /// any client could steer its requests.
    #[arg(long)]
    pub conformance_test_hooks: bool,
    #[command(flatten)]
    pub ignored: compat::IgnoredFlags,
}

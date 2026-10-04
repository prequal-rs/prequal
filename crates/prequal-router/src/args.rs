use std::{net::SocketAddr, time::Duration};

use clap::Parser;
use prequal_llm::Engine;

use crate::{discovery::ServicePort, dns::Backend};

#[derive(Parser)]
#[command(version, about)]
pub struct Args {
    /// Engine replicas as host:port (alternative to --k8s-service). Names are re-resolved every 2 s, and a name
    /// with several addresses (a headless Service, scaled compose services) adds one replica per address.
    #[arg(required_unless_present = "k8s_service", conflicts_with = "k8s_service")]
    pub backends: Vec<Backend>,
    /// Discover replicas from a Kubernetes Service, as `namespace/name`.
    #[arg(long)]
    pub k8s_service: Option<String>,
    /// Service port name or number to route to.
    #[arg(long, default_value = "http")]
    pub k8s_port: ServicePort,
    /// Address for client (OpenAI API) traffic.
    #[arg(long, default_value = "127.0.0.1:8000")]
    pub listen: SocketAddr,
    /// Address for `/healthz` (liveness) and `/readyz` (some replica up, not shutting down).
    #[arg(long, default_value = "127.0.0.1:8081")]
    pub admin_listen: SocketAddr,
    /// `vllm` or `sglang`: which metric names to read.
    #[arg(long, default_value = "vllm")]
    pub engine: Engine,
    #[arg(long, default_value = "/metrics")]
    pub metrics_path: String,
    /// Routing policy: `prequal`, or a baseline (round-robin, least-request, llmd-default, llmd-optimized,
    /// llmd-guide, sglang-cache-aware, dynamo).
    #[arg(long, default_value = "prequal")]
    pub policy: String,
    /// Engine metrics scrape interval.
    #[arg(long, default_value_t = 50)]
    pub scrape_ms: u64,
    /// Experimental late binding: hold requests in one router-wide queue until a replica has fewer than this many
    /// awaiting their first token. 0 (default) routes immediately; in our shared-prefix benchmarks late binding
    /// scattered prefixes and lowered cache hit rates, so leave it off unless prompts share little.
    #[arg(long, default_value_t = 0)]
    pub admission_limit: u32,
    /// Largest request body accepted (413 above it).
    #[arg(long, default_value_t = 16)]
    pub max_body_mib: usize,
    /// Request bytes buffered at once across all requests; requests that would exceed it get 503.
    #[arg(long, default_value_t = 64)]
    pub max_buffered_mib: usize,
    /// Client connections served at once; further ones wait in the listen backlog.
    #[arg(long, default_value_t = 4096)]
    pub max_connections: usize,
    /// Concurrent HTTP/2 streams per client connection.
    #[arg(long, default_value_t = 256)]
    pub max_streams: u32,
    /// Seconds a client may take to send its request headers (HTTP/1); 0 = no limit.
    #[arg(long, default_value_t = 30)]
    pub header_timeout_secs: u64,
    /// Seconds a client may take to send its request body (408 after); 0 = no limit.
    #[arg(long, default_value_t = 60)]
    pub body_timeout_secs: u64,
    /// Milliseconds to open a connection to a replica (502 after); 0 = no limit.
    #[arg(long, default_value_t = 5000)]
    pub connect_timeout_ms: u64,
    /// Seconds to wait for a replica's response headers (504 after); 0 = no limit. Non-streaming completions send
    /// headers only when done, so this bounds them too; streamed bodies have no time limit.
    #[arg(long, default_value_t = 600)]
    pub response_timeout_secs: u64,
    /// Seconds in-flight requests may run on after SIGTERM/SIGINT before the router exits anyway. Keep it below
    /// the pod's `terminationGracePeriodSeconds` (30 by default).
    #[arg(long, default_value_t = 25)]
    pub drain_secs: u64,
    /// Single-threaded runtimes serving client connections (see `shards`). Add more only once one saturates.
    #[arg(long, default_value_t = 1)]
    pub shards: usize,
    /// Microseconds a shard lingers before parking, to handle more tokens per wakeup (adds up to that per token).
    #[arg(long, default_value_t = 100)]
    pub coalesce_us: u64,
}

/// `Some(duration)`, or `None` (no limit) for 0.
pub fn limit(duration: Duration) -> Option<Duration> {
    (!duration.is_zero()).then_some(duration)
}

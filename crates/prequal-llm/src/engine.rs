use std::{net::SocketAddr, str::FromStr};

use prequal_core::ProbeResponse;
use prequal_tower::{ProbeClient, Prober};

use crate::metrics::{Aggregate, read_label, read_metrics};

/// An inference engine whose Prometheus `/metrics` reports queue depth and KV-cache pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Engine {
    /// vLLM (and llm-d-inference-sim): `vllm:*` metrics.
    Vllm,
    /// SGLang: `sglang:*` metrics.
    Sglang,
}

struct EngineMetrics {
    running: &'static str,
    waiting: &'static str,
    kv_usage: &'static str,
    /// Cumulative prompt tokens looked up in / found in the prefix cache.
    prefix_queries: Option<&'static str>,
    prefix_hits: Option<&'static str>,
    /// KV-cache capacity in tokens as a plain metric (vLLM publishes it as labels instead).
    capacity_tokens: Option<&'static str>,
}

/// One scrape of an engine's load and prefix-cache counters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct EngineStats {
    /// Requests in the running batch.
    pub running: f64,
    /// Requests queued.
    pub waiting: f64,
    /// KV-cache usage, 0 to 1.
    pub kv_usage: f64,
    /// Cumulative prompt tokens looked up in the prefix cache, when the engine publishes it.
    pub prefix_queries: Option<f64>,
    /// Cumulative prompt tokens found in the prefix cache, when the engine publishes it.
    pub prefix_hits: Option<f64>,
    /// KV-cache capacity in tokens, when the engine publishes it.
    pub cache_tokens: Option<f64>,
}

impl EngineStats {
    /// Load gauges only; the optional counters are `None`.
    #[must_use]
    pub fn new(running: f64, waiting: f64, kv_usage: f64) -> Self {
        Self { running, waiting, kv_usage, ..Self::default() }
    }

    /// These stats with non-finite gauges read as 0 and non-finite counters as absent, so that stats built by hand
    /// (not parsed) cannot poison routing scores either.
    pub(crate) fn finite(self) -> Self {
        let gauge = |v: f64| if v.is_finite() { v } else { 0.0 };
        let counter = |v: Option<f64>| v.filter(|v| v.is_finite());
        Self {
            running: gauge(self.running),
            waiting: gauge(self.waiting),
            kv_usage: gauge(self.kv_usage).clamp(0.0, 1.0),
            prefix_queries: counter(self.prefix_queries),
            prefix_hits: counter(self.prefix_hits),
            cache_tokens: counter(self.cache_tokens),
        }
    }
}

impl Engine {
    fn metrics(self) -> EngineMetrics {
        match self {
            Self::Vllm => EngineMetrics {
                running: "vllm:num_requests_running",
                waiting: "vllm:num_requests_waiting",
                kv_usage: "vllm:kv_cache_usage_perc",
                prefix_queries: Some("vllm:prefix_cache_queries_total"),
                prefix_hits: Some("vllm:prefix_cache_hits_total"),
                capacity_tokens: None,
            },
            Self::Sglang => EngineMetrics {
                running: "sglang:num_running_reqs",
                waiting: "sglang:num_queue_reqs",
                kv_usage: "sglang:token_usage",
                prefix_queries: None,
                prefix_hits: None,
                capacity_tokens: Some("sglang:max_total_num_tokens"),
            },
        }
    }

    /// Parses a scrape; `None` if the queue metrics are missing (not this engine, or not ready).
    pub fn stats(self, exposition: &str) -> Option<EngineStats> {
        let m = self.metrics();
        let (sum, max) = (Aggregate::Sum, Aggregate::Max);
        // llm-d-inference-sim exposes the prefix counters without vLLM's `_total` suffix.
        let bare = |name: Option<&'static str>| name.and_then(|n| n.strip_suffix("_total"));
        let [running, waiting, kv_usage, prefix_queries, prefix_hits, capacity, bare_queries, bare_hits] = read_metrics(
            exposition,
            [
                (Some(m.running), sum),
                (Some(m.waiting), sum),
                (Some(m.kv_usage), max),
                (m.prefix_queries, sum),
                (m.prefix_hits, sum),
                (m.capacity_tokens, sum),
                (bare(m.prefix_queries), sum),
                (bare(m.prefix_hits), sum),
            ],
        );
        let (prefix_queries, prefix_hits) = (prefix_queries.or(bare_queries), prefix_hits.or(bare_hits));
        Some(EngineStats {
            running: running?,
            waiting: waiting?,
            kv_usage: kv_usage.unwrap_or(0.0).clamp(0.0, 1.0),
            prefix_queries,
            prefix_hits,
            cache_tokens: match self {
                Self::Vllm => {
                    let label = |l| {
                        read_label(exposition, "vllm:cache_config_info", l)?
                            .parse::<f64>()
                            .ok()
                            .filter(|v| v.is_finite())
                    };
                    label("num_gpu_blocks").zip(label("block_size")).map(|(blocks, size)| blocks * size)
                }
                Self::Sglang => capacity,
            },
        })
    }

    /// Maps engine metrics onto a Prequal load report: RIF = running + waiting requests, and the
    /// latency slot carries KV-cache usage in parts per million, so among non-hot replicas HCL
    /// prefers the one with the most KV headroom. `None` if the queue metrics are missing.
    pub fn load_report(self, exposition: &str) -> Option<ProbeResponse> {
        let stats = self.stats(exposition)?;
        Some(ProbeResponse {
            rif: (stats.running + stats.waiting).round().clamp(0.0, f64::from(u32::MAX)) as u32,
            latency_us: (stats.kv_usage * 1e6) as u64,
        })
    }
}

impl FromStr for Engine {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "vllm" => Ok(Self::Vllm),
            "sglang" => Ok(Self::Sglang),
            other => Err(format!("unknown engine {other:?} (expected vllm or sglang)")),
        }
    }
}

/// Scrapes an engine replica's Prometheus endpoint over a pooled keep-alive connection.
#[derive(Clone, Debug)]
pub struct EngineProber {
    engine: Engine,
    path: String,
    client: ProbeClient,
}

impl EngineProber {
    /// Scrapes `GET /metrics` as `engine` publishes it.
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Self { engine, path: "/metrics".to_owned(), client: ProbeClient::default() }
    }

    /// Scrapes `path` instead of `/metrics`.
    #[must_use]
    pub fn with_path(mut self, path: &str) -> Self {
        self.path = path.to_owned();
        self
    }

    /// `None` on transport errors, non-2xx, or an exposition without the queue metrics.
    pub async fn stats(&self, addr: SocketAddr) -> Option<EngineStats> {
        let response = self.client.get(addr, &self.path).await.ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        self.engine.stats(std::str::from_utf8(&response.body).ok()?)
    }
}

/// Keyed by address; use `prequal_pingora::InetProber` to probe Pingora backends.
impl Prober<SocketAddr> for EngineProber {
    async fn probe(&self, addr: &SocketAddr) -> Option<ProbeResponse> {
        let response = self.client.get(*addr, &self.path).await.ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        self.engine.load_report(std::str::from_utf8(&response.body).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_vllm_and_sglang_metrics() {
        let vllm = "vllm:num_requests_running{e=\"0\"} 5\nvllm:num_requests_waiting{e=\"0\"} 3\nvllm:kv_cache_usage_perc{e=\"0\"} 0.75\n";
        assert_eq!(Engine::Vllm.load_report(vllm), Some(ProbeResponse { rif: 8, latency_us: 750_000 }));
        let sglang = "sglang:num_running_reqs 2\nsglang:num_queue_reqs 0\n";
        assert_eq!(Engine::Sglang.load_report(sglang), Some(ProbeResponse { rif: 2, latency_us: 0 }));
        assert_eq!(Engine::Vllm.load_report(sglang), None);
    }

    #[test]
    fn non_finite_gauges_never_reach_the_stats() {
        let base = [
            ("vllm:num_requests_running", "1"),
            ("vllm:num_requests_waiting", "2"),
            ("vllm:kv_cache_usage_perc", "0.5"),
        ];
        for bad in ["NaN", "+Inf", "-Inf"] {
            for i in 0..base.len() {
                let exposition: String = base
                    .iter()
                    .enumerate()
                    .map(|(j, (name, value))| format!("{name}{{e=\"0\"}} {}\n", if i == j { bad } else { value }))
                    .collect();
                let stats = Engine::Vllm.stats(&exposition);
                match i {
                    // A missing queue gauge is a failed scrape, not zero load.
                    0 | 1 => assert_eq!(stats, None, "{bad} in {}", base[i].0),
                    _ => assert_eq!(stats.map(|s| s.kv_usage), Some(0.0), "{bad} in {}", base[i].0),
                }
            }
        }
        let hand_built =
            EngineStats { running: f64::NAN, waiting: f64::INFINITY, kv_usage: f64::NAN, ..Default::default() };
        assert_eq!(hand_built.finite(), EngineStats::default());
    }

    #[test]
    fn reads_prefix_cache_counters_and_capacity() {
        let vllm = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\n\
                    vllm:prefix_cache_queries_total{e=\"0\"} 1000\nvllm:prefix_cache_hits_total{e=\"0\"} 600\n\
                    vllm:cache_config_info{block_size=\"16\",num_gpu_blocks=\"100\"} 1\n";
        let stats = Engine::Vllm.stats(vllm).unwrap();
        assert_eq!(
            (stats.prefix_queries, stats.prefix_hits, stats.cache_tokens),
            (Some(1000.0), Some(600.0), Some(1600.0))
        );
        let sim = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\n\
                   vllm:prefix_cache_queries{m=\"q\"} 30\nvllm:prefix_cache_hits{m=\"q\"} 12\n";
        let stats = Engine::Vllm.stats(sim).unwrap();
        assert_eq!((stats.prefix_queries, stats.prefix_hits), (Some(30.0), Some(12.0)));
        let sglang = "sglang:num_running_reqs 2\nsglang:num_queue_reqs 1\nsglang:max_total_num_tokens 50000\n";
        assert_eq!(Engine::Sglang.stats(sglang).unwrap().cache_tokens, Some(50000.0));
    }
}

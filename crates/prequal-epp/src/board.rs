//! llm-d's views of the scheduler's latest engine scrapes: the saturation estimate (for shedding) and the
//! `llm_d_epp_*` pool gauges.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::SocketAddr,
    sync::RwLock,
    time::{Duration, Instant},
};

use prequal_llm::{EngineStats, ReplicaStatus};

use crate::request_metrics::escape;

/// llm-d's utilization detector: an endpoint is saturated at 5 waiting requests or 80% KV-cache use.
const QUEUE_SATURATED: f64 = 5.0;
const KV_SATURATED: f64 = 0.8;
/// Scrapes older than this mark an endpoint saturated (llm-d: 200 ms at a 50 ms scrape interval).
const DETECTOR_STALENESS: Duration = Duration::from_millis(200);
/// Only endpoints scraped this recently count in the pool gauges (llm-d's `--metrics-staleness-threshold`).
const GAUGE_STALENESS: Duration = Duration::from_secs(2);

/// llm-d's endpoint names (`<pod>-rank-<target port index>`) plus the staleness bounds for reading scrapes.
pub struct Board {
    names: RwLock<BTreeMap<SocketAddr, String>>,
    detector_staleness: Duration,
    gauge_staleness: Duration,
}

impl Board {
    /// Staleness bounds stretch to four scrape intervals when scraping is slower than llm-d's default.
    pub fn new(scrape_interval: Duration) -> Self {
        Self {
            names: RwLock::default(),
            detector_staleness: DETECTOR_STALENESS.max(scrape_interval * 4),
            gauge_staleness: GAUGE_STALENESS.max(scrape_interval * 4),
        }
    }

    pub fn sync(&self, named: &BTreeMap<SocketAddr, String>) {
        self.names.write().unwrap_or_else(|p| p.into_inner()).clone_from(named);
    }

    /// llm-d's pool saturation: the mean over `allowed` replicas of `max(waiting / 5, kv / 0.8)`, a stale or
    /// unscraped replica counting 1. No replicas is fully saturated. Shedding starts at 1.
    pub fn saturation(&self, replicas: &[ReplicaStatus], allowed: impl Fn(&SocketAddr) -> bool, now: Instant) -> f64 {
        let scores: Vec<f64> = replicas
            .iter()
            .filter(|r| allowed(&r.addr))
            .map(|r| match r.scrape {
                Some((s, at)) if now.duration_since(at) <= self.detector_staleness => {
                    (s.waiting / QUEUE_SATURATED).max(s.kv_usage / KV_SATURATED)
                }
                _ => 1.0,
            })
            .collect();
        if scores.is_empty() { 1.0 } else { scores.iter().sum::<f64>() / scores.len() as f64 }
    }

    /// llm-d's gauges for InferencePool `pool` in `namespace`: averages and sample standard deviations over freshly
    /// scraped replicas (omitted while there are none), their count, every replica's last queue length, and this
    /// EPP's requests in flight per replica.
    pub fn render(&self, pool: &str, namespace: &str, replicas: &[ReplicaStatus], now: Instant, out: &mut String) {
        let names = self.names.read().unwrap_or_else(|p| p.into_inner());
        let name = |addr: &SocketAddr| names.get(addr).map_or_else(|| addr.to_string(), |n| escape(n));
        let fresh: Vec<EngineStats> = replicas
            .iter()
            .filter_map(|r| r.scrape.filter(|(_, at)| now.duration_since(*at) <= self.gauge_staleness))
            .map(|(stats, _)| stats)
            .collect();
        let pool = escape(pool);
        let mut gauge = |name: &str, help: &str, value: f64| {
            let _ = writeln!(out, "# HELP llm_d_epp_{name} [ALPHA] {help}\n# TYPE llm_d_epp_{name} gauge");
            let _ = writeln!(out, "llm_d_epp_{name}{{name=\"{pool}\"}} {value}");
        };
        if !fresh.is_empty() {
            type Read = fn(&EngineStats) -> f64;
            let series: [(&str, &str, Read); 3] = [
                ("kv_cache_utilization", "KV-cache utilization (0 to 1)", |s| s.kv_usage),
                ("queue_size", "waiting-queue length", |s| s.waiting),
                ("running_requests", "running requests", |s| s.running),
            ];
            for (name, what, read) in series {
                let values: Vec<f64> = fresh.iter().map(read).collect();
                let (mean, std_dev) = mean_and_sample_std_dev(&values);
                gauge(&format!("average_{name}"), &format!("Average {what} across the pool's endpoints."), mean);
                gauge(&format!("std_dev_{name}"), &format!("Standard deviation of {what} across endpoints."), std_dev);
            }
        }
        gauge("ready_endpoints", "Endpoints with fresh model-server metrics in the pool.", fresh.len() as f64);
        let _ = writeln!(out, "# HELP llm_d_epp_per_endpoint_queue_size [ALPHA] Waiting-queue length per endpoint.");
        let _ = writeln!(out, "# TYPE llm_d_epp_per_endpoint_queue_size gauge");
        for r in replicas {
            if let Some((stats, _)) = r.scrape {
                let endpoint = name(&r.addr);
                let _ = writeln!(
                    out,
                    "llm_d_epp_per_endpoint_queue_size{{name=\"{pool}\",model_server_endpoint=\"{endpoint}\"}} {}",
                    stats.waiting
                );
            }
        }
        let _ = writeln!(
            out,
            "# HELP llm_d_epp_inflight_requests [ALPHA] Requests this EPP routed to each endpoint that have not \
             completed (not split by fairness_id or priority).\n# TYPE llm_d_epp_inflight_requests gauge"
        );
        let namespace = escape(namespace);
        for r in replicas {
            let endpoint = name(&r.addr);
            let _ = writeln!(
                out,
                "llm_d_epp_inflight_requests{{endpoint_name=\"{endpoint}\",namespace=\"{namespace}\"}} {}",
                r.in_flight
            );
        }
    }
}

/// Rounded to two decimals, with the n-1 standard deviation (n floored at 2), as llm-d reports them.
fn mean_and_sample_std_dev(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    let round = |v: f64| (v * 100.0).round() / 100.0;
    (round(mean), round(variance.sqrt()))
}

#[cfg(test)]
mod tests {
    use prequal_llm::{Prompt, Scheduler, policy};

    use super::*;

    fn stats(waiting: f64, kv_usage: f64) -> EngineStats {
        EngineStats::new(2.0, waiting, kv_usage)
    }

    fn fleet(addrs: &[&str]) -> (Board, Scheduler, Vec<SocketAddr>) {
        let addrs: Vec<SocketAddr> = addrs.iter().map(|a| a.parse().unwrap()).collect();
        let board = Board::new(Duration::from_millis(50));
        board.sync(&addrs.iter().enumerate().map(|(i, a)| (*a, format!("pod-{i}-rank-0"))).collect());
        let scheduler = Scheduler::new(policy::by_name("prequal").unwrap());
        scheduler.sync(addrs.iter().copied());
        (board, scheduler, addrs)
    }

    #[test]
    fn saturation_is_the_mean_utilization_with_stale_endpoints_full() {
        let (board, scheduler, addrs) = fleet(&["10.0.0.1:8000", "10.0.0.2:8000", "10.0.0.3:8000"]);
        scheduler.observe(addrs[0], stats(10.0, 0.4));
        scheduler.observe(addrs[1], stats(0.0, 0.4));
        let (replicas, now) = (scheduler.replicas(), Instant::now());
        assert!((board.saturation(&replicas, |a| *a != addrs[2], now) - 1.25).abs() < 1e-9, "(2 + 0.5) / 2");
        assert!((board.saturation(&replicas, |_| true, now) - 3.5 / 3.0).abs() < 1e-9, "unscraped counts 1");
        let later = now + Duration::from_millis(201);
        assert_eq!(board.saturation(&replicas, |a| *a == addrs[1], later), 1.0, "stale");
        assert_eq!(board.saturation(&replicas, |_| false, now), 1.0, "no endpoints");
    }

    #[test]
    fn renders_llm_d_pool_gauges() {
        let (board, scheduler, addrs) = fleet(&["10.0.0.1:8000", "10.0.0.2:8000"]);
        let mut out = String::new();
        board.render("vllm-pool", "ns", &scheduler.replicas(), Instant::now(), &mut out);
        assert!(out.contains("llm_d_epp_ready_endpoints{name=\"vllm-pool\"} 0"));
        assert!(!out.contains("average_queue_size"), "no fresh endpoints, no averages");
        scheduler.observe(addrs[0], stats(1.0, 0.25));
        scheduler.observe(addrs[1], stats(4.0, 0.5));
        let _ticket = scheduler.route(&Prompt::default(), 1, |a| *a == addrs[1]).unwrap();
        let mut out = String::new();
        board.render("vllm-pool", "ns", &scheduler.replicas(), Instant::now(), &mut out);
        for line in [
            "llm_d_epp_ready_endpoints{name=\"vllm-pool\"} 2",
            "llm_d_epp_average_queue_size{name=\"vllm-pool\"} 2.5",
            "llm_d_epp_std_dev_queue_size{name=\"vllm-pool\"} 2.12",
            "llm_d_epp_average_kv_cache_utilization{name=\"vllm-pool\"} 0.38",
            "llm_d_epp_average_running_requests{name=\"vllm-pool\"} 2",
            "llm_d_epp_per_endpoint_queue_size{name=\"vllm-pool\",model_server_endpoint=\"pod-1-rank-0\"} 4",
            "llm_d_epp_inflight_requests{endpoint_name=\"pod-0-rank-0\",namespace=\"ns\"} 0",
            "llm_d_epp_inflight_requests{endpoint_name=\"pod-1-rank-0\",namespace=\"ns\"} 1",
        ] {
            assert!(out.contains(line), "missing {line} in\n{out}");
        }
    }
}

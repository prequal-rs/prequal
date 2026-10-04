//! Replica-choice policies over a shared view of the fleet. [`Prequal`] is ours; [`baselines`] reimplements
//! incumbent routers' published scoring so they can be benchmarked on equal footing (same scrapes, same index).
//! Implement [`Policy`] for your own.

mod api;
pub mod baselines;
mod cold_lru;
mod prequal;
mod share;

use std::net::{IpAddr, SocketAddr};

pub use api::{Candidate, Policy, PolicyRng, Request};
use prequal::Fresh;
pub use prequal::Prequal;

/// Index of the first maximum, as incumbent max-score pickers do (deterministic on ties).
pub(crate) fn first_max(scores: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::NEG_INFINITY);
    for (i, score) in scores.enumerate() {
        if score > best.1 {
            best = (i, score);
        }
    }
    best.0
}

/// Rendezvous (highest-random-weight) score of `addr` for `key`: every router ranks replicas identically for a key,
/// whatever its build. A fixed SplitMix64 chain, not `DefaultHasher`, whose output std may change between releases.
pub(crate) fn rendezvous(key: u64, addr: &SocketAddr) -> u64 {
    let (family, ip) = match addr.ip() {
        IpAddr::V4(ip) => (4, u128::from(u32::from(ip))),
        IpAddr::V6(ip) => (6, u128::from(ip)),
    };
    let port = u64::from(addr.port()) | (family << 16);
    [ip as u64, (ip >> 64) as u64, port].into_iter().fold(splitmix64(key), |h, word| splitmix64(h ^ word))
}

fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
#[test]
fn rendezvous_is_pinned_across_builds() {
    let v4: SocketAddr = "10.0.0.1:8000".parse().unwrap();
    let v6: SocketAddr = "[fd00::1]:8000".parse().unwrap();
    assert_eq!(
        [rendezvous(0, &v4), rendezvous(42, &v4), rendezvous(42, &v6)],
        [16_462_006_299_482_721_661, 15_448_117_843_373_563_554, 15_707_181_232_448_393_668]
    );
}

/// The policy names [`by_name`] accepts, besides the `prequal` variants it documents.
pub const NAMES: &[&str] = &[
    "prequal",
    "round-robin",
    "least-request",
    "llmd-default",
    "llmd-optimized",
    "llmd-guide",
    "sglang-cache-aware",
    "dynamo",
];

/// A policy by name: one of [`NAMES`]; or a [`Prequal`] variant: `prequal-home` (cold prompts return to their key's
/// last replica while its cache model is uncalibrated: for engines without prefix-cache counters, such as SGLang),
/// `prequal-rif` (queue counts running requests too), `prequal-fresh` (own pending prefills count for cold prompts
/// too), `prequal-scraped`
/// (scraped load only, no hash placement), or `prequal:<prefix>:<queue>:<kv>[:<overload>[:<margin>[:<cold>]]]`
/// (its weights).
#[must_use]
pub fn by_name(name: &str) -> Option<Box<dyn Policy>> {
    use baselines::*;
    if let Some(params) = name.strip_prefix("prequal:") {
        let given: Vec<f64> = params.split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
        let d = Prequal::default();
        let mut w = [d.prefix_weight, d.queue_weight, d.kv_weight, d.overload_weight, d.overload_margin, d.cold_weight];
        if !(3..=w.len()).contains(&given.len()) {
            return None;
        }
        w[..given.len()].copy_from_slice(&given);
        let [prefix_weight, queue_weight, kv_weight, overload_weight, overload_margin, cold_weight] = w;
        return Some(Box::new(Prequal {
            prefix_weight,
            queue_weight,
            kv_weight,
            overload_weight,
            overload_margin,
            cold_weight,
            ..d
        }));
    }
    Some(match name {
        "prequal" => Box::new(Prequal::default()),
        "prequal-rif" => Box::new(Prequal { rif_queue: true, ..Prequal::default() }),
        "prequal-home" => Box::new(Prequal { home: true, ..Prequal::default() }),
        "prequal-fresh" => Box::new(Prequal { fresh: Fresh::Always, ..Prequal::default() }),
        "prequal-scraped" => Box::new(Prequal { fresh: Fresh::Never, hash_below_share: 0.0, ..Prequal::default() }),
        "round-robin" => Box::new(RoundRobin::default()),
        "least-request" => Box::new(LeastRequest),
        "llmd-default" => Box::new(LlmdDefault),
        "llmd-optimized" => Box::new(LlmdOptimized::default()),
        "llmd-guide" => Box::new(LlmdGuide),
        "sglang-cache-aware" => Box::new(SglangCacheAware),
        "dynamo" => Box::new(Dynamo),
        _ => return None,
    })
}

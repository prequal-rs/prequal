//! Core of Prequal (Wydrowski et al., NSDI'24): an async probe pool plus hot-cold lexicographic
//! (HCL) replica selection, with outlier ejection. Runtime-agnostic: callers supply time and RNG.
//!
//! Most users want `prequal-tower` or `prequal-pingora`, which drive this state machine for you.
//!
//! The RNG is any [`rand::Rng`]; this crate's `rand` is re-exported as [`rand`], and a `rand` major
//! upgrade is a breaking release of this crate.
//!
//! # Example
//!
//! ```
//! use prequal_core::{Config, Prequal, ProbeResponse};
//! use rand::{SeedableRng, rngs::SmallRng};
//!
//! let mut rng = SmallRng::seed_from_u64(1);
//! let mut balancer = Prequal::new(Config::default(), 4);
//! let now_us = 0;
//!
//! // Per query: probe a few replicas (asynchronously, in a real client) and pool their answers...
//! for replica in balancer.probe_targets(&mut rng) {
//!     let report = ProbeResponse { rif: replica as u32, latency_us: 1_000 };
//!     balancer.record_probe(replica, report, now_us, &mut rng);
//! }
//! // ...then route with hot-cold selection over the pool and report the outcome.
//! let replica = balancer.select(now_us, &mut rng);
//! balancer.record_success(replica);
//! assert!(replica < 4);
//! ```

mod balancer;
mod config;
mod frac;
mod health;
mod history;
mod pool;
mod probe;

pub use balancer::{Counters, Prequal};
pub use config::Config;
pub use probe::{HEADER_LATENCY_US, HEADER_RIF, ProbeResponse};
/// The `rand` version whose `Rng` trait this crate's methods take.
pub use rand;

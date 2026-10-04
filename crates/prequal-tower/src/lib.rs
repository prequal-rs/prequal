//! Tower balancer that routes requests with Prequal (Wydrowski et al., NSDI'24): asynchronous
//! probes, hot-cold lexicographic selection, piggybacked load reports, and outlier ejection driven
//! by request outcomes.
//!
//! Servers report requests-in-flight (see `prequal-server`); the balancer keeps a small pool of
//! recent reports and routes each request to a replica that is not "hot". Probing never blocks a
//! request: answers gathered for earlier requests route later ones.
//!
//! # Example
//!
//! ```
//! use std::{convert::Infallible, future::{Ready, ready}, task::{Context, Poll}};
//! use prequal_tower::{Config, PrequalBalance, ProbeResponse, Prober};
//! use tower_service::Service;
//!
//! /// Asks replica `key` for its load, e.g. `GET /prequal/probe` returning `x-prequal-rif`.
//! struct LoadProber;
//!
//! impl Prober<usize> for LoadProber {
//!     async fn probe(&self, key: &usize) -> Option<ProbeResponse> {
//!         Some(ProbeResponse { rif: *key as u32, latency_us: 0 })
//!     }
//! }
//!
//! # #[derive(Clone)] struct Replica(usize);
//! # impl Service<()> for Replica {
//! #     type Response = usize; type Error = Infallible; type Future = Ready<Result<usize, Infallible>>;
//! #     fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> { Poll::Ready(Ok(())) }
//! #     fn call(&mut self, _: ()) -> Self::Future { ready(Ok(self.0)) }
//! # }
//! # #[tokio::main(flavor = "current_thread")] async fn main() -> Result<(), tower::BoxError> {
//! let replicas = (0..8).map(Replica).collect();
//! // Piggybacking reports on responses allows few explicit probes; see `PrequalHandle`.
//! let mut config = Config::default();
//! (config.probes_per_query, config.removes_per_query) = (1.0, 0.34);
//! let mut balancer = PrequalBalance::from_services(replicas, LoadProber, config);
//!
//! std::future::poll_fn(|cx| balancer.poll_ready(cx)).await?;
//! let served_by = balancer.call(()).await?;
//! assert!(served_by < 8);
//! println!("{:?} {:?}", balancer.counters(), balancer.probe_counts());
//! # Ok(()) }
//! ```
//!
//! # Tuning
//!
//! - Keep `removes_per_query` near a third of pool inflow (probes plus piggybacked reports per
//!   query); if removals outpace inflow the pool never fills and routing degrades to random.
//! - Ejection: `eject_after_failures` consecutive failures eject a replica with exponential
//!   backoff; at most `max_ejected_fraction` of replicas are ejected at once. Use
//!   [`PrequalBalance::with_classifier`] to count e.g. HTTP 5xx responses as failures.
//! - Export [`PrequalBalance::counters`] and [`PrequalBalance::probe_counts`] as metrics.

mod balance;
mod client;
mod driver;
mod future;
mod handle;
mod list;
mod prober;
mod service;
mod stats;

pub use balance::PrequalBalance;
pub use client::{GetResponse, ProbeClient};
pub use driver::ProbeDriver;
pub use future::{Classify, ErrorsOnly, ResponseFuture};
pub use handle::PrequalHandle;
pub use list::StaticList;
pub use prequal_core::{Config, Counters, ProbeResponse};
pub use prober::Prober;
pub use stats::ProbeCounts;

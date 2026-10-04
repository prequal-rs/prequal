//! [Prequal](https://arxiv.org/abs/2312.10172) load balancing for [Pingora](https://github.com/cloudflare/pingora).
//!
//! Pingora's built-in selections (round robin, random, hashing) ignore backend load. [`Prequal`]
//! is a drop-in [`BackendSelection`](pingora_load_balancing::selection::BackendSelection) that
//! routes each request to a backend chosen from asynchronously probed load reports, skips
//! backends ejected after consecutive failures, and still defers to Pingora's health checks.
//!
//! Wiring (see `examples/proxy.rs`):
//! 1. Create a [`PrequalSelector`] and build `LoadBalancer::<Prequal>::from_backends_with_config`
//!    with it, so Prequal state survives discovery and health-check rebuilds.
//! 2. In `upstream_peer`, `lb.select(b"", max_iterations)` and remember the backend in `CTX`.
//! 3. In `upstream_response_filter`, call [`PrequalSelector::on_response`]; in `fail_to_connect`
//!    and `error_while_proxy`, call [`PrequalSelector::on_failure`].
//!
//! Backends should answer `GET /prequal/probe` and ideally attach the same load headers to every
//! response (see `prequal-server`). Probing needs a Tokio runtime, which Pingora provides.
//!
//! # Example
//!
//! ```no_run
//! use pingora_load_balancing::{Backends, LoadBalancer, discovery::Static};
//! use prequal_pingora::{Config, Prequal, PrequalSelector};
//!
//! let mut config = Config::default();
//! (config.probes_per_query, config.removes_per_query) = (0.5, 0.5);
//! let selector = PrequalSelector::new(config);
//! let discovery = Static::try_from_iter(["10.0.0.1:8000", "10.0.0.2:8000"]).unwrap();
//! let lb = LoadBalancer::<Prequal>::from_backends_with_config(Backends::new(discovery), Some(selector.clone()));
//!
//! // In `ProxyHttp::upstream_peer`:
//! let backend = lb.select(b"", 256).expect("a healthy backend");
//! // ...and once the upstream answers (status and headers from `upstream_response_filter`):
//! selector.on_response(&backend, 200, &http::HeaderMap::new());
//! ```

#[cfg(feature = "kubernetes")]
pub mod kubernetes;
mod prober;
#[cfg(feature = "proxy")]
pub mod proxy;
mod selection;
mod selector;

pub use prequal_core::{Config, ProbeResponse};
pub use prequal_tower::{GetResponse, ProbeClient, Prober};
pub use prober::{HttpProber, InetProber};
pub use selection::{Prequal, PrequalIter};
pub use selector::PrequalSelector;

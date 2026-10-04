//! Server side of Prequal: track requests in flight (RIF), estimate latency, and answer probes.
//!
//! Wrap the routes that do real work in [`ProbeLayer`], answer probes from the same
//! [`ProbeState`], and ideally piggyback the report on every response too, so clients need few
//! explicit probes. Report headers are [`HEADER_RIF`] and [`HEADER_LATENCY_US`] (compatible with
//! envoy-prequal).
//!
//! [`RifOnly`] is the recommended estimator; [`RecentMedian`] (the paper's) and
//! [`ServiceTimeModel`] can make clients herd onto few replicas and are experimental.
//!
//! # Example
//!
//! ```
//! use prequal_server::{ProbeLayer, ProbeState, RifOnly, HEADER_RIF};
//! use tower_layer::Layer;
//!
//! # #[derive(Clone)] struct App;
//! let state = ProbeState::new(RifOnly);
//! let _work_service = ProbeLayer::new(state.clone()).layer(App);
//!
//! // In the probe handler (and optionally on every response):
//! let report = state.probe();
//! let headers = [(HEADER_RIF, report.rif.to_string())];
//! assert_eq!(headers[0].1, "0");
//! ```

mod estimator;
mod layer;
mod median;
mod state;

pub use estimator::{LatencyEstimator, RifOnly, ServiceTimeModel};
pub use layer::{ProbeLayer, ProbeService, ResponseFuture};
pub use median::RecentMedian;
pub use prequal_core::{HEADER_LATENCY_US, HEADER_RIF, ProbeResponse};
pub use state::{InFlight, ProbeState};

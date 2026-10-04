use std::{
    fmt,
    sync::{Arc, RwLock},
};

use http::HeaderMap;
use pingora_load_balancing::Backend;
use prequal_core::{Config, Counters, HEADER_LATENCY_US, HEADER_RIF, ProbeResponse};
use prequal_tower::{PrequalHandle, ProbeCounts, ProbeDriver, Prober};

use crate::HttpProber;

type IsFailure = Box<dyn Fn(u16) -> bool + Send + Sync>;

struct Inner {
    handle: PrequalHandle<Backend>,
    driver: ProbeDriver<Backend>,
    is_failure: RwLock<IsFailure>,
}

/// Prequal state shared by every selector a `LoadBalancer<Prequal>` builds. Pass it as the
/// balancer's config (`LoadBalancer::from_backends_with_config`) so the probe pool and ejection
/// state survive discovery and health updates. Cheap to clone; keep a clone in your proxy to
/// report outcomes with [`PrequalSelector::on_response`] and [`PrequalSelector::on_failure`].
/// Clones share all state, including [`PrequalSelector::with_failure_statuses`].
#[derive(Clone)]
pub struct PrequalSelector {
    inner: Arc<Inner>,
}

impl PrequalSelector {
    /// Probes backends with [`HttpProber::default`] (`GET /prequal/probe`).
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self::with_prober(config, HttpProber::default())
    }

    /// Probes backends with `prober`.
    #[must_use]
    pub fn with_prober<P: Prober<Backend>>(config: Config, prober: P) -> Self {
        let handle = PrequalHandle::new(config);
        let driver = ProbeDriver::new(prober, handle.clone());
        let is_failure: IsFailure = Box::new(|status| status >= 500);
        Self { inner: Arc::new(Inner { handle, driver, is_failure: RwLock::new(is_failure) }) }
    }

    /// Which HTTP statuses count as failures for outlier ejection (default: 5xx). Applies to every clone.
    #[must_use]
    pub fn with_failure_statuses(self, is_failure: impl Fn(u16) -> bool + Send + Sync + 'static) -> Self {
        *self.inner.is_failure.write().unwrap_or_else(|p| p.into_inner()) = Box::new(is_failure);
        self
    }

    /// Call from `upstream_response_filter`: records the outcome and any piggybacked load report.
    pub fn on_response(&self, backend: &Backend, status: u16, headers: &HeaderMap) {
        let inner = &self.inner;
        if let Some(report) = load_report(headers) {
            inner.handle.record(backend, report);
        }
        let failed = (inner.is_failure.read().unwrap_or_else(|p| p.into_inner()))(status);
        if failed {
            inner.handle.record_failure(backend);
        } else {
            inner.handle.record_success(backend);
        }
    }

    /// Call from `fail_to_connect` / `error_while_proxy`.
    pub fn on_failure(&self, backend: &Backend) {
        self.inner.handle.record_failure(backend);
    }

    /// Selection and ejection totals since creation.
    pub fn counters(&self) -> Counters {
        self.inner.handle.counters()
    }

    /// Probe outcomes since creation.
    pub fn probe_counts(&self) -> ProbeCounts {
        self.inner.driver.counts()
    }

    pub(crate) fn handle(&self) -> &PrequalHandle<Backend> {
        &self.inner.handle
    }

    pub(crate) fn driver(&self) -> &ProbeDriver<Backend> {
        &self.inner.driver
    }
}

impl fmt::Debug for PrequalSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrequalSelector")
            .field("backends", &self.inner.handle.len())
            .field("driver", &self.inner.driver)
            .finish_non_exhaustive()
    }
}

fn load_report(headers: &HeaderMap) -> Option<ProbeResponse> {
    ProbeResponse::from_header_values(
        headers.get(HEADER_RIF)?.to_str().ok()?,
        headers.get(HEADER_LATENCY_US)?.to_str().ok()?,
    )
}

#[cfg(test)]
#[test]
fn failure_statuses_set_after_cloning_apply_to_every_clone() {
    let selector = PrequalSelector::new(Config::default());
    let clone = selector.clone();
    let _selector = selector.with_failure_statuses(|status| status == 429);
    let is_failure = clone.inner.is_failure.read().unwrap();
    assert!(is_failure(429) && !is_failure(503));
}

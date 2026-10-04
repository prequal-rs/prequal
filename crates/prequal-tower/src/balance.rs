use std::{collections::HashMap, fmt, hash::Hash, pin::Pin, sync::Arc, task::Context, task::Poll, time::Duration};

use prequal_core::{Config, Counters};
use tower::{
    BoxError,
    discover::{Change, Discover},
};

use crate::{ErrorsOnly, PrequalHandle, ProbeCounts, ProbeDriver, Prober, StaticList};

/// Client-side Prequal balancer over a discovered set of endpoints. Each request queues async
/// probes (answers feed later requests), is routed by HCL over the probe pool, and reports its
/// outcome for outlier ejection. Needs a Tokio runtime by the first `poll_ready`.
pub struct PrequalBalance<D: Discover, C = ErrorsOnly> {
    pub(crate) discover: D,
    pub(crate) endpoints: HashMap<D::Key, D::Service>,
    pub(crate) handle: PrequalHandle<D::Key>,
    pub(crate) classify: Arc<C>,
    driver: ProbeDriver<D::Key>,
    target_keys: Vec<D::Key>,
    pub(crate) chosen: Option<D::Key>,
}

impl<S> PrequalBalance<StaticList<S>> {
    /// Balances over a fixed list; the prober and piggyback feedback use list positions as keys.
    #[must_use]
    pub fn from_services<P: Prober<usize>>(services: Vec<S>, prober: P, config: Config) -> Self {
        Self::new(StaticList::new(services), prober, config)
    }
}

impl<D> PrequalBalance<D>
where
    D: Discover,
    D::Key: Clone + Eq + Hash + Send + 'static,
{
    /// Defaults: 50 ms probe timeout, at most 64 probes outstanding, only `Err`s count as failures.
    #[must_use]
    pub fn new<P: Prober<D::Key>>(discover: D, prober: P, config: Config) -> Self {
        Self::with_handle(PrequalHandle::new(config), discover, prober)
    }

    /// Uses a handle created up front, typically cloned into endpoints for piggybacking.
    ///
    /// # Panics
    /// If the handle already tracks endpoints (i.e. belongs to another balancer).
    #[must_use]
    pub fn with_handle<P: Prober<D::Key>>(handle: PrequalHandle<D::Key>, discover: D, prober: P) -> Self {
        assert!(handle.is_empty(), "PrequalHandle already in use by another balancer");
        Self {
            discover,
            endpoints: HashMap::new(),
            driver: ProbeDriver::new(prober, handle.clone()),
            handle,
            classify: Arc::new(ErrorsOnly),
            target_keys: Vec::new(),
            chosen: None,
        }
    }
}

impl<D, C> PrequalBalance<D, C>
where
    D: Discover,
    D::Key: Clone + Eq + Hash + Send + 'static,
{
    /// Sets which `Ok` responses also count as failures (e.g. HTTP 5xx) for outlier ejection.
    #[must_use]
    pub fn with_classifier<C2>(self, classify: C2) -> PrequalBalance<D, C2> {
        PrequalBalance {
            discover: self.discover,
            endpoints: self.endpoints,
            handle: self.handle,
            classify: Arc::new(classify),
            driver: self.driver,
            target_keys: self.target_keys,
            chosen: self.chosen,
        }
    }

    /// Probes slower than `timeout` count as timed out (default 50 ms).
    #[must_use]
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.driver = self.driver.with_timeout(timeout);
        self
    }

    /// Caps outstanding probes (default 64); further probes are skipped while at the cap.
    #[must_use]
    pub fn with_max_in_flight_probes(mut self, max: usize) -> Self {
        self.driver = self.driver.with_max_in_flight(max);
        self
    }

    /// The shared state, to clone into endpoints for piggybacked load reports.
    pub fn handle(&self) -> &PrequalHandle<D::Key> {
        &self.handle
    }

    /// Endpoints currently discovered.
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Whether no endpoint is discovered yet.
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// Probe outcomes since creation.
    pub fn probe_counts(&self) -> ProbeCounts {
        self.driver.counts()
    }

    /// Selection and ejection totals since creation.
    pub fn counters(&self) -> Counters {
        self.handle.counters()
    }

    /// Applies pending discovery changes. Returns whether the endpoint set changed.
    pub(crate) fn poll_discover(&mut self, cx: &mut Context<'_>) -> Result<bool, BoxError>
    where
        D: Unpin,
        D::Error: Into<BoxError>,
    {
        let mut changed = false;
        while let Poll::Ready(Some(change)) = Pin::new(&mut self.discover).poll_discover(cx) {
            changed = true;
            match change.map_err(Into::into)? {
                Change::Insert(key, service) => {
                    self.handle.lock().insert(key.clone());
                    self.endpoints.insert(key, service);
                }
                Change::Remove(key) => self.remove(&key),
            }
        }
        Ok(changed)
    }

    pub(crate) fn remove(&mut self, key: &D::Key) {
        self.handle.lock().remove(key);
        self.endpoints.remove(key);
    }

    /// Picks an endpoint and queues probes. `None` only without endpoints.
    pub(crate) fn choose(&mut self) -> Option<D::Key> {
        loop {
            // `PrequalHandle::sync` on this balancer's handle can make its keys differ from the endpoints.
            if self.handle.len() != self.endpoints.len() {
                self.handle.sync(self.endpoints.keys());
            }
            let chosen = self.handle.choose(&mut self.target_keys)?;
            for target in self.target_keys.drain(..) {
                self.driver.request(target);
            }
            if self.endpoints.contains_key(&chosen) {
                return Some(chosen);
            }
            self.handle.sync(self.endpoints.keys());
        }
    }
}

impl<D: Discover, C> fmt::Debug for PrequalBalance<D, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrequalBalance").field("endpoints", &self.endpoints.len()).finish_non_exhaustive()
    }
}

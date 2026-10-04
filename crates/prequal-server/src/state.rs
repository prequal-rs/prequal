use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};

use prequal_core::{HEADER_LATENCY_US, HEADER_RIF, ProbeResponse};

use crate::{LatencyEstimator, RifOnly};

/// Shared per-server load state: requests in flight plus a latency estimator. Cheap to clone.
#[derive(Debug)]
pub struct ProbeState<E = RifOnly> {
    inner: Arc<Inner<E>>,
}

#[derive(Debug)]
struct Inner<E> {
    rif: AtomicU32,
    estimator: E,
    wants_latency: bool,
    origin: Instant,
}

impl<E> Clone for ProbeState<E> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl Default for ProbeState {
    fn default() -> Self {
        Self::new(RifOnly)
    }
}

impl<E: LatencyEstimator> ProbeState<E> {
    /// Zero requests in flight, reporting latency from `estimator`.
    #[must_use]
    pub fn new(estimator: E) -> Self {
        let wants_latency = estimator.wants_latency();
        let inner = Inner { rif: AtomicU32::new(0), estimator, wants_latency, origin: Instant::now() };
        Self { inner: Arc::new(inner) }
    }

    /// Requests currently in flight.
    pub fn rif(&self) -> u32 {
        self.inner.rif.load(Ordering::Relaxed)
    }

    fn now_us(&self) -> u64 {
        if self.inner.wants_latency { self.inner.origin.elapsed().as_micros() as u64 } else { 0 }
    }

    /// Counts a request as in flight until the returned guard drops, then records its latency.
    #[must_use = "the request stops counting as in flight when the guard drops"]
    pub fn start(&self) -> InFlight<E> {
        let rif_at_arrival = self.inner.rif.fetch_add(1, Ordering::Relaxed) + 1;
        InFlight { state: self.clone(), rif_at_arrival, start_us: self.now_us() }
    }

    /// The current load report.
    pub fn probe(&self) -> ProbeResponse {
        let rif = self.rif();
        let latency_us = if self.inner.wants_latency { self.inner.estimator.estimate(rif, self.now_us()) } else { 0 };
        ProbeResponse { rif, latency_us }
    }

    /// The current load report as `(header name, value)` pairs, to answer a probe or piggyback on a response.
    pub fn probe_headers(&self) -> [(&'static str, String); 2] {
        let p = self.probe();
        [(HEADER_RIF, p.rif.to_string()), (HEADER_LATENCY_US, p.latency_us.to_string())]
    }
}

/// One request in flight ([`ProbeState::start`]); dropping it ends the request and records its latency.
#[derive(Debug)]
#[must_use = "the request stops counting as in flight when the guard drops"]
pub struct InFlight<E: LatencyEstimator> {
    state: ProbeState<E>,
    rif_at_arrival: u32,
    start_us: u64,
}

impl<E: LatencyEstimator> Drop for InFlight<E> {
    fn drop(&mut self) {
        let inner = &self.state.inner;
        inner.rif.fetch_sub(1, Ordering::Relaxed);
        if inner.wants_latency {
            let now = self.state.now_us();
            inner.estimator.record(self.rif_at_arrival, now - self.start_us, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_requests_in_flight() {
        let state = ProbeState::default();
        let a = state.start();
        let b = state.start();
        assert_eq!(state.probe().rif, 2);
        drop(a);
        assert_eq!(state.probe_headers()[0], (HEADER_RIF, "1".to_owned()));
        drop(b);
        assert_eq!(state.rif(), 0);
    }
}

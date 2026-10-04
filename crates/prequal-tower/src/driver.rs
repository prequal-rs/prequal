use std::{
    fmt,
    hash::Hash,
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::sync::mpsc;

use crate::{PrequalHandle, ProbeCounts, Prober, stats::ProbeStats};

#[derive(Clone, Copy)]
struct Limits {
    timeout: Duration,
    max_in_flight: usize,
}

type Start<K> = Box<dyn Fn(Limits) -> mpsc::Sender<K> + Send + Sync>;

/// Runs all probes for one balancer on a single Tokio task, spawned on the first request.
/// Answers feed the handle's pool; health (ejection) is driven by real request outcomes only.
/// A bounded queue plus an in-flight cap make probing shed load instead of piling up.
pub struct ProbeDriver<K> {
    limits: Limits,
    start: Start<K>,
    sender: OnceLock<mpsc::Sender<K>>,
    stats: Arc<ProbeStats>,
}

impl<K> ProbeDriver<K>
where
    K: Clone + Eq + Hash + Send + 'static,
{
    /// Feeds `prober`'s answers to `handle`. Defaults: 50 ms probe timeout, at most 64 probes outstanding.
    #[must_use]
    pub fn new<P: Prober<K>>(prober: P, handle: PrequalHandle<K>) -> Self {
        let prober = Arc::new(prober);
        let stats = Arc::new(ProbeStats::default());
        let task_stats = Arc::clone(&stats);
        let start: Start<K> =
            Box::new(move |limits| spawn(Arc::clone(&prober), handle.clone(), Arc::clone(&task_stats), limits));
        let limits = Limits { timeout: Duration::from_millis(50), max_in_flight: 64 };
        Self { limits, start, sender: OnceLock::new(), stats }
    }

    /// Probes slower than `timeout` count as timed out. Takes effect only before the first request.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.limits.timeout = timeout;
        self
    }

    /// Caps outstanding probes (at least 1). Takes effect only before the first request.
    #[must_use]
    pub fn with_max_in_flight(mut self, max: usize) -> Self {
        self.limits.max_in_flight = max.max(1);
        self
    }

    /// Queues a probe of `key`, or counts it as skipped when the queue is full.
    /// Needs a Tokio runtime on first call.
    pub fn request(&self, key: K) {
        let sender = self.sender.get_or_init(|| (self.start)(self.limits));
        if sender.try_send(key).is_err() {
            ProbeStats::bump(&self.stats.skipped);
        }
    }

    /// Probe outcomes since creation.
    pub fn counts(&self) -> ProbeCounts {
        self.stats.snapshot()
    }
}

impl<K> fmt::Debug for ProbeDriver<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeDriver")
            .field("timeout", &self.limits.timeout)
            .field("max_in_flight", &self.limits.max_in_flight)
            .field("counts", &self.stats.snapshot())
            .finish_non_exhaustive()
    }
}

fn spawn<K, P>(prober: Arc<P>, handle: PrequalHandle<K>, stats: Arc<ProbeStats>, limits: Limits) -> mpsc::Sender<K>
where
    K: Clone + Eq + Hash + Send + 'static,
    P: Prober<K>,
{
    let (tx, mut rx) = mpsc::channel::<K>(limits.max_in_flight);
    tokio::spawn(async move {
        let mut in_flight = FuturesUnordered::new();
        loop {
            tokio::select! {
                target = rx.recv(), if in_flight.len() < limits.max_in_flight => {
                    let Some(key) = target else { break };
                    ProbeStats::bump(&stats.sent);
                    let prober = Arc::clone(&prober);
                    in_flight.push(async move {
                        let outcome = tokio::time::timeout(limits.timeout, prober.probe(&key)).await;
                        (key, outcome)
                    });
                }
                Some((key, outcome)) = in_flight.next(), if !in_flight.is_empty() => {
                    match outcome {
                        Ok(Some(response)) => {
                            ProbeStats::bump(&stats.answered);
                            handle.record(&key, response);
                        }
                        Ok(None) => ProbeStats::bump(&stats.failed),
                        Err(_) => ProbeStats::bump(&stats.timed_out),
                    }
                }
            }
        }
    });
    tx
}

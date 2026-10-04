use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub(crate) struct ProbeStats {
    pub(crate) sent: AtomicU64,
    pub(crate) answered: AtomicU64,
    pub(crate) failed: AtomicU64,
    pub(crate) timed_out: AtomicU64,
    pub(crate) skipped: AtomicU64,
}

impl ProbeStats {
    pub(crate) fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> ProbeCounts {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        ProbeCounts {
            sent: get(&self.sent),
            answered: get(&self.answered),
            failed: get(&self.failed),
            timed_out: get(&self.timed_out),
            skipped: get(&self.skipped),
        }
    }
}

/// Probe outcomes since the balancer was created. `skipped` probes were dropped because
/// `max_in_flight_probes` were already outstanding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProbeCounts {
    /// Probes started.
    pub sent: u64,
    /// Probes answered with a load report.
    pub answered: u64,
    /// Probes the prober reported as failed.
    pub failed: u64,
    /// Probes that exceeded the probe timeout.
    pub timed_out: u64,
    /// Probes dropped because the in-flight cap was reached.
    pub skipped: u64,
}

impl std::ops::Add for ProbeCounts {
    type Output = Self;

    fn add(self, o: Self) -> Self {
        Self {
            sent: self.sent + o.sent,
            answered: self.answered + o.answered,
            failed: self.failed + o.failed,
            timed_out: self.timed_out + o.timed_out,
            skipped: self.skipped + o.skipped,
        }
    }
}

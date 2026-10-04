use std::sync::atomic::{AtomicU64, Ordering};

/// Server-side latency estimate reported in probes. `rif_at_arrival` includes the request itself.
pub trait LatencyEstimator: Send + Sync + 'static {
    /// A request that arrived at `rif_at_arrival` completed after `latency_us`.
    fn record(&self, rif_at_arrival: u32, latency_us: u64, now_us: u64);

    /// Estimated latency, in microseconds, of a request arriving now at `rif`.
    fn estimate(&self, rif: u32, now_us: u64) -> u64;

    /// `false` lets [`crate::ProbeState`] skip clock reads and `record` calls entirely.
    fn wants_latency(&self) -> bool {
        true
    }
}

impl<E: LatencyEstimator + ?Sized> LatencyEstimator for Box<E> {
    fn record(&self, rif_at_arrival: u32, latency_us: u64, now_us: u64) {
        (**self).record(rif_at_arrival, latency_us, now_us);
    }

    fn estimate(&self, rif: u32, now_us: u64) -> u64 {
        (**self).estimate(rif, now_us)
    }

    fn wants_latency(&self) -> bool {
        (**self).wants_latency()
    }
}

/// Reports zero latency, reducing HCL to "least RIF among probed replicas".
/// The most robust default in our simulation.
#[derive(Clone, Copy, Debug, Default)]
pub struct RifOnly;

impl LatencyEstimator for RifOnly {
    fn record(&self, _: u32, _: u64, _: u64) {}

    fn estimate(&self, _: u32, _: u64) -> u64 {
        0
    }

    fn wants_latency(&self) -> bool {
        false
    }
}

/// Experimental model: EWMA of per-request service time scaled by queueing over `cores`.
/// Avoids the survivorship bias of [`crate::RecentMedian`] right after a RIF jump.
#[derive(Debug)]
pub struct ServiceTimeModel {
    cores: f64,
    alpha: f64,
    ewma_bits: AtomicU64,
}

const NO_SAMPLES: u64 = f64::NAN.to_bits();

impl ServiceTimeModel {
    /// A server with `cores` parallel workers, smoothing service times with EWMA weight `alpha` (0 to 1).
    #[must_use]
    pub fn new(cores: f64, alpha: f64) -> Self {
        Self { cores: cores.max(1.0), alpha: alpha.clamp(0.0, 1.0), ewma_bits: AtomicU64::new(NO_SAMPLES) }
    }

    fn contention(&self, rif: f64) -> f64 {
        (rif / self.cores).max(1.0)
    }
}

impl Default for ServiceTimeModel {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        Self::new(cores as f64, 0.2)
    }
}

impl LatencyEstimator for ServiceTimeModel {
    fn record(&self, rif_at_arrival: u32, latency_us: u64, _: u64) {
        let service = latency_us as f64 / self.contention(f64::from(rif_at_arrival));
        let _ = self.ewma_bits.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
            let prev = f64::from_bits(bits);
            let next = if prev.is_nan() { service } else { prev + self.alpha * (service - prev) };
            Some(next.to_bits())
        });
    }

    fn estimate(&self, rif: u32, _: u64) -> u64 {
        let ewma = f64::from_bits(self.ewma_bits.load(Ordering::Relaxed));
        if ewma.is_nan() { 0 } else { (ewma * self.contention(f64::from(rif) + 1.0)) as u64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_time_scales_with_queueing() {
        let m = ServiceTimeModel::new(4.0, 1.0);
        assert_eq!(m.estimate(0, 0), 0);
        m.record(8, 2_000, 0);
        assert_eq!(m.estimate(3, 0), 1_000);
        assert_eq!(m.estimate(11, 0), 3_000);
    }
}

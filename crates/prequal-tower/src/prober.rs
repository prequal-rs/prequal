use std::future::Future;

use prequal_core::ProbeResponse;

/// Fetches a load report from the replica identified by discovery key `key`.
/// `None` means the probe failed; the balancer simply doesn't add it to the pool.
pub trait Prober<K>: Send + Sync + 'static {
    /// Asks replica `key` for its load.
    fn probe(&self, key: &K) -> impl Future<Output = Option<ProbeResponse>> + Send;
}

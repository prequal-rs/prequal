use std::{net::SocketAddr, sync::Arc};

use pingora_load_balancing::Backend;
use prequal_core::{HEADER_LATENCY_US, HEADER_RIF, ProbeResponse};
use prequal_tower::Prober;

use crate::ProbeClient;

/// Probes backends with `GET <path>`, reading the load headers from `prequal-server`
/// (`x-prequal-rif`, `x-prequal-latency-us`).
#[derive(Clone, Debug)]
pub struct HttpProber {
    path: Arc<str>,
    client: ProbeClient,
}

impl HttpProber {
    /// Probes `GET path` (default `/prequal/probe`).
    #[must_use]
    pub fn new(path: &str) -> Self {
        Self { path: path.into(), client: ProbeClient::default() }
    }
}

impl Default for HttpProber {
    fn default() -> Self {
        Self::new("/prequal/probe")
    }
}

/// Adapts a prober keyed by `SocketAddr` (e.g. `prequal_llm::EngineProber`) to Pingora backends;
/// non-inet backends are reported as failed probes.
#[derive(Clone, Debug)]
pub struct InetProber<P>(pub P);

impl<P: Prober<SocketAddr>> Prober<Backend> for InetProber<P> {
    async fn probe(&self, backend: &Backend) -> Option<ProbeResponse> {
        self.0.probe(backend.addr.as_inet()?).await
    }
}

impl Prober<Backend> for HttpProber {
    async fn probe(&self, backend: &Backend) -> Option<ProbeResponse> {
        let addr = *backend.addr.as_inet()?;
        let response = self.client.get(addr, &self.path).await.ok()?;
        if !(200..300).contains(&response.status) {
            return None;
        }
        ProbeResponse::from_header_values(response.header(HEADER_RIF)?, response.header(HEADER_LATENCY_US)?)
    }
}

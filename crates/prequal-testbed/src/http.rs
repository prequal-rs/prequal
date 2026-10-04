use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use prequal_core::{HEADER_LATENCY_US, HEADER_RIF, ProbeResponse};
use prequal_tower::{PrequalHandle, Prober};
use reqwest::header::HeaderMap;
use tower::Service;

fn load_report(headers: &HeaderMap) -> Option<ProbeResponse> {
    ProbeResponse::from_header_values(
        headers.get(HEADER_RIF)?.to_str().ok()?,
        headers.get(HEADER_LATENCY_US)?.to_str().ok()?,
    )
}

/// One replica's work endpoint; the request is the work size in microseconds.
#[derive(Clone)]
pub struct HttpEndpoint {
    client: reqwest::Client,
    url: String,
    feedback: Option<(PrequalHandle<usize>, usize)>,
}

impl HttpEndpoint {
    pub fn new(client: reqwest::Client, addr: SocketAddr) -> Self {
        Self { client, url: format!("http://{addr}/work"), feedback: None }
    }

    /// Feeds the load report piggybacked on each response into `handle` as replica `replica`.
    pub fn with_feedback(mut self, handle: PrequalHandle<usize>, replica: usize) -> Self {
        self.feedback = Some((handle, replica));
        self
    }
}

impl Service<u64> for HttpEndpoint {
    type Response = ();
    type Error = reqwest::Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), reqwest::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, units_us: u64) -> Self::Future {
        let request = self.client.post(format!("{}?units_us={units_us}", self.url));
        let feedback = self.feedback.clone();
        Box::pin(async move {
            let response = request.send().await?.error_for_status()?;
            if let Some((handle, replica)) = feedback
                && let Some(report) = load_report(response.headers())
            {
                handle.record(&replica, report);
            }
            response.bytes().await?;
            Ok(())
        })
    }
}

pub struct HttpProber {
    client: reqwest::Client,
    urls: Vec<String>,
}

impl HttpProber {
    pub fn new(client: reqwest::Client, addrs: &[SocketAddr]) -> Self {
        Self { client, urls: addrs.iter().map(|a| format!("http://{a}/prequal/probe")).collect() }
    }
}

impl Prober<usize> for HttpProber {
    async fn probe(&self, replica: &usize) -> Option<ProbeResponse> {
        let response = self.client.get(&self.urls[*replica]).send().await.ok()?;
        load_report(response.headers())
    }
}

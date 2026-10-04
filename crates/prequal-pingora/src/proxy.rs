//! Ready-made reverse proxy (feature `proxy`): route with any selection and, for [`Prequal`](crate::Prequal),
//! report outcomes back to the [`PrequalSelector`].

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use pingora_core::{
    Error, ErrorType, Result, server::Server, services::background::background_service, upstreams::peer::HttpPeer,
};
use pingora_http::ResponseHeader;
use pingora_load_balancing::{
    Backend, LoadBalancer,
    health_check::TcpHealthCheck,
    selection::{BackendIter, BackendSelection},
};
use pingora_proxy::{ProxyHttp, Session};

use crate::PrequalSelector;

/// The [`ProxyHttp`] that [`serve`] runs: picks a backend with `S` and reports outcomes to the [`PrequalSelector`].
pub struct BalancedProxy<S: BackendSelection> {
    lb: Arc<LoadBalancer<S>>,
    /// `None` for selections that take no outcome feedback (e.g. round robin).
    selector: Option<PrequalSelector>,
}

impl<S: BackendSelection> std::fmt::Debug for BalancedProxy<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalancedProxy").field("selector", &self.selector).finish_non_exhaustive()
    }
}

#[async_trait]
impl<S> ProxyHttp for BalancedProxy<S>
where
    S: BackendSelection + Send + Sync + 'static,
    S::Iter: BackendIter,
{
    type CTX = Option<Backend>;

    fn new_ctx(&self) -> Self::CTX {
        None
    }

    async fn upstream_peer(&self, _: &mut Session, ctx: &mut Self::CTX) -> Result<Box<HttpPeer>> {
        let backend =
            self.lb.select(b"", 256).ok_or_else(|| Error::explain(ErrorType::ConnectNoRoute, "no healthy backend"))?;
        let addr =
            *backend.addr.as_inet().ok_or_else(|| Error::explain(ErrorType::InternalError, "non-inet backend"))?;
        *ctx = Some(backend);
        Ok(Box::new(HttpPeer::new(addr, false, String::new())))
    }

    async fn upstream_response_filter(
        &self,
        _: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if let (Some(selector), Some(backend)) = (&self.selector, ctx) {
            selector.on_response(backend, response.status.as_u16(), &response.headers);
        }
        Ok(())
    }

    fn fail_to_connect(&self, _: &mut Session, _: &HttpPeer, ctx: &mut Self::CTX, e: Box<Error>) -> Box<Error> {
        if let (Some(selector), Some(backend)) = (&self.selector, ctx) {
            selector.on_failure(backend);
        }
        e
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<Error> {
        if let (Some(selector), Some(backend)) = (&self.selector, ctx) {
            selector.on_failure(backend);
        }
        // Pingora's default retry policy, unchanged.
        let mut e = e.more_context(format!("Peer: {peer}"));
        if !session.req_header().method.is_idempotent() || session.as_ref().retry_buffer_truncated() {
            e.set_retry(false);
        } else {
            e.retry.decide_reuse(client_reused);
        }
        e
    }
}

/// Runs a proxy on `listen` in front of `lb`, with TCP health checks every second and discovery
/// refreshed every 2 s unless `lb.update_frequency` is already set. Pass the
/// balancer's [`PrequalSelector`] (or `None` for selections without feedback). Never returns.
pub fn serve<S>(mut server: Server, mut lb: LoadBalancer<S>, selector: Option<PrequalSelector>, listen: &str) -> !
where
    S: BackendSelection + Send + Sync + 'static,
    S::Iter: BackendIter,
{
    lb.set_health_check(TcpHealthCheck::new());
    lb.health_check_frequency = Some(Duration::from_secs(1));
    lb.update_frequency.get_or_insert(Duration::from_secs(2));
    let background = background_service("health check", lb);
    let proxy = BalancedProxy { lb: background.task(), selector };
    let mut service = pingora_proxy::http_proxy_service(&server.configuration, proxy);
    service.add_tcp(listen);
    server.add_service(background);
    server.add_service(service);
    server.run_forever()
}

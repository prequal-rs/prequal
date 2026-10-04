//! Health endpoints on their own port, so no replica path is shadowed: `/healthz` (the process is serving) and
//! `/readyz` (some replica is up and shutdown has not begun).

use std::{
    convert::Infallible,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body_util::Full;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use prequal_llm::Scheduler;
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct Health {
    pub scheduler: Scheduler,
    pub draining: Arc<AtomicBool>,
}

impl Health {
    fn respond<B>(&self, request: &Request<B>) -> Response<Full<Bytes>> {
        let (status, message) = match request.uri().path() {
            "/healthz" => (StatusCode::OK, "ok"),
            "/readyz" if self.draining.load(Ordering::Relaxed) => (StatusCode::SERVICE_UNAVAILABLE, "shutting down"),
            "/readyz" if self.scheduler.up_count() == 0 => (StatusCode::SERVICE_UNAVAILABLE, "no replica up"),
            "/readyz" => (StatusCode::OK, "ready"),
            _ => (StatusCode::NOT_FOUND, "not found"),
        };
        let mut response = Response::new(Full::new(Bytes::from_static(message.as_bytes())));
        *response.status_mut() = status;
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        response
    }
}

/// Serves the health endpoints on `listener` until the process exits.
pub async fn serve(listener: TcpListener, health: Health) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                accept_failed(&e).await;
                continue;
            }
        };
        let health = health.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let response = health.respond(&request);
                async move { Ok::<_, Infallible>(response) }
            });
            let _ = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(5))
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

/// Out of descriptors and the like: logs and backs off instead of spinning.
pub async fn accept_failed(error: &io::Error) {
    eprintln!("prequal-router: accept failed: {error}");
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use prequal_llm::policy;

    use super::*;

    #[test]
    fn ready_only_with_a_replica_up_and_not_draining() {
        let health = Health {
            scheduler: Scheduler::new(policy::by_name("prequal").unwrap()),
            draining: Arc::new(AtomicBool::new(false)),
        };
        let status = |path: &str| health.respond(&Request::get(path).body(()).unwrap()).status();
        assert_eq!((status("/healthz"), status("/readyz")), (StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE));
        let replica: SocketAddr = "10.0.0.1:8000".parse().unwrap();
        health.scheduler.sync([replica]);
        assert_eq!(status("/readyz"), StatusCode::OK);
        health.scheduler.mark_down(replica);
        assert_eq!(status("/readyz"), StatusCode::SERVICE_UNAVAILABLE, "every replica down");
        health.scheduler.sync([]);
        health.scheduler.sync([replica]);
        health.draining.store(true, Ordering::Relaxed);
        assert_eq!((status("/healthz"), status("/readyz")), (StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE));
        assert_eq!(status("/v1/models"), StatusCode::NOT_FOUND);
    }
}

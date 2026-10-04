//! The data path: buffer the request body, route it, forward it, and stream the response back while telling the
//! routing ticket when the first byte (the first token, for streaming completions) and the end arrive.

use std::{
    convert::Infallible,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, Uri, header};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use prequal_llm::{DEFAULT_MAX_TOKENS, Prompt, Scheduler, Ticket, max_tokens};
use tokio::sync::Semaphore;

use crate::body::{self, Buffered};

pub type RouterBody = BoxBody<Bytes, hyper::Error>;

/// Per-request limits, shared by every shard.
#[derive(Clone)]
pub struct Limits {
    pub max_body: usize,
    /// Bytes of request bodies buffered at once, across all shards.
    pub buffered: Arc<Semaphore>,
    pub body_timeout: Option<Duration>,
    /// Until the replica's response headers; streamed response bodies have no limit.
    pub response_timeout: Option<Duration>,
}

#[derive(Clone)]
pub struct Forwarder {
    pub scheduler: Scheduler,
    pub client: Client<HttpConnector, Buffered>,
    pub limits: Limits,
}

impl Forwarder {
    pub async fn handle(self, request: Request<Incoming>) -> Result<Response<RouterBody>, Infallible> {
        let (mut parts, body) = request.into_parts();
        let body = match within(self.limits.body_timeout, body::read(body, self.limits.max_body, &self.limits.buffered))
            .await
        {
            Some(Ok(body)) => body,
            Some(Err(e)) => {
                let (status, message) = e.reply();
                return Ok(reply(status, message));
            }
            None => return Ok(reply(StatusCode::REQUEST_TIMEOUT, "request body not received in time")),
        };
        let prompt = Prompt::from_body(body.bytes());
        let output = max_tokens(body.bytes()).unwrap_or(DEFAULT_MAX_TOKENS);
        let Some(ticket) = self.scheduler.acquire(&prompt, output, |_| true).await else {
            return Ok(reply(StatusCode::SERVICE_UNAVAILABLE, "no replica available"));
        };
        let path = parts.uri.path_and_query().map_or("/", |p| p.as_str());
        parts.uri = match Uri::try_from(format!("http://{}{path}", ticket.addr())) {
            Ok(uri) => uri,
            Err(_) => return Ok(reply(StatusCode::BAD_REQUEST, "bad request path")),
        };
        parts.headers.remove(header::HOST);
        match within(self.limits.response_timeout, self.client.request(Request::from_parts(parts, body))).await {
            Some(Ok(response)) => {
                let failed = response.status().is_server_error();
                let (parts, body) = response.into_parts();
                let tracked = Tracked { inner: body, ticket: Some(ticket), failed };
                Ok(Response::from_parts(parts, tracked.boxed()))
            }
            Some(Err(_)) => {
                ticket.failed();
                Ok(reply(StatusCode::BAD_GATEWAY, "replica unreachable"))
            }
            None => {
                ticket.failed();
                Ok(reply(StatusCode::GATEWAY_TIMEOUT, "replica sent no response headers in time"))
            }
        }
    }
}

/// `future`'s output, or `None` if `limit` passes first.
async fn within<F: Future>(limit: Option<Duration>, future: F) -> Option<F::Output> {
    match limit {
        Some(limit) => tokio::time::timeout(limit, future).await.ok(),
        None => Some(future.await),
    }
}

fn reply(status: StatusCode, message: &'static str) -> Response<RouterBody> {
    let mut response = Response::new(Full::new(Bytes::from_static(message.as_bytes())).map_err(|e| match e {}).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
}

/// Response body that reports the first data frame and the end (or failure) of the stream to its ticket.
struct Tracked {
    inner: Incoming,
    ticket: Option<Ticket>,
    failed: bool,
}

impl Body for Tracked {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let frame = ready!(Pin::new(&mut self.inner).poll_frame(cx));
        match &frame {
            Some(Ok(f)) if f.is_data() => {
                if let Some(ticket) = self.ticket.as_mut() {
                    ticket.first_token();
                }
            }
            Some(Err(_)) => self.failed = true,
            _ => {}
        }
        if !matches!(frame, Some(Ok(_))) {
            self.finish();
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Tracked {
    fn finish(&mut self) {
        match self.ticket.take() {
            Some(ticket) if self.failed => ticket.failed(),
            _ => {}
        }
    }
}

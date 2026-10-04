use std::{
    fmt,
    future::Future,
    hash::Hash,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use tower::BoxError;

use crate::PrequalHandle;

/// Decides whether a successful (`Ok`) response still counts as a failure for outlier ejection,
/// e.g. an HTTP 5xx. `Err` results always count as failures.
pub trait Classify<Resp>: Send + Sync + 'static {
    /// Whether `response` counts as a failure of the replica that sent it.
    fn is_failure(&self, response: &Resp) -> bool;
}

/// Default classifier: only `Err` results are failures.
#[derive(Clone, Copy, Debug, Default)]
pub struct ErrorsOnly;

impl<Resp> Classify<Resp> for ErrorsOnly {
    fn is_failure(&self, _: &Resp) -> bool {
        false
    }
}

impl<Resp, F> Classify<Resp> for F
where
    F: Fn(&Resp) -> bool + Send + Sync + 'static,
{
    fn is_failure(&self, response: &Resp) -> bool {
        self(response)
    }
}

pin_project! {
    /// Reports the outcome of a routed request to the balancer's health tracking.
    pub struct ResponseFuture<F, K, C> {
        #[pin]
        inner: F,
        report: Option<(PrequalHandle<K>, K)>,
        classify: Arc<C>,
    }
}

impl<F, K, C> ResponseFuture<F, K, C> {
    pub(crate) fn new(inner: F, handle: PrequalHandle<K>, key: K, classify: Arc<C>) -> Self {
        Self { inner, report: Some((handle, key)), classify }
    }
}

impl<F, K, C> fmt::Debug for ResponseFuture<F, K, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFuture").field("reported", &self.report.is_none()).finish_non_exhaustive()
    }
}

impl<F, K, C, T, E> Future for ResponseFuture<F, K, C>
where
    F: Future<Output = Result<T, E>>,
    E: Into<BoxError>,
    K: Clone + Eq + Hash,
    C: Classify<T>,
{
    type Output = Result<T, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let result = std::task::ready!(this.inner.poll(cx));
        if let Some((handle, key)) = this.report.take() {
            match &result {
                Ok(response) if !this.classify.is_failure(response) => handle.record_success(&key),
                _ => handle.record_failure(&key),
            }
        }
        Poll::Ready(result.map_err(Into::into))
    }
}

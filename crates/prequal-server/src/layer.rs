use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use tower_layer::Layer;
use tower_service::Service;

use crate::{InFlight, LatencyEstimator, ProbeState};

/// Wraps a service so every request is counted in [`ProbeState`]'s RIF and latency estimate.
/// Apply it to the routes that do real work, not to the probe endpoint.
#[derive(Debug)]
pub struct ProbeLayer<E> {
    state: ProbeState<E>,
}

impl<E> Clone for ProbeLayer<E> {
    fn clone(&self) -> Self {
        Self { state: self.state.clone() }
    }
}

impl<E> ProbeLayer<E> {
    /// Counts requests in `state`.
    #[must_use]
    pub fn new(state: ProbeState<E>) -> Self {
        Self { state }
    }
}

impl<S, E> Layer<S> for ProbeLayer<E> {
    type Service = ProbeService<S, E>;

    fn layer(&self, inner: S) -> Self::Service {
        ProbeService { inner, state: self.state.clone() }
    }
}

/// The service [`ProbeLayer`] produces: `S`, with every request counted in the [`ProbeState`].
#[derive(Debug)]
pub struct ProbeService<S, E> {
    inner: S,
    state: ProbeState<E>,
}

impl<S: Clone, E> Clone for ProbeService<S, E> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), state: self.state.clone() }
    }
}

impl<S, E, Req> Service<Req> for ProbeService<S, E>
where
    S: Service<Req>,
    E: LatencyEstimator,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future, E>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let guard = self.state.start();
        ResponseFuture { inner: self.inner.call(req), guard: Some(guard) }
    }
}

pin_project! {
    /// [`ProbeService`]'s response future: the request counts as in flight until it completes (or is dropped).
    #[derive(Debug)]
    pub struct ResponseFuture<F, E>
    where
        E: LatencyEstimator,
    {
        #[pin]
        inner: F,
        guard: Option<InFlight<E>>,
    }
}

impl<F: Future, E: LatencyEstimator> Future for ResponseFuture<F, E> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let out = std::task::ready!(this.inner.poll(cx));
        this.guard.take();
        Poll::Ready(out)
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, future::Ready, task::Waker};

    use super::*;
    use crate::RifOnly;

    #[derive(Clone)]
    struct Echo;

    impl Service<u32> for Echo {
        type Response = u32;
        type Error = Infallible;
        type Future = Ready<Result<u32, Infallible>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: u32) -> Self::Future {
            std::future::ready(Ok(req))
        }
    }

    #[test]
    fn counts_until_response_completes() {
        let state = ProbeState::new(RifOnly);
        let mut svc = ProbeLayer::new(state.clone()).layer(Echo);
        let mut fut = Box::pin(svc.call(5));
        assert_eq!(state.rif(), 1);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(5))));
        assert_eq!(state.rif(), 0);
    }
}

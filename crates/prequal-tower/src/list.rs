use std::{
    convert::Infallible,
    iter::Enumerate,
    pin::Pin,
    task::{Context, Poll},
    vec,
};

use futures_util::Stream;
use tower::discover::Change;

/// Fixed discovery over a list of services, keyed by position. Unlike tower's `ServiceList`, it
/// needs no request-type annotation.
#[derive(Debug)]
pub struct StaticList<S> {
    inner: Enumerate<vec::IntoIter<S>>,
}

impl<S> StaticList<S> {
    /// Discovers `services[i]` under key `i`.
    #[must_use]
    pub fn new(services: Vec<S>) -> Self {
        Self { inner: services.into_iter().enumerate() }
    }
}

impl<S> Unpin for StaticList<S> {}

impl<S> Stream for StaticList<S> {
    type Item = Result<Change<usize, S>, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.inner.next().map(|(key, service)| Ok(Change::Insert(key, service))))
    }
}

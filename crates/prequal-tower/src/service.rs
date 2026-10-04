use std::{
    hash::Hash,
    sync::Arc,
    task::{Context, Poll},
};

use tower::{BoxError, discover::Discover};
use tower_service::Service;

use crate::{Classify, PrequalBalance, ResponseFuture};

impl<D, C, Req> Service<Req> for PrequalBalance<D, C>
where
    D: Discover + Unpin,
    D::Key: Clone + Eq + Hash + Send + 'static,
    D::Error: Into<BoxError>,
    D::Service: Service<Req>,
    <D::Service as Service<Req>>::Error: Into<BoxError>,
    C: Classify<<D::Service as Service<Req>>::Response>,
{
    type Response = <D::Service as Service<Req>>::Response;
    type Error = BoxError;
    type Future = ResponseFuture<<D::Service as Service<Req>>::Future, D::Key, C>;

    /// Pending while discovery has produced no endpoints. An endpoint whose `poll_ready` fails is
    /// evicted (as tower's p2c does) until discovery inserts it again.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.poll_discover(cx)? {
            self.chosen = None;
        }
        loop {
            let key = match &self.chosen {
                Some(chosen) => chosen.clone(),
                None => {
                    let Some(chosen) = self.choose() else { return Poll::Pending };
                    self.chosen = Some(chosen.clone());
                    chosen
                }
            };
            let Some(endpoint) = self.endpoints.get_mut(&key) else {
                self.chosen = None;
                continue;
            };
            match endpoint.poll_ready(cx) {
                Poll::Ready(Ok(())) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    self.chosen = None;
                    self.remove(&key);
                }
            }
        }
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let key = self.chosen.take().expect("poll_ready must succeed before call");
        let inner = self.endpoints.get_mut(&key).expect("poll_ready checked the endpoint").call(req);
        ResponseFuture::new(inner, self.handle.clone(), key, Arc::clone(&self.classify))
    }
}

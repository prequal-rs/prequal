//! Request bodies: read under a per-request cap and a router-wide byte budget, then forwarded from memory.

use std::{
    convert::Infallible,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
use http::StatusCode;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Why a request body could not be buffered.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    TooLarge,
    /// The router-wide budget for buffered bodies is spent.
    Overloaded,
    Client,
}

impl ReadError {
    pub fn reply(&self) -> (StatusCode, &'static str) {
        match self {
            Self::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
            Self::Overloaded => (StatusCode::SERVICE_UNAVAILABLE, "router overloaded: too many request bytes buffered"),
            Self::Client => (StatusCode::BAD_REQUEST, "request body unreadable"),
        }
    }
}

/// A fully buffered request body, holding its share of the byte budget until the upstream connection takes it.
pub struct Buffered {
    data: Option<Bytes>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Buffered {
    pub fn bytes(&self) -> &[u8] {
        self.data.as_deref().unwrap_or_default()
    }
}

/// Buffers `body` (at most `max` bytes), taking each chunk's size from `budget`.
pub async fn read<B: Body<Data = Bytes> + Unpin>(
    mut body: B,
    max: usize,
    budget: &Arc<Semaphore>,
) -> Result<Buffered, ReadError> {
    if body.size_hint().lower() > max as u64 {
        return Err(ReadError::TooLarge);
    }
    let mut buffer = BytesMut::new();
    let mut permit: Option<OwnedSemaphorePermit> = None;
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.map_err(|_| ReadError::Client)?.into_data() else { continue };
        if buffer.len() + data.len() > max {
            return Err(ReadError::TooLarge);
        }
        let size = u32::try_from(data.len()).map_err(|_| ReadError::TooLarge)?;
        let more = Arc::clone(budget).try_acquire_many_owned(size).map_err(|_| ReadError::Overloaded)?;
        match permit.as_mut() {
            Some(held) => held.merge(more),
            None => permit = Some(more),
        }
        buffer.extend_from_slice(&data);
    }
    Ok(Buffered { data: Some(buffer.freeze()), permit })
}

impl Body for Buffered {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.permit = None;
        Poll::Ready(self.data.take().filter(|d| !d.is_empty()).map(|d| Ok(Frame::data(d))))
    }

    fn is_end_stream(&self) -> bool {
        self.data.as_ref().is_none_or(Bytes::is_empty)
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.data.as_ref().map_or(0, |d| d.len() as u64))
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::Full;

    use super::*;

    #[tokio::test]
    async fn caps_size_and_budget_and_releases_the_budget_once_sent() {
        let budget = Arc::new(Semaphore::new(10));
        assert_eq!(
            read(Full::new(Bytes::from_static(b"0123456789AB")), 11, &budget).await.err(),
            Some(ReadError::TooLarge)
        );
        let mut held = read(Full::new(Bytes::from_static(b"01234567")), 16, &budget).await.unwrap();
        assert_eq!((held.bytes(), budget.available_permits()), (&b"01234567"[..], 2));
        assert_eq!(read(Full::new(Bytes::from_static(b"abc")), 16, &budget).await.err(), Some(ReadError::Overloaded));
        let frame = held.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!((&frame[..], budget.available_permits()), (&b"01234567"[..], 10));
        assert!(held.is_end_stream());
    }
}

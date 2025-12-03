//! Custom body wrapper for detecting when HTTP responses are fully sent.
//!
//! This module provides `OnResponseSentBody`, a wrapper around HTTP response bodies
//! that fires a callback when the body has been fully consumed by the client.
//! This is used to trigger process freezing at exactly the right moment - after
//! the runtime has received the complete invocation payload.

use http_body::{Body, Frame, SizeHint};
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

pin_project! {
    /// A body wrapper that fires a callback when the body is fully consumed.
    ///
    /// This wrapper is used to detect when an HTTP response has been completely
    /// sent to the client. The callback is invoked when `poll_frame` returns
    /// `Poll::Ready(None)`, indicating no more data is available.
    pub struct OnResponseSentBody<B, F> {
        #[pin]
        inner: B,
        on_complete: Option<F>,
    }
}

impl<B, F> OnResponseSentBody<B, F>
where
    F: FnOnce(),
{
    /// Creates a new body wrapper with a completion callback.
    ///
    /// The callback will be invoked exactly once when the body is fully consumed.
    #[allow(dead_code)]
    pub fn new(inner: B, on_complete: F) -> Self {
        Self {
            inner,
            on_complete: Some(on_complete),
        }
    }
}

impl<B, F> Body for OnResponseSentBody<B, F>
where
    B: Body,
    F: FnOnce(),
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();

        match this.inner.poll_frame(cx) {
            Poll::Ready(None) => {
                if let Some(callback) = this.on_complete.take() {
                    callback();
                }
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::Full;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn test_callback_fires_on_body_end() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let body = Full::new(Bytes::from("hello"));
        let wrapped = OnResponseSentBody::new(body, move || {
            called_clone.store(true, Ordering::SeqCst);
        });

        let collected = http_body_util::BodyExt::collect(wrapped).await.unwrap();
        assert_eq!(collected.to_bytes(), Bytes::from("hello"));
        assert!(called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_callback_fires_once() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_clone = count.clone();

        let body = Full::new(Bytes::from("test"));
        let wrapped = OnResponseSentBody::new(body, move || {
            count_clone.fetch_add(1, Ordering::SeqCst);
        });

        let _ = http_body_util::BodyExt::collect(wrapped).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_empty_body_fires_callback() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let body = Full::new(Bytes::new());
        let wrapped = OnResponseSentBody::new(body, move || {
            called_clone.store(true, Ordering::SeqCst);
        });

        let _ = http_body_util::BodyExt::collect(wrapped).await;
        assert!(called.load(Ordering::SeqCst));
    }
}

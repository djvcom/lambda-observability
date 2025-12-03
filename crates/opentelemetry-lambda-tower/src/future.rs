//! Future implementation that manages span lifecycle and flushing.

use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_semantic_conventions::attribute::{ERROR_MESSAGE, OTEL_STATUS_CODE};
use pin_project::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tracing::Span;

/// Future that wraps an instrumented handler and manages span lifecycle.
///
/// This future:
/// 1. Polls the inner future until completion
/// 2. Records success/error status on the span
/// 3. Ends the span
/// 4. Optionally flushes the tracer provider before returning
///
/// The flush ensures spans are exported before Lambda freezes the process.
/// Critically, the future does NOT return until the flush completes (or times
/// out), ensuring spans are not lost due to Lambda freezing the execution
/// environment.
#[pin_project]
pub struct OtelTracingFuture<F, T, E> {
    #[pin]
    inner: F,
    #[pin]
    flush_future: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    span: Option<Span>,
    tracer_provider: Option<Arc<SdkTracerProvider>>,
    flush_on_end: bool,
    flush_timeout: Duration,
    pending_result: Option<Result<T, E>>,
}

impl<F, T, E> OtelTracingFuture<F, T, E> {
    /// Creates a new tracing future wrapping the given future.
    pub(crate) fn new(
        inner: F,
        span: Span,
        tracer_provider: Option<Arc<SdkTracerProvider>>,
        flush_on_end: bool,
        flush_timeout: Duration,
    ) -> Self {
        Self {
            inner,
            flush_future: None,
            span: Some(span),
            tracer_provider,
            flush_on_end,
            flush_timeout,
            pending_result: None,
        }
    }
}

impl<F, T, E> Future for OtelTracingFuture<F, T, E>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    type Output = Result<T, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        if this.pending_result.is_none() {
            match this.inner.poll(cx) {
                Poll::Ready(result) => {
                    if let Some(span) = this.span.as_ref() {
                        match &result {
                            Ok(_) => {
                                span.record(OTEL_STATUS_CODE, "OK");
                            }
                            Err(e) => {
                                span.record(OTEL_STATUS_CODE, "ERROR");
                                span.record(ERROR_MESSAGE, e.to_string().as_str());
                            }
                        }
                    }

                    let _ = this.span.take();

                    if *this.flush_on_end
                        && let Some(provider) = this.tracer_provider.take()
                    {
                        let timeout = *this.flush_timeout;
                        let flush_future = Box::pin(async move {
                            let _ = tokio::time::timeout(timeout, flush_tracer_provider(provider))
                                .await;
                        });
                        *this.flush_future = Some(flush_future);
                        *this.pending_result = Some(result);
                    } else {
                        return Poll::Ready(result);
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }

        if let Some(flush_fut) = this.flush_future.as_mut().as_pin_mut() {
            match flush_fut.poll(cx) {
                Poll::Ready(()) => {
                    *this.flush_future = None;
                    return Poll::Ready(
                        this.pending_result
                            .take()
                            .expect("pending_result should be set when flushing"),
                    );
                }
                Poll::Pending => return Poll::Pending,
            }
        }

        Poll::Ready(
            this.pending_result
                .take()
                .expect("pending_result should be set"),
        )
    }
}

/// Flushes the tracer provider to ensure spans are exported.
async fn flush_tracer_provider(provider: Arc<SdkTracerProvider>) {
    if let Err(e) = provider.force_flush() {
        tracing::warn!(target: "otel_lifecycle", error = ?e, "Failed to flush tracer provider");
    }
}

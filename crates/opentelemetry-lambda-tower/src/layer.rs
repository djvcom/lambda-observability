//! Tower Layer implementation for OpenTelemetry tracing.

use crate::service::OtelTracingService;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::sync::Arc;
use std::time::Duration;
use tower::Layer;

/// Tower layer that adds OpenTelemetry tracing to Lambda handlers.
///
/// This layer wraps a service with tracing instrumentation that:
/// - Extracts trace context from Lambda events
/// - Creates spans with semantic attributes following OTel conventions
/// - Handles cold start detection
/// - Flushes the tracer provider after each invocation
///
/// # Type Parameters
///
/// * `E` - The trace context extractor type
///
/// # Example
///
/// ```ignore
/// use opentelemetry_lambda_tower::{OtelTracingLayer, HttpEventExtractor};
/// use tower::ServiceBuilder;
///
/// let layer = OtelTracingLayer::new(HttpEventExtractor::new());
///
/// let service = ServiceBuilder::new()
///     .layer(layer)
///     .service(my_handler);
/// ```
#[derive(Clone)]
pub struct OtelTracingLayer<E> {
    extractor: E,
    tracer_provider: Option<Arc<SdkTracerProvider>>,
    logger_provider: Option<Arc<SdkLoggerProvider>>,
    flush_on_end: bool,
    flush_timeout: Duration,
}

impl<E> OtelTracingLayer<E> {
    /// Creates a new tracing layer with the given extractor.
    ///
    /// Uses default settings:
    /// - Flush on end: enabled
    /// - Flush timeout: 5 seconds
    /// - No tracer provider (uses global provider)
    /// - No logger provider (logs not flushed)
    pub fn new(extractor: E) -> Self {
        Self {
            extractor,
            tracer_provider: None,
            logger_provider: None,
            flush_on_end: true,
            flush_timeout: Duration::from_secs(5),
        }
    }

    /// Creates a builder for more detailed configuration.
    pub fn builder(extractor: E) -> OtelTracingLayerBuilder<E> {
        OtelTracingLayerBuilder::new(extractor)
    }
}

impl<S, E> Layer<S> for OtelTracingLayer<E>
where
    E: Clone,
{
    type Service = OtelTracingService<S, E>;

    fn layer(&self, inner: S) -> Self::Service {
        OtelTracingService::new(
            inner,
            self.extractor.clone(),
            self.tracer_provider.clone(),
            self.logger_provider.clone(),
            self.flush_on_end,
            self.flush_timeout,
        )
    }
}

/// Builder for configuring an [`OtelTracingLayer`].
///
/// # Example
///
/// ```ignore
/// use opentelemetry_lambda_tower::{OtelTracingLayer, HttpEventExtractor};
/// use std::time::Duration;
///
/// let layer = OtelTracingLayer::builder(HttpEventExtractor::new())
///     .tracer_provider(my_provider)
///     .flush_timeout(Duration::from_secs(10))
///     .build();
/// ```
#[must_use = "builders do nothing unless .build() is called"]
pub struct OtelTracingLayerBuilder<E> {
    extractor: E,
    tracer_provider: Option<Arc<SdkTracerProvider>>,
    logger_provider: Option<Arc<SdkLoggerProvider>>,
    flush_on_end: bool,
    flush_timeout: Duration,
}

impl<E> OtelTracingLayerBuilder<E> {
    /// Creates a new builder with the given extractor.
    pub fn new(extractor: E) -> Self {
        Self {
            extractor,
            tracer_provider: None,
            logger_provider: None,
            flush_on_end: true,
            flush_timeout: Duration::from_secs(5),
        }
    }

    /// Sets the tracer provider to use for flushing.
    ///
    /// If not set, the layer will attempt to use the global tracer provider.
    pub fn tracer_provider(mut self, provider: Arc<SdkTracerProvider>) -> Self {
        self.tracer_provider = Some(provider);
        self
    }

    /// Sets the logger provider to use for flushing.
    ///
    /// If not set, logs will not be explicitly flushed after each invocation.
    /// This is required if you want to ensure logs are exported before Lambda
    /// freezes the execution environment.
    pub fn logger_provider(mut self, provider: Arc<SdkLoggerProvider>) -> Self {
        self.logger_provider = Some(provider);
        self
    }

    /// Sets whether to flush the tracer provider after each invocation.
    ///
    /// Default: `true`
    ///
    /// Flushing ensures spans are exported before Lambda freezes the
    /// execution environment. Disable only if you're handling flushing
    /// elsewhere (e.g., in an extension).
    pub fn flush_on_end(mut self, flush: bool) -> Self {
        self.flush_on_end = flush;
        self
    }

    /// Sets the timeout for flush operations.
    ///
    /// Default: 5 seconds
    ///
    /// If the flush doesn't complete within this duration, it's abandoned
    /// to prevent blocking the Lambda response.
    pub fn flush_timeout(mut self, timeout: Duration) -> Self {
        self.flush_timeout = timeout;
        self
    }

    /// Builds the configured layer.
    pub fn build(self) -> OtelTracingLayer<E> {
        OtelTracingLayer {
            extractor: self.extractor,
            tracer_provider: self.tracer_provider,
            logger_provider: self.logger_provider,
            flush_on_end: self.flush_on_end,
            flush_timeout: self.flush_timeout,
        }
    }
}

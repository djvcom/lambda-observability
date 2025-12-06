//! Tower Service implementation for OpenTelemetry tracing.

use crate::cold_start::check_cold_start;
use crate::extractor::TraceContextExtractor;
use crate::future::OtelTracingFuture;
use lambda_runtime::LambdaEvent;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_semantic_conventions::attribute::{
    CLOUD_ACCOUNT_ID, CLOUD_PROVIDER, CLOUD_REGION, FAAS_MAX_MEMORY, FAAS_NAME, FAAS_VERSION,
};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::Service;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Tower service that instruments Lambda handlers with OpenTelemetry tracing.
///
/// This service wraps an inner service and:
/// 1. Extracts trace context from the Lambda event
/// 2. Creates a span with appropriate semantic attributes
/// 3. Invokes the inner service within the span context
/// 4. Flushes the tracer provider after completion
///
/// # Type Parameters
///
/// * `S` - The inner service type
/// * `E` - The trace context extractor type
#[derive(Clone)]
pub struct OtelTracingService<S, E> {
    inner: S,
    extractor: E,
    tracer_provider: Option<Arc<SdkTracerProvider>>,
    logger_provider: Option<Arc<SdkLoggerProvider>>,
    flush_on_end: bool,
    flush_timeout: Duration,
}

impl<S, E> OtelTracingService<S, E> {
    /// Creates a new tracing service wrapping the given service.
    pub(crate) fn new(
        inner: S,
        extractor: E,
        tracer_provider: Option<Arc<SdkTracerProvider>>,
        logger_provider: Option<Arc<SdkLoggerProvider>>,
        flush_on_end: bool,
        flush_timeout: Duration,
    ) -> Self {
        Self {
            inner,
            extractor,
            tracer_provider,
            logger_provider,
            flush_on_end,
            flush_timeout,
        }
    }
}

impl<S, E, T> Service<LambdaEvent<T>> for OtelTracingService<S, E>
where
    S: Service<LambdaEvent<T>>,
    S::Error: std::fmt::Display,
    E: TraceContextExtractor<T>,
    T: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = OtelTracingFuture<S::Future, S::Response, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, event: LambdaEvent<T>) -> Self::Future {
        let (payload, lambda_ctx) = event.into_parts();

        // Extract trace context from the event
        let parent_context = self.extractor.extract_context(&payload);

        // Extract any span links (for SQS/SNS batch processing)
        let links = self.extractor.extract_links(&payload);

        // Check for cold start
        let is_cold_start = check_cold_start();

        // Generate span name from extractor
        let span_name = self.extractor.span_name(&payload, &lambda_ctx);

        // Create the span with tracing crate
        let span = tracing::info_span!(
            "lambda.invoke",
            otel.name = %span_name,
            otel.kind = "server",
            faas.trigger = %self.extractor.trigger_type(),
            faas.invocation_id = %lambda_ctx.request_id,
            faas.coldstart = is_cold_start,
        );

        // Set the parent context from extraction
        let _ = span.set_parent(parent_context);

        // Add span links if any
        for link in links {
            span.add_link(link.span_context.clone());
        }

        // Record event-specific attributes
        self.extractor.record_attributes(&payload, &span);

        // Record Lambda context attributes
        record_lambda_context_attributes(&span, &lambda_ctx);

        // Reconstruct the event for the inner service
        let event = LambdaEvent::new(payload, lambda_ctx);

        // Call inner service with the span as parent context.
        // We pass the inner future directly without .instrument() so that we
        // have the only reference to the span. This ensures the span can be
        // fully closed before we flush.
        let future = {
            let _guard = span.enter();
            self.inner.call(event)
        };

        OtelTracingFuture::new(
            future,
            span,
            self.tracer_provider.clone(),
            self.logger_provider.clone(),
            self.flush_on_end,
            self.flush_timeout,
        )
    }
}

/// Records standard Lambda context attributes on the span.
fn record_lambda_context_attributes(span: &Span, ctx: &lambda_runtime::Context) {
    span.record(CLOUD_PROVIDER, "aws");
    span.record(FAAS_NAME, ctx.env_config.function_name.as_str());
    span.record(FAAS_VERSION, ctx.env_config.version.as_str());

    let memory_bytes = ctx.env_config.memory as i64 * 1024 * 1024;
    span.record(FAAS_MAX_MEMORY, memory_bytes);

    if let Ok(region) = std::env::var("AWS_REGION") {
        span.record(CLOUD_REGION, region.as_str());
    }

    span.record("aws.lambda.invoked_arn", ctx.invoked_function_arn.as_str());

    if let Some(account_id) = ctx.invoked_function_arn.split(':').nth(4) {
        span.record(CLOUD_ACCOUNT_ID, account_id);
    }
}

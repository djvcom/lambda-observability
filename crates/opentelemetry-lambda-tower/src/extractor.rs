//! Trait definition for trace context extraction from Lambda events.

use lambda_runtime::Context as LambdaContext;
use opentelemetry::Context;
use opentelemetry::trace::Link;
use tracing::Span;

/// Extracts trace context from Lambda event payloads.
///
/// Different event sources carry trace context in different locations:
/// - HTTP: `traceparent` header using W3C Trace Context
/// - SQS: `AWSTraceHeader` in message system attributes
/// - SNS: Similar to SQS
///
/// Implementations of this trait provide event-specific extraction logic
/// and semantic attributes following OpenTelemetry conventions.
///
/// # Type Parameters
///
/// * `T` - The Lambda event payload type (e.g., `ApiGatewayV2httpRequest`, `SqsEvent`)
///
/// # Example
///
/// ```ignore
/// use opentelemetry_lambda_tower::TraceContextExtractor;
///
/// struct MyExtractor;
///
/// impl TraceContextExtractor<MyEvent> for MyExtractor {
///     fn extract_context(&self, _payload: &MyEvent) -> opentelemetry::Context {
///         opentelemetry::Context::current()
///     }
///
///     fn trigger_type(&self) -> &'static str {
///         "other"
///     }
///
///     fn span_name(&self, _payload: &MyEvent, ctx: &LambdaContext) -> String {
///         ctx.env_config.function_name.clone()
///     }
///
///     fn record_attributes(&self, _payload: &MyEvent, _span: &tracing::Span) {}
/// }
/// ```
pub trait TraceContextExtractor<T>: Clone + Send + Sync + 'static {
    /// Extracts parent context for creating child spans.
    ///
    /// For HTTP events, this extracts the W3C `traceparent` header.
    /// For message-based events, this typically returns the current context
    /// since span links are used instead of parent-child relationships.
    ///
    /// Returns `Context::current()` if no valid parent context is found.
    fn extract_context(&self, payload: &T) -> Context;

    /// Extracts span links for async message correlation.
    ///
    /// Used for SQS/SNS where messages may come from different traces.
    /// Each message's trace context becomes a link rather than a parent,
    /// preserving the async boundary semantics.
    ///
    /// Default implementation returns an empty vector (no links).
    fn extract_links(&self, _payload: &T) -> Vec<Link> {
        vec![]
    }

    /// Returns the FaaS trigger type for semantic conventions.
    ///
    /// Valid values per OpenTelemetry spec:
    /// - `"http"` - HTTP/API Gateway triggers
    /// - `"pubsub"` - Message queue triggers (SQS, SNS)
    /// - `"datasource"` - Database triggers (DynamoDB Streams)
    /// - `"timer"` - Scheduled triggers (EventBridge, CloudWatch Events)
    /// - `"other"` - Other trigger types
    fn trigger_type(&self) -> &'static str;

    /// Generates span name based on event and Lambda context.
    ///
    /// For HTTP events, this should return `"{method} {route}"` format.
    /// For SQS events, this should return `"{queue_name} process"` format.
    /// Falls back to the function name from Lambda context.
    fn span_name(&self, payload: &T, lambda_ctx: &LambdaContext) -> String;

    /// Records event-specific attributes on the span.
    ///
    /// This method is called after span creation to add semantic
    /// attributes specific to the event type. Implementations should
    /// use `span.record()` to add attributes.
    ///
    /// Common attributes by trigger type:
    ///
    /// **HTTP:**
    /// - `http.request.method`
    /// - `url.path`
    /// - `http.route`
    ///
    /// **SQS/SNS:**
    /// - `messaging.system`
    /// - `messaging.operation.type`
    /// - `messaging.destination.name`
    fn record_attributes(&self, payload: &T, span: &Span);
}

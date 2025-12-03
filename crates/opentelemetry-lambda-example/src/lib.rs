//! Example Lambda functions with OpenTelemetry instrumentation.
//!
//! This crate demonstrates the recommended approach for instrumenting AWS Lambda
//! functions with OpenTelemetry using companion crates:
//!
//! - `opentelemetry-configuration` - SDK setup with layered configuration
//! - `opentelemetry-lambda-tower` - Tower middleware for automatic instrumentation
//!
//! ## Supported Event Types
//!
//! - **HTTP** - API Gateway HTTP API (v2) requests
//! - **SQS** - Simple Queue Service batch messages
//! - **SNS** - Simple Notification Service messages
//!
//! ## Example: HTTP Handler
//!
//! ```ignore
//! use opentelemetry_lambda_example::{init_telemetry, create_http_service};
//! use lambda_runtime::run;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), lambda_runtime::Error> {
//!     let guard = init_telemetry()?;
//!     let service = create_http_service(&guard);
//!     run(service).await
//! }
//! ```
//!
//! ## Example: SQS Handler
//!
//! ```ignore
//! use opentelemetry_lambda_example::{init_telemetry, create_sqs_service};
//! use lambda_runtime::run;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), lambda_runtime::Error> {
//!     let guard = init_telemetry()?;
//!     let service = create_sqs_service(&guard);
//!     run(service).await
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aws_lambda_events::apigw::ApiGatewayV2httpRequest;
use aws_lambda_events::sqs::SqsEvent;
use lambda_runtime::LambdaEvent;
use opentelemetry_configuration::{OtelGuard, OtelSdkBuilder, SdkError};
use opentelemetry_lambda_tower::{ApiGatewayV2Extractor, OtelTracingLayer, SqsEventExtractor};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceBuilder;

/// Request payload extracted from the HTTP body.
#[derive(Debug, Deserialize, Default)]
pub struct RequestBody {
    /// The input message to process.
    #[serde(default)]
    pub message: String,

    /// Optional flag to simulate an error.
    #[serde(default)]
    pub simulate_error: bool,

    /// Optional delay in milliseconds to simulate work.
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

/// Response payload from the Lambda function.
#[derive(Debug, Serialize)]
pub struct Response {
    /// The processed message.
    pub message: String,

    /// The request ID from Lambda.
    pub request_id: String,
}

/// HTTP API Gateway v2 response format.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// Response body as JSON string.
    pub body: String,
}

/// SQS batch processing response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqsBatchResponse {
    /// List of message IDs that failed processing.
    pub batch_item_failures: Vec<BatchItemFailure>,
}

/// A failed SQS message.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchItemFailure {
    /// The message ID that failed.
    pub item_identifier: String,
}

/// Initialises the OpenTelemetry SDK using the configuration crate.
///
/// This sets up:
/// - Tracer provider with OTLP export (HTTP or gRPC based on config)
/// - Meter provider for metrics
/// - Logger provider for logs
/// - Tracing subscriber with OpenTelemetry and log bridging
///
/// Configuration is loaded from:
/// 1. Sensible defaults (localhost:4318 for HTTP)
/// 2. Optional config file at `/var/task/otel-config.toml`
/// 3. Environment variables with `OTEL_` prefix
///
/// # Errors
///
/// Returns an error if the SDK fails to initialise.
///
/// # Example
///
/// ```ignore
/// let guard = init_telemetry()?;
/// // guard manages lifecycle - flush and shutdown on drop
/// ```
pub fn init_telemetry() -> Result<OtelGuard, SdkError> {
    OtelSdkBuilder::new()
        .with_file("/var/task/otel-config.toml")
        .with_standard_env()
        .service_name("opentelemetry-lambda-example")
        .service_version(env!("CARGO_PKG_VERSION"))
        .build()
}

/// Creates an instrumented HTTP handler service.
///
/// This returns a Tower service that:
/// 1. Extracts trace context from the `traceparent` header or X-Ray env var
/// 2. Creates a span with FaaS semantic attributes
/// 3. Calls the inner handler
/// 4. Flushes spans to the OTLP exporter before responding
///
/// # Arguments
///
/// * `guard` - The OTel guard containing the tracer provider
///
/// # Example
///
/// ```ignore
/// let guard = init_telemetry()?;
/// let service = create_http_service(&guard);
/// lambda_runtime::run(service).await?;
/// ```
pub fn create_http_service(
    guard: &OtelGuard,
) -> impl tower::Service<
    LambdaEvent<ApiGatewayV2httpRequest>,
    Response = HttpResponse,
    Error = lambda_runtime::Error,
    Future = impl std::future::Future<Output = Result<HttpResponse, lambda_runtime::Error>> + Send,
> + Clone {
    let provider = guard.tracer_provider().map(|p| Arc::new(p.clone()));

    let mut layer_builder = OtelTracingLayer::builder(ApiGatewayV2Extractor::new())
        .flush_on_end(true)
        .flush_timeout(Duration::from_secs(5));

    if let Some(provider) = provider {
        layer_builder = layer_builder.tracer_provider(provider);
    }

    let layer = layer_builder.build();

    ServiceBuilder::new().layer(layer).service_fn(http_handler)
}

/// Creates an instrumented SQS handler service.
///
/// This returns a Tower service that:
/// 1. Creates span links for each message's trace context (batch processing pattern)
/// 2. Creates a span with messaging semantic attributes
/// 3. Processes the batch
/// 4. Flushes spans before responding
///
/// # Arguments
///
/// * `guard` - The OTel guard containing the tracer provider
///
/// # Example
///
/// ```ignore
/// let guard = init_telemetry()?;
/// let service = create_sqs_service(&guard);
/// lambda_runtime::run(service).await?;
/// ```
pub fn create_sqs_service(
    guard: &OtelGuard,
) -> impl tower::Service<
    LambdaEvent<SqsEvent>,
    Response = SqsBatchResponse,
    Error = lambda_runtime::Error,
    Future = impl std::future::Future<Output = Result<SqsBatchResponse, lambda_runtime::Error>> + Send,
> + Clone {
    let provider = guard.tracer_provider().map(|p| Arc::new(p.clone()));

    let mut layer_builder = OtelTracingLayer::builder(SqsEventExtractor::new())
        .flush_on_end(true)
        .flush_timeout(Duration::from_secs(5));

    if let Some(provider) = provider {
        layer_builder = layer_builder.tracer_provider(provider);
    }

    let layer = layer_builder.build();

    ServiceBuilder::new().layer(layer).service_fn(sqs_handler)
}

/// HTTP handler function for use with `lambda_runtime::service_fn`.
///
/// This is the inner handler logic without instrumentation. For production use,
/// prefer `create_http_service` which wraps this with OpenTelemetry tracing.
///
/// For testing, this function can be used directly with `service_fn`:
///
/// ```ignore
/// use lambda_runtime::service_fn;
/// use opentelemetry_lambda_example::http_handler;
///
/// let service = service_fn(http_handler);
/// lambda_runtime::run(service).await;
/// ```
pub async fn http_handler(
    event: LambdaEvent<ApiGatewayV2httpRequest>,
) -> Result<HttpResponse, lambda_runtime::Error> {
    let (request, context) = event.into_parts();
    let request_id = context.request_id.clone();

    let body = request.body.as_deref().unwrap_or("{}");
    let payload: RequestBody = serde_json::from_str(body).unwrap_or_default();

    tracing::info!(
        request_id = %request_id,
        message = %payload.message,
        method = %request.request_context.http.method,
        path = ?request.raw_path,
        "Processing HTTP request"
    );

    if let Some(delay) = payload.delay_ms {
        tracing::debug!(delay_ms = delay, "Simulating work");
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    if payload.simulate_error {
        tracing::error!("Simulated error requested");
        return Ok(HttpResponse {
            status_code: 500,
            body: serde_json::to_string(&serde_json::json!({
                "error": "Simulated error"
            }))?,
        });
    }

    let response_message = if payload.message.is_empty() {
        "Hello from Lambda!".to_string()
    } else {
        format!("Processed: {}", payload.message)
    };

    tracing::info!(response = %response_message, "Request completed");

    let response_body = Response {
        message: response_message,
        request_id,
    };

    Ok(HttpResponse {
        status_code: 200,
        body: serde_json::to_string(&response_body)?,
    })
}

/// The inner SQS handler logic.
async fn sqs_handler(
    event: LambdaEvent<SqsEvent>,
) -> Result<SqsBatchResponse, lambda_runtime::Error> {
    let (sqs_event, context) = event.into_parts();

    tracing::info!(
        request_id = %context.request_id,
        message_count = sqs_event.records.len(),
        "Processing SQS batch"
    );

    let mut batch_item_failures = Vec::new();

    for record in &sqs_event.records {
        let message_id = record.message_id.as_deref().unwrap_or("unknown");

        tracing::info!(
            message_id = %message_id,
            "Processing SQS message"
        );

        let body = record.body.as_deref().unwrap_or("{}");
        let payload: RequestBody = serde_json::from_str(body).unwrap_or_default();

        if let Some(delay) = payload.delay_ms {
            tracing::debug!(delay_ms = delay, message_id = %message_id, "Simulating work");
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        if payload.simulate_error {
            tracing::warn!(message_id = %message_id, "Message processing failed (simulated)");
            batch_item_failures.push(BatchItemFailure {
                item_identifier: message_id.to_string(),
            });
        } else {
            tracing::info!(message_id = %message_id, "Message processed successfully");
        }
    }

    tracing::info!(
        processed = sqs_event.records.len() - batch_item_failures.len(),
        failed = batch_item_failures.len(),
        "Batch processing complete"
    );

    Ok(SqsBatchResponse {
        batch_item_failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_body_defaults() {
        let body: RequestBody = serde_json::from_str("{}").unwrap();
        assert!(body.message.is_empty());
        assert!(!body.simulate_error);
        assert!(body.delay_ms.is_none());
    }

    #[test]
    fn test_request_body_with_values() {
        let body: RequestBody = serde_json::from_str(
            r#"{
            "message": "hello",
            "simulate_error": true,
            "delay_ms": 100
        }"#,
        )
        .unwrap();

        assert_eq!(body.message, "hello");
        assert!(body.simulate_error);
        assert_eq!(body.delay_ms, Some(100));
    }

    #[test]
    fn test_sqs_batch_response_serialization() {
        let response = SqsBatchResponse {
            batch_item_failures: vec![BatchItemFailure {
                item_identifier: "msg-123".to_string(),
            }],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("batchItemFailures"));
        assert!(json.contains("itemIdentifier"));
        assert!(json.contains("msg-123"));
    }
}

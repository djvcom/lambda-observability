//! OpenTelemetry Tower middleware for AWS Lambda.
//!
//! This crate provides a Tower middleware layer that automatically instruments
//! AWS Lambda handlers with OpenTelemetry tracing. It extracts trace context
//! from various event sources (HTTP, SQS, SNS, etc.), creates properly
//! attributed spans following OpenTelemetry semantic conventions, and handles
//! span lifecycle management including flushing before Lambda freezes.
//!
//! # Architecture
//!
//! The middleware uses the `tracing` crate as its primary API, with
//! `tracing-opentelemetry` bridging to OpenTelemetry for export. This allows
//! natural use of `tracing` macros (`info!`, `debug!`, etc.) throughout your
//! handler code.
//!
//! # Usage
//!
//! ```no_run
//! use lambda_runtime::{run, service_fn, LambdaEvent, Error};
//! use opentelemetry_lambda_tower::{OtelTracingLayer, HttpEventExtractor};
//! use aws_lambda_events::apigw::ApiGatewayV2httpRequest;
//! use tower::ServiceBuilder;
//!
//! async fn handler(
//!     event: LambdaEvent<ApiGatewayV2httpRequest>,
//! ) -> Result<serde_json::Value, Error> {
//!     // Your handler logic - spans are automatically created
//!     tracing::info!("Processing request");
//!     Ok(serde_json::json!({"statusCode": 200}))
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Error> {
//!     // Initialise your tracer provider and set up tracing-opentelemetry subscriber
//!     // ...
//!
//!     let tracing_layer = OtelTracingLayer::new(HttpEventExtractor::new());
//!
//!     let service = ServiceBuilder::new()
//!         .layer(tracing_layer)
//!         .service(service_fn(handler));
//!
//!     run(service).await
//! }
//! ```
//!
//! # Trace Context Extraction
//!
//! Different event sources carry trace context in different locations:
//!
//! - **HTTP (API Gateway)**: `traceparent` header using W3C Trace Context
//! - **SQS**: `AWSTraceHeader` in message system attributes (creates span links)
//! - **SNS**: Similar to SQS
//!
//! The middleware automatically detects and extracts context appropriately.
//!
//! # Features
//!
//! - `http` - API Gateway v1/v2 extractor (enabled by default)
//! - `sqs` - SQS event extractor (enabled by default)
//! - `sns` - SNS event extractor
//! - `full` - All extractors

mod cold_start;
mod extractor;
mod future;
mod layer;
mod service;

pub mod extractors;

pub use cold_start::check_cold_start;
pub use extractor::TraceContextExtractor;
pub use future::OtelTracingFuture;
pub use layer::{OtelTracingLayer, OtelTracingLayerBuilder};
pub use service::OtelTracingService;

// Re-export commonly used extractors at crate root
#[cfg(feature = "http")]
pub use extractors::http::{ApiGatewayV1Extractor, ApiGatewayV2Extractor, HttpEventExtractor};

#[cfg(feature = "sqs")]
pub use extractors::sqs::SqsEventExtractor;

#[cfg(feature = "sns")]
pub use extractors::sns::SnsEventExtractor;

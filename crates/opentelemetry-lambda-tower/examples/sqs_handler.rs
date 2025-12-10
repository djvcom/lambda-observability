//! SQS Lambda example with OpenTelemetry tracing.
//!
//! This example mirrors the AWS Lambda Rust Runtime's `basic-sqs` example
//! but adds automatic OpenTelemetry instrumentation via `OtelTracingLayer`.
//!
//! The tracing layer provides:
//! - Automatic trace context extraction from SQS message attributes
//! - Span links for batch message processing (connecting to upstream traces)
//! - Properly attributed spans following OpenTelemetry semantic conventions
//! - Automatic span flushing before Lambda freezes
//!
//! # Running
//!
//! ```bash
//! cargo build --example sqs_handler --release
//! ```

use aws_lambda_events::sqs::SqsEventObj;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use opentelemetry_lambda_tower::{OtelTracingLayer, SqsEventExtractor};
use serde::{Deserialize, Serialize};
use tower::ServiceBuilder;

#[derive(Deserialize, Serialize)]
struct Data {
    id: String,
    text: String,
}

async fn function_handler(event: LambdaEvent<SqsEventObj<Data>>) -> Result<(), Error> {
    for record in &event.payload.records {
        tracing::info!(
            id = ?record.body.id,
            text = ?record.body.text,
            "Processing SQS message"
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    let tracing_layer = OtelTracingLayer::new(SqsEventExtractor::new());

    let service = ServiceBuilder::new()
        .layer(tracing_layer)
        .service(service_fn(function_handler));

    run(service).await
}

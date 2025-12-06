//! HTTP handler Lambda runtime for E2E testing with real processes.
//!
//! This binary provides a complete Lambda runtime with OpenTelemetry instrumentation
//! that exports telemetry via OTLP.
//!
//! Environment variables:
//! - `AWS_LAMBDA_RUNTIME_API` - Required, set by simulator
//! - `OTEL_EXPORTER_OTLP_ENDPOINT` - OTLP collector endpoint (default: http://127.0.0.1:4318)
//! - Standard Lambda environment variables (function name, memory, etc.)

use lambda_runtime::Runtime;
use opentelemetry_lambda_example::{create_http_service, init_telemetry};

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    let guard = init_telemetry().expect("Failed to initialise telemetry");

    let service = create_http_service(&guard);

    // Use Runtime directly instead of run() to avoid the automatic TracingLayer.
    // The OtelTracingLayer already provides tracing with proper flush semantics.
    // Using run() would add a parent "Lambda runtime invoke" span that doesn't
    // get flushed before Lambda freezes the process.
    Runtime::new(service).run().await
}

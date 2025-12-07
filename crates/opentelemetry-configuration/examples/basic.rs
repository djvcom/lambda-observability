//! Basic example demonstrating minimal OpenTelemetry SDK setup.
//!
//! Run with: cargo run --example basic

use opentelemetry_configuration::{OtelSdkBuilder, SdkError};

fn main() -> Result<(), SdkError> {
    // Initialise the OpenTelemetry SDK with default settings.
    // The guard manages provider lifecycle - when dropped, it flushes and shuts down.
    let _guard = OtelSdkBuilder::new()
        .service_name("basic-example")
        .endpoint("http://localhost:4318")
        .build()?;

    // Use the tracing crate for instrumentation
    tracing::info!("Application started");
    tracing::info!(user_id = 42, "Processing request");

    // Spans are automatically exported when the guard is dropped
    Ok(())
}

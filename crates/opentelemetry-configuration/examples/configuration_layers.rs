//! Example demonstrating layered configuration.
//!
//! This shows how configuration is merged from multiple sources with clear
//! precedence: defaults → files → environment variables → programmatic.
//!
//! Run with: OTEL_SERVICE_NAME=env-override cargo run --example configuration_layers

use opentelemetry_configuration::{OtelSdkBuilder, Protocol, SdkError};

fn main() -> Result<(), SdkError> {
    // Configuration is layered with clear precedence:
    // 1. Defaults (built-in sensible defaults)
    // 2. File configuration (TOML files, if present)
    // 3. Environment variables (standard OTEL_* vars)
    // 4. Programmatic configuration (code-level overrides)

    let builder = OtelSdkBuilder::new()
        // Read standard OTEL_* environment variables
        .with_standard_env()
        // Programmatic overrides take precedence over env vars
        .endpoint("http://localhost:4318")
        .protocol(Protocol::HttpBinary)
        .service_name("layered-config-example")
        // Configure resource attributes
        .resource(|r| {
            r.service_version("1.0.0")
                .deployment_environment("development")
                .attribute("custom.team", "platform")
        })
        // Add authentication header
        .header("Authorization", "Bearer my-token")
        // Disable metrics if not needed
        .metrics(false);

    // Extract config for inspection (useful for debugging)
    let config = builder.extract_config()?;
    println!("Effective endpoint: {:?}", config.endpoint.url);
    println!("Service name: {:?}", config.resource.service_name);
    println!("Traces enabled: {}", config.traces.enabled);
    println!("Metrics enabled: {}", config.metrics.enabled);
    println!("Logs enabled: {}", config.logs.enabled);

    // Build and initialise the SDK
    let _guard = OtelSdkBuilder::new()
        .with_standard_env()
        .endpoint("http://localhost:4318")
        .service_name("layered-config-example")
        .build()?;

    tracing::info!("Configuration layers example running");

    Ok(())
}

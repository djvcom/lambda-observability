//! AWS Lambda OpenTelemetry Extension binary.
//!
//! This extension collects OpenTelemetry signals (traces, metrics, logs) from
//! Lambda functions and exports them to configured OTLP backends.
//!
//! # Configuration
//!
//! Configuration is loaded from (in order of priority):
//! 1. Default values
//! 2. Config file: `/var/task/otel-extension.toml`
//! 3. Environment variables with `LAMBDA_OTEL_` prefix
//!
//! # Environment Variables
//!
//! - `LAMBDA_OTEL_EXPORTER_ENDPOINT` - OTLP endpoint URL
//! - `LAMBDA_OTEL_EXPORTER_PROTOCOL` - Protocol: only `http` is supported
//! - `LAMBDA_OTEL_FLUSH_STRATEGY` - Flush strategy: `default`, `end`, `periodic`, `continuous`
//!
//! See the crate documentation for full configuration options.

use opentelemetry_lambda_extension::{Config, ExtensionRuntime, Result};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_tracing()?;

    let config = Config::load()?;
    tracing::debug!(?config, "Configuration loaded");

    ExtensionRuntime::new(config).run().await?;

    Ok(())
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).without_time())
        .with(filter)
        .try_init()?;

    Ok(())
}

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
//! - `LAMBDA_OTEL_EXPORTER_PROTOCOL` - Protocol: `http` or `grpc`
//! - `LAMBDA_OTEL_FLUSH_STRATEGY` - Flush strategy: `default`, `end`, `periodic`, `continuous`
//!
//! See the crate documentation for full configuration options.

use anyhow::{Context, Result};
use opentelemetry_lambda_extension::{Config, ExtensionRuntime};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing().context("failed to initialise tracing subscriber")?;

    let config = Config::load().context("failed to load configuration")?;
    tracing::debug!(?config, "Configuration loaded");

    ExtensionRuntime::new(config)
        .run()
        .await
        .context("extension runtime failed")?;

    Ok(())
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,opentelemetry_lambda_extension=debug"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).without_time())
        .with(filter)
        .try_init()
        .context("failed to initialise tracing registry")?;

    Ok(())
}

//! Configuration loading and management.
//!
//! This module provides layered configuration for the extension using figment.
//! Configuration is loaded from (in order of priority):
//! 1. Default values (compiled in)
//! 2. Config file: `/var/task/otel-extension.toml` (optional)
//! 3. Standard OpenTelemetry environment variables (`OTEL_*`)
//! 4. Extension-specific environment variables (`LAMBDA_OTEL_*`)
//!
//! # Supported Standard Environment Variables
//!
//! The following standard OpenTelemetry environment variables are supported:
//!
//! | Variable | Config Path | Description |
//! |----------|-------------|-------------|
//! | `OTEL_EXPORTER_OTLP_ENDPOINT` | `exporter.endpoint` | OTLP endpoint URL |
//! | `OTEL_EXPORTER_OTLP_PROTOCOL` | `exporter.protocol` | Protocol (http or grpc) |
//! | `OTEL_EXPORTER_OTLP_HEADERS` | `exporter.headers` | Comma-separated key=value pairs |
//! | `OTEL_EXPORTER_OTLP_COMPRESSION` | `exporter.compression` | Compression (gzip or none) |
//!
//! Extension-specific variables with `LAMBDA_OTEL_` prefix take precedence.

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const DEFAULT_CONFIG_PATH: &str = "/var/task/otel-extension.toml";
const ENV_PREFIX: &str = "LAMBDA_OTEL_";

/// OTLP protocol for export.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// gRPC protocol (port 4317).
    Grpc,
    /// HTTP/protobuf protocol (port 4318).
    #[default]
    Http,
}

/// Compression algorithm for OTLP export.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// No compression.
    None,
    /// Gzip compression.
    #[default]
    Gzip,
}

/// Flush strategy for buffered signals.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlushStrategy {
    /// Adaptive strategy based on invocation patterns.
    #[default]
    Default,
    /// Flush at the end of each invocation.
    End,
    /// Periodic flush at fixed intervals.
    Periodic,
    /// Continuous flush every 20 seconds.
    Continuous,
}

/// Main configuration struct for the extension.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// OTLP exporter configuration.
    pub exporter: ExporterConfig,
    /// OTLP receiver configuration.
    pub receiver: ReceiverConfig,
    /// Flush behaviour configuration.
    pub flush: FlushConfig,
    /// Span correlation configuration.
    pub correlation: CorrelationConfig,
    /// Telemetry API configuration.
    pub telemetry_api: TelemetryApiConfig,
}

impl Config {
    /// Loads configuration from all sources.
    ///
    /// Configuration is loaded in the following order (later sources override earlier):
    /// 1. Default values
    /// 2. Config file at `/var/task/otel-extension.toml` (if it exists)
    /// 3. Environment variables with `LAMBDA_OTEL_` prefix
    ///
    /// # Errors
    ///
    /// Returns an error if configuration parsing fails.
    #[allow(clippy::result_large_err)]
    pub fn load() -> Result<Self, figment::Error> {
        Self::load_from_path(DEFAULT_CONFIG_PATH)
    }

    /// Loads configuration from a custom config file path.
    ///
    /// # Errors
    ///
    /// Returns an error if configuration parsing fails.
    #[allow(clippy::result_large_err)]
    pub fn load_from_path<P: AsRef<Path>>(config_path: P) -> Result<Self, figment::Error> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if config_path.as_ref().exists() {
            figment = figment.merge(Toml::file(config_path));
        }

        figment = figment.merge(standard_otel_env());
        figment = figment.merge(Env::prefixed(ENV_PREFIX).split("_"));

        figment.extract()
    }

    /// Creates a new config builder for testing.
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::new()
    }
}

/// OTLP exporter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExporterConfig {
    /// OTLP endpoint URL.
    pub endpoint: Option<String>,
    /// Protocol to use for export.
    pub protocol: Protocol,
    /// Request timeout in milliseconds.
    #[serde(with = "duration_ms")]
    pub timeout: Duration,
    /// Compression algorithm.
    pub compression: Compression,
    /// Additional headers to send with requests.
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl Default for ExporterConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            protocol: Protocol::Http,
            timeout: Duration::from_millis(500),
            compression: Compression::Gzip,
            headers: HashMap::new(),
        }
    }
}

/// OTLP receiver configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReceiverConfig {
    /// gRPC port (default 4317).
    pub grpc_port: u16,
    /// HTTP port (default 4318).
    pub http_port: u16,
    /// Whether to enable the gRPC receiver.
    pub grpc_enabled: bool,
    /// Whether to enable the HTTP receiver.
    pub http_enabled: bool,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            grpc_port: 4317,
            http_port: 4318,
            grpc_enabled: true,
            http_enabled: true,
        }
    }
}

/// Flush behaviour configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FlushConfig {
    /// Flush strategy to use.
    pub strategy: FlushStrategy,
    /// Periodic flush interval in milliseconds.
    #[serde(with = "duration_ms")]
    pub interval: Duration,
    /// Maximum batch size in bytes.
    pub max_batch_bytes: usize,
    /// Maximum entries per batch.
    pub max_batch_entries: usize,
}

impl Default for FlushConfig {
    fn default() -> Self {
        Self {
            strategy: FlushStrategy::Default,
            interval: Duration::from_secs(20),
            max_batch_bytes: 4 * 1024 * 1024,
            max_batch_entries: 1000,
        }
    }
}

/// Span correlation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CorrelationConfig {
    /// Maximum time to wait for parent span context in milliseconds.
    #[serde(with = "duration_ms")]
    pub max_correlation_delay: Duration,
    /// Maximum buffered events per invocation.
    pub max_buffered_events_per_invocation: usize,
    /// Maximum total buffered events.
    pub max_total_buffered_events: usize,
    /// Maximum lifetime for invocation context in milliseconds.
    #[serde(with = "duration_ms")]
    pub max_invocation_lifetime: Duration,
    /// Whether to emit orphaned spans without parent context.
    pub emit_orphaned_spans: bool,
}

impl Default for CorrelationConfig {
    fn default() -> Self {
        Self {
            max_correlation_delay: Duration::from_millis(500),
            max_buffered_events_per_invocation: 50,
            max_total_buffered_events: 500,
            max_invocation_lifetime: Duration::from_secs(15 * 60),
            emit_orphaned_spans: true,
        }
    }
}

/// Telemetry API configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryApiConfig {
    /// Whether to enable Telemetry API subscription.
    pub enabled: bool,
    /// Port for receiving Telemetry API events.
    pub listener_port: u16,
    /// Buffer size for Telemetry API events.
    pub buffer_size: usize,
}

impl Default for TelemetryApiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listener_port: 9999,
            buffer_size: 256,
        }
    }
}

/// Builder for constructing configuration programmatically.
#[must_use = "builders do nothing unless .build() is called"]
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    /// Creates a new config builder with default values.
    pub fn new() -> Self {
        Self {
            config: Config::default(),
        }
    }

    /// Sets the exporter endpoint.
    pub fn exporter_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.exporter.endpoint = Some(endpoint.into());
        self
    }

    /// Sets the exporter protocol.
    pub fn exporter_protocol(mut self, protocol: Protocol) -> Self {
        self.config.exporter.protocol = protocol;
        self
    }

    /// Sets the exporter timeout.
    pub fn exporter_timeout(mut self, timeout: Duration) -> Self {
        self.config.exporter.timeout = timeout;
        self
    }

    /// Sets the flush strategy.
    pub fn flush_strategy(mut self, strategy: FlushStrategy) -> Self {
        self.config.flush.strategy = strategy;
        self
    }

    /// Sets the flush interval.
    pub fn flush_interval(mut self, interval: Duration) -> Self {
        self.config.flush.interval = interval;
        self
    }

    /// Sets the correlation delay.
    pub fn correlation_delay(mut self, delay: Duration) -> Self {
        self.config.correlation.max_correlation_delay = delay;
        self
    }

    /// Sets whether to emit orphaned spans.
    pub fn emit_orphaned_spans(mut self, emit: bool) -> Self {
        self.config.correlation.emit_orphaned_spans = emit;
        self
    }

    /// Enables or disables the gRPC receiver.
    pub fn grpc_receiver(mut self, enabled: bool) -> Self {
        self.config.receiver.grpc_enabled = enabled;
        self
    }

    /// Enables or disables the HTTP receiver.
    pub fn http_receiver(mut self, enabled: bool) -> Self {
        self.config.receiver.http_enabled = enabled;
        self
    }

    /// Sets the gRPC receiver port.
    pub fn grpc_port(mut self, port: u16) -> Self {
        self.config.receiver.grpc_port = port;
        self
    }

    /// Sets the HTTP receiver port.
    pub fn http_port(mut self, port: u16) -> Self {
        self.config.receiver.http_port = port;
        self
    }

    /// Enables or disables the Telemetry API.
    pub fn telemetry_api(mut self, enabled: bool) -> Self {
        self.config.telemetry_api.enabled = enabled;
        self
    }

    /// Builds the configuration.
    pub fn build(self) -> Config {
        self.config
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Partial exporter config for standard OTEL env var overrides.
#[derive(Debug, Default, Serialize)]
struct PartialExporterConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol: Option<Protocol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compression: Option<Compression>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    headers: HashMap<String, String>,
}

/// Partial config for standard OTEL env var overrides.
#[derive(Debug, Default, Serialize)]
struct PartialConfig {
    #[serde(skip_serializing_if = "is_partial_exporter_empty")]
    exporter: PartialExporterConfig,
}

fn is_partial_exporter_empty(config: &PartialExporterConfig) -> bool {
    config.endpoint.is_none()
        && config.protocol.is_none()
        && config.compression.is_none()
        && config.headers.is_empty()
}

fn standard_otel_env() -> Serialized<PartialConfig> {
    let mut config = PartialConfig::default();

    if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        config.exporter.endpoint = Some(endpoint);
    }

    if let Ok(protocol) = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL") {
        config.exporter.protocol = match protocol.to_lowercase().as_str() {
            "grpc" => Some(Protocol::Grpc),
            "http/protobuf" | "http" => Some(Protocol::Http),
            _ => None,
        };
    }

    if let Ok(compression) = std::env::var("OTEL_EXPORTER_OTLP_COMPRESSION") {
        config.exporter.compression = match compression.to_lowercase().as_str() {
            "gzip" => Some(Compression::Gzip),
            "none" => Some(Compression::None),
            _ => None,
        };
    }

    if let Ok(headers_str) = std::env::var("OTEL_EXPORTER_OTLP_HEADERS") {
        for pair in headers_str.split(',') {
            if let Some((key, value)) = pair.split_once('=') {
                config
                    .exporter
                    .headers
                    .insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }

    Serialized::defaults(config)
}

mod duration_ms {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_millis() as u64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ms = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();

        assert!(config.exporter.endpoint.is_none());
        assert_eq!(config.exporter.protocol, Protocol::Http);
        assert_eq!(config.exporter.timeout, Duration::from_millis(500));
        assert_eq!(config.exporter.compression, Compression::Gzip);

        assert_eq!(config.receiver.grpc_port, 4317);
        assert_eq!(config.receiver.http_port, 4318);
        assert!(config.receiver.grpc_enabled);
        assert!(config.receiver.http_enabled);

        assert_eq!(config.flush.strategy, FlushStrategy::Default);
        assert_eq!(config.flush.interval, Duration::from_secs(20));

        assert_eq!(
            config.correlation.max_correlation_delay,
            Duration::from_millis(500)
        );
        assert!(config.correlation.emit_orphaned_spans);

        assert!(config.telemetry_api.enabled);
    }

    #[test]
    fn test_config_builder() {
        let config = Config::builder()
            .exporter_endpoint("https://collector:4318")
            .exporter_protocol(Protocol::Grpc)
            .exporter_timeout(Duration::from_millis(1000))
            .flush_strategy(FlushStrategy::Continuous)
            .flush_interval(Duration::from_secs(10))
            .correlation_delay(Duration::from_millis(200))
            .emit_orphaned_spans(false)
            .grpc_receiver(false)
            .http_receiver(true)
            .grpc_port(5317)
            .http_port(5318)
            .telemetry_api(false)
            .build();

        assert_eq!(
            config.exporter.endpoint,
            Some("https://collector:4318".to_string())
        );
        assert_eq!(config.exporter.protocol, Protocol::Grpc);
        assert_eq!(config.exporter.timeout, Duration::from_millis(1000));
        assert_eq!(config.flush.strategy, FlushStrategy::Continuous);
        assert_eq!(config.flush.interval, Duration::from_secs(10));
        assert_eq!(
            config.correlation.max_correlation_delay,
            Duration::from_millis(200)
        );
        assert!(!config.correlation.emit_orphaned_spans);
        assert!(!config.receiver.grpc_enabled);
        assert!(config.receiver.http_enabled);
        assert_eq!(config.receiver.grpc_port, 5317);
        assert_eq!(config.receiver.http_port, 5318);
        assert!(!config.telemetry_api.enabled);
    }

    #[test]
    fn test_load_from_toml() {
        let toml_content = r#"
[exporter]
endpoint = "https://test-collector:4318"
protocol = "grpc"
timeout = 1000

[receiver]
grpc_port = 5317
http_port = 5318
grpc_enabled = false

[flush]
strategy = "periodic"
interval = 15000

[correlation]
max_correlation_delay = 300
emit_orphaned_spans = false
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_path(temp_file.path()).unwrap();

        assert_eq!(
            config.exporter.endpoint,
            Some("https://test-collector:4318".to_string())
        );
        assert_eq!(config.exporter.protocol, Protocol::Grpc);
        assert_eq!(config.exporter.timeout, Duration::from_millis(1000));
        assert_eq!(config.receiver.grpc_port, 5317);
        assert_eq!(config.receiver.http_port, 5318);
        assert!(!config.receiver.grpc_enabled);
        assert_eq!(config.flush.strategy, FlushStrategy::Periodic);
        assert_eq!(config.flush.interval, Duration::from_secs(15));
        assert_eq!(
            config.correlation.max_correlation_delay,
            Duration::from_millis(300)
        );
        assert!(!config.correlation.emit_orphaned_spans);
    }

    #[test]
    fn test_load_nonexistent_file_uses_defaults() {
        let config = Config::load_from_path("/nonexistent/path/config.toml").unwrap();

        assert!(config.exporter.endpoint.is_none());
        assert_eq!(config.receiver.grpc_port, 4317);
    }

    #[test]
    fn test_protocol_serialization() {
        assert_eq!(serde_json::to_string(&Protocol::Grpc).unwrap(), "\"grpc\"");
        assert_eq!(serde_json::to_string(&Protocol::Http).unwrap(), "\"http\"");
    }

    #[test]
    fn test_compression_serialization() {
        assert_eq!(
            serde_json::to_string(&Compression::None).unwrap(),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&Compression::Gzip).unwrap(),
            "\"gzip\""
        );
    }

    #[test]
    fn test_flush_strategy_serialization() {
        assert_eq!(
            serde_json::to_string(&FlushStrategy::Default).unwrap(),
            "\"default\""
        );
        assert_eq!(
            serde_json::to_string(&FlushStrategy::End).unwrap(),
            "\"end\""
        );
        assert_eq!(
            serde_json::to_string(&FlushStrategy::Periodic).unwrap(),
            "\"periodic\""
        );
        assert_eq!(
            serde_json::to_string(&FlushStrategy::Continuous).unwrap(),
            "\"continuous\""
        );
    }
}

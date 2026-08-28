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
//! | `OTEL_EXPORTER_OTLP_PROTOCOL` | `exporter.protocol` | Protocol (only `http` is supported) |
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
    /// gRPC protocol (port 4317). Parsed for compatibility but not
    /// supported: the exporter rejects it at startup with a clear error.
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
    /// Returns [`ExtensionError::Config`](crate::ExtensionError::Config) if
    /// configuration parsing fails.
    pub fn load() -> crate::error::Result<Self> {
        Self::load_from_path(DEFAULT_CONFIG_PATH)
    }

    /// Loads configuration from a custom config file path.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError::Config`](crate::ExtensionError::Config) if
    /// configuration parsing fails.
    pub fn load_from_path<P: AsRef<Path>>(config_path: P) -> crate::error::Result<Self> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if config_path.as_ref().exists() {
            figment = figment.merge(Toml::file(config_path));
        }

        figment = figment.merge(standard_otel_env());
        figment = figment.merge(prefixed_env());

        Ok(figment.extract()?)
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
    /// HTTP port (default 4318).
    pub http_port: u16,
    /// Whether to enable the HTTP receiver.
    pub http_enabled: bool,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            http_port: 4318,
            http_enabled: true,
        }
    }
}

/// How long to hold the `/next` poll waiting for invocation completion
/// signals before flushing in the post-invocation window.
///
/// Holding `/next` keeps the execution environment thawed so exports cannot
/// be interrupted by a freeze. The hold releases as soon as a completion
/// signal arrives (a wrapper's `POST /invocation/complete` or the
/// `platform.runtimeDone` event), so its cost is normally a few
/// milliseconds of billed duration after the response has been sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompletionWait {
    /// Hold until a completion signal arrives, bounded by the invocation
    /// deadline. Holding is disabled automatically after a hold times out
    /// and re-enabled when signals are next observed.
    #[default]
    Auto,
    /// Never hold `/next`; flush opportunistically at the next INVOKE
    /// instead. Telemetry may be delayed by one invocation and the final
    /// export before a freeze is not guaranteed.
    Off,
    /// As `auto`, but cap the hold at the given duration in milliseconds.
    #[serde(untagged)]
    Cap(#[serde(with = "duration_ms")] Duration),
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
    /// How long to hold `/next` waiting for invocation completion.
    pub completion_wait: CompletionWait,
    /// Maximum bytes of encoded telemetry buffered across all signal
    /// queues. When the budget is exceeded, the oldest signals are dropped.
    ///
    /// This bounds the *encoded* size; the decoded structures on the heap
    /// are typically two to five times larger, so size this well below the
    /// function's memory allowance. Defaults to 10% of
    /// `AWS_LAMBDA_FUNCTION_MEMORY_SIZE` clamped to 4–32 MiB, or 16 MiB
    /// when the variable is not set.
    pub max_queue_bytes: usize,
    /// Maximum number of signals buffered across all signal queues.
    pub max_queue_entries: usize,
}

impl Default for FlushConfig {
    fn default() -> Self {
        Self {
            strategy: FlushStrategy::Default,
            interval: Duration::from_secs(20),
            max_batch_bytes: 4 * 1024 * 1024,
            max_batch_entries: 1000,
            completion_wait: CompletionWait::Auto,
            max_queue_bytes: default_max_queue_bytes(),
            max_queue_entries: 4096,
        }
    }
}

/// Derives the default queue byte budget from the function's memory size.
fn default_max_queue_bytes() -> usize {
    const MIN: usize = 4 * 1024 * 1024;
    const MAX: usize = 32 * 1024 * 1024;
    const FALLBACK: usize = 16 * 1024 * 1024;

    std::env::var("AWS_LAMBDA_FUNCTION_MEMORY_SIZE")
        .ok()
        .and_then(|mb| mb.parse::<usize>().ok())
        .map(|mb| (mb * 1024 * 1024 / 10).clamp(MIN, MAX))
        .unwrap_or(FALLBACK)
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
    /// Buffer size for Telemetry API events. Also sets the capacity of the
    /// channel carrying signals from the OTLP receiver to the aggregator;
    /// when that channel is full the receiver responds with `503` to signal
    /// backpressure.
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

    /// Enables or disables the HTTP receiver.
    pub fn http_receiver(mut self, enabled: bool) -> Self {
        self.config.receiver.http_enabled = enabled;
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

/// Top-level config sections, used to map environment variable names onto
/// nested config paths.
const CONFIG_SECTIONS: &[&str] = &[
    "telemetry_api",
    "exporter",
    "receiver",
    "flush",
    "correlation",
];

/// Builds the `LAMBDA_OTEL_`-prefixed environment provider.
///
/// A plain `split("_")` cannot address fields whose names contain
/// underscores (`LAMBDA_OTEL_RECEIVER_HTTP_PORT` would map to
/// `receiver.http.port` rather than `receiver.http_port`), so variable names
/// are mapped onto `section.field` by matching the known section prefixes.
fn prefixed_env() -> Env {
    Env::prefixed(ENV_PREFIX)
        .map(|key| {
            let lower = key.as_str().to_ascii_lowercase();
            for section in CONFIG_SECTIONS {
                if let Some(field) = lower
                    .strip_prefix(section)
                    .and_then(|rest| rest.strip_prefix('_'))
                {
                    return format!("{section}.{field}").into();
                }
            }
            lower.into()
        })
        .split(".")
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
    use serial_test::serial;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();

        assert!(config.exporter.endpoint.is_none());
        assert_eq!(config.exporter.protocol, Protocol::Http);
        assert_eq!(config.exporter.timeout, Duration::from_millis(500));
        assert_eq!(config.exporter.compression, Compression::Gzip);

        assert_eq!(config.receiver.http_port, 4318);
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
            .http_receiver(true)
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
        assert!(config.receiver.http_enabled);
        assert_eq!(config.receiver.http_port, 5318);
        assert!(!config.telemetry_api.enabled);
    }

    #[test]
    #[serial(config_env)]
    fn test_load_from_toml() {
        let toml_content = r#"
[exporter]
endpoint = "https://test-collector:4318"
protocol = "grpc"
timeout = 1000

[receiver]
http_port = 5318

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
        assert_eq!(config.receiver.http_port, 5318);
        assert_eq!(config.flush.strategy, FlushStrategy::Periodic);
        assert_eq!(config.flush.interval, Duration::from_secs(15));
        assert_eq!(
            config.correlation.max_correlation_delay,
            Duration::from_millis(300)
        );
        assert!(!config.correlation.emit_orphaned_spans);
    }

    #[test]
    #[serial(config_env)]
    fn test_load_nonexistent_file_uses_defaults() {
        let config = Config::load_from_path("/nonexistent/path/config.toml").unwrap();

        assert!(config.exporter.endpoint.is_none());
        assert_eq!(config.receiver.http_port, 4318);
    }

    #[test]
    #[serial(config_env)]
    fn test_env_vars_map_to_multi_word_fields() {
        temp_env::with_vars(
            [
                ("LAMBDA_OTEL_RECEIVER_HTTP_PORT", Some("24418")),
                ("LAMBDA_OTEL_FLUSH_MAX_BATCH_ENTRIES", Some("42")),
                ("LAMBDA_OTEL_TELEMETRY_API_BUFFER_SIZE", Some("77")),
                ("LAMBDA_OTEL_EXPORTER_ENDPOINT", Some("http://env:4318")),
            ],
            || {
                let config = Config::load_from_path("/nonexistent/path/config.toml").unwrap();

                assert_eq!(config.receiver.http_port, 24418);
                assert_eq!(config.flush.max_batch_entries, 42);
                assert_eq!(config.telemetry_api.buffer_size, 77);
                assert_eq!(
                    config.exporter.endpoint,
                    Some("http://env:4318".to_string())
                );
            },
        );
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

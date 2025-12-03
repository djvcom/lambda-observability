//! Configuration types for the OpenTelemetry SDK.
//!
//! These types are designed to be deserialised from multiple sources using
//! figment, supporting layered configuration from defaults, files, and
//! environment variables.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// OTLP export protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// gRPC protocol (default port 4317).
    Grpc,
    /// HTTP with Protocol Buffers encoding (default port 4318).
    #[default]
    #[serde(alias = "http_binary", alias = "http-binary")]
    HttpBinary,
    /// HTTP with JSON encoding (default port 4318).
    #[serde(alias = "http_json", alias = "http-json")]
    HttpJson,
}

impl Protocol {
    /// Returns the default endpoint for this protocol.
    pub fn default_endpoint(&self) -> &'static str {
        match self {
            Protocol::Grpc => "http://localhost:4317",
            Protocol::HttpBinary | Protocol::HttpJson => "http://localhost:4318",
        }
    }

    /// Returns the default port for this protocol.
    pub fn default_port(&self) -> u16 {
        match self {
            Protocol::Grpc => 4317,
            Protocol::HttpBinary | Protocol::HttpJson => 4318,
        }
    }
}

/// Complete OpenTelemetry SDK configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OtelSdkConfig {
    /// Endpoint configuration.
    pub endpoint: EndpointConfig,

    /// Resource configuration.
    pub resource: ResourceConfig,

    /// Traces configuration.
    pub traces: SignalConfig,

    /// Metrics configuration.
    pub metrics: SignalConfig,

    /// Logs configuration.
    pub logs: SignalConfig,

    /// Whether to initialise the tracing subscriber.
    pub init_tracing_subscriber: bool,
}

impl Default for OtelSdkConfig {
    fn default() -> Self {
        Self {
            endpoint: EndpointConfig::default(),
            resource: ResourceConfig::default(),
            traces: SignalConfig::default_enabled(),
            metrics: SignalConfig::default_enabled(),
            logs: SignalConfig::default_enabled(),
            init_tracing_subscriber: true,
        }
    }
}

impl OtelSdkConfig {
    /// Returns the effective endpoint URL, using protocol defaults if not specified.
    pub fn effective_endpoint(&self) -> String {
        self.endpoint
            .url
            .clone()
            .unwrap_or_else(|| self.endpoint.protocol.default_endpoint().to_string())
    }

    /// Returns the endpoint URL for a specific signal type.
    pub fn signal_endpoint(&self, signal_path: &str) -> String {
        let base = self.effective_endpoint();
        let base = base.trim_end_matches('/');

        match self.endpoint.protocol {
            Protocol::Grpc => base.to_string(),
            Protocol::HttpBinary | Protocol::HttpJson => {
                format!("{}{}", base, signal_path)
            }
        }
    }

    /// Merges another config into this one, with `other` taking precedence.
    pub fn merge(mut self, other: Self) -> Self {
        self.endpoint = self.endpoint.merge(other.endpoint);
        self.resource = self.resource.merge(other.resource);
        self.traces = self.traces.merge(other.traces);
        self.metrics = self.metrics.merge(other.metrics);
        self.logs = self.logs.merge(other.logs);

        if other.init_tracing_subscriber != Self::default().init_tracing_subscriber {
            self.init_tracing_subscriber = other.init_tracing_subscriber;
        }

        self
    }
}

/// Endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointConfig {
    /// OTLP endpoint URL.
    ///
    /// If not specified, uses the protocol's default:
    /// - gRPC: `http://localhost:4317`
    /// - HTTP: `http://localhost:4318`
    pub url: Option<String>,

    /// Export protocol.
    pub protocol: Protocol,

    /// Request timeout.
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,

    /// HTTP headers for authentication or customisation.
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            url: None,
            protocol: Protocol::default(),
            timeout: Duration::from_secs(10),
            headers: HashMap::new(),
        }
    }
}

impl EndpointConfig {
    /// Merges another config into this one, with `other` taking precedence.
    pub fn merge(mut self, other: Self) -> Self {
        if other.url.is_some() {
            self.url = other.url;
        }
        if other.protocol != Protocol::default() {
            self.protocol = other.protocol;
        }
        if other.timeout != Self::default().timeout {
            self.timeout = other.timeout;
        }
        self.headers.extend(other.headers);
        self
    }
}

/// Resource configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceConfig {
    /// Service name.
    ///
    /// If not specified, attempts to detect from `AWS_LAMBDA_FUNCTION_NAME`.
    pub service_name: Option<String>,

    /// Service version.
    pub service_version: Option<String>,

    /// Deployment environment (e.g., "production", "staging").
    pub deployment_environment: Option<String>,

    /// Additional resource attributes.
    #[serde(default)]
    pub attributes: HashMap<String, String>,

    /// Whether to detect Lambda resource attributes from environment.
    #[serde(default = "default_true")]
    pub detect_lambda: bool,
}

fn default_true() -> bool {
    true
}

impl ResourceConfig {
    /// Creates a new resource config with a service name.
    pub fn with_service_name(name: impl Into<String>) -> Self {
        Self {
            service_name: Some(name.into()),
            ..Default::default()
        }
    }

    /// Merges another config into this one, with `other` taking precedence.
    pub fn merge(mut self, other: Self) -> Self {
        if other.service_name.is_some() {
            self.service_name = other.service_name;
        }
        if other.service_version.is_some() {
            self.service_version = other.service_version;
        }
        if other.deployment_environment.is_some() {
            self.deployment_environment = other.deployment_environment;
        }
        self.attributes.extend(other.attributes);
        if !other.detect_lambda {
            self.detect_lambda = false;
        }
        self
    }

    /// Detects resource attributes from the Lambda environment.
    pub fn detect_from_environment(&mut self) {
        if !self.detect_lambda {
            return;
        }

        if self.service_name.is_none() {
            self.service_name = std::env::var("AWS_LAMBDA_FUNCTION_NAME").ok();
        }

        if self.service_version.is_none() {
            self.service_version = std::env::var("AWS_LAMBDA_FUNCTION_VERSION").ok();
        }

        if let Ok(region) = std::env::var("AWS_REGION") {
            self.attributes
                .entry("cloud.region".to_string())
                .or_insert(region);
        }

        if let Ok(memory) = std::env::var("AWS_LAMBDA_FUNCTION_MEMORY_SIZE") {
            self.attributes
                .entry("faas.max_memory".to_string())
                .or_insert(memory);
        }

        self.attributes
            .entry("cloud.provider".to_string())
            .or_insert_with(|| "aws".to_string());

        self.attributes
            .entry("faas.instance".to_string())
            .or_insert_with(|| std::env::var("AWS_LAMBDA_LOG_STREAM_NAME").unwrap_or_default());
    }
}

/// Configuration for an individual signal type (traces, metrics, logs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SignalConfig {
    /// Whether this signal is enabled.
    pub enabled: bool,

    /// Batch export configuration.
    pub batch: BatchConfig,
}

impl SignalConfig {
    /// Creates a default config with the signal enabled.
    pub fn default_enabled() -> Self {
        Self {
            enabled: true,
            batch: BatchConfig::default(),
        }
    }

    /// Merges another config into this one, with `other` taking precedence.
    pub fn merge(mut self, other: Self) -> Self {
        self.enabled = other.enabled;
        self.batch = self.batch.merge(other.batch);
        self
    }
}

/// Batch exporter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BatchConfig {
    /// Maximum queue size.
    pub max_queue_size: usize,

    /// Maximum batch size for export.
    pub max_export_batch_size: usize,

    /// Scheduled delay between exports.
    #[serde(with = "humantime_serde")]
    pub scheduled_delay: Duration,

    /// Maximum time to wait for export to complete.
    #[serde(with = "humantime_serde")]
    pub export_timeout: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 2048,
            max_export_batch_size: 512,
            scheduled_delay: Duration::from_secs(5),
            export_timeout: Duration::from_secs(30),
        }
    }
}

impl BatchConfig {
    /// Merges another config into this one, with `other` taking precedence.
    pub fn merge(mut self, other: Self) -> Self {
        let default = Self::default();
        if other.max_queue_size != default.max_queue_size {
            self.max_queue_size = other.max_queue_size;
        }
        if other.max_export_batch_size != default.max_export_batch_size {
            self.max_export_batch_size = other.max_export_batch_size;
        }
        if other.scheduled_delay != default.scheduled_delay {
            self.scheduled_delay = other.scheduled_delay;
        }
        if other.export_timeout != default.export_timeout {
            self.export_timeout = other.export_timeout;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_default() {
        assert_eq!(Protocol::default(), Protocol::HttpBinary);
    }

    #[test]
    fn test_protocol_default_endpoint() {
        assert_eq!(Protocol::Grpc.default_endpoint(), "http://localhost:4317");
        assert_eq!(
            Protocol::HttpBinary.default_endpoint(),
            "http://localhost:4318"
        );
        assert_eq!(
            Protocol::HttpJson.default_endpoint(),
            "http://localhost:4318"
        );
    }

    #[test]
    fn test_protocol_serde() {
        let protocol: Protocol = serde_json::from_str(r#""grpc""#).unwrap();
        assert_eq!(protocol, Protocol::Grpc);

        let protocol: Protocol = serde_json::from_str(r#""httpbinary""#).unwrap();
        assert_eq!(protocol, Protocol::HttpBinary);

        let protocol: Protocol = serde_json::from_str(r#""http_binary""#).unwrap();
        assert_eq!(protocol, Protocol::HttpBinary);

        let protocol: Protocol = serde_json::from_str(r#""http-json""#).unwrap();
        assert_eq!(protocol, Protocol::HttpJson);
    }

    #[test]
    fn test_otel_sdk_config_effective_endpoint() {
        let config = OtelSdkConfig::default();
        assert_eq!(config.effective_endpoint(), "http://localhost:4318");

        let mut config = OtelSdkConfig::default();
        config.endpoint.protocol = Protocol::Grpc;
        assert_eq!(config.effective_endpoint(), "http://localhost:4317");

        let mut config = OtelSdkConfig::default();
        config.endpoint.url = Some("http://collector:4318".to_string());
        assert_eq!(config.effective_endpoint(), "http://collector:4318");
    }

    #[test]
    fn test_otel_sdk_config_signal_endpoint() {
        let config = OtelSdkConfig::default();
        assert_eq!(
            config.signal_endpoint("/v1/traces"),
            "http://localhost:4318/v1/traces"
        );

        let mut config = OtelSdkConfig::default();
        config.endpoint.protocol = Protocol::Grpc;
        assert_eq!(
            config.signal_endpoint("/v1/traces"),
            "http://localhost:4317"
        );
    }

    #[test]
    fn test_resource_config_with_service_name() {
        let config = ResourceConfig::with_service_name("my-service");
        assert_eq!(config.service_name, Some("my-service".to_string()));
    }

    #[test]
    fn test_resource_config_merge() {
        let base = ResourceConfig {
            service_name: Some("base".to_string()),
            service_version: Some("1.0.0".to_string()),
            attributes: [("key1".to_string(), "value1".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        let override_config = ResourceConfig {
            service_name: Some("override".to_string()),
            attributes: [("key2".to_string(), "value2".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        let merged = base.merge(override_config);
        assert_eq!(merged.service_name, Some("override".to_string()));
        assert_eq!(merged.service_version, Some("1.0.0".to_string()));
        assert_eq!(merged.attributes.get("key1"), Some(&"value1".to_string()));
        assert_eq!(merged.attributes.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_signal_config_default() {
        let config = SignalConfig::default();
        assert!(!config.enabled);

        let config = SignalConfig::default_enabled();
        assert!(config.enabled);
    }

    #[test]
    fn test_batch_config_defaults() {
        let config = BatchConfig::default();
        assert_eq!(config.max_queue_size, 2048);
        assert_eq!(config.max_export_batch_size, 512);
        assert_eq!(config.scheduled_delay, Duration::from_secs(5));
        assert_eq!(config.export_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_endpoint_config_merge() {
        let base = EndpointConfig {
            url: Some("http://base:4318".to_string()),
            headers: [("auth".to_string(), "token1".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        let override_config = EndpointConfig {
            url: Some("http://override:4318".to_string()),
            headers: [("x-custom".to_string(), "value".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        let merged = base.merge(override_config);
        assert_eq!(merged.url, Some("http://override:4318".to_string()));
        assert_eq!(merged.headers.get("auth"), Some(&"token1".to_string()));
        assert_eq!(merged.headers.get("x-custom"), Some(&"value".to_string()));
    }
}

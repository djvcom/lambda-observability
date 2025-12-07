//! Builder for OpenTelemetry SDK configuration.
//!
//! The builder supports layered configuration from multiple sources:
//! 1. Compiled defaults (protocol-specific endpoints)
//! 2. Configuration files (TOML, JSON, YAML)
//! 3. Environment variables
//! 4. Programmatic overrides
//!
//! Sources are merged in order, with later sources taking precedence.

use crate::SdkError;
use crate::config::{OtelSdkConfig, Protocol, ResourceConfig};
use crate::fallback::ExportFallback;
use crate::guard::OtelGuard;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use opentelemetry_sdk::Resource;
use std::path::Path;

/// Builder for configuring and initialising the OpenTelemetry SDK.
///
/// # Example
///
/// ```no_run
/// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
///
/// fn main() -> Result<(), SdkError> {
///     // Simple case - uses defaults (localhost:4318 for HTTP)
///     let _guard = OtelSdkBuilder::new().build()?;
///
///     // With environment variables
///     let _guard = OtelSdkBuilder::new()
///         .with_env("OTEL_")
///         .build()?;
///
///     // Full configuration
///     let _guard = OtelSdkBuilder::new()
///         .with_file("/var/task/otel-config.toml")
///         .with_env("OTEL_")
///         .endpoint("http://collector:4318")
///         .service_name("my-lambda")
///         .build()?;
///
///     Ok(())
/// }
/// ```
#[must_use = "builders do nothing unless .build() is called"]
pub struct OtelSdkBuilder {
    figment: Figment,
    fallback: ExportFallback,
    custom_resource: Option<Resource>,
    resource_attributes: std::collections::HashMap<String, String>,
}

impl OtelSdkBuilder {
    /// Creates a new builder with default configuration.
    ///
    /// Defaults include:
    /// - Protocol: HTTP with protobuf encoding
    /// - Endpoint: `http://localhost:4318` (or 4317 for gRPC)
    /// - All signals enabled (traces, metrics, logs)
    /// - Tracing subscriber initialisation enabled
    /// - Lambda resource detection enabled
    pub fn new() -> Self {
        Self {
            figment: Figment::from(Serialized::defaults(OtelSdkConfig::default())),
            fallback: ExportFallback::default(),
            custom_resource: None,
            resource_attributes: std::collections::HashMap::new(),
        }
    }

    /// Creates a builder from an existing figment.
    ///
    /// This allows power users to construct complex configuration chains
    /// before passing them to the SDK builder.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use figment::{Figment, providers::{Env, Format, Toml}};
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// let figment = Figment::new()
    ///     .merge(Toml::file("/etc/otel-defaults.toml"))
    ///     .merge(Toml::file("/var/task/otel-config.toml"))
    ///     .merge(Env::prefixed("OTEL_").split("_"));
    ///
    /// let _guard = OtelSdkBuilder::from_figment(figment)
    ///     .service_name("my-lambda")
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn from_figment(figment: Figment) -> Self {
        Self {
            figment,
            fallback: ExportFallback::default(),
            custom_resource: None,
            resource_attributes: std::collections::HashMap::new(),
        }
    }

    /// Merges configuration from a TOML file.
    ///
    /// If the file doesn't exist, it's silently skipped.
    /// This allows optional configuration files that may or may not be present.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// let _guard = OtelSdkBuilder::new()
    ///     .with_file("/var/task/otel-config.toml")  // Optional
    ///     .with_file("./otel-local.toml")           // For development
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn with_file<P: AsRef<Path>>(mut self, path: P) -> Self {
        let path = path.as_ref();
        if path.exists() {
            self.figment = self.figment.merge(Toml::file(path));
        }
        self
    }

    /// Merges configuration from environment variables with the given prefix.
    ///
    /// Environment variables are split on underscores to match nested config.
    /// For example, with prefix `OTEL_`:
    /// - `OTEL_ENDPOINT_URL` → `endpoint.url`
    /// - `OTEL_ENDPOINT_PROTOCOL` → `endpoint.protocol`
    /// - `OTEL_TRACES_ENABLED` → `traces.enabled`
    /// - `OTEL_RESOURCE_SERVICE_NAME` → `resource.service_name`
    ///
    /// # Example
    ///
    /// ```bash
    /// export OTEL_ENDPOINT_URL=http://collector:4318
    /// export OTEL_ENDPOINT_PROTOCOL=grpc
    /// export OTEL_TRACES_ENABLED=true
    /// export OTEL_RESOURCE_SERVICE_NAME=my-lambda
    /// ```
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// let _guard = OtelSdkBuilder::new()
    ///     .with_env("OTEL_")
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn with_env(mut self, prefix: &str) -> Self {
        self.figment = self.figment.merge(Env::prefixed(prefix).split("_"));
        self
    }

    /// Merges configuration from standard OpenTelemetry environment variables.
    ///
    /// This reads the standard `OTEL_*` environment variables as defined by
    /// the OpenTelemetry specification:
    /// - `OTEL_EXPORTER_OTLP_ENDPOINT` → endpoint URL
    /// - `OTEL_EXPORTER_OTLP_PROTOCOL` → protocol (grpc, http/protobuf, http/json)
    /// - `OTEL_SERVICE_NAME` → service name
    /// - `OTEL_TRACES_EXPORTER` → traces exporter (otlp, none)
    /// - `OTEL_METRICS_EXPORTER` → metrics exporter (otlp, none)
    /// - `OTEL_LOGS_EXPORTER` → logs exporter (otlp, none)
    pub fn with_standard_env(mut self) -> Self {
        // Map standard OTEL env vars to our config structure
        if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            self.figment = self
                .figment
                .merge(Serialized::default("endpoint.url", endpoint));
        }

        if let Ok(protocol) = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL") {
            let protocol = match protocol.as_str() {
                "grpc" => "grpc",
                "http/protobuf" => "httpbinary",
                "http/json" => "httpjson",
                _ => "httpbinary",
            };
            self.figment = self
                .figment
                .merge(Serialized::default("endpoint.protocol", protocol));
        }

        if let Ok(service_name) = std::env::var("OTEL_SERVICE_NAME") {
            self.figment = self
                .figment
                .merge(Serialized::default("resource.service_name", service_name));
        }

        if let Ok(exporter) = std::env::var("OTEL_TRACES_EXPORTER") {
            let enabled = exporter != "none";
            self.figment = self
                .figment
                .merge(Serialized::default("traces.enabled", enabled));
        }

        if let Ok(exporter) = std::env::var("OTEL_METRICS_EXPORTER") {
            let enabled = exporter != "none";
            self.figment = self
                .figment
                .merge(Serialized::default("metrics.enabled", enabled));
        }

        if let Ok(exporter) = std::env::var("OTEL_LOGS_EXPORTER") {
            let enabled = exporter != "none";
            self.figment = self
                .figment
                .merge(Serialized::default("logs.enabled", enabled));
        }

        self
    }

    /// Sets the OTLP endpoint URL explicitly.
    ///
    /// This overrides any configuration from files or environment variables.
    ///
    /// For HTTP protocols, signal-specific paths (`/v1/traces`, `/v1/metrics`,
    /// `/v1/logs`) are appended automatically.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// let _guard = OtelSdkBuilder::new()
    ///     .endpoint("http://collector.internal:4318")
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn endpoint(mut self, url: impl Into<String>) -> Self {
        self.figment = self
            .figment
            .merge(Serialized::default("endpoint.url", url.into()));
        self
    }

    /// Sets the export protocol.
    ///
    /// This overrides any configuration from files or environment variables.
    ///
    /// The default endpoint changes based on protocol:
    /// - `Protocol::Grpc` → `http://localhost:4317`
    /// - `Protocol::HttpBinary` → `http://localhost:4318`
    /// - `Protocol::HttpJson` → `http://localhost:4318`
    pub fn protocol(mut self, protocol: Protocol) -> Self {
        let protocol_str = match protocol {
            Protocol::Grpc => "grpc",
            Protocol::HttpBinary => "httpbinary",
            Protocol::HttpJson => "httpjson",
        };
        self.figment = self
            .figment
            .merge(Serialized::default("endpoint.protocol", protocol_str));
        self
    }

    /// Sets the service name resource attribute.
    ///
    /// This is the most commonly configured resource attribute and identifies
    /// your service in the telemetry backend.
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.figment = self
            .figment
            .merge(Serialized::default("resource.service_name", name.into()));
        self
    }

    /// Sets the service version resource attribute.
    pub fn service_version(mut self, version: impl Into<String>) -> Self {
        self.figment = self.figment.merge(Serialized::default(
            "resource.service_version",
            version.into(),
        ));
        self
    }

    /// Sets the deployment environment resource attribute.
    pub fn deployment_environment(mut self, env: impl Into<String>) -> Self {
        self.figment = self.figment.merge(Serialized::default(
            "resource.deployment_environment",
            env.into(),
        ));
        self
    }

    /// Adds a resource attribute.
    pub fn resource_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.resource_attributes.insert(key.into(), value.into());
        self
    }

    /// Provides a pre-built OpenTelemetry Resource.
    ///
    /// This takes precedence over individual resource configuration.
    /// Use this when you need fine-grained control over resource construction.
    pub fn with_resource(mut self, resource: Resource) -> Self {
        self.custom_resource = Some(resource);
        self
    }

    /// Configures the resource using a builder function.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// let _guard = OtelSdkBuilder::new()
    ///     .resource(|r| r
    ///         .service_name("my-lambda")
    ///         .service_version(env!("CARGO_PKG_VERSION"))
    ///         .deployment_environment("production"))
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn resource<F>(mut self, f: F) -> Self
    where
        F: FnOnce(ResourceConfigBuilder) -> ResourceConfigBuilder,
    {
        let builder = f(ResourceConfigBuilder::new());
        let config = builder.build();

        if let Some(name) = &config.service_name {
            self.figment = self
                .figment
                .merge(Serialized::default("resource.service_name", name.clone()));
        }
        if let Some(version) = &config.service_version {
            self.figment = self.figment.merge(Serialized::default(
                "resource.service_version",
                version.clone(),
            ));
        }
        if let Some(env) = &config.deployment_environment {
            self.figment = self.figment.merge(Serialized::default(
                "resource.deployment_environment",
                env.clone(),
            ));
        }
        for (key, value) in config.attributes {
            self.resource_attributes.insert(key, value);
        }

        self
    }

    /// Enables or disables trace collection.
    ///
    /// Default: enabled
    pub fn traces(mut self, enabled: bool) -> Self {
        self.figment = self
            .figment
            .merge(Serialized::default("traces.enabled", enabled));
        self
    }

    /// Enables or disables metrics collection.
    ///
    /// Default: enabled
    pub fn metrics(mut self, enabled: bool) -> Self {
        self.figment = self
            .figment
            .merge(Serialized::default("metrics.enabled", enabled));
        self
    }

    /// Enables or disables log collection.
    ///
    /// Default: enabled
    pub fn logs(mut self, enabled: bool) -> Self {
        self.figment = self
            .figment
            .merge(Serialized::default("logs.enabled", enabled));
        self
    }

    /// Disables automatic tracing subscriber initialisation.
    ///
    /// By default, the SDK sets up a `tracing-subscriber` with
    /// `tracing-opentelemetry` and `opentelemetry-appender-tracing` integration.
    /// Disable this if you want to configure the subscriber yourself.
    pub fn without_tracing_subscriber(mut self) -> Self {
        self.figment = self
            .figment
            .merge(Serialized::default("init_tracing_subscriber", false));
        self
    }

    /// Sets the fallback strategy for failed exports (planned feature).
    ///
    /// **Note:** The fallback API is defined but not yet wired into the export
    /// pipeline. The handler will be stored but not invoked on export failures.
    /// Full implementation is planned for a future release.
    ///
    /// When implemented, the fallback handler will be called when an export
    /// fails after all retry attempts have been exhausted. It will receive the
    /// original OTLP request payload, which can be preserved via alternative
    /// transport.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, ExportFallback, SdkError};
    ///
    /// let _guard = OtelSdkBuilder::new()
    ///     .fallback(ExportFallback::Stdout)
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn fallback(mut self, fallback: ExportFallback) -> Self {
        self.fallback = fallback;
        self
    }

    /// Sets a custom fallback handler using a closure (planned feature).
    ///
    /// **Note:** The fallback API is defined but not yet wired into the export
    /// pipeline. The handler will be stored but not invoked on export failures.
    /// Full implementation is planned for a future release.
    ///
    /// When implemented, the closure will receive the full
    /// [`ExportFailure`](super::ExportFailure) including the original OTLP
    /// request payload.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// let _guard = OtelSdkBuilder::new()
    ///     .with_fallback(|failure| {
    ///         // Write the protobuf payload to S3, a queue, etc.
    ///         let bytes = failure.request.to_protobuf();
    ///         eprintln!(
    ///             "Failed to export {} ({} bytes): {}",
    ///             failure.request.signal_type(),
    ///             bytes.len(),
    ///             failure.error
    ///         );
    ///         Ok(())
    ///     })
    ///     .build()?;
    /// # Ok::<(), SdkError>(())
    /// ```
    pub fn with_fallback<F>(mut self, f: F) -> Self
    where
        F: Fn(super::ExportFailure) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
            + Send
            + Sync
            + 'static,
    {
        self.fallback = ExportFallback::custom(f);
        self
    }

    /// Adds an HTTP header to all export requests.
    ///
    /// Useful for authentication or custom routing.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let header_key = format!("endpoint.headers.{}", key.into());
        self.figment = self
            .figment
            .merge(Serialized::default(&header_key, value.into()));
        self
    }

    /// Extracts the configuration for inspection or debugging.
    ///
    /// # Errors
    ///
    /// Returns an error if configuration extraction fails or if the endpoint
    /// URL is invalid.
    pub fn extract_config(&self) -> Result<OtelSdkConfig, SdkError> {
        let mut config: OtelSdkConfig = self
            .figment
            .extract()
            .map_err(|e| SdkError::Config(Box::new(e)))?;

        // Merge resource attributes that couldn't go through figment
        config
            .resource
            .attributes
            .extend(self.resource_attributes.clone());

        // Validate endpoint URL if provided
        if let Some(ref url) = config.endpoint.url {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(SdkError::InvalidEndpoint { url: url.clone() });
            }
        }

        Ok(config)
    }

    /// Builds and initialises the OpenTelemetry SDK.
    ///
    /// Returns an [`OtelGuard`] that manages provider lifecycle. When the
    /// guard is dropped, all providers are flushed and shut down.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Configuration extraction fails
    /// - Provider initialisation fails
    /// - Tracing subscriber initialisation fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
    ///
    /// fn main() -> Result<(), SdkError> {
    ///     let _guard = OtelSdkBuilder::new()
    ///         .with_env("OTEL_")
    ///         .service_name("my-lambda")
    ///         .build()?;
    ///
    ///     tracing::info!("Application started");
    ///
    ///     // Guard automatically shuts down providers on drop
    ///     Ok(())
    /// }
    /// ```
    pub fn build(self) -> Result<OtelGuard, SdkError> {
        let mut config: OtelSdkConfig = self
            .figment
            .extract()
            .map_err(|e| SdkError::Config(Box::new(e)))?;

        // Merge resource attributes that couldn't go through figment
        config.resource.attributes.extend(self.resource_attributes);

        // Validate endpoint URL if provided
        if let Some(ref url) = config.endpoint.url {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(SdkError::InvalidEndpoint { url: url.clone() });
            }
        }

        // Detect Lambda resource attributes from environment
        config.resource.detect_from_environment();

        OtelGuard::from_config(config, self.fallback, self.custom_resource)
    }
}

impl Default for OtelSdkBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for resource configuration.
///
/// Used with [`OtelSdkBuilder::resource`] for fluent configuration.
#[derive(Default)]
#[must_use = "builders do nothing unless .build() is called"]
pub struct ResourceConfigBuilder {
    config: ResourceConfig,
}

impl ResourceConfigBuilder {
    /// Creates a new resource config builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the service name.
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.config.service_name = Some(name.into());
        self
    }

    /// Sets the service version.
    pub fn service_version(mut self, version: impl Into<String>) -> Self {
        self.config.service_version = Some(version.into());
        self
    }

    /// Sets the deployment environment.
    pub fn deployment_environment(mut self, env: impl Into<String>) -> Self {
        self.config.deployment_environment = Some(env.into());
        self
    }

    /// Adds a resource attribute.
    pub fn attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.attributes.insert(key.into(), value.into());
        self
    }

    /// Disables automatic Lambda resource detection.
    pub fn without_lambda_detection(mut self) -> Self {
        self.config.detect_lambda = false;
        self
    }

    /// Builds the resource configuration.
    pub fn build(self) -> ResourceConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_default() {
        let builder = OtelSdkBuilder::new();
        let config = builder.extract_config().unwrap();

        assert!(config.traces.enabled);
        assert!(config.metrics.enabled);
        assert!(config.logs.enabled);
        assert!(config.init_tracing_subscriber);
        assert_eq!(config.endpoint.protocol, Protocol::HttpBinary);
    }

    #[test]
    fn test_builder_endpoint() {
        let builder = OtelSdkBuilder::new().endpoint("http://collector:4318");
        let config = builder.extract_config().unwrap();

        assert_eq!(
            config.endpoint.url,
            Some("http://collector:4318".to_string())
        );
    }

    #[test]
    fn test_builder_protocol() {
        let builder = OtelSdkBuilder::new().protocol(Protocol::Grpc);
        let config = builder.extract_config().unwrap();

        assert_eq!(config.endpoint.protocol, Protocol::Grpc);
    }

    #[test]
    fn test_builder_service_name() {
        let builder = OtelSdkBuilder::new().service_name("my-service");
        let config = builder.extract_config().unwrap();

        assert_eq!(config.resource.service_name, Some("my-service".to_string()));
    }

    #[test]
    fn test_builder_disable_signals() {
        let builder = OtelSdkBuilder::new()
            .traces(false)
            .metrics(false)
            .logs(false);
        let config = builder.extract_config().unwrap();

        assert!(!config.traces.enabled);
        assert!(!config.metrics.enabled);
        assert!(!config.logs.enabled);
    }

    #[test]
    fn test_builder_resource_fluent() {
        let builder = OtelSdkBuilder::new().resource(|r| {
            r.service_name("my-service")
                .service_version("1.0.0")
                .deployment_environment("production")
                .attribute("custom.key", "custom.value")
        });
        let config = builder.extract_config().unwrap();

        assert_eq!(config.resource.service_name, Some("my-service".to_string()));
        assert_eq!(config.resource.service_version, Some("1.0.0".to_string()));
        assert_eq!(
            config.resource.deployment_environment,
            Some("production".to_string())
        );
        assert_eq!(
            config.resource.attributes.get("custom.key"),
            Some(&"custom.value".to_string())
        );
    }

    #[test]
    fn test_builder_without_tracing_subscriber() {
        let builder = OtelSdkBuilder::new().without_tracing_subscriber();
        let config = builder.extract_config().unwrap();

        assert!(!config.init_tracing_subscriber);
    }

    #[test]
    fn test_builder_header() {
        let builder = OtelSdkBuilder::new().header("Authorization", "Bearer token123");
        let config = builder.extract_config().unwrap();

        assert_eq!(
            config.endpoint.headers.get("Authorization"),
            Some(&"Bearer token123".to_string())
        );
    }

    #[test]
    fn test_builder_fallback() {
        let builder = OtelSdkBuilder::new().fallback(ExportFallback::Stdout);
        assert!(matches!(builder.fallback, ExportFallback::Stdout));
    }

    #[test]
    fn test_builder_custom_fallback() {
        let builder = OtelSdkBuilder::new().with_fallback(|_failure| Ok(()));
        assert!(matches!(builder.fallback, ExportFallback::Custom(_)));
    }

    #[test]
    fn test_with_standard_env_endpoint() {
        temp_env::with_var("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://custom:4318"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert_eq!(config.endpoint.url, Some("http://custom:4318".to_string()));
        });
    }

    #[test]
    fn test_with_standard_env_service_name() {
        temp_env::with_var("OTEL_SERVICE_NAME", Some("test-service"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert_eq!(config.resource.service_name, Some("test-service".to_string()));
        });
    }

    #[test]
    fn test_with_standard_env_protocol_grpc() {
        temp_env::with_var("OTEL_EXPORTER_OTLP_PROTOCOL", Some("grpc"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert_eq!(config.endpoint.protocol, Protocol::Grpc);
        });
    }

    #[test]
    fn test_with_standard_env_protocol_http_protobuf() {
        temp_env::with_var("OTEL_EXPORTER_OTLP_PROTOCOL", Some("http/protobuf"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert_eq!(config.endpoint.protocol, Protocol::HttpBinary);
        });
    }

    #[test]
    fn test_with_standard_env_traces_disabled() {
        temp_env::with_var("OTEL_TRACES_EXPORTER", Some("none"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert!(!config.traces.enabled);
        });
    }

    #[test]
    fn test_with_standard_env_metrics_disabled() {
        temp_env::with_var("OTEL_METRICS_EXPORTER", Some("none"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert!(!config.metrics.enabled);
        });
    }

    #[test]
    fn test_with_standard_env_logs_disabled() {
        temp_env::with_var("OTEL_LOGS_EXPORTER", Some("none"), || {
            let builder = OtelSdkBuilder::new().with_standard_env();
            let config = builder.extract_config().unwrap();
            assert!(!config.logs.enabled);
        });
    }

    #[test]
    fn test_with_standard_env_multiple_vars() {
        temp_env::with_vars(
            [
                ("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://collector:4317")),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", Some("grpc")),
                ("OTEL_SERVICE_NAME", Some("multi-test")),
                ("OTEL_TRACES_EXPORTER", Some("otlp")),
            ],
            || {
                let builder = OtelSdkBuilder::new().with_standard_env();
                let config = builder.extract_config().unwrap();

                assert_eq!(
                    config.endpoint.url,
                    Some("http://collector:4317".to_string())
                );
                assert_eq!(config.endpoint.protocol, Protocol::Grpc);
                assert_eq!(config.resource.service_name, Some("multi-test".to_string()));
                assert!(config.traces.enabled);
            },
        );
    }

    #[test]
    fn test_programmatic_overrides_env() {
        temp_env::with_vars(
            [
                ("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://env:4318")),
                ("OTEL_SERVICE_NAME", Some("env-service")),
            ],
            || {
                let builder = OtelSdkBuilder::new()
                    .with_standard_env()
                    .endpoint("http://programmatic:4318")
                    .service_name("programmatic-service");
                let config = builder.extract_config().unwrap();

                assert_eq!(
                    config.endpoint.url,
                    Some("http://programmatic:4318".to_string())
                );
                assert_eq!(
                    config.resource.service_name,
                    Some("programmatic-service".to_string())
                );
            },
        );
    }

    #[test]
    fn test_invalid_endpoint_url_rejected() {
        let builder = OtelSdkBuilder::new().endpoint("not-a-valid-url");
        let result = builder.extract_config();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SdkError::InvalidEndpoint { ref url } if url == "not-a-valid-url"),
            "Expected InvalidEndpoint error, got: {:?}",
            err
        );
    }

    #[test]
    fn test_valid_http_endpoint_accepted() {
        let builder = OtelSdkBuilder::new().endpoint("http://localhost:4318");
        let config = builder.extract_config().unwrap();
        assert_eq!(
            config.endpoint.url,
            Some("http://localhost:4318".to_string())
        );
    }

    #[test]
    fn test_valid_https_endpoint_accepted() {
        let builder = OtelSdkBuilder::new().endpoint("https://collector.example.com:4318");
        let config = builder.extract_config().unwrap();
        assert_eq!(
            config.endpoint.url,
            Some("https://collector.example.com:4318".to_string())
        );
    }
}

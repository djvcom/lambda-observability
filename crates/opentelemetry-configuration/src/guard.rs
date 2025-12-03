//! OpenTelemetry provider lifecycle management.
//!
//! The [`OtelGuard`] manages the lifecycle of OpenTelemetry providers (traces,
//! metrics, logs). When dropped, it automatically flushes pending data and
//! shuts down providers gracefully.

use crate::config::{OtelSdkConfig, Protocol};
use crate::error::SdkError;
use crate::fallback::ExportFallback;
use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{
    BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider,
};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{
    BatchConfigBuilder as TraceBatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider,
};
use tonic::metadata::{MetadataKey, MetadataValue};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Guard that manages OpenTelemetry provider lifecycle.
///
/// When this guard is dropped, it automatically flushes and shuts down all
/// configured providers. This ensures telemetry is exported before the Lambda
/// execution environment freezes.
///
/// # Example
///
/// ```no_run
/// use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
///
/// fn main() -> Result<(), SdkError> {
///     let _guard = OtelSdkBuilder::new()
///         .service_name("my-lambda")
///         .build()?;
///
///     tracing::info!("Application running");
///
///     // On drop, all providers are flushed and shut down
///     Ok(())
/// }
/// ```
pub struct OtelGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    #[allow(dead_code)]
    fallback: ExportFallback,
}

impl OtelGuard {
    /// Creates an OtelGuard from configuration.
    ///
    /// This is typically called by [`OtelSdkBuilder::build`](super::OtelSdkBuilder::build).
    pub(crate) fn from_config(
        config: OtelSdkConfig,
        fallback: ExportFallback,
        custom_resource: Option<Resource>,
    ) -> Result<Self, SdkError> {
        let resource = custom_resource.unwrap_or_else(|| build_resource(&config));

        let tracer_provider = if config.traces.enabled {
            Some(build_tracer_provider(&config, resource.clone())?)
        } else {
            None
        };

        let meter_provider = if config.metrics.enabled {
            Some(build_meter_provider(&config, resource.clone())?)
        } else {
            None
        };

        let logger_provider = if config.logs.enabled {
            Some(build_logger_provider(&config, resource)?)
        } else {
            None
        };

        // Set global providers
        if let Some(ref provider) = tracer_provider {
            opentelemetry::global::set_tracer_provider(provider.clone());
        }
        if let Some(ref provider) = meter_provider {
            opentelemetry::global::set_meter_provider(provider.clone());
        }

        // Initialise tracing subscriber if requested
        if config.init_tracing_subscriber {
            init_subscriber(&tracer_provider, &logger_provider)?;
        }

        Ok(Self {
            tracer_provider,
            meter_provider,
            logger_provider,
            fallback,
        })
    }

    /// Returns the tracer provider if configured.
    pub fn tracer_provider(&self) -> Option<&SdkTracerProvider> {
        self.tracer_provider.as_ref()
    }

    /// Returns the meter provider if configured.
    pub fn meter_provider(&self) -> Option<&SdkMeterProvider> {
        self.meter_provider.as_ref()
    }

    /// Returns the logger provider if configured.
    pub fn logger_provider(&self) -> Option<&SdkLoggerProvider> {
        self.logger_provider.as_ref()
    }

    /// Flushes all configured providers.
    ///
    /// This method is called automatically on drop, but can be called manually
    /// if you need to ensure telemetry is exported at a specific point.
    ///
    /// Flush errors are logged via `tracing::warn!` with target `otel_lifecycle`.
    /// To see these warnings, enable the target in your `RUST_LOG` filter:
    /// `RUST_LOG=otel_lifecycle=warn`
    pub fn flush(&self) {
        if let Some(provider) = &self.tracer_provider
            && let Err(e) = provider.force_flush()
        {
            tracing::warn!(target: "otel_lifecycle", error = %e, "Failed to flush tracer provider");
        }

        if let Some(provider) = &self.meter_provider
            && let Err(e) = provider.force_flush()
        {
            tracing::warn!(target: "otel_lifecycle", error = %e, "Failed to flush meter provider");
        }

        if let Some(provider) = &self.logger_provider
            && let Err(e) = provider.force_flush()
        {
            tracing::warn!(target: "otel_lifecycle", error = %e, "Failed to flush logger provider");
        }
    }

    /// Shuts down all configured providers.
    ///
    /// This consumes the guard and shuts down all providers immediately.
    /// Any further attempts to use the providers will fail.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered during shutdown.
    pub fn shutdown(mut self) -> Result<(), SdkError> {
        if let Some(provider) = self.tracer_provider.take() {
            provider.force_flush().map_err(SdkError::Flush)?;
            provider.shutdown().map_err(SdkError::Shutdown)?;
        }

        if let Some(provider) = self.logger_provider.take() {
            provider.force_flush().map_err(SdkError::Flush)?;
            provider.shutdown().map_err(SdkError::Shutdown)?;
        }

        if let Some(provider) = self.meter_provider.take() {
            provider.force_flush().map_err(SdkError::Flush)?;
            provider.shutdown().map_err(SdkError::Shutdown)?;
        }

        Ok(())
    }
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take() {
            let _ = provider.force_flush();
            if let Err(e) = provider.shutdown() {
                eprintln!("Error shutting down tracer provider: {e}");
            }
        }

        if let Some(provider) = self.logger_provider.take() {
            let _ = provider.force_flush();
            if let Err(e) = provider.shutdown() {
                eprintln!("Error shutting down logger provider: {e}");
            }
        }

        if let Some(provider) = self.meter_provider.take() {
            let _ = provider.force_flush();
            if let Err(e) = provider.shutdown() {
                eprintln!("Error shutting down meter provider: {e}");
            }
        }
    }
}

fn build_resource(config: &OtelSdkConfig) -> Resource {
    let mut attributes: Vec<KeyValue> = config
        .resource
        .attributes
        .iter()
        .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
        .collect();

    if let Some(name) = &config.resource.service_name {
        attributes.push(KeyValue::new("service.name", name.clone()));
    }

    if let Some(version) = &config.resource.service_version {
        attributes.push(KeyValue::new("service.version", version.clone()));
    }

    if let Some(env) = &config.resource.deployment_environment {
        attributes.push(KeyValue::new("deployment.environment.name", env.clone()));
    }

    Resource::builder().with_attributes(attributes).build()
}

fn build_tracer_provider(
    config: &OtelSdkConfig,
    resource: Resource,
) -> Result<SdkTracerProvider, SdkError> {
    let exporter = match config.endpoint.protocol {
        Protocol::Grpc => {
            let endpoint = config.effective_endpoint();
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout);

            if !config.endpoint.headers.is_empty() {
                let mut metadata = tonic::metadata::MetadataMap::new();
                for (key, value) in &config.endpoint.headers {
                    if let (Ok(k), Ok(v)) = (
                        key.parse::<MetadataKey<_>>(),
                        value.parse::<MetadataValue<_>>(),
                    ) {
                        metadata.insert(k, v);
                    }
                }
                builder = builder.with_metadata(metadata);
            }

            builder.build().map_err(SdkError::TraceExporter)?
        }
        Protocol::HttpBinary => {
            let endpoint = config.signal_endpoint("/v1/traces");
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);

            if !config.endpoint.headers.is_empty() {
                builder = builder.with_headers(config.endpoint.headers.clone());
            }

            builder.build().map_err(SdkError::TraceExporter)?
        }
        Protocol::HttpJson => {
            let endpoint = config.signal_endpoint("/v1/traces");
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpJson);

            if !config.endpoint.headers.is_empty() {
                builder = builder.with_headers(config.endpoint.headers.clone());
            }

            builder.build().map_err(SdkError::TraceExporter)?
        }
    };

    let batch_config = TraceBatchConfigBuilder::default()
        .with_max_queue_size(config.traces.batch.max_queue_size)
        .with_max_export_batch_size(config.traces.batch.max_export_batch_size)
        .with_scheduled_delay(config.traces.batch.scheduled_delay)
        .build();

    let span_processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(batch_config)
        .build();

    Ok(SdkTracerProvider::builder()
        .with_span_processor(span_processor)
        .with_resource(resource)
        .build())
}

fn build_meter_provider(
    config: &OtelSdkConfig,
    resource: Resource,
) -> Result<SdkMeterProvider, SdkError> {
    let exporter = match config.endpoint.protocol {
        Protocol::Grpc => {
            let endpoint = config.effective_endpoint();
            let mut builder = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout);

            if !config.endpoint.headers.is_empty() {
                let mut metadata = tonic::metadata::MetadataMap::new();
                for (key, value) in &config.endpoint.headers {
                    if let (Ok(k), Ok(v)) = (
                        key.parse::<MetadataKey<_>>(),
                        value.parse::<MetadataValue<_>>(),
                    ) {
                        metadata.insert(k, v);
                    }
                }
                builder = builder.with_metadata(metadata);
            }

            builder.build().map_err(SdkError::MetricExporter)?
        }
        Protocol::HttpBinary => {
            let endpoint = config.signal_endpoint("/v1/metrics");
            let mut builder = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);

            if !config.endpoint.headers.is_empty() {
                builder = builder.with_headers(config.endpoint.headers.clone());
            }

            builder.build().map_err(SdkError::MetricExporter)?
        }
        Protocol::HttpJson => {
            let endpoint = config.signal_endpoint("/v1/metrics");
            let mut builder = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpJson);

            if !config.endpoint.headers.is_empty() {
                builder = builder.with_headers(config.endpoint.headers.clone());
            }

            builder.build().map_err(SdkError::MetricExporter)?
        }
    };

    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
        .with_interval(config.metrics.batch.scheduled_delay)
        .build();

    Ok(SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build())
}

fn build_logger_provider(
    config: &OtelSdkConfig,
    resource: Resource,
) -> Result<SdkLoggerProvider, SdkError> {
    let exporter = match config.endpoint.protocol {
        Protocol::Grpc => {
            let endpoint = config.effective_endpoint();
            let mut builder = opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout);

            if !config.endpoint.headers.is_empty() {
                let mut metadata = tonic::metadata::MetadataMap::new();
                for (key, value) in &config.endpoint.headers {
                    if let (Ok(k), Ok(v)) = (
                        key.parse::<MetadataKey<_>>(),
                        value.parse::<MetadataValue<_>>(),
                    ) {
                        metadata.insert(k, v);
                    }
                }
                builder = builder.with_metadata(metadata);
            }

            builder.build().map_err(SdkError::LogExporter)?
        }
        Protocol::HttpBinary => {
            let endpoint = config.signal_endpoint("/v1/logs");
            let mut builder = opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);

            if !config.endpoint.headers.is_empty() {
                builder = builder.with_headers(config.endpoint.headers.clone());
            }

            builder.build().map_err(SdkError::LogExporter)?
        }
        Protocol::HttpJson => {
            let endpoint = config.signal_endpoint("/v1/logs");
            let mut builder = opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .with_endpoint(&endpoint)
                .with_timeout(config.endpoint.timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpJson);

            if !config.endpoint.headers.is_empty() {
                builder = builder.with_headers(config.endpoint.headers.clone());
            }

            builder.build().map_err(SdkError::LogExporter)?
        }
    };

    let batch_config = LogBatchConfigBuilder::default()
        .with_max_queue_size(config.logs.batch.max_queue_size)
        .with_max_export_batch_size(config.logs.batch.max_export_batch_size)
        .with_scheduled_delay(config.logs.batch.scheduled_delay)
        .build();

    let log_processor = BatchLogProcessor::builder(exporter)
        .with_batch_config(batch_config)
        .build();

    Ok(SdkLoggerProvider::builder()
        .with_log_processor(log_processor)
        .with_resource(resource)
        .build())
}

fn init_subscriber(
    tracer_provider: &Option<SdkTracerProvider>,
    logger_provider: &Option<SdkLoggerProvider>,
) -> Result<(), SdkError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .without_time();

    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

    match (tracer_provider, logger_provider) {
        (Some(tp), Some(lp)) => {
            let tracer = tp.tracer("lambda-otel-extension");
            let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            let log_layer = OpenTelemetryTracingBridge::new(lp);
            registry.with(telemetry_layer).with(log_layer).try_init()?;
        }
        (Some(tp), None) => {
            let tracer = tp.tracer("lambda-otel-extension");
            let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            registry.with(telemetry_layer).try_init()?;
        }
        (None, Some(lp)) => {
            let log_layer = OpenTelemetryTracingBridge::new(lp);
            registry.with(log_layer).try_init()?;
        }
        (None, None) => {
            registry.try_init()?;
        }
    }

    Ok(())
}

//! Error types for SDK initialisation and lifecycle.

use figment::Error as FigmentError;

/// Errors from SDK initialisation and lifecycle.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SdkError {
    /// Failed to extract configuration from sources.
    #[error("configuration error: {0}")]
    Config(#[source] Box<FigmentError>),

    /// Failed to create trace exporter.
    #[error("failed to create trace exporter")]
    TraceExporter(#[source] opentelemetry_otlp::ExporterBuildError),

    /// Failed to create metric exporter.
    #[error("failed to create metric exporter")]
    MetricExporter(#[source] opentelemetry_otlp::ExporterBuildError),

    /// Failed to create log exporter.
    #[error("failed to create log exporter")]
    LogExporter(#[source] opentelemetry_otlp::ExporterBuildError),

    /// Failed to initialise tracing subscriber.
    #[error("failed to initialise tracing subscriber")]
    TracingSubscriber(#[from] tracing_subscriber::util::TryInitError),

    /// Failed to flush providers.
    #[error("failed to flush providers")]
    Flush(#[source] opentelemetry_sdk::error::OTelSdkError),

    /// Failed to shut down providers.
    #[error("failed to shut down providers")]
    Shutdown(#[source] opentelemetry_sdk::error::OTelSdkError),

    /// Invalid endpoint URL format.
    #[error("invalid endpoint URL: {url} (must start with http:// or https://)")]
    InvalidEndpoint {
        /// The invalid URL that was provided.
        url: String,
    },
}

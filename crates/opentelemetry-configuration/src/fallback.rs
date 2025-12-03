//! Export fallback handling for failed OTLP exports.
//!
//! When an export fails after all retry attempts, the fallback handler is
//! invoked with the original OTLP request payload. This allows users to
//! preserve telemetry data by writing to disk, queuing, or sending to an
//! alternative endpoint.
//!
//! # Example
//!
//! ```no_run
//! use opentelemetry_configuration::{ExportFallback, ExportFailure, FallbackHandler};
//! use std::path::PathBuf;
//!
//! // Use a predefined fallback
//! let fallback = ExportFallback::Stdout;
//!
//! // Or use a closure for custom handling
//! let fallback = ExportFallback::custom(|failure| {
//!     eprintln!("Export failed: {}", failure.error);
//!     // Write the protobuf payload somewhere
//!     let bytes = failure.request.to_protobuf();
//!     // ... send to S3, queue, backup collector, etc.
//!     Ok(())
//! });
//! ```

use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
    trace::v1::ExportTraceServiceRequest,
};
use prost::Message;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// The original OTLP request that failed to export.
///
/// This enum contains the exact protobuf payload that was meant to be sent
/// to the collector. You can serialise it with [`to_protobuf()`](Self::to_protobuf)
/// and send it via any transport mechanism.
#[derive(Debug, Clone)]
pub enum FailedRequest {
    /// A traces export request.
    Traces(ExportTraceServiceRequest),
    /// A metrics export request.
    Metrics(ExportMetricsServiceRequest),
    /// A logs export request.
    Logs(ExportLogsServiceRequest),
}

impl FailedRequest {
    /// Serialises the request to protobuf bytes.
    ///
    /// This is the canonical wire format expected by OTLP collectors.
    /// You can send these bytes to any collector endpoint using your
    /// preferred transport (HTTP, gRPC, file, queue, etc.).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let bytes = failure.request.to_protobuf();
    /// s3_client.put_object(bucket, key, bytes).await?;
    /// ```
    pub fn to_protobuf(&self) -> Vec<u8> {
        match self {
            Self::Traces(req) => req.encode_to_vec(),
            Self::Metrics(req) => req.encode_to_vec(),
            Self::Logs(req) => req.encode_to_vec(),
        }
    }

    /// Serialises the request to JSON.
    ///
    /// OTLP/JSON is less compact than protobuf but useful for debugging
    /// and systems that prefer JSON transport.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialisation fails.
    pub fn to_json(&self) -> serde_json::Result<String> {
        match self {
            Self::Traces(req) => serde_json::to_string(req),
            Self::Metrics(req) => serde_json::to_string(req),
            Self::Logs(req) => serde_json::to_string(req),
        }
    }

    /// Serialises the request to pretty-printed JSON.
    ///
    /// Useful for debugging and human-readable output.
    pub fn to_json_pretty(&self) -> serde_json::Result<String> {
        match self {
            Self::Traces(req) => serde_json::to_string_pretty(req),
            Self::Metrics(req) => serde_json::to_string_pretty(req),
            Self::Logs(req) => serde_json::to_string_pretty(req),
        }
    }

    /// Returns the estimated serialised size in bytes.
    ///
    /// Useful for implementing size-based filtering or batching logic
    /// in fallback handlers.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Traces(req) => req.encoded_len(),
            Self::Metrics(req) => req.encoded_len(),
            Self::Logs(req) => req.encoded_len(),
        }
    }

    /// Returns the signal type as a string.
    ///
    /// Useful for logging, metrics, and routing decisions.
    pub fn signal_type(&self) -> &'static str {
        match self {
            Self::Traces(_) => "traces",
            Self::Metrics(_) => "metrics",
            Self::Logs(_) => "logs",
        }
    }

    /// Returns the OTLP HTTP path for this signal type.
    ///
    /// Useful when forwarding to an alternative collector.
    pub fn otlp_path(&self) -> &'static str {
        match self {
            Self::Traces(_) => "/v1/traces",
            Self::Metrics(_) => "/v1/metrics",
            Self::Logs(_) => "/v1/logs",
        }
    }

    /// Returns the number of items in the request.
    ///
    /// For traces, this is the number of spans.
    /// For metrics, this is the number of data points.
    /// For logs, this is the number of log records.
    pub fn item_count(&self) -> usize {
        match self {
            Self::Traces(req) => req
                .resource_spans
                .iter()
                .flat_map(|rs| &rs.scope_spans)
                .map(|ss| ss.spans.len())
                .sum(),
            Self::Metrics(req) => req
                .resource_metrics
                .iter()
                .flat_map(|rm| &rm.scope_metrics)
                .flat_map(|sm| &sm.metrics)
                .map(|m| match &m.data {
                    Some(data) => count_metric_data_points(data),
                    None => 0,
                })
                .sum(),
            Self::Logs(req) => req
                .resource_logs
                .iter()
                .flat_map(|rl| &rl.scope_logs)
                .map(|sl| sl.log_records.len())
                .sum(),
        }
    }
}

fn count_metric_data_points(data: &opentelemetry_proto::tonic::metrics::v1::metric::Data) -> usize {
    use opentelemetry_proto::tonic::metrics::v1::metric::Data;
    match data {
        Data::Gauge(g) => g.data_points.len(),
        Data::Sum(s) => s.data_points.len(),
        Data::Histogram(h) => h.data_points.len(),
        Data::ExponentialHistogram(eh) => eh.data_points.len(),
        Data::Summary(s) => s.data_points.len(),
    }
}

/// Details of a failed export after all retry attempts have been exhausted.
#[derive(Debug)]
pub struct ExportFailure {
    /// The error that caused the export to fail.
    pub error: Box<dyn std::error::Error + Send + Sync>,

    /// The original OTLP request that failed to export.
    ///
    /// This contains the full payload ready to be serialised and sent
    /// via an alternative transport.
    pub request: FailedRequest,

    /// When the failure occurred.
    pub timestamp: SystemTime,
}

impl ExportFailure {
    /// Creates a new export failure.
    pub fn new(
        error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
        request: FailedRequest,
    ) -> Self {
        Self {
            error: error.into(),
            request,
            timestamp: SystemTime::now(),
        }
    }

    /// Returns the size of the failed request in bytes.
    pub fn size_bytes(&self) -> usize {
        self.request.encoded_len()
    }

    /// Returns the error message as a string.
    pub fn error_message(&self) -> String {
        self.error.to_string()
    }
}

/// Handler for export failures after all retry attempts have been exhausted.
///
/// Implementations should focus on preserving data (write to disk, queue,
/// alternative endpoint) rather than attempting additional retries.
///
/// # Example
///
/// ```ignore
/// use opentelemetry_configuration::{FallbackHandler, ExportFailure};
///
/// struct S3FallbackHandler {
///     bucket: String,
///     client: aws_sdk_s3::Client,
/// }
///
/// impl FallbackHandler for S3FallbackHandler {
///     fn handle_failure(&self, failure: ExportFailure)
///         -> Result<(), Box<dyn std::error::Error + Send + Sync>>
///     {
///         let key = format!(
///             "failed-exports/{}/{}.pb",
///             failure.request.signal_type(),
///             failure.timestamp.duration_since(std::time::UNIX_EPOCH)?.as_millis()
///         );
///
///         // In a real implementation, you'd use async here
///         let bytes = failure.request.to_protobuf();
///         // self.client.put_object().bucket(&self.bucket).key(&key).body(bytes)...
///
///         Ok(())
///     }
/// }
/// ```
pub trait FallbackHandler: Send + Sync {
    /// Handle a failed export.
    ///
    /// # Arguments
    ///
    /// * `failure` - Details of the failed export including the original OTLP request
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the fallback successfully handled the failure,
    /// or an error if the fallback itself failed. Errors are logged but
    /// otherwise ignored.
    fn handle_failure(
        &self,
        failure: ExportFailure,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Predefined fallback strategies for common scenarios.
#[derive(Clone, Default)]
pub enum ExportFallback {
    /// Discard failed exports silently.
    ///
    /// Use this when telemetry loss is acceptable and you don't want
    /// any overhead from fallback handling.
    None,

    /// Log failed exports via `tracing::warn!`.
    ///
    /// This is the default. It logs the error and signal type but not
    /// the full payload. Uses target `otel_lifecycle` so it can be filtered
    /// via `RUST_LOG=otel_lifecycle=warn`.
    #[default]
    LogError,

    /// Write failed exports as JSON to stdout.
    ///
    /// Useful for Lambda where stdout goes to CloudWatch Logs.
    /// The output is a JSON object with `signal_type`, `error`, and `request` fields.
    Stdout,

    /// Write failed exports as JSON to stderr.
    Stderr,

    /// Write failed exports as protobuf files to a directory.
    ///
    /// Files are named `{signal_type}-{timestamp_ms}.pb`.
    File(PathBuf),

    /// Use a custom fallback handler.
    Custom(Arc<dyn FallbackHandler>),
}

impl ExportFallback {
    /// Creates a custom fallback from a closure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use opentelemetry_configuration::ExportFallback;
    ///
    /// let fallback = ExportFallback::custom(|failure| {
    ///     // Write protobuf bytes to your preferred destination
    ///     let bytes = failure.request.to_protobuf();
    ///     eprintln!(
    ///         "Failed to export {} ({} bytes): {}",
    ///         failure.request.signal_type(),
    ///         bytes.len(),
    ///         failure.error
    ///     );
    ///     Ok(())
    /// });
    /// ```
    pub fn custom<F>(f: F) -> Self
    where
        F: Fn(ExportFailure) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
            + Send
            + Sync
            + 'static,
    {
        Self::Custom(Arc::new(ClosureFallbackHandler(f)))
    }

    /// Handles a failed export using this fallback strategy.
    ///
    /// Errors are logged via `tracing` with target `otel_lifecycle`.
    /// To see these warnings, enable the target in your `RUST_LOG` filter:
    /// `RUST_LOG=otel_lifecycle=warn`
    pub fn handle(&self, failure: ExportFailure) {
        let result = match self {
            Self::None => Ok(()),
            Self::LogError => {
                tracing::warn!(
                    target: "otel_lifecycle",
                    signal_type = failure.request.signal_type(),
                    item_count = failure.request.item_count(),
                    size_bytes = failure.size_bytes(),
                    error = %failure.error,
                    "Export failed"
                );
                Ok(())
            }
            Self::Stdout => write_json_to_stdout(&failure),
            Self::Stderr => write_json_to_stderr(&failure),
            Self::File(dir) => write_protobuf_to_file(dir, &failure),
            Self::Custom(handler) => handler.handle_failure(failure),
        };

        if let Err(e) = result {
            tracing::error!(target: "otel_lifecycle", error = %e, "Fallback handler failed");
        }
    }
}

impl fmt::Debug for ExportFallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::LogError => write!(f, "LogError"),
            Self::Stdout => write!(f, "Stdout"),
            Self::Stderr => write!(f, "Stderr"),
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::Custom(_) => write!(f, "Custom(...)"),
        }
    }
}

struct ClosureFallbackHandler<F>(F);

impl<F> FallbackHandler for ClosureFallbackHandler<F>
where
    F: Fn(ExportFailure) -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + Sync,
{
    fn handle_failure(
        &self,
        failure: ExportFailure,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (self.0)(failure)
    }
}

fn write_json_to_stdout(
    failure: &ExportFailure,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = serde_json::json!({
        "otlp_fallback": {
            "signal_type": failure.request.signal_type(),
            "error": failure.error_message(),
            "item_count": failure.request.item_count(),
            "size_bytes": failure.size_bytes(),
            "timestamp": failure.timestamp
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            "request": match &failure.request {
                FailedRequest::Traces(req) => serde_json::to_value(req)?,
                FailedRequest::Metrics(req) => serde_json::to_value(req)?,
                FailedRequest::Logs(req) => serde_json::to_value(req)?,
            }
        }
    });
    println!("{}", serde_json::to_string(&json)?);
    Ok(())
}

fn write_json_to_stderr(
    failure: &ExportFailure,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = serde_json::json!({
        "otlp_fallback": {
            "signal_type": failure.request.signal_type(),
            "error": failure.error_message(),
            "item_count": failure.request.item_count(),
            "size_bytes": failure.size_bytes(),
            "timestamp": failure.timestamp
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            "request": match &failure.request {
                FailedRequest::Traces(req) => serde_json::to_value(req)?,
                FailedRequest::Metrics(req) => serde_json::to_value(req)?,
                FailedRequest::Logs(req) => serde_json::to_value(req)?,
            }
        }
    });
    eprintln!("{}", serde_json::to_string(&json)?);
    Ok(())
}

fn write_protobuf_to_file(
    dir: &PathBuf,
    failure: &ExportFailure,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let timestamp_ms = failure
        .timestamp
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let filename = format!("{}-{}.pb", failure.request.signal_type(), timestamp_ms);
    let path = dir.join(filename);

    std::fs::create_dir_all(dir)?;
    std::fs::write(path, failure.request.to_protobuf())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::{
        common::v1::AnyValue,
        common::v1::any_value::Value as AnyValueEnum,
        logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
        trace::v1::{ResourceSpans, ScopeSpans, Span},
    };

    fn create_test_traces_request() -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: None,
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![
                        Span {
                            name: "test-span-1".to_string(),
                            ..Default::default()
                        },
                        Span {
                            name: "test-span-2".to_string(),
                            ..Default::default()
                        },
                    ],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        }
    }

    fn create_test_logs_request() -> ExportLogsServiceRequest {
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        body: Some(AnyValue {
                            value: Some(AnyValueEnum::StringValue("test log".to_string())),
                        }),
                        ..Default::default()
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        }
    }

    #[test]
    fn test_failed_request_signal_type() {
        let traces = FailedRequest::Traces(create_test_traces_request());
        assert_eq!(traces.signal_type(), "traces");

        let logs = FailedRequest::Logs(create_test_logs_request());
        assert_eq!(logs.signal_type(), "logs");
    }

    #[test]
    fn test_failed_request_otlp_path() {
        let traces = FailedRequest::Traces(create_test_traces_request());
        assert_eq!(traces.otlp_path(), "/v1/traces");

        let logs = FailedRequest::Logs(create_test_logs_request());
        assert_eq!(logs.otlp_path(), "/v1/logs");
    }

    #[test]
    fn test_failed_request_item_count() {
        let traces = FailedRequest::Traces(create_test_traces_request());
        assert_eq!(traces.item_count(), 2);

        let logs = FailedRequest::Logs(create_test_logs_request());
        assert_eq!(logs.item_count(), 1);
    }

    #[test]
    fn test_failed_request_to_protobuf() {
        let traces = FailedRequest::Traces(create_test_traces_request());
        let bytes = traces.to_protobuf();
        assert!(!bytes.is_empty());
        assert!(bytes.len() > 10);
    }

    #[test]
    fn test_failed_request_encoded_len() {
        let traces = FailedRequest::Traces(create_test_traces_request());
        let len = traces.encoded_len();
        let bytes = traces.to_protobuf();
        assert_eq!(len, bytes.len());
    }

    #[test]
    fn test_failed_request_to_json() {
        let traces = FailedRequest::Traces(create_test_traces_request());
        let json = traces.to_json().unwrap();
        assert!(json.contains("test-span-1"));
        assert!(json.contains("test-span-2"));
    }

    #[test]
    fn test_export_failure_creation() {
        let request = FailedRequest::Traces(create_test_traces_request());
        let failure = ExportFailure::new("connection refused", request);

        assert_eq!(failure.error_message(), "connection refused");
        assert!(failure.size_bytes() > 0);
    }

    #[test]
    fn test_export_fallback_none() {
        let fallback = ExportFallback::None;
        let request = FailedRequest::Traces(create_test_traces_request());
        let failure = ExportFailure::new("test error", request);

        // Should not panic
        fallback.handle(failure);
    }

    #[test]
    fn test_export_fallback_custom() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let fallback = ExportFallback::custom(move |failure| {
            called_clone.store(true, Ordering::SeqCst);
            assert_eq!(failure.request.signal_type(), "traces");
            Ok(())
        });

        let request = FailedRequest::Traces(create_test_traces_request());
        let failure = ExportFailure::new("test error", request);

        fallback.handle(failure);
        assert!(called.load(Ordering::SeqCst));
    }

    #[test]
    fn test_export_fallback_debug() {
        assert_eq!(format!("{:?}", ExportFallback::None), "None");
        assert_eq!(format!("{:?}", ExportFallback::LogError), "LogError");
        assert_eq!(format!("{:?}", ExportFallback::Stdout), "Stdout");
        assert!(
            format!(
                "{:?}",
                ExportFallback::Custom(Arc::new(ClosureFallbackHandler(|_| Ok(()))))
            )
            .contains("Custom")
        );
    }

    #[test]
    fn test_export_fallback_default() {
        let fallback = ExportFallback::default();
        assert!(matches!(fallback, ExportFallback::LogError));
    }
}

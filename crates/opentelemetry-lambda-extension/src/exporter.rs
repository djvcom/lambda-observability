//! OTLP signal exporter with retry and fallback.
//!
//! This module provides the flusher for exporting OTLP signals to a remote endpoint.
//! It includes retry logic with exponential backoff and stdout JSON fallback on failure.

use crate::aggregator::BatchedSignal;
use crate::config::{Compression, ExporterConfig, Protocol};
use prost::Message;
use reqwest::Client;
use serde::Serialize;
use std::io::Write;
use std::time::Duration;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(50);

/// Result of an export operation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportResult {
    /// Export succeeded.
    Success,
    /// Export failed after retries, data written to stdout.
    Fallback,
    /// Export failed and no data was sent (no endpoint configured).
    Skipped,
}

/// Error during export.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// HTTP request failed.
    #[error("HTTP request failed")]
    Http(#[from] reqwest::Error),

    /// Server returned an error status.
    #[error("server returned {status}: {body}")]
    Status {
        /// HTTP status code returned by server.
        status: u16,
        /// Response body from server.
        body: String,
    },

    /// Encoding failed.
    #[error("failed to encode request")]
    Encode(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// No endpoint configured.
    #[error("no endpoint configured")]
    NoEndpoint,
}

impl ExportError {
    pub(crate) fn encode<E: std::error::Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Encode(Box::new(error))
    }

    pub(crate) fn status(status: u16, body: impl Into<String>) -> Self {
        Self::Status {
            status,
            body: body.into(),
        }
    }
}

/// OTLP exporter for sending signals to a remote endpoint.
pub struct OtlpExporter {
    config: ExporterConfig,
    client: Client,
}

impl OtlpExporter {
    /// Creates a new OTLP exporter with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created.
    pub fn new(config: ExporterConfig) -> Result<Self, ExportError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(ExportError::Http)?;

        Ok(Self { config, client })
    }

    /// Creates a new exporter with default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created.
    pub fn with_defaults() -> Result<Self, ExportError> {
        Self::new(ExporterConfig::default())
    }

    /// Exports a batch of signals.
    ///
    /// This method handles retries and fallback to stdout on failure.
    pub async fn export(&self, batch: BatchedSignal) -> ExportResult {
        if self.config.endpoint.is_none() {
            tracing::debug!("No endpoint configured, skipping export");
            return ExportResult::Skipped;
        }

        let result = self.export_with_retry(&batch).await;

        match result {
            Ok(()) => ExportResult::Success,
            Err(e) => {
                tracing::warn!(error = %e, "Export failed after retries, falling back to stdout");
                self.emit_to_stdout(&batch);
                ExportResult::Fallback
            }
        }
    }

    async fn export_with_retry(&self, batch: &BatchedSignal) -> Result<(), ExportError> {
        let mut last_error = None;
        let mut backoff = INITIAL_BACKOFF;

        for attempt in 0..MAX_RETRIES {
            match self.try_export(batch).await {
                Ok(()) => return Ok(()),
                Err(ExportError::Status { status, ref body }) if !Self::is_retryable(status) => {
                    tracing::error!(status, "Received non-retryable status code, not retrying");
                    return Err(ExportError::status(status, body.clone()));
                }
                Err(e) => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES,
                        error = %e,
                        "Export attempt failed"
                    );
                    last_error = Some(e);

                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(backoff).await;
                        backoff *= 2;
                    }
                }
            }
        }

        Err(last_error.unwrap_or(ExportError::NoEndpoint))
    }

    /// Determines if a status code is retryable per OTLP specification.
    ///
    /// Retryable: 408 (Request Timeout), 429 (Too Many Requests), 5xx (Server Errors)
    /// Non-retryable: 400, 401, 403, 404, and other 4xx client errors
    fn is_retryable(status: u16) -> bool {
        matches!(status, 408 | 429) || (500..600).contains(&status)
    }

    async fn try_export(&self, batch: &BatchedSignal) -> Result<(), ExportError> {
        let endpoint = self
            .config
            .endpoint
            .as_ref()
            .ok_or(ExportError::NoEndpoint)?;

        let (path, body, content_type) = match batch {
            BatchedSignal::Traces(req) => {
                ("/v1/traces", self.encode_request(req)?, self.content_type())
            }
            BatchedSignal::Metrics(req) => (
                "/v1/metrics",
                self.encode_request(req)?,
                self.content_type(),
            ),
            BatchedSignal::Logs(req) => {
                ("/v1/logs", self.encode_request(req)?, self.content_type())
            }
        };

        let url = format!("{}{}", endpoint, path);

        let mut request = self
            .client
            .post(&url)
            .header("Content-Type", content_type)
            .body(body);

        for (key, value) in &self.config.headers {
            request = request.header(key, value);
        }

        if self.config.compression == Compression::Gzip {
            request = request.header("Content-Encoding", "gzip");
        }

        let response = request.send().await.map_err(ExportError::Http)?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(ExportError::status(status.as_u16(), body))
        }
    }

    fn encode_request<T: Message>(&self, request: &T) -> Result<Vec<u8>, ExportError> {
        let mut buf = Vec::with_capacity(request.encoded_len());
        request.encode(&mut buf).map_err(ExportError::encode)?;

        if self.config.compression == Compression::Gzip {
            use flate2::Compression as GzCompression;
            use flate2::write::GzEncoder;

            let mut encoder = GzEncoder::new(Vec::new(), GzCompression::default());
            encoder.write_all(&buf).map_err(ExportError::encode)?;
            encoder.finish().map_err(ExportError::encode)
        } else {
            Ok(buf)
        }
    }

    fn content_type(&self) -> &'static str {
        match self.config.protocol {
            Protocol::Http => "application/x-protobuf",
            Protocol::Grpc => "application/grpc",
        }
    }

    fn emit_to_stdout(&self, batch: &BatchedSignal) {
        use std::io::Write as _;

        let fallback = match batch {
            BatchedSignal::Traces(req) => OtlpFallback {
                otlp_fallback: OtlpFallbackData {
                    signal_type: "traces",
                    request: serde_json::to_value(req).unwrap_or_default(),
                },
            },
            BatchedSignal::Metrics(req) => OtlpFallback {
                otlp_fallback: OtlpFallbackData {
                    signal_type: "metrics",
                    request: serde_json::to_value(req).unwrap_or_default(),
                },
            },
            BatchedSignal::Logs(req) => OtlpFallback {
                otlp_fallback: OtlpFallbackData {
                    signal_type: "logs",
                    request: serde_json::to_value(req).unwrap_or_default(),
                },
            },
        };

        if let Ok(json) = serde_json::to_string(&fallback) {
            // Use explicit I/O to handle broken pipes gracefully
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{}", json);
        }
    }

    /// Returns whether an endpoint is configured.
    pub fn has_endpoint(&self) -> bool {
        self.config.endpoint.is_some()
    }

    /// Returns the configured endpoint URL.
    pub fn endpoint(&self) -> Option<&str> {
        self.config.endpoint.as_deref()
    }
}

#[derive(Serialize)]
struct OtlpFallback<'a> {
    otlp_fallback: OtlpFallbackData<'a>,
}

#[derive(Serialize)]
struct OtlpFallbackData<'a> {
    #[serde(rename = "type")]
    signal_type: &'a str,
    request: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    use std::error::Error;

    fn make_trace_batch() -> BatchedSignal {
        BatchedSignal::Traces(ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "test-span".to_string(),
                        trace_id: vec![1; 16],
                        span_id: vec![1; 8],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        })
    }

    #[tokio::test]
    async fn test_export_no_endpoint_skips() {
        let exporter = OtlpExporter::with_defaults().unwrap();
        let batch = make_trace_batch();

        let result = exporter.export(batch).await;
        assert_eq!(result, ExportResult::Skipped);
    }

    #[test]
    fn test_has_endpoint() {
        let exporter = OtlpExporter::with_defaults().unwrap();
        assert!(!exporter.has_endpoint());

        let config = ExporterConfig {
            endpoint: Some("http://localhost:4318".to_string()),
            ..Default::default()
        };
        let exporter = OtlpExporter::new(config).unwrap();
        assert!(exporter.has_endpoint());
    }

    #[test]
    fn test_encode_request() {
        let config = ExporterConfig {
            compression: Compression::None,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(config).unwrap();

        let request = ExportTraceServiceRequest::default();
        let encoded = exporter.encode_request(&request);
        assert!(encoded.is_ok());
    }

    #[test]
    fn test_encode_request_with_gzip() {
        let config = ExporterConfig {
            compression: Compression::Gzip,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(config).unwrap();

        let request = ExportTraceServiceRequest::default();
        let encoded = exporter.encode_request(&request);
        assert!(encoded.is_ok());
    }

    #[test]
    fn test_content_type() {
        let config = ExporterConfig {
            protocol: Protocol::Http,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(config).unwrap();
        assert_eq!(exporter.content_type(), "application/x-protobuf");

        let config = ExporterConfig {
            protocol: Protocol::Grpc,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(config).unwrap();
        assert_eq!(exporter.content_type(), "application/grpc");
    }

    #[test]
    fn test_export_error_display() {
        let err = ExportError::NoEndpoint;
        assert_eq!(format!("{}", err), "no endpoint configured");

        let err = ExportError::status(500, "Internal Server Error");
        assert!(format!("{}", err).contains("500"));
        assert!(matches!(err, ExportError::Status { status: 500, .. }));
    }

    #[test]
    fn test_export_error_chain() {
        let io_err = std::io::Error::other("test error");
        let err = ExportError::encode(io_err);

        assert!(err.source().is_some());
        assert!(format!("{}", err).contains("encode"));
    }

    #[test]
    fn test_is_retryable() {
        assert!(OtlpExporter::is_retryable(408));
        assert!(OtlpExporter::is_retryable(429));
        assert!(OtlpExporter::is_retryable(500));
        assert!(OtlpExporter::is_retryable(502));
        assert!(OtlpExporter::is_retryable(503));
        assert!(OtlpExporter::is_retryable(504));

        assert!(!OtlpExporter::is_retryable(400));
        assert!(!OtlpExporter::is_retryable(401));
        assert!(!OtlpExporter::is_retryable(403));
        assert!(!OtlpExporter::is_retryable(404));
        assert!(!OtlpExporter::is_retryable(405));
    }
}

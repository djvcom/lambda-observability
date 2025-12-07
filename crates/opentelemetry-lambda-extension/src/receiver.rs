//! OTLP receiver for collecting signals from Lambda functions.
//!
//! This module provides HTTP endpoints that receive OTLP signals (traces, metrics, logs)
//! from the Lambda function. It supports both protobuf and JSON content types.

use crate::config::ReceiverConfig;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{
        HeaderMap, StatusCode,
        header::{CONTENT_ENCODING, CONTENT_TYPE},
    },
    response::IntoResponse,
    routing::{get, post},
};
use flate2::read::GzDecoder;
use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
    trace::v1::ExportTraceServiceRequest,
};
use prost::Message;
use serde::Serialize;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

/// Signals received from the Lambda function.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Signal {
    /// Trace spans.
    Traces(ExportTraceServiceRequest),
    /// Metrics.
    Metrics(ExportMetricsServiceRequest),
    /// Log records.
    Logs(ExportLogsServiceRequest),
}

/// Handle for interacting with a running OTLP receiver.
///
/// This handle can be used to query the receiver's status, trigger flushes,
/// and get the actual bound address (useful when port 0 is used for dynamic allocation).
#[derive(Clone)]
pub struct ReceiverHandle {
    state: Arc<ReceiverState>,
    local_addr: SocketAddr,
}

impl ReceiverHandle {
    /// Returns the actual bound address of the receiver.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Returns the port the receiver is listening on.
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Returns the URL for the OTLP HTTP receiver.
    pub fn url(&self) -> String {
        format!("http://{}", self.local_addr)
    }

    /// Returns the number of signals received.
    pub fn signals_received(&self) -> u64 {
        self.state.signals_received.load(Ordering::Relaxed)
    }

    /// Triggers an immediate flush and waits for it to complete.
    ///
    /// Returns `Ok(())` when the flush completes, or `Err` on timeout.
    pub async fn flush(&self, timeout: std::time::Duration) -> Result<(), FlushError> {
        // Signal that a flush is requested
        self.state.flush_requested.notify_one();

        // Wait for flush to complete
        tokio::time::timeout(timeout, self.state.flush_complete.notified())
            .await
            .map_err(|_| FlushError::Timeout)?;

        Ok(())
    }

    /// Notifies that a flush has completed.
    ///
    /// This should be called by the runtime after flushing all signals.
    pub fn notify_flush_complete(&self) {
        self.state.flush_complete.notify_waiters();
    }

    /// Returns a future that resolves when a flush is requested.
    pub async fn wait_for_flush_request(&self) {
        self.state.flush_requested.notified().await;
    }

    /// Returns a reference to the flush request notifier.
    pub fn flush_requested_notify(&self) -> Arc<Notify> {
        self.state.flush_requested.clone()
    }
}

/// Error returned when a flush operation fails.
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
pub enum FlushError {
    /// The flush operation timed out.
    #[error("flush operation timed out")]
    Timeout,
}

/// OTLP HTTP receiver for collecting signals.
pub struct OtlpReceiver {
    config: ReceiverConfig,
    signal_tx: mpsc::Sender<Signal>,
    cancel_token: CancellationToken,
}

impl OtlpReceiver {
    /// Creates a new OTLP receiver.
    ///
    /// # Arguments
    ///
    /// * `config` - Receiver configuration
    /// * `signal_tx` - Channel for sending received signals to the aggregator
    /// * `cancel_token` - Token for graceful shutdown
    pub fn new(
        config: ReceiverConfig,
        signal_tx: mpsc::Sender<Signal>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            config,
            signal_tx,
            cancel_token,
        }
    }

    /// Starts the HTTP receiver and returns a handle for interacting with it.
    ///
    /// The handle can be used to query the receiver's status, trigger flushes,
    /// and get the actual bound address.
    ///
    /// # Returns
    ///
    /// Returns `Ok((handle, future))` where `handle` can be used to interact with
    /// the receiver and `future` should be spawned to run the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the server fails to bind to the address.
    pub async fn start(
        self,
    ) -> Result<
        (
            ReceiverHandle,
            std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
        ),
        std::io::Error,
    > {
        if !self.config.http_enabled {
            tracing::info!("HTTP receiver disabled");
            let state = Arc::new(ReceiverState::new(self.signal_tx));
            let handle = ReceiverHandle {
                state,
                local_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            };
            let cancel_token = self.cancel_token;
            let future = Box::pin(async move {
                cancel_token.cancelled().await;
            });
            return Ok((handle, future));
        }

        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.http_port));
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;

        let state = Arc::new(ReceiverState::new(self.signal_tx));
        let handle = ReceiverHandle {
            state: state.clone(),
            local_addr,
        };

        let app = Router::new()
            .route("/health", get(handle_health))
            .route("/v1/traces", post(handle_traces))
            .route("/v1/metrics", post(handle_metrics))
            .route("/v1/logs", post(handle_logs))
            .with_state(state);

        tracing::info!(port = local_addr.port(), "OTLP HTTP receiver started");

        let cancel_token = self.cancel_token;
        let future = Box::pin(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(cancel_token.cancelled_owned())
                .await;
        });

        Ok((handle, future))
    }
}

/// Health check response.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Status of the receiver ("ready" or "starting").
    pub status: &'static str,
    /// Number of signals received.
    pub signals_received: u64,
}

struct ReceiverState {
    signal_tx: mpsc::Sender<Signal>,
    signals_received: AtomicU64,
    flush_requested: Arc<Notify>,
    flush_complete: Arc<Notify>,
}

impl ReceiverState {
    fn new(signal_tx: mpsc::Sender<Signal>) -> Self {
        Self {
            signal_tx,
            signals_received: AtomicU64::new(0),
            flush_requested: Arc::new(Notify::new()),
            flush_complete: Arc::new(Notify::new()),
        }
    }
}

async fn handle_health(State(state): State<Arc<ReceiverState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ready",
        signals_received: state.signals_received.load(Ordering::Relaxed),
    })
}

async fn handle_traces(
    State(state): State<Arc<ReceiverState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers.get(CONTENT_TYPE).cloned();
    let content_encoding = headers.get(CONTENT_ENCODING).cloned();
    let request =
        match parse_request::<ExportTraceServiceRequest>(&content_type, &content_encoding, &body) {
            Ok(req) => req,
            Err(e) => return e,
        };

    match state.signal_tx.try_send(Signal::Traces(request)) {
        Ok(()) => {
            state.signals_received.fetch_add(1, Ordering::Relaxed);
            StatusCode::OK
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("Trace signal channel full, signalling backpressure");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::error!("Trace signal channel closed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn handle_metrics(
    State(state): State<Arc<ReceiverState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers.get(CONTENT_TYPE).cloned();
    let content_encoding = headers.get(CONTENT_ENCODING).cloned();
    let request =
        match parse_request::<ExportMetricsServiceRequest>(&content_type, &content_encoding, &body)
        {
            Ok(req) => req,
            Err(e) => return e,
        };

    match state.signal_tx.try_send(Signal::Metrics(request)) {
        Ok(()) => {
            state.signals_received.fetch_add(1, Ordering::Relaxed);
            StatusCode::OK
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("Metrics signal channel full, signalling backpressure");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::error!("Metrics signal channel closed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn handle_logs(
    State(state): State<Arc<ReceiverState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers.get(CONTENT_TYPE).cloned();
    let content_encoding = headers.get(CONTENT_ENCODING).cloned();
    let request =
        match parse_request::<ExportLogsServiceRequest>(&content_type, &content_encoding, &body) {
            Ok(req) => req,
            Err(e) => return e,
        };

    match state.signal_tx.try_send(Signal::Logs(request)) {
        Ok(()) => {
            state.signals_received.fetch_add(1, Ordering::Relaxed);
            StatusCode::OK
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("Logs signal channel full, signalling backpressure");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::error!("Logs signal channel closed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn parse_request<T>(
    content_type: &Option<axum::http::HeaderValue>,
    content_encoding: &Option<axum::http::HeaderValue>,
    body: &Bytes,
) -> Result<T, StatusCode>
where
    T: Message + Default + serde::de::DeserializeOwned,
{
    let is_gzip = content_encoding
        .as_ref()
        .and_then(|ce| ce.to_str().ok())
        .is_some_and(|ce| ce.contains("gzip"));

    let decompressed: Vec<u8>;
    let body_bytes: &[u8] = if is_gzip {
        decompressed = decompress_gzip(body)?;
        &decompressed
    } else {
        body.as_ref()
    };

    let is_json = content_type
        .as_ref()
        .and_then(|ct| ct.to_str().ok())
        .is_some_and(|ct| ct.contains("application/json"));

    if is_json {
        serde_json::from_slice(body_bytes).map_err(|e| {
            tracing::error!(error = %e, "Failed to parse JSON request");
            StatusCode::BAD_REQUEST
        })
    } else {
        T::decode(body_bytes).map_err(|e| {
            tracing::error!(error = %e, "Failed to parse protobuf request");
            StatusCode::BAD_REQUEST
        })
    }
}

fn decompress_gzip(body: &Bytes) -> Result<Vec<u8>, StatusCode> {
    let mut decoder = GzDecoder::new(body.as_ref());
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).map_err(|e| {
        tracing::error!(error = %e, "Failed to decompress gzip body");
        StatusCode::BAD_REQUEST
    })?;
    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

    #[test]
    fn test_signal_debug() {
        let request = ExportTraceServiceRequest::default();
        let signal = Signal::Traces(request);
        let debug = format!("{:?}", signal);
        assert!(debug.contains("Traces"));
    }

    #[test]
    fn test_parse_traces_protobuf() {
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "test-span".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let encoded = request.encode_to_vec();
        let content_type = Some(axum::http::HeaderValue::from_static(
            "application/x-protobuf",
        ));

        let parsed: ExportTraceServiceRequest =
            parse_request(&content_type, &None, &Bytes::from(encoded)).unwrap();

        assert_eq!(
            parsed.resource_spans[0].scope_spans[0].spans[0].name,
            "test-span"
        );
    }

    #[test]
    fn test_parse_traces_json() {
        let json = r#"{"resourceSpans":[]}"#;
        let content_type = Some(axum::http::HeaderValue::from_static("application/json"));

        let parsed: ExportTraceServiceRequest =
            parse_request(&content_type, &None, &Bytes::from(json)).unwrap();

        assert!(parsed.resource_spans.is_empty());
    }

    #[test]
    fn test_parse_invalid_protobuf() {
        let content_type = Some(axum::http::HeaderValue::from_static(
            "application/x-protobuf",
        ));
        let result: Result<ExportTraceServiceRequest, _> =
            parse_request(&content_type, &None, &Bytes::from("invalid"));

        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_json() {
        let content_type = Some(axum::http::HeaderValue::from_static("application/json"));
        let result: Result<ExportTraceServiceRequest, _> =
            parse_request(&content_type, &None, &Bytes::from("{invalid}"));

        assert!(result.is_err());
    }

    #[test]
    fn test_parse_gzip_compressed_protobuf() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "compressed-span".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let encoded = request.encode_to_vec();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&encoded).unwrap();
        let compressed = encoder.finish().unwrap();

        let content_type = Some(axum::http::HeaderValue::from_static(
            "application/x-protobuf",
        ));
        let content_encoding = Some(axum::http::HeaderValue::from_static("gzip"));

        let parsed: ExportTraceServiceRequest =
            parse_request(&content_type, &content_encoding, &Bytes::from(compressed)).unwrap();

        assert_eq!(
            parsed.resource_spans[0].scope_spans[0].spans[0].name,
            "compressed-span"
        );
    }
}

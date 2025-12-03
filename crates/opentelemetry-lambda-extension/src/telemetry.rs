//! Telemetry API client and listener.
//!
//! This module provides functionality for subscribing to the Lambda Telemetry API
//! and receiving platform events via HTTP push.

use axum::{
    Router, body::Bytes, extract::State, http::StatusCode, response::IntoResponse, routing::post,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Types of telemetry events from the Telemetry API.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryType {
    /// Platform events (start, end, report, fault, extension).
    Platform,
    /// Function logs from stdout/stderr.
    Function,
    /// Extension logs.
    Extension,
}

/// Buffering configuration for Telemetry API subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferingConfig {
    /// Maximum number of events to buffer before sending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
    /// Maximum size in bytes to buffer before sending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u32>,
    /// Maximum time in milliseconds to buffer before sending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}

impl Default for BufferingConfig {
    fn default() -> Self {
        Self {
            max_items: Some(1000),
            max_bytes: Some(256 * 1024),
            timeout_ms: Some(25),
        }
    }
}

/// Destination configuration for Telemetry API subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationConfig {
    /// Protocol to use (HTTP only supported).
    pub protocol: String,
    /// URI to send events to.
    #[serde(rename = "URI")]
    pub uri: String,
}

/// Subscription request for the Telemetry API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySubscription {
    /// Schema version.
    pub schema_version: String,
    /// Types of telemetry to subscribe to.
    pub types: Vec<TelemetryType>,
    /// Buffering configuration.
    pub buffering: BufferingConfig,
    /// Destination for events.
    pub destination: DestinationConfig,
}

impl TelemetrySubscription {
    /// Creates a new subscription request for platform events.
    pub fn platform_events(listener_uri: impl Into<String>) -> Self {
        Self {
            schema_version: "2022-12-13".to_string(),
            types: vec![TelemetryType::Platform],
            buffering: BufferingConfig::default(),
            destination: DestinationConfig {
                protocol: "HTTP".to_string(),
                uri: listener_uri.into(),
            },
        }
    }

    /// Creates a subscription for all event types.
    pub fn all_events(listener_uri: impl Into<String>) -> Self {
        Self {
            schema_version: "2022-12-13".to_string(),
            types: vec![
                TelemetryType::Platform,
                TelemetryType::Function,
                TelemetryType::Extension,
            ],
            buffering: BufferingConfig::default(),
            destination: DestinationConfig {
                protocol: "HTTP".to_string(),
                uri: listener_uri.into(),
            },
        }
    }

    /// Sets custom buffering configuration.
    pub fn with_buffering(mut self, config: BufferingConfig) -> Self {
        self.buffering = config;
        self
    }
}

/// Platform telemetry event from Lambda.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TelemetryEvent {
    /// Platform initialization start.
    #[serde(rename = "platform.initStart")]
    InitStart {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: InitStartRecord,
    },
    /// Platform initialization complete (runtime ready).
    #[serde(rename = "platform.initRuntimeDone")]
    InitRuntimeDone {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: InitRuntimeDoneRecord,
    },
    /// Platform invocation start.
    #[serde(rename = "platform.start")]
    Start {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: StartRecord,
    },
    /// Platform runtime invocation complete.
    #[serde(rename = "platform.runtimeDone")]
    RuntimeDone {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: RuntimeDoneRecord,
    },
    /// Platform invocation report.
    #[serde(rename = "platform.report")]
    Report {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: ReportRecord,
    },
    /// Platform fault.
    #[serde(rename = "platform.fault")]
    Fault {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: FaultRecord,
    },
    /// Extension event.
    #[serde(rename = "platform.extension")]
    Extension {
        /// Event time in ISO 8601 format.
        time: String,
        /// Event record.
        record: ExtensionRecord,
    },
    /// Function log line.
    #[serde(rename = "function")]
    Function {
        /// Event time in ISO 8601 format.
        time: String,
        /// Log record.
        record: String,
    },
    /// Extension log line.
    #[serde(rename = "extension")]
    ExtensionLog {
        /// Event time in ISO 8601 format.
        time: String,
        /// Log record.
        record: String,
    },
}

/// Record for platform.initStart event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitStartRecord {
    /// Initialization type (on-demand, provisioned-concurrency).
    pub initialization_type: String,
    /// Phase of initialization.
    #[serde(default)]
    pub phase: String,
    /// Runtime version.
    #[serde(default)]
    pub runtime_version: Option<String>,
    /// Runtime version ARN.
    #[serde(default)]
    pub runtime_version_arn: Option<String>,
}

/// Record for platform.initRuntimeDone event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitRuntimeDoneRecord {
    /// Initialization type.
    pub initialization_type: String,
    /// Status of initialization.
    #[serde(default)]
    pub status: String,
    /// Phase of initialization.
    #[serde(default)]
    pub phase: String,
}

/// Record for platform.start event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRecord {
    /// Request ID for this invocation.
    pub request_id: String,
    /// Version of the function.
    #[serde(default)]
    pub version: Option<String>,
    /// Tracing information.
    #[serde(default)]
    pub tracing: Option<TracingRecord>,
}

/// Tracing information in platform events.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracingRecord {
    /// Span ID.
    #[serde(default)]
    pub span_id: Option<String>,
    /// Trace type.
    #[serde(rename = "type", default)]
    pub trace_type: Option<String>,
    /// Trace value (X-Ray header).
    #[serde(default)]
    pub value: Option<String>,
}

/// Record for platform.runtimeDone event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDoneRecord {
    /// Request ID for this invocation.
    pub request_id: String,
    /// Status of the invocation.
    pub status: String,
    /// Metrics for this invocation.
    #[serde(default)]
    pub metrics: Option<RuntimeMetrics>,
    /// Tracing information.
    #[serde(default)]
    pub tracing: Option<TracingRecord>,
    /// Spans produced during invocation.
    #[serde(default)]
    pub spans: Vec<SpanRecord>,
}

/// Runtime metrics from platform.runtimeDone event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeMetrics {
    /// Duration in milliseconds.
    pub duration_ms: f64,
    /// Produced bytes.
    #[serde(default)]
    pub produced_bytes: Option<u64>,
}

/// Span record from platform events.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanRecord {
    /// Name of the span.
    pub name: String,
    /// Start time in milliseconds.
    pub start: f64,
    /// Duration in milliseconds.
    pub duration_ms: f64,
}

/// Record for platform.report event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRecord {
    /// Request ID for this invocation.
    pub request_id: String,
    /// Status of the invocation.
    pub status: String,
    /// Metrics for this invocation.
    pub metrics: ReportMetrics,
    /// Tracing information.
    #[serde(default)]
    pub tracing: Option<TracingRecord>,
}

/// Metrics from platform.report event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportMetrics {
    /// Duration in milliseconds.
    pub duration_ms: f64,
    /// Billed duration in milliseconds.
    pub billed_duration_ms: u64,
    /// Memory size in MB.
    #[serde(rename = "memorySizeMB")]
    pub memory_size_mb: u64,
    /// Max memory used in MB.
    #[serde(rename = "maxMemoryUsedMB")]
    pub max_memory_used_mb: u64,
    /// Init duration in milliseconds (cold start only).
    #[serde(default)]
    pub init_duration_ms: Option<f64>,
    /// Restore duration in milliseconds (SnapStart only).
    #[serde(default)]
    pub restore_duration_ms: Option<f64>,
}

/// Record for platform.fault event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaultRecord {
    /// Request ID for this invocation.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Fault message.
    #[serde(default)]
    pub fault_message: Option<String>,
}

/// Record for platform.extension event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionRecord {
    /// Name of the extension.
    pub name: String,
    /// State of the extension.
    pub state: String,
    /// Events the extension subscribes to.
    #[serde(default)]
    pub events: Vec<String>,
}

/// Error from Telemetry API operations.
#[non_exhaustive]
#[derive(Debug)]
pub enum TelemetryError {
    /// Failed to parse telemetry event.
    Parse(String),
}

impl std::fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelemetryError::Parse(msg) => write!(f, "Parse error: {}", msg),
        }
    }
}

impl std::error::Error for TelemetryError {}

/// HTTP listener for receiving Telemetry API events.
pub struct TelemetryListener {
    port: u16,
    event_tx: mpsc::Sender<Vec<TelemetryEvent>>,
    cancel_token: CancellationToken,
}

impl TelemetryListener {
    /// Creates a new Telemetry API listener.
    ///
    /// # Arguments
    ///
    /// * `port` - Port to listen on
    /// * `event_tx` - Channel to send received events
    /// * `cancel_token` - Token for graceful shutdown
    pub fn new(
        port: u16,
        event_tx: mpsc::Sender<Vec<TelemetryEvent>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            port,
            event_tx,
            cancel_token,
        }
    }

    /// Returns the listener URI for use in subscription requests.
    ///
    /// In a real Lambda environment, this would use `sandbox.localdomain`
    /// which resolves to the execution environment. For local testing,
    /// we use `127.0.0.1` to ensure routable connectivity.
    pub fn listener_uri(&self) -> String {
        // Check if we're running in Lambda (AWS_LAMBDA_FUNCTION_NAME is set)
        if std::env::var("AWS_LAMBDA_FUNCTION_NAME").is_ok() {
            format!("http://sandbox.localdomain:{}", self.port)
        } else {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    /// Starts the HTTP listener.
    ///
    /// This method blocks until the cancellation token is triggered.
    pub async fn run(self) -> Result<(), std::io::Error> {
        let state = ListenerState {
            event_tx: self.event_tx,
        };

        let app = Router::new()
            .route("/", post(handle_telemetry))
            .with_state(Arc::new(state));

        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        let listener = TcpListener::bind(addr).await?;

        tracing::info!(port = self.port, "Telemetry API listener started");

        axum::serve(listener, app)
            .with_graceful_shutdown(self.cancel_token.cancelled_owned())
            .await
    }
}

struct ListenerState {
    event_tx: mpsc::Sender<Vec<TelemetryEvent>>,
}

async fn handle_telemetry(
    State(state): State<Arc<ListenerState>>,
    body: Bytes,
) -> impl IntoResponse {
    let events: Vec<TelemetryEvent> = match serde_json::from_slice(&body) {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse telemetry events");
            return StatusCode::BAD_REQUEST;
        }
    };

    tracing::debug!(count = events.len(), "Received telemetry events");

    match state.event_tx.try_send(events) {
        Ok(()) => StatusCode::OK,
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("Telemetry event channel full");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::error!("Telemetry event channel closed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_subscription_platform() {
        let sub = TelemetrySubscription::platform_events("http://localhost:9999");

        assert_eq!(sub.schema_version, "2022-12-13");
        assert_eq!(sub.types, vec![TelemetryType::Platform]);
        assert_eq!(sub.destination.uri, "http://localhost:9999");
    }

    #[test]
    fn test_telemetry_subscription_all() {
        let sub = TelemetrySubscription::all_events("http://localhost:9999");

        assert_eq!(sub.types.len(), 3);
        assert!(sub.types.contains(&TelemetryType::Platform));
        assert!(sub.types.contains(&TelemetryType::Function));
        assert!(sub.types.contains(&TelemetryType::Extension));
    }

    #[test]
    fn test_parse_start_event() {
        let json = r#"[{
            "type": "platform.start",
            "time": "2022-10-12T00:00:00.000Z",
            "record": {
                "requestId": "test-request-id",
                "version": "$LATEST"
            }
        }]"#;

        let events: Vec<TelemetryEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(events.len(), 1);

        match &events[0] {
            TelemetryEvent::Start { record, .. } => {
                assert_eq!(record.request_id, "test-request-id");
                assert_eq!(record.version, Some("$LATEST".to_string()));
            }
            _ => panic!("Expected Start event"),
        }
    }

    #[test]
    fn test_parse_report_event() {
        let json = r#"[{
            "type": "platform.report",
            "time": "2022-10-12T00:00:00.000Z",
            "record": {
                "requestId": "test-request-id",
                "status": "success",
                "metrics": {
                    "durationMs": 100.5,
                    "billedDurationMs": 200,
                    "memorySizeMB": 128,
                    "maxMemoryUsedMB": 64
                }
            }
        }]"#;

        let events: Vec<TelemetryEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(events.len(), 1);

        match &events[0] {
            TelemetryEvent::Report { record, .. } => {
                assert_eq!(record.request_id, "test-request-id");
                assert_eq!(record.status, "success");
                assert_eq!(record.metrics.duration_ms, 100.5);
                assert_eq!(record.metrics.billed_duration_ms, 200);
            }
            _ => panic!("Expected Report event"),
        }
    }

    #[test]
    fn test_parse_runtime_done_event() {
        let json = r#"[{
            "type": "platform.runtimeDone",
            "time": "2022-10-12T00:00:00.000Z",
            "record": {
                "requestId": "test-request-id",
                "status": "success",
                "metrics": {
                    "durationMs": 50.0
                },
                "spans": [
                    {"name": "responseLatency", "start": 0.0, "durationMs": 10.0}
                ]
            }
        }]"#;

        let events: Vec<TelemetryEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(events.len(), 1);

        match &events[0] {
            TelemetryEvent::RuntimeDone { record, .. } => {
                assert_eq!(record.request_id, "test-request-id");
                assert_eq!(record.spans.len(), 1);
                assert_eq!(record.spans[0].name, "responseLatency");
            }
            _ => panic!("Expected RuntimeDone event"),
        }
    }

    #[test]
    fn test_parse_init_events() {
        let json = r#"[
            {
                "type": "platform.initStart",
                "time": "2022-10-12T00:00:00.000Z",
                "record": {
                    "initializationType": "on-demand",
                    "phase": "init"
                }
            },
            {
                "type": "platform.initRuntimeDone",
                "time": "2022-10-12T00:00:01.000Z",
                "record": {
                    "initializationType": "on-demand",
                    "status": "success",
                    "phase": "init"
                }
            }
        ]"#;

        let events: Vec<TelemetryEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(events.len(), 2);

        match &events[0] {
            TelemetryEvent::InitStart { record, .. } => {
                assert_eq!(record.initialization_type, "on-demand");
            }
            _ => panic!("Expected InitStart event"),
        }

        match &events[1] {
            TelemetryEvent::InitRuntimeDone { record, .. } => {
                assert_eq!(record.status, "success");
            }
            _ => panic!("Expected InitRuntimeDone event"),
        }
    }

    #[test]
    fn test_parse_function_log() {
        let json = r#"[{
            "type": "function",
            "time": "2022-10-12T00:00:00.000Z",
            "record": "Hello from Lambda!"
        }]"#;

        let events: Vec<TelemetryEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(events.len(), 1);

        match &events[0] {
            TelemetryEvent::Function { record, .. } => {
                assert_eq!(record, "Hello from Lambda!");
            }
            _ => panic!("Expected Function event"),
        }
    }

    #[test]
    fn test_listener_uri() {
        let (tx, _rx) = mpsc::channel(10);
        let listener = TelemetryListener::new(9999, tx, CancellationToken::new());

        // In non-Lambda environment (no AWS_LAMBDA_FUNCTION_NAME), uses 127.0.0.1
        assert_eq!(listener.listener_uri(), "http://127.0.0.1:9999");
    }

    #[test]
    fn test_telemetry_error_display() {
        let err = TelemetryError::Parse("parse error".to_string());
        assert!(format!("{}", err).contains("parse error"));
    }

    #[test]
    fn test_buffering_config_default() {
        let config = BufferingConfig::default();

        assert_eq!(config.max_items, Some(1000));
        assert_eq!(config.max_bytes, Some(256 * 1024));
        assert_eq!(config.timeout_ms, Some(25));
    }
}

//! Telemetry API types and event schemas.
//!
//! Implements the AWS Lambda Telemetry API event schemas as documented at:
//! <https://docs.aws.amazon.com/lambda/latest/dg/telemetry-api.html>
//!
//! Schema version: 2022-12-13

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Subscription configuration for the Telemetry API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySubscription {
    /// Types of events to subscribe to.
    pub types: Vec<TelemetryEventType>,

    /// Buffering configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffering: Option<BufferingConfig>,

    /// HTTP destination for telemetry events.
    pub destination: Destination,
}

/// Types of telemetry events that can be subscribed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryEventType {
    /// Platform lifecycle events.
    Platform,

    /// Function logs.
    Function,

    /// Extension logs.
    Extension,
}

/// Buffering configuration for telemetry events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferingConfig {
    /// Maximum number of events to buffer before sending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,

    /// Maximum bytes to buffer before sending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u32>,

    /// Maximum time in milliseconds to buffer events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}

/// HTTP destination for telemetry events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Destination {
    /// Protocol (must be "HTTP").
    pub protocol: String,

    /// URI to send events to.
    #[serde(rename = "URI")]
    pub uri: String,
}

/// A telemetry event sent to extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Timestamp of the event in RFC3339 format.
    pub time: DateTime<Utc>,

    /// Event type.
    #[serde(rename = "type")]
    pub event_type: String,

    /// Event-specific record data.
    pub record: serde_json::Value,
}

/// Platform event: invocation started.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformStart {
    /// Request ID for this invocation.
    pub request_id: String,

    /// Version (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Tracing context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracing: Option<TraceContext>,
}

/// Platform event: runtime finished processing invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformRuntimeDone {
    /// Request ID for this invocation.
    pub request_id: String,

    /// Status of the invocation.
    pub status: RuntimeStatus,

    /// Metrics for the runtime execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RuntimeDoneMetrics>,

    /// Trace spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,

    /// Tracing context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracing: Option<TraceContext>,
}

/// Platform event: invocation report with metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformReport {
    /// Request ID for this invocation.
    pub request_id: String,

    /// Status of the invocation.
    pub status: RuntimeStatus,

    /// Metrics for this invocation.
    pub metrics: ReportMetrics,

    /// Trace spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,

    /// Tracing context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracing: Option<TraceContext>,
}

/// Platform event: initialization started.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInitStart {
    /// Initialization type.
    pub initialization_type: InitializationType,

    /// Phase (init or invoke).
    pub phase: Phase,

    /// Function name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_name: Option<String>,

    /// Function version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_version: Option<String>,

    /// Instance ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,

    /// Instance maximum memory in MB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_max_memory: Option<u32>,

    /// Runtime version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,

    /// Runtime version ARN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version_arn: Option<String>,

    /// Tracing context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracing: Option<TraceContext>,
}

/// Platform event: runtime finished initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInitRuntimeDone {
    /// Initialization type.
    pub initialization_type: InitializationType,

    /// Phase (init or invoke).
    pub phase: Phase,

    /// Status of initialization.
    pub status: RuntimeStatus,

    /// Trace spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,

    /// Tracing context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracing: Option<TraceContext>,
}

/// Platform event: initialization report with metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInitReport {
    /// Initialization type.
    pub initialization_type: InitializationType,

    /// Phase (init or invoke).
    pub phase: Phase,

    /// Status of initialization.
    pub status: RuntimeStatus,

    /// Metrics for initialization.
    pub metrics: InitReportMetrics,

    /// Trace spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,

    /// Tracing context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracing: Option<TraceContext>,
}

/// Runtime execution status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeStatus {
    /// Successful execution.
    Success,

    /// Runtime error.
    Error,

    /// Runtime failure.
    Failure,

    /// Execution timeout.
    Timeout,
}

/// Initialization type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitializationType {
    /// On-demand initialization.
    OnDemand,

    /// Provisioned concurrency.
    ProvisionedConcurrency,

    /// SnapStart initialization.
    SnapStart,
}

/// Lambda execution phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Initialization phase.
    Init,

    /// Invocation phase.
    Invoke,
}

/// Metrics for runtime execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDoneMetrics {
    /// Duration in milliseconds.
    pub duration_ms: f64,

    /// Number of bytes produced (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub produced_bytes: Option<u64>,
}

/// Metrics for invocation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportMetrics {
    /// Duration in milliseconds.
    pub duration_ms: f64,

    /// Billed duration in milliseconds.
    pub billed_duration_ms: u64,

    /// Memory size in MB.
    #[serde(rename = "memorySizeMB")]
    pub memory_size_mb: u64,

    /// Maximum memory used in MB.
    #[serde(rename = "maxMemoryUsedMB")]
    pub max_memory_used_mb: u64,

    /// Init duration in milliseconds (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_duration_ms: Option<f64>,

    /// Restore duration in milliseconds (optional, SnapStart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_duration_ms: Option<f64>,

    /// Billed restore duration in milliseconds (optional, SnapStart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billed_restore_duration_ms: Option<u64>,
}

/// Metrics for initialization report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitReportMetrics {
    /// Duration in milliseconds.
    pub duration_ms: f64,
}

/// Trace context for X-Ray.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceContext {
    /// Tracing type (X-Amzn-Trace-Id).
    #[serde(rename = "type")]
    pub trace_type: String,

    /// Trace ID value.
    pub value: String,

    /// Span ID (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

/// A trace span.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    /// Span name.
    pub name: String,

    /// Start time in RFC3339 format.
    pub start: DateTime<Utc>,

    /// Duration in milliseconds.
    pub duration_ms: f64,
}

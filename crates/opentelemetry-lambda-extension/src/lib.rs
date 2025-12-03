//! AWS Lambda Extension for OpenTelemetry signal collection and export.
//!
//! This extension integrates with the AWS Lambda Extensions API (via the
//! `lambda_extension` crate) to collect OpenTelemetry traces, metrics, and
//! logs from Lambda functions and export them to configured backends.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod aggregator;
pub mod config;
pub mod context;
pub mod conversion;
pub mod error;
pub mod exporter;
pub mod flush;
pub mod receiver;
pub mod resource;
pub mod runtime;
pub mod service;
pub mod telemetry;
pub mod tracing;

pub use aggregator::{BatchedSignal, SignalAggregator};
pub use config::{
    Compression, Config, CorrelationConfig, ExporterConfig, FlushConfig, FlushStrategy, Protocol,
    ReceiverConfig, TelemetryApiConfig,
};
pub use context::{
    InvocationContextManager, PlatformEvent, PlatformEventType, RequestId, SpanContext,
};
pub use conversion::{MetricsConverter, SpanConverter, TelemetryProcessor};
pub use error::{ExtensionError, Result};
pub use exporter::{ExportError, ExportResult, OtlpExporter};
pub use flush::{FlushManager, FlushReason};
pub use receiver::{FlushError, HealthResponse, OtlpReceiver, ReceiverHandle, Signal};
pub use resource::{ResourceBuilder, detect_resource};
pub use runtime::{ExtensionRuntime, RuntimeBuilder, RuntimeError};
pub use service::{EventsService, ExtensionState, TelemetryService};
pub use telemetry::{
    ReportMetrics, ReportRecord, RuntimeDoneRecord, StartRecord, TelemetryError, TelemetryEvent,
    TelemetryListener, TelemetrySubscription, TelemetryType,
};
pub use tracing::{W3CTraceContext, XRayTraceHeader};

pub use opentelemetry_configuration::{
    ExportFailure, ExportFallback, FailedRequest, FallbackHandler, OtelGuard, OtelSdkBuilder,
    OtelSdkConfig, Protocol as OtelProtocol, SdkError,
};

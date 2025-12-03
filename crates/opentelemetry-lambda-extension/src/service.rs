//! Tower services for Lambda extension lifecycle and telemetry processing.
//!
//! This module provides Tower `Service` implementations that integrate with the
//! `lambda_extension` crate for proper lifecycle management. Using the official
//! Lambda extension library ensures correct handling of SHUTDOWN events and
//! telemetry delivery timing.
//!
//! The services use a shared `RwLock` to coordinate shutdown with telemetry
//! processing. The `TelemetryService` holds a read lock while processing events,
//! and the `EventsService` acquires a write lock on SHUTDOWN before performing
//! the final flush. This ensures all in-flight telemetry is processed before
//! shutdown completes.

use crate::aggregator::SignalAggregator;
use crate::config::Config;
use crate::conversion::{MetricsConverter, TelemetryProcessor};
use crate::exporter::OtlpExporter;
use crate::flush::FlushManager;
use crate::receiver::Signal;
use lambda_extension::{Error, LambdaEvent, LambdaTelemetry, LambdaTelemetryRecord, NextEvent};
use opentelemetry_proto::tonic::resource::v1::Resource;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tower::Service;

/// Shared state for extension services.
///
/// This holds the components that need to be shared between the events
/// processor and telemetry processor services.
pub struct ExtensionState {
    pub(crate) aggregator: Arc<SignalAggregator>,
    pub(crate) exporter: Arc<OtlpExporter>,
    pub(crate) flush_manager: Arc<Mutex<FlushManager>>,
    pub(crate) telemetry_processor: Arc<Mutex<TelemetryProcessor>>,
    pub(crate) metrics_converter: MetricsConverter,
    #[allow(dead_code)]
    pub(crate) config: Config,
    /// Lock to coordinate shutdown with telemetry processing.
    ///
    /// `TelemetryService` acquires a read lock while processing events.
    /// `EventsService` acquires a write lock on SHUTDOWN before final flush.
    /// This ensures all in-flight telemetry is processed before shutdown.
    processing_lock: RwLock<()>,
}

impl ExtensionState {
    /// Creates new extension state with the given configuration and resource.
    pub fn new(config: Config, resource: Resource) -> Result<Self, crate::exporter::ExportError> {
        let exporter = OtlpExporter::new(config.exporter.clone())?;

        Ok(Self {
            aggregator: Arc::new(SignalAggregator::new(config.flush.clone())),
            exporter: Arc::new(exporter),
            flush_manager: Arc::new(Mutex::new(FlushManager::new(config.flush.clone()))),
            telemetry_processor: Arc::new(Mutex::new(TelemetryProcessor::new(resource.clone()))),
            metrics_converter: MetricsConverter::new(resource),
            config,
            processing_lock: RwLock::new(()),
        })
    }

    /// Performs a flush of all pending signals to the exporter.
    pub async fn flush_all(&self) {
        let batches = self.aggregator.get_all_batches().await;
        let mut flush_manager = self.flush_manager.lock().await;

        for batch in batches {
            let result = self.exporter.export(batch).await;
            match result {
                crate::exporter::ExportResult::Success => {
                    flush_manager.record_flush();
                }
                crate::exporter::ExportResult::Fallback
                | crate::exporter::ExportResult::Skipped => {
                    flush_manager.record_flush_timeout();
                }
            }
        }
    }

    /// Waits for any in-progress telemetry processing to complete.
    ///
    /// This acquires a write lock on the processing lock, which blocks until
    /// all read locks (held by `TelemetryService` during processing) are released.
    /// The timeout prevents indefinite blocking if something goes wrong.
    pub async fn wait_for_processing_complete(&self, timeout: Duration) {
        let result = tokio::time::timeout(timeout, self.processing_lock.write()).await;
        if result.is_err() {
            tracing::warn!(
                timeout_ms = timeout.as_millis(),
                "Timed out waiting for telemetry processing to complete"
            );
        }
        // Lock is immediately dropped, we just needed to wait for it
    }

    /// Performs a final flush draining all signals.
    pub async fn final_flush(&self) {
        tracing::info!("Performing final flush");

        let batches = self.aggregator.drain_all().await;
        let count = batches.len();

        for batch in batches {
            let result = self.exporter.export(batch).await;
            tracing::debug!(?result, "Final flush batch");
        }

        let dropped = self.aggregator.dropped_count().await;
        if dropped > 0 {
            tracing::warn!(
                dropped = dropped,
                "Signals were dropped due to queue limits"
            );
        }

        tracing::info!(batches = count, dropped = dropped, "Final flush complete");
    }
}

/// Tower service for processing Lambda extension lifecycle events.
///
/// This service handles INVOKE and SHUTDOWN events from the Extensions API.
/// On SHUTDOWN, it performs a final flush of all buffered telemetry.
pub struct EventsService {
    state: Arc<ExtensionState>,
}

impl EventsService {
    /// Creates a new events service with the given shared state.
    pub fn new(state: Arc<ExtensionState>) -> Self {
        Self { state }
    }
}

impl Service<LambdaEvent> for EventsService {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, event: LambdaEvent) -> Self::Future {
        let state = Arc::clone(&self.state);

        Box::pin(async move {
            match event.next {
                NextEvent::Invoke(invoke) => {
                    tracing::debug!(request_id = %invoke.request_id, "Received INVOKE event");

                    // Record invocation for adaptive flush pattern detection
                    {
                        let mut flush_manager = state.flush_manager.lock().await;
                        flush_manager.record_invocation();
                    }

                    // Check if we should flush based on pending count
                    let pending = state.aggregator.pending_count().await;
                    let should_flush = {
                        let flush_manager = state.flush_manager.lock().await;
                        flush_manager
                            .should_flush(Some(invoke.deadline_ms as i64), pending, false)
                            .is_some()
                    };

                    if should_flush {
                        tracing::debug!(pending, "Flushing during invocation");
                        state.flush_all().await;
                    }
                }
                NextEvent::Shutdown(shutdown) => {
                    tracing::info!(reason = ?shutdown.shutdown_reason, "Received SHUTDOWN event");

                    // Wait for any in-flight telemetry processing to complete
                    // This ensures we don't flush before the last batch of telemetry
                    // (e.g., platform.report) has been processed and added to the aggregator
                    state
                        .wait_for_processing_complete(Duration::from_millis(500))
                        .await;

                    // Emit shutdown metric
                    let shutdown_reason = format!("{:?}", shutdown.shutdown_reason);
                    let shutdown_metric = state
                        .metrics_converter
                        .create_shutdown_metric(&shutdown_reason);
                    state.aggregator.add(Signal::Metrics(shutdown_metric)).await;

                    // Final flush of all signals
                    state.final_flush().await;
                }
            }

            Ok(())
        })
    }
}

/// Tower service for processing Lambda Telemetry API events.
///
/// This service receives platform telemetry events and converts them to
/// OTLP metrics and traces, adding them to the aggregator for export.
#[derive(Clone)]
pub struct TelemetryService {
    state: Arc<ExtensionState>,
}

impl TelemetryService {
    /// Creates a new telemetry service with the given shared state.
    pub fn new(state: Arc<ExtensionState>) -> Self {
        Self { state }
    }
}

impl Service<Vec<LambdaTelemetry>> for TelemetryService {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, events: Vec<LambdaTelemetry>) -> Self::Future {
        let state = Arc::clone(&self.state);

        Box::pin(async move {
            // Acquire read lock to prevent shutdown from flushing while we're processing
            let _guard = state.processing_lock.read().await;

            tracing::debug!(count = events.len(), "Processing telemetry events");

            // Convert lambda_extension telemetry events to our internal format
            let internal_events = convert_telemetry_events(events);

            // Process through our TelemetryProcessor
            let (metrics, _traces) = {
                let mut processor = state.telemetry_processor.lock().await;
                processor.process_events(internal_events)
            };

            // Add metrics to aggregator
            for metric in metrics {
                state
                    .aggregator
                    .add(Signal::Metrics(
                        opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest {
                            resource_metrics: metric.resource_metrics,
                        },
                    ))
                    .await;
            }

            Ok(())
        })
    }
}

/// Converts lambda_extension telemetry events to our internal format.
fn convert_telemetry_events(events: Vec<LambdaTelemetry>) -> Vec<crate::telemetry::TelemetryEvent> {
    use crate::telemetry::{
        ReportMetrics, ReportRecord, RuntimeDoneRecord, RuntimeMetrics, SpanRecord, StartRecord,
        TelemetryEvent, TracingRecord,
    };

    events
        .into_iter()
        .filter_map(|event| {
            let time = event.time.to_rfc3339();

            match event.record {
                LambdaTelemetryRecord::PlatformStart {
                    request_id,
                    version,
                    tracing,
                } => Some(TelemetryEvent::Start {
                    time,
                    record: StartRecord {
                        request_id,
                        version,
                        tracing: tracing.map(|t| TracingRecord {
                            span_id: None,
                            trace_type: Some(format!("{:?}", t.r#type)),
                            value: Some(t.value),
                        }),
                    },
                }),

                LambdaTelemetryRecord::PlatformRuntimeDone {
                    request_id,
                    status,
                    error_type: _,
                    metrics,
                    spans,
                    tracing,
                } => Some(TelemetryEvent::RuntimeDone {
                    time,
                    record: RuntimeDoneRecord {
                        request_id,
                        status: format!("{:?}", status),
                        metrics: metrics.map(|m| RuntimeMetrics {
                            duration_ms: m.duration_ms,
                            produced_bytes: m.produced_bytes,
                        }),
                        spans: spans
                            .into_iter()
                            .map(|s| SpanRecord {
                                name: s.name,
                                start: s.start.timestamp_millis() as f64,
                                duration_ms: s.duration_ms,
                            })
                            .collect(),
                        tracing: tracing.map(|t| TracingRecord {
                            span_id: None,
                            trace_type: Some(format!("{:?}", t.r#type)),
                            value: Some(t.value),
                        }),
                    },
                }),

                LambdaTelemetryRecord::PlatformReport {
                    request_id,
                    status,
                    error_type: _,
                    metrics,
                    spans: _,
                    tracing,
                } => Some(TelemetryEvent::Report {
                    time,
                    record: ReportRecord {
                        request_id,
                        status: format!("{:?}", status),
                        metrics: ReportMetrics {
                            duration_ms: metrics.duration_ms,
                            billed_duration_ms: metrics.billed_duration_ms,
                            memory_size_mb: metrics.memory_size_mb,
                            max_memory_used_mb: metrics.max_memory_used_mb,
                            init_duration_ms: metrics.init_duration_ms,
                            restore_duration_ms: metrics.restore_duration_ms,
                        },
                        tracing: tracing.map(|t| TracingRecord {
                            span_id: None,
                            trace_type: Some(format!("{:?}", t.r#type)),
                            value: Some(t.value),
                        }),
                    },
                }),

                // Log other events but don't convert them
                _ => {
                    tracing::trace!(?event, "Ignoring non-platform telemetry event");
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lambda_extension::LambdaTelemetry;

    #[test]
    fn test_extension_state_creation() {
        let config = Config::default();
        let sdk_resource = crate::resource::detect_resource();
        let proto_resource = crate::resource::to_proto_resource(&sdk_resource);

        // This will fail if exporter can't be created, but that's fine for unit tests
        let result = ExtensionState::new(config, proto_resource);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simulator_telemetry_format_deserialization() {
        // This is the exact format our simulator sends
        let json = r#"[{"time":"2025-11-30T22:29:09.581655Z","type":"platform.start","record":{"requestId":"38432cb4-cb8b-4162-982d-923d3c3f6d10","tracing":{"type":"X-Amzn-Trace-Id","value":"Root=1-692cc535-0338d3516cb745b7b41f878e"},"version":"$LATEST"}}]"#;

        let result: Result<Vec<LambdaTelemetry>, _> = serde_json::from_str(json);
        match &result {
            Ok(events) => println!("Success: {:?}", events),
            Err(e) => println!("Error: {}", e),
        }
        assert!(result.is_ok(), "Failed to deserialize: {:?}", result.err());
    }

    #[test]
    fn test_full_simulator_batch_deserialization() {
        // Full batch similar to what the test produces
        let json = r#"[{"time":"2025-11-30T22:35:51.565094Z","type":"platform.start","record":{"requestId":"0c90003a-8970-474c-b696-fca5336ef4f5","tracing":{"type":"X-Amzn-Trace-Id","value":"Root=1-692cc6c7-f2ce8d3383524609b99c07a9"},"version":"$LATEST"}},{"time":"2025-11-30T22:35:51.565857Z","type":"platform.initRuntimeDone","record":{"initializationType":"on-demand","phase":"init","status":"success"}},{"time":"2025-11-30T22:35:51.565857Z","type":"platform.initReport","record":{"initializationType":"on-demand","phase":"init","status":"success","metrics":{"durationMs":565.4}}},{"time":"2025-11-30T22:35:51.578834Z","type":"platform.runtimeDone","record":{"requestId":"0c90003a-8970-474c-b696-fca5336ef4f5","status":"success","metrics":{"durationMs":13.74},"spans":[],"tracing":{"type":"X-Amzn-Trace-Id","value":"Root=1-692cc6c7-f2ce8d3383524609b99c07a9"}}},{"time":"2025-11-30T22:35:51.578909Z","type":"platform.report","record":{"requestId":"0c90003a-8970-474c-b696-fca5336ef4f5","status":"success","metrics":{"durationMs":13.74,"billedDurationMs":100,"memorySizeMB":128,"maxMemoryUsedMB":64},"tracing":{"type":"X-Amzn-Trace-Id","value":"Root=1-692cc6c7-f2ce8d3383524609b99c07a9"}}}]"#;

        let result: Result<Vec<LambdaTelemetry>, _> = serde_json::from_str(json);
        match &result {
            Ok(events) => {
                println!("Success: {} events parsed", events.len());
                for (i, event) in events.iter().enumerate() {
                    println!("  Event {}: {:?}", i, event);
                }
            }
            Err(e) => println!("Error: {}", e),
        }
        assert!(result.is_ok(), "Failed to deserialize: {:?}", result.err());
    }
}

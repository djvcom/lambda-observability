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
use crate::completion::{CompletionOutcome, CompletionSource, CompletionTracker};
use crate::config::{CompletionWait, Config, FlushStrategy};
use crate::conversion::{MetricsConverter, TelemetryProcessor};
use crate::exporter::OtlpExporter;
use crate::flush::FlushManager;
use crate::receiver::Signal;
use lambda_extension::{
    Error, InvokeEvent, LambdaEvent, LambdaTelemetry, LambdaTelemetryRecord, NextEvent,
    ShutdownEvent,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, oneshot};
use tower::Service;

/// Safety margin subtracted from the invocation deadline when holding
/// `/next`, leaving room for the flush itself before the function times out.
const HOLD_DEADLINE_MARGIN: Duration = Duration::from_secs(1);

/// Budget granted to each flush in the invocation path. This is the margin
/// reserved by [`HOLD_DEADLINE_MARGIN`]: the hold releases early enough
/// that the flush can spend this long. The budget is measured from when the
/// flush starts, not from the invocation deadline — the deadline bounds the
/// runtime's handler, not the extension, and the environment stays thawed
/// while the INVOKE handler is still running.
const INVOKE_FLUSH_BUDGET: Duration = HOLD_DEADLINE_MARGIN;

/// Grace period after a completion signal for signals already accepted by
/// the receiver to reach the aggregator before the post-invoke flush reads it.
const COMPLETION_SETTLE_DELAY: Duration = Duration::from_millis(20);

/// Computes the instant until which `/next` may be held for the invocation
/// with the given epoch-millisecond deadline.
///
/// Returns `None` when holding is disabled by configuration or there is no
/// usable window before the deadline.
fn hold_deadline(deadline_ms: u64, completion_wait: CompletionWait) -> Option<Instant> {
    let cap = match completion_wait {
        CompletionWait::Off => return None,
        CompletionWait::Auto => None,
        CompletionWait::Cap(cap) => Some(cap),
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let remaining = Duration::from_millis(deadline_ms.saturating_sub(now_ms))
        .saturating_sub(HOLD_DEADLINE_MARGIN);
    if remaining.is_zero() {
        return None;
    }

    let hold = match cap {
        Some(cap) => remaining.min(cap),
        None => remaining,
    };
    Some(Instant::now() + hold)
}

/// Safety margin subtracted from the SHUTDOWN deadline, leaving room to
/// signal completion and exit before Lambda sends SIGKILL.
const SHUTDOWN_DEADLINE_MARGIN: Duration = Duration::from_millis(200);

/// Converts the epoch-millisecond deadline from a SHUTDOWN event into the
/// instant by which all shutdown work must finish.
fn shutdown_deadline(deadline_ms: u64) -> Instant {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let remaining = Duration::from_millis(deadline_ms.saturating_sub(now_ms))
        .saturating_sub(SHUTDOWN_DEADLINE_MARGIN);
    Instant::now() + remaining
}

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
    pub(crate) completion: Arc<CompletionTracker>,
    pub(crate) config: Config,
    /// Lock to coordinate shutdown with telemetry processing.
    ///
    /// `TelemetryService` acquires a read lock while processing events.
    /// `EventsService` acquires a write lock on SHUTDOWN before final flush.
    /// This ensures all in-flight telemetry is processed before shutdown.
    processing_lock: RwLock<()>,
    /// Channel to signal that shutdown processing is complete.
    ///
    /// The sender is stored in a Mutex so it can be taken when shutdown occurs.
    /// The receiver should be used with `tokio::select!` to exit the event loop.
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl ExtensionState {
    /// Creates new extension state with the given configuration and resource.
    ///
    /// Returns the state and a receiver that will be signalled when shutdown is complete.
    /// Use the receiver with `tokio::select!` to exit the event loop gracefully.
    pub fn new(
        config: Config,
        resource: Resource,
    ) -> crate::error::Result<(Self, oneshot::Receiver<()>)> {
        let exporter = OtlpExporter::new(config.exporter.clone())?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let state = Self {
            aggregator: Arc::new(SignalAggregator::new(config.flush.clone())),
            exporter: Arc::new(exporter),
            flush_manager: Arc::new(Mutex::new(FlushManager::new(config.flush.clone()))),
            telemetry_processor: Arc::new(Mutex::new(TelemetryProcessor::new(resource.clone()))),
            metrics_converter: MetricsConverter::new(resource),
            completion: Arc::new(CompletionTracker::new()),
            config,
            processing_lock: RwLock::new(()),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
        };

        Ok((state, shutdown_rx))
    }

    /// Signals that shutdown processing is complete.
    ///
    /// This should be called after `final_flush()` to allow the event loop to exit.
    pub async fn signal_shutdown_complete(&self) {
        if let Some(tx) = self.shutdown_tx.lock().await.take() {
            let _ = tx.send(());
            tracing::debug!("Shutdown complete signal sent");
        }
    }

    /// Performs a flush of all pending signals to the exporter.
    ///
    /// The optional `deadline` bounds the total time spent exporting; see
    /// [`OtlpExporter::export`].
    pub async fn flush_all(&self, deadline: Option<Instant>) {
        let batches = self.aggregator.get_all_batches().await;
        let mut flush_manager = self.flush_manager.lock().await;

        for batch in batches {
            let result = self.exporter.export(batch, deadline).await;
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

    /// Holds the invocation open until a completion signal arrives or the
    /// hold deadline expires, then performs the post-invocation flush.
    ///
    /// This runs inside the INVOKE event handler, which delays the next
    /// `/next` poll and therefore keeps the execution environment thawed:
    /// Lambda only freezes once the runtime has responded and every
    /// extension is parked on `/next`. Everything exported here is
    /// guaranteed not to be interrupted by a freeze.
    ///
    /// While holding, periodic flush strategies still fire so long-running
    /// invocations export incrementally rather than accumulating everything
    /// until the end.
    pub async fn hold_and_flush(&self) {
        loop {
            let periodic_tick = self.time_until_periodic_flush().await;

            tokio::select! {
                outcome = self.completion.wait_for_completion() => {
                    match outcome {
                        CompletionOutcome::Completed(source) => {
                            tracing::debug!(?source, "Invocation complete, entering post-invoke flush window");
                            tokio::time::sleep(COMPLETION_SETTLE_DELAY).await;
                        }
                        CompletionOutcome::DeadlineExpired => {
                            tracing::warn!(
                                "Hold deadline expired without completion signal; flushing and releasing /next"
                            );
                        }
                    }
                    break;
                }
                _ = tokio::time::sleep(periodic_tick.unwrap_or(Duration::MAX)), if periodic_tick.is_some() => {
                    let pending = self.aggregator.pending_count().await;
                    let should_flush = {
                        let flush_manager = self.flush_manager.lock().await;
                        flush_manager.should_flush(None, pending, false).is_some()
                    };
                    if should_flush {
                        tracing::debug!(pending, "Periodic flush during held invocation");
                        self.flush_all(Some(Instant::now() + INVOKE_FLUSH_BUDGET)).await;
                    }
                }
            }
        }

        self.post_invoke_flush().await;
    }

    /// Returns the time until the next periodic flush is due, or `None`
    /// when the effective strategy never flushes mid-invocation.
    ///
    /// The returned duration is never zero: an already-due flush is picked
    /// up on the tick arm of the hold loop, which must always prefer the
    /// completion arm first.
    async fn time_until_periodic_flush(&self) -> Option<Duration> {
        let flush_manager = self.flush_manager.lock().await;
        match self.config.flush.strategy {
            FlushStrategy::Periodic | FlushStrategy::Continuous => Some(
                flush_manager
                    .time_until_next_flush()
                    .max(Duration::from_millis(10)),
            ),
            FlushStrategy::Default | FlushStrategy::End => None,
        }
    }

    /// Flushes pending signals in the post-invocation window when the flush
    /// strategy calls for it, within the invoke-path flush budget.
    pub async fn post_invoke_flush(&self) {
        let pending = self.aggregator.pending_count().await;
        if pending == 0 {
            return;
        }

        let should_flush = {
            let flush_manager = self.flush_manager.lock().await;
            flush_manager
                .should_flush_on_invocation_end(pending)
                .is_some()
                || flush_manager.should_flush(None, pending, false).is_some()
        };

        if should_flush {
            tracing::debug!(pending, "Flushing in post-invocation window");
            self.flush_all(Some(Instant::now() + INVOKE_FLUSH_BUDGET))
                .await;
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

    /// Performs a final flush draining all signals, bounded by `deadline`.
    ///
    /// Signals are drained in batch-sized chunks rather than one merged
    /// request, so a large backlog neither spikes memory during encoding
    /// nor turns into a single all-or-nothing export. Once the deadline is
    /// reached, remaining batches are abandoned and their counts logged —
    /// exceeding the SHUTDOWN grace period would mean SIGKILL mid-export
    /// and silently lost telemetry anyway.
    pub async fn final_flush(&self, deadline: Instant) {
        tracing::info!("Performing final flush");

        let mut exported = 0usize;
        let mut abandoned_batches = 0usize;

        'drain: loop {
            let batches = self.aggregator.get_all_batches().await;
            if batches.is_empty() {
                break;
            }

            let mut batches = batches.into_iter();
            while let Some(batch) = batches.next() {
                if Instant::now() >= deadline {
                    abandoned_batches += 1 + batches.len();
                    break 'drain;
                }
                let result = self.exporter.export(batch, Some(deadline)).await;
                tracing::debug!(?result, "Final flush batch");
                exported += 1;
            }
        }

        let pending_signals = self.aggregator.pending_count().await;
        if abandoned_batches > 0 || pending_signals > 0 {
            tracing::warn!(
                abandoned_batches,
                pending_signals,
                "Final flush deadline reached with telemetry still pending"
            );
        }

        let dropped = self.aggregator.dropped_count().await;
        if dropped > 0 {
            tracing::warn!(
                dropped = dropped,
                "Signals were dropped due to queue limits"
            );
        }

        tracing::info!(
            batches = exported,
            abandoned_batches,
            dropped,
            "Final flush complete"
        );
    }

    /// Handles an INVOKE lifecycle event.
    ///
    /// The hold deadline is registered with the completion tracker even when
    /// holding is skipped, so signals arriving later are judged against the
    /// window they should have arrived in when signal health is updated.
    /// When holding is enabled and healthy, `/next` is held until completion
    /// so the flush runs in the guaranteed post-invocation window; otherwise
    /// any backlog from previous invocations is flushed immediately, while
    /// the environment is known to be thawed.
    async fn handle_invoke(&self, invoke: InvokeEvent) {
        tracing::debug!(request_id = %invoke.request_id, "Received INVOKE event");

        self.flush_manager.lock().await.record_invocation();

        let deadline = hold_deadline(invoke.deadline_ms, self.config.flush.completion_wait);
        self.completion.begin(
            invoke.request_id.clone(),
            deadline.unwrap_or_else(Instant::now),
        );

        match deadline {
            Some(_) if self.completion.should_hold() => self.hold_and_flush().await,
            _ => self.post_invoke_flush().await,
        }
    }

    /// Handles a SHUTDOWN lifecycle event.
    ///
    /// All work is budgeted against the deadline carried by the event:
    /// exceeding it means SIGKILL, so anything that cannot finish in time is
    /// abandoned deliberately rather than cut off mid-export. Waiting for
    /// in-flight telemetry processing (such as a `platform.report` still
    /// being added to the aggregator) is capped at a quarter of the budget.
    async fn handle_shutdown(&self, shutdown: ShutdownEvent) {
        tracing::info!(reason = ?shutdown.shutdown_reason, "Received SHUTDOWN event");

        let deadline = shutdown_deadline(shutdown.deadline_ms);
        let remaining = deadline.saturating_duration_since(Instant::now());

        let processing_wait = (remaining / 4).min(Duration::from_millis(500));
        self.wait_for_processing_complete(processing_wait).await;

        let shutdown_reason = format!("{:?}", shutdown.shutdown_reason);
        let shutdown_metric = self
            .metrics_converter
            .create_shutdown_metric(&shutdown_reason);
        self.aggregator.add(Signal::Metrics(shutdown_metric)).await;

        self.final_flush(deadline).await;
        self.signal_shutdown_complete().await;
    }

    /// Processes a batch of Telemetry API events into the aggregator, then
    /// signals invocation completion for any `runtimeDone` events observed.
    ///
    /// Completion is signalled only after the events and their derived
    /// metrics are in the aggregator, so the post-invocation flush picks
    /// them up. The flush itself is owned by the INVOKE handler's hold
    /// loop, which runs before the extension re-polls `/next` and therefore
    /// cannot be interrupted by a freeze — flushing here could be.
    ///
    /// Holds a read lock on the processing lock throughout, preventing the
    /// SHUTDOWN handler from flushing mid-processing.
    async fn process_telemetry(&self, events: Vec<LambdaTelemetry>) {
        let _guard = self.processing_lock.read().await;

        tracing::debug!(count = events.len(), "Processing telemetry events");

        let runtime_done_ids: Vec<String> = events
            .iter()
            .filter_map(|e| match &e.record {
                LambdaTelemetryRecord::PlatformRuntimeDone { request_id, .. } => {
                    Some(request_id.clone())
                }
                _ => None,
            })
            .collect();

        let internal_events = convert_telemetry_events(events);

        let (metrics, _traces) = {
            let mut processor = self.telemetry_processor.lock().await;
            processor.process_events(internal_events)
        };

        for metric in metrics {
            self.aggregator
                .add(Signal::Metrics(
                    opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest {
                        resource_metrics: metric.resource_metrics,
                    },
                ))
                .await;
        }

        for request_id in runtime_done_ids {
            self.completion
                .complete(Some(&request_id), CompletionSource::RuntimeDone);
        }
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
                NextEvent::Invoke(invoke) => state.handle_invoke(invoke).await,
                NextEvent::Shutdown(shutdown) => state.handle_shutdown(shutdown).await,
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
            state.process_telemetry(events).await;
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
        let (_state, _shutdown_rx) = result.unwrap();
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

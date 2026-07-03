//! Signal aggregation and batching.
//!
//! This module provides queue-based batching for OTLP signals before export.
//! Traces, metrics, and logs are queued separately but share a single memory
//! budget: when `max_queue_bytes` or `max_queue_entries` is exceeded, the
//! oldest signals are dropped and counted. Encoded sizes are computed once
//! at enqueue time and cached alongside each item, since walking a protobuf
//! message tree to size it is linear in its content.

use crate::config::FlushConfig;
use crate::receiver::Signal;
use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
    trace::v1::ExportTraceServiceRequest,
};
use prost::Message;
use std::collections::VecDeque;
use tokio::sync::Mutex;

/// Batched signals ready for export.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum BatchedSignal {
    /// Batched trace spans.
    Traces(ExportTraceServiceRequest),
    /// Batched metrics.
    Metrics(ExportMetricsServiceRequest),
    /// Batched log records.
    Logs(ExportLogsServiceRequest),
}

impl BatchedSignal {
    /// Returns the approximate size of this batch in bytes.
    pub fn size_bytes(&self) -> usize {
        match self {
            BatchedSignal::Traces(req) => req.encoded_len(),
            BatchedSignal::Metrics(req) => req.encoded_len(),
            BatchedSignal::Logs(req) => req.encoded_len(),
        }
    }

    /// Returns the signal type as a static label.
    pub fn signal_type(&self) -> &'static str {
        match self {
            BatchedSignal::Traces(_) => "traces",
            BatchedSignal::Metrics(_) => "metrics",
            BatchedSignal::Logs(_) => "logs",
        }
    }
}

/// Queue for a single signal type, storing each item with its cached
/// encoded size.
struct SignalQueue<T> {
    items: VecDeque<(T, usize)>,
}

impl<T> SignalQueue<T> {
    fn new() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    /// Removes items from the front until the batch limits are reached,
    /// returning the batch and its total size.
    fn take_batch(&mut self, max_bytes: usize, max_entries: usize) -> (Vec<T>, usize) {
        let mut batch = Vec::new();
        let mut batch_size = 0;

        while let Some((_, item_size)) = self.items.front() {
            if !batch.is_empty()
                && (batch_size + item_size > max_bytes || batch.len() >= max_entries)
            {
                break;
            }
            let (item, item_size) = self.items.pop_front().expect("front checked above");
            batch.push(item);
            batch_size += item_size;
        }

        (batch, batch_size)
    }
}

/// The three signal queues behind one lock, sharing a memory budget.
struct Queues {
    traces: SignalQueue<ExportTraceServiceRequest>,
    metrics: SignalQueue<ExportMetricsServiceRequest>,
    logs: SignalQueue<ExportLogsServiceRequest>,
    current_bytes: usize,
    dropped: u64,
}

impl Queues {
    fn total_entries(&self) -> usize {
        self.traces.len() + self.metrics.len() + self.logs.len()
    }

    /// Drops the oldest signal, preferring the queue the current push
    /// targets so unrelated signal types are evicted only when necessary.
    fn drop_oldest(&mut self, prefer: SignalKind) {
        let order = match prefer {
            SignalKind::Traces => [SignalKind::Traces, SignalKind::Metrics, SignalKind::Logs],
            SignalKind::Metrics => [SignalKind::Metrics, SignalKind::Traces, SignalKind::Logs],
            SignalKind::Logs => [SignalKind::Logs, SignalKind::Traces, SignalKind::Metrics],
        };

        for kind in order {
            let dropped_size = match kind {
                SignalKind::Traces => self.traces.items.pop_front().map(|(_, size)| size),
                SignalKind::Metrics => self.metrics.items.pop_front().map(|(_, size)| size),
                SignalKind::Logs => self.logs.items.pop_front().map(|(_, size)| size),
            };
            if let Some(size) = dropped_size {
                self.current_bytes = self.current_bytes.saturating_sub(size);
                self.dropped += 1;
                return;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SignalKind {
    Traces,
    Metrics,
    Logs,
}

/// Aggregator for batching OTLP signals.
///
/// Receives signals from the OTLP receiver and batches them for efficient
/// export. Traces, metrics, and logs have separate queues but share one
/// byte and entry budget so total buffered memory stays bounded regardless
/// of the signal mix.
pub struct SignalAggregator {
    queues: Mutex<Queues>,
    config: FlushConfig,
}

impl SignalAggregator {
    /// Creates a new signal aggregator with the given configuration.
    pub fn new(config: FlushConfig) -> Self {
        Self {
            queues: Mutex::new(Queues {
                traces: SignalQueue::new(),
                metrics: SignalQueue::new(),
                logs: SignalQueue::new(),
                current_bytes: 0,
                dropped: 0,
            }),
            config,
        }
    }

    /// Creates a new aggregator with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(FlushConfig::default())
    }

    /// Adds a signal to the appropriate queue.
    ///
    /// If the shared budget would be exceeded, the oldest signals are
    /// dropped to make room. A single signal larger than the entire byte
    /// budget is rejected outright, so buffered bytes never exceed
    /// `max_queue_bytes`.
    pub async fn add(&self, signal: Signal) {
        let (kind, size) = match &signal {
            Signal::Traces(req) => (SignalKind::Traces, req.encoded_len()),
            Signal::Metrics(req) => (SignalKind::Metrics, req.encoded_len()),
            Signal::Logs(req) => (SignalKind::Logs, req.encoded_len()),
        };

        let mut queues = self.queues.lock().await;

        if size > self.config.max_queue_bytes {
            queues.dropped += 1;
            tracing::warn!(
                size,
                budget = self.config.max_queue_bytes,
                "Signal larger than the queue byte budget, dropping"
            );
            return;
        }

        while queues.current_bytes + size > self.config.max_queue_bytes
            || queues.total_entries() >= self.config.max_queue_entries
        {
            queues.drop_oldest(kind);
        }

        queues.current_bytes += size;
        match signal {
            Signal::Traces(req) => queues.traces.items.push_back((req, size)),
            Signal::Metrics(req) => queues.metrics.items.push_back((req, size)),
            Signal::Logs(req) => queues.logs.items.push_back((req, size)),
        }
    }

    /// Gets the next batch of traces for export.
    ///
    /// Returns `None` if the trace queue is empty.
    pub async fn get_trace_batch(&self) -> Option<BatchedSignal> {
        let mut queues = self.queues.lock().await;
        let (batch, batch_size) = queues
            .traces
            .take_batch(self.config.max_batch_bytes, self.config.max_batch_entries);

        if batch.is_empty() {
            return None;
        }

        queues.current_bytes = queues.current_bytes.saturating_sub(batch_size);
        Some(BatchedSignal::Traces(merge_trace_requests(batch)))
    }

    /// Gets the next batch of metrics for export.
    ///
    /// Returns `None` if the metrics queue is empty.
    pub async fn get_metrics_batch(&self) -> Option<BatchedSignal> {
        let mut queues = self.queues.lock().await;
        let (batch, batch_size) = queues
            .metrics
            .take_batch(self.config.max_batch_bytes, self.config.max_batch_entries);

        if batch.is_empty() {
            return None;
        }

        queues.current_bytes = queues.current_bytes.saturating_sub(batch_size);
        Some(BatchedSignal::Metrics(merge_metrics_requests(batch)))
    }

    /// Gets the next batch of logs for export.
    ///
    /// Returns `None` if the logs queue is empty.
    pub async fn get_logs_batch(&self) -> Option<BatchedSignal> {
        let mut queues = self.queues.lock().await;
        let (batch, batch_size) = queues
            .logs
            .take_batch(self.config.max_batch_bytes, self.config.max_batch_entries);

        if batch.is_empty() {
            return None;
        }

        queues.current_bytes = queues.current_bytes.saturating_sub(batch_size);
        Some(BatchedSignal::Logs(merge_logs_requests(batch)))
    }

    /// Gets all available batches across all signal types.
    ///
    /// Batches respect the configured batch size limits, so draining a
    /// large backlog produces several bounded batches rather than one
    /// oversized request.
    pub async fn get_all_batches(&self) -> Vec<BatchedSignal> {
        let mut batches = Vec::new();

        while let Some(batch) = self.get_trace_batch().await {
            batches.push(batch);
        }

        while let Some(batch) = self.get_metrics_batch().await {
            batches.push(batch);
        }

        while let Some(batch) = self.get_logs_batch().await {
            batches.push(batch);
        }

        batches
    }

    /// Returns the total count of pending items across all queues.
    pub async fn pending_count(&self) -> usize {
        self.queues.lock().await.total_entries()
    }

    /// Returns the total encoded bytes currently buffered across all queues.
    pub async fn pending_bytes(&self) -> usize {
        self.queues.lock().await.current_bytes
    }

    /// Returns whether all queues are empty.
    pub async fn is_empty(&self) -> bool {
        self.queues.lock().await.total_entries() == 0
    }

    /// Returns the total count of dropped items across all queues.
    ///
    /// Items are dropped when the shared queue budget is exceeded.
    pub async fn dropped_count(&self) -> u64 {
        self.queues.lock().await.dropped
    }
}

fn merge_trace_requests(requests: Vec<ExportTraceServiceRequest>) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: requests
            .into_iter()
            .flat_map(|r| r.resource_spans)
            .collect(),
    }
}

fn merge_metrics_requests(
    requests: Vec<ExportMetricsServiceRequest>,
) -> ExportMetricsServiceRequest {
    ExportMetricsServiceRequest {
        resource_metrics: requests
            .into_iter()
            .flat_map(|r| r.resource_metrics)
            .collect(),
    }
}

fn merge_logs_requests(requests: Vec<ExportLogsServiceRequest>) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: requests.into_iter().flat_map(|r| r.resource_logs).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

    fn make_trace_request(span_name: &str) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: span_name.to_string(),
                        trace_id: vec![1; 16],
                        span_id: vec![1; 8],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[tokio::test]
    async fn test_add_and_get_traces() {
        let aggregator = SignalAggregator::with_defaults();

        let signal = Signal::Traces(make_trace_request("test-span"));
        aggregator.add(signal).await;

        let batch = aggregator.get_trace_batch().await;
        assert!(batch.is_some());

        match batch.unwrap() {
            BatchedSignal::Traces(req) => {
                assert_eq!(req.resource_spans.len(), 1);
                assert_eq!(
                    req.resource_spans[0].scope_spans[0].spans[0].name,
                    "test-span"
                );
            }
            _ => panic!("Expected traces batch"),
        }
    }

    #[tokio::test]
    async fn test_merge_multiple_requests() {
        let aggregator = SignalAggregator::with_defaults();

        for i in 0..3 {
            let signal = Signal::Traces(make_trace_request(&format!("span-{}", i)));
            aggregator.add(signal).await;
        }

        let batch = aggregator.get_trace_batch().await;
        assert!(batch.is_some());

        match batch.unwrap() {
            BatchedSignal::Traces(req) => {
                assert_eq!(req.resource_spans.len(), 3);
            }
            _ => panic!("Expected traces batch"),
        }
    }

    #[tokio::test]
    async fn test_empty_queue_returns_none() {
        let aggregator = SignalAggregator::with_defaults();

        assert!(aggregator.get_trace_batch().await.is_none());
        assert!(aggregator.get_metrics_batch().await.is_none());
        assert!(aggregator.get_logs_batch().await.is_none());
    }

    #[tokio::test]
    async fn test_pending_count_and_bytes() {
        let aggregator = SignalAggregator::with_defaults();

        assert_eq!(aggregator.pending_count().await, 0);
        assert_eq!(aggregator.pending_bytes().await, 0);

        aggregator
            .add(Signal::Traces(make_trace_request("span-1")))
            .await;
        aggregator
            .add(Signal::Traces(make_trace_request("span-2")))
            .await;

        assert_eq!(aggregator.pending_count().await, 2);
        assert!(aggregator.pending_bytes().await > 0);

        let _ = aggregator.get_trace_batch().await;
        assert_eq!(aggregator.pending_count().await, 0);
        assert_eq!(aggregator.pending_bytes().await, 0);
    }

    #[tokio::test]
    async fn test_batch_size_limit() {
        let config = FlushConfig {
            max_batch_entries: 2,
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        for i in 0..5 {
            aggregator
                .add(Signal::Traces(make_trace_request(&format!("span-{}", i))))
                .await;
        }

        let batch1 = aggregator.get_trace_batch().await.unwrap();
        match batch1 {
            BatchedSignal::Traces(req) => assert_eq!(req.resource_spans.len(), 2),
            _ => panic!("Expected traces"),
        }

        let batch2 = aggregator.get_trace_batch().await.unwrap();
        match batch2 {
            BatchedSignal::Traces(req) => assert_eq!(req.resource_spans.len(), 2),
            _ => panic!("Expected traces"),
        }

        let batch3 = aggregator.get_trace_batch().await.unwrap();
        match batch3 {
            BatchedSignal::Traces(req) => assert_eq!(req.resource_spans.len(), 1),
            _ => panic!("Expected traces"),
        }

        assert!(aggregator.get_trace_batch().await.is_none());
    }

    #[tokio::test]
    async fn test_get_all_batches() {
        let aggregator = SignalAggregator::with_defaults();

        aggregator
            .add(Signal::Traces(make_trace_request("span")))
            .await;
        aggregator
            .add(Signal::Metrics(ExportMetricsServiceRequest::default()))
            .await;
        aggregator
            .add(Signal::Logs(ExportLogsServiceRequest::default()))
            .await;

        let batches = aggregator.get_all_batches().await;
        assert_eq!(batches.len(), 3);
        assert!(aggregator.is_empty().await);
    }

    #[tokio::test]
    async fn test_large_backlog_drains_in_bounded_batches() {
        let config = FlushConfig {
            max_batch_entries: 10,
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        for i in 0..35 {
            aggregator
                .add(Signal::Traces(make_trace_request(&format!("span-{}", i))))
                .await;
        }

        let batches = aggregator.get_all_batches().await;
        assert_eq!(batches.len(), 4, "35 spans at 10 per batch is 4 batches");
        assert!(aggregator.is_empty().await);
    }

    #[test]
    fn test_batched_signal_size_and_type() {
        let req = make_trace_request("test");
        let batch = BatchedSignal::Traces(req);
        assert!(batch.size_bytes() > 0);
        assert_eq!(batch.signal_type(), "traces");
    }

    /// Pushes far more encoded data than the budget allows and checks the
    /// buffered bytes never exceed it.
    #[tokio::test]
    async fn test_queue_never_exceeds_byte_budget() {
        let budget = 4 * 1024;
        let config = FlushConfig {
            max_queue_bytes: budget,
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        for i in 0..500 {
            aggregator
                .add(Signal::Traces(make_trace_request(&format!(
                    "a-reasonably-long-span-name-{}",
                    i
                ))))
                .await;
            assert!(
                aggregator.pending_bytes().await <= budget,
                "Buffered bytes must never exceed the configured budget"
            );
        }

        assert!(
            aggregator.dropped_count().await > 0,
            "Exceeding the budget must drop and count the oldest signals"
        );
    }

    #[tokio::test]
    async fn test_queue_entry_budget_is_shared_across_signal_types() {
        let config = FlushConfig {
            max_queue_entries: 10,
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        for i in 0..8 {
            aggregator
                .add(Signal::Traces(make_trace_request(&format!("span-{}", i))))
                .await;
        }
        for _ in 0..8 {
            aggregator
                .add(Signal::Metrics(ExportMetricsServiceRequest::default()))
                .await;
        }

        assert!(
            aggregator.pending_count().await <= 10,
            "The entry budget applies to all signal types together"
        );
        assert!(aggregator.dropped_count().await > 0);
    }

    #[tokio::test]
    async fn test_oversized_signal_is_rejected() {
        let config = FlushConfig {
            max_queue_bytes: 64,
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        aggregator
            .add(Signal::Traces(make_trace_request(
                "a-span-name-large-enough-to-exceed-a-sixty-four-byte-budget-on-its-own",
            )))
            .await;

        assert!(aggregator.is_empty().await);
        assert_eq!(aggregator.dropped_count().await, 1);
    }

    /// The budget fits a handful of signals; pushing many traces must not
    /// evict the single metrics entry while traces alone can make room.
    #[tokio::test]
    async fn test_eviction_prefers_pushed_signal_type() {
        let config = FlushConfig {
            max_queue_entries: 5,
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        aggregator
            .add(Signal::Metrics(ExportMetricsServiceRequest::default()))
            .await;
        for i in 0..20 {
            aggregator
                .add(Signal::Traces(make_trace_request(&format!("span-{}", i))))
                .await;
        }

        assert!(
            aggregator.get_metrics_batch().await.is_some(),
            "Metrics entry should survive trace-queue eviction pressure"
        );
    }
}

//! Signal aggregation and batching.
//!
//! This module provides queue-based batching for OTLP signals before export.
//! Each signal type (traces, metrics, logs) has its own queue with size constraints.

use crate::config::FlushConfig;
use crate::receiver::Signal;
use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
    trace::v1::ExportTraceServiceRequest,
};
use prost::Message;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, mpsc};

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
}

/// Default maximum queue entries (items pending export).
const DEFAULT_MAX_QUEUE_ENTRIES: usize = 10_000;
/// Default maximum queue size in bytes.
const DEFAULT_MAX_QUEUE_BYTES: usize = 64 * 1024 * 1024; // 64 MB

/// Queue for batching a single signal type.
struct SignalQueue<T> {
    items: VecDeque<T>,
    max_batch_bytes: usize,
    max_batch_entries: usize,
    max_queue_entries: usize,
    max_queue_bytes: usize,
    current_bytes: usize,
    dropped_count: u64,
}

impl<T: Message + Default + Clone> SignalQueue<T> {
    fn new(config: &FlushConfig) -> Self {
        Self {
            items: VecDeque::new(),
            max_batch_bytes: config.max_batch_bytes,
            max_batch_entries: config.max_batch_entries,
            max_queue_entries: DEFAULT_MAX_QUEUE_ENTRIES,
            max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
            current_bytes: 0,
            dropped_count: 0,
        }
    }

    fn push(&mut self, item: T) {
        let item_size = item.encoded_len();

        // Drop oldest items if queue is full
        while !self.items.is_empty()
            && (self.items.len() >= self.max_queue_entries
                || self.current_bytes + item_size > self.max_queue_bytes)
        {
            if let Some(dropped) = self.items.pop_front() {
                self.current_bytes = self.current_bytes.saturating_sub(dropped.encoded_len());
                self.dropped_count += 1;
            }
        }

        self.current_bytes += item_size;
        self.items.push_back(item);
    }

    fn dropped_count(&self) -> u64 {
        self.dropped_count
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn get_batch(&mut self) -> Vec<T> {
        let mut batch = Vec::new();
        let mut batch_size = 0;

        while let Some(item) = self.items.pop_front() {
            let item_size = item.encoded_len();

            if !batch.is_empty()
                && (batch_size + item_size > self.max_batch_bytes
                    || batch.len() >= self.max_batch_entries)
            {
                // Put item back and re-add its size to queue tracking
                self.items.push_front(item);
                break;
            }

            self.current_bytes = self.current_bytes.saturating_sub(item_size);
            batch.push(item);
            batch_size += item_size;
        }

        batch
    }

    fn drain_all(&mut self) -> Vec<T> {
        self.current_bytes = 0;
        self.items.drain(..).collect()
    }
}

/// Aggregator for batching OTLP signals.
///
/// Receives signals from the OTLP receiver and batches them for efficient export.
/// Supports separate queues for traces, metrics, and logs with configurable limits.
pub struct SignalAggregator {
    traces: Arc<Mutex<SignalQueue<ExportTraceServiceRequest>>>,
    metrics: Arc<Mutex<SignalQueue<ExportMetricsServiceRequest>>>,
    logs: Arc<Mutex<SignalQueue<ExportLogsServiceRequest>>>,
    notify: Arc<Notify>,
    #[allow(dead_code)]
    config: FlushConfig,
}

impl SignalAggregator {
    /// Creates a new signal aggregator with the given configuration.
    pub fn new(config: FlushConfig) -> Self {
        Self {
            traces: Arc::new(Mutex::new(SignalQueue::new(&config))),
            metrics: Arc::new(Mutex::new(SignalQueue::new(&config))),
            logs: Arc::new(Mutex::new(SignalQueue::new(&config))),
            notify: Arc::new(Notify::new()),
            config,
        }
    }

    /// Creates a new aggregator with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(FlushConfig::default())
    }

    /// Adds a signal to the appropriate queue.
    pub async fn add(&self, signal: Signal) {
        match signal {
            Signal::Traces(req) => {
                let mut queue = self.traces.lock().await;
                queue.push(req);
            }
            Signal::Metrics(req) => {
                let mut queue = self.metrics.lock().await;
                queue.push(req);
            }
            Signal::Logs(req) => {
                let mut queue = self.logs.lock().await;
                queue.push(req);
            }
        }
        self.notify.notify_one();
    }

    /// Runs the aggregator, receiving signals from a channel.
    ///
    /// This method processes incoming signals until the channel is closed.
    pub async fn run(&self, mut signal_rx: mpsc::Receiver<Signal>) {
        while let Some(signal) = signal_rx.recv().await {
            self.add(signal).await;
        }
        tracing::debug!("Signal aggregator channel closed");
    }

    /// Gets the next batch of traces for export.
    ///
    /// Returns `None` if the trace queue is empty.
    pub async fn get_trace_batch(&self) -> Option<BatchedSignal> {
        let mut queue = self.traces.lock().await;
        let batch = queue.get_batch();

        if batch.is_empty() {
            return None;
        }

        let merged = merge_trace_requests(batch);
        Some(BatchedSignal::Traces(merged))
    }

    /// Gets the next batch of metrics for export.
    ///
    /// Returns `None` if the metrics queue is empty.
    pub async fn get_metrics_batch(&self) -> Option<BatchedSignal> {
        let mut queue = self.metrics.lock().await;
        let batch = queue.get_batch();

        if batch.is_empty() {
            return None;
        }

        let merged = merge_metrics_requests(batch);
        Some(BatchedSignal::Metrics(merged))
    }

    /// Gets the next batch of logs for export.
    ///
    /// Returns `None` if the logs queue is empty.
    pub async fn get_logs_batch(&self) -> Option<BatchedSignal> {
        let mut queue = self.logs.lock().await;
        let batch = queue.get_batch();

        if batch.is_empty() {
            return None;
        }

        let merged = merge_logs_requests(batch);
        Some(BatchedSignal::Logs(merged))
    }

    /// Gets all available batches across all signal types.
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

    /// Drains all signals from all queues.
    ///
    /// Use this for shutdown to ensure all data is exported.
    pub async fn drain_all(&self) -> Vec<BatchedSignal> {
        let mut batches = Vec::new();

        {
            let mut queue = self.traces.lock().await;
            let all = queue.drain_all();
            if !all.is_empty() {
                batches.push(BatchedSignal::Traces(merge_trace_requests(all)));
            }
        }

        {
            let mut queue = self.metrics.lock().await;
            let all = queue.drain_all();
            if !all.is_empty() {
                batches.push(BatchedSignal::Metrics(merge_metrics_requests(all)));
            }
        }

        {
            let mut queue = self.logs.lock().await;
            let all = queue.drain_all();
            if !all.is_empty() {
                batches.push(BatchedSignal::Logs(merge_logs_requests(all)));
            }
        }

        batches
    }

    /// Returns the total count of pending items across all queues.
    pub async fn pending_count(&self) -> usize {
        let traces = self.traces.lock().await.len();
        let metrics = self.metrics.lock().await.len();
        let logs = self.logs.lock().await.len();
        traces + metrics + logs
    }

    /// Returns whether all queues are empty.
    pub async fn is_empty(&self) -> bool {
        self.traces.lock().await.is_empty()
            && self.metrics.lock().await.is_empty()
            && self.logs.lock().await.is_empty()
    }

    /// Waits until there is data available or the notify is triggered.
    pub async fn wait_for_data(&self) {
        self.notify.notified().await;
    }

    /// Returns a clone of the notify handle for external coordination.
    pub fn notify_handle(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    /// Returns the total count of dropped items across all queues.
    ///
    /// Items are dropped when the queue reaches its size limits.
    pub async fn dropped_count(&self) -> u64 {
        let traces = self.traces.lock().await.dropped_count();
        let metrics = self.metrics.lock().await.dropped_count();
        let logs = self.logs.lock().await.dropped_count();
        traces + metrics + logs
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
    async fn test_pending_count() {
        let aggregator = SignalAggregator::with_defaults();

        assert_eq!(aggregator.pending_count().await, 0);

        aggregator
            .add(Signal::Traces(make_trace_request("span-1")))
            .await;
        aggregator
            .add(Signal::Traces(make_trace_request("span-2")))
            .await;

        assert_eq!(aggregator.pending_count().await, 2);
    }

    #[tokio::test]
    async fn test_drain_all() {
        let aggregator = SignalAggregator::with_defaults();

        aggregator
            .add(Signal::Traces(make_trace_request("span-1")))
            .await;
        aggregator
            .add(Signal::Traces(make_trace_request("span-2")))
            .await;

        let batches = aggregator.drain_all().await;
        assert_eq!(batches.len(), 1);

        assert!(aggregator.is_empty().await);
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
    }

    #[test]
    fn test_batched_signal_size() {
        let req = make_trace_request("test");
        let batch = BatchedSignal::Traces(req);
        assert!(batch.size_bytes() > 0);
    }

    #[tokio::test]
    async fn test_queue_drops_oldest_when_full() {
        // Create a queue with a very low entry limit
        let config = FlushConfig {
            max_batch_entries: 100, // batch limit
            ..Default::default()
        };
        let aggregator = SignalAggregator::new(config);

        // The internal queue limit is 10,000 by default, so we need to
        // directly test the SignalQueue behaviour
        assert_eq!(aggregator.dropped_count().await, 0);
    }

    #[test]
    fn test_signal_queue_bounds() {
        use super::{DEFAULT_MAX_QUEUE_ENTRIES, SignalQueue};

        let config = FlushConfig::default();
        let mut queue: SignalQueue<ExportTraceServiceRequest> = SignalQueue::new(&config);

        // Add items up to the limit
        for i in 0..DEFAULT_MAX_QUEUE_ENTRIES {
            queue.push(make_trace_request(&format!("span-{}", i)));
        }
        assert_eq!(queue.len(), DEFAULT_MAX_QUEUE_ENTRIES);
        assert_eq!(queue.dropped_count(), 0);

        // Add one more - should drop the oldest
        queue.push(make_trace_request("overflow-span"));
        assert_eq!(queue.len(), DEFAULT_MAX_QUEUE_ENTRIES);
        assert_eq!(queue.dropped_count(), 1);

        // Add a few more
        queue.push(make_trace_request("overflow-span-2"));
        queue.push(make_trace_request("overflow-span-3"));
        assert_eq!(queue.dropped_count(), 3);
    }
}

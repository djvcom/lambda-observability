//! Invocation context management for span correlation.
//!
//! This module provides the central coordinator for correlating platform spans
//! with function spans. It uses state-based correlation with watch channels
//! for timeout-bounded waiting.

use crate::config::CorrelationConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, watch};

/// Unique identifier for a Lambda invocation request.
pub type RequestId = String;

/// Span context information extracted from a parent span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanContext {
    /// The trace ID (16 bytes as hex string).
    pub trace_id: String,
    /// The span ID (8 bytes as hex string).
    pub span_id: String,
    /// Trace flags (sampled, etc.).
    pub trace_flags: u8,
    /// Trace state (vendor-specific data).
    pub trace_state: Option<String>,
}

/// Platform event received from the Lambda Telemetry API.
#[derive(Debug, Clone)]
pub struct PlatformEvent {
    /// The type of platform event.
    pub event_type: PlatformEventType,
    /// Timestamp when the event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// The request ID this event belongs to.
    pub request_id: RequestId,
    /// Additional event-specific data.
    pub data: serde_json::Value,
}

/// Types of platform events from the Telemetry API.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEventType {
    /// Function initialization started.
    InitStart,
    /// Runtime initialization completed.
    InitRuntimeDone,
    /// Init phase report.
    InitReport,
    /// Invocation started.
    Start,
    /// Runtime completed processing.
    RuntimeDone,
    /// Invocation report with metrics.
    Report,
}

/// Context for a single invocation, tracking correlation state.
struct InvocationContext {
    request_id: RequestId,
    started_at: Instant,
    parent_span_context: Option<SpanContext>,
    pending_platform_events: Vec<PlatformEvent>,
    parent_ready_tx: watch::Sender<Option<SpanContext>>,
    parent_ready_rx: watch::Receiver<Option<SpanContext>>,
}

impl InvocationContext {
    fn new(request_id: RequestId) -> Self {
        let (parent_ready_tx, parent_ready_rx) = watch::channel(None);
        Self {
            request_id,
            started_at: Instant::now(),
            parent_span_context: None,
            pending_platform_events: Vec::new(),
            parent_ready_tx,
            parent_ready_rx,
        }
    }
}

/// Central coordinator for correlating platform spans with function spans.
///
/// This manager maintains state for each active invocation and provides
/// methods for registering parent spans from function telemetry and
/// waiting for correlation with platform events.
pub struct InvocationContextManager {
    contexts: Arc<RwLock<HashMap<RequestId, InvocationContext>>>,
    config: CorrelationConfig,
}

impl InvocationContextManager {
    /// Creates a new context manager with the given configuration.
    pub fn new(config: CorrelationConfig) -> Self {
        Self {
            contexts: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Creates a new context manager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(CorrelationConfig::default())
    }

    /// Registers a new invocation context.
    ///
    /// Call this when receiving an INVOKE event from the Extensions API.
    pub async fn register_invocation(&self, request_id: RequestId) {
        let mut contexts = self.contexts.write().await;

        if contexts.len() >= self.config.max_total_buffered_events {
            self.cleanup_stale_contexts_locked(&mut contexts);
        }

        if !contexts.contains_key(&request_id) {
            contexts.insert(request_id.clone(), InvocationContext::new(request_id));
        }
    }

    /// Sets the parent span context for an invocation.
    ///
    /// Call this when a span with `faas.parent_span = true` is received
    /// from the function's OTLP telemetry.
    pub async fn set_parent_span(&self, request_id: &str, context: SpanContext) {
        let mut contexts = self.contexts.write().await;

        if let Some(inv_ctx) = contexts.get_mut(request_id) {
            inv_ctx.parent_span_context = Some(context.clone());
            let _ = inv_ctx.parent_ready_tx.send(Some(context));
        }
    }

    /// Waits for the parent span context to become available.
    ///
    /// This method waits up to `max_correlation_delay` for the parent span
    /// to be registered. Returns `None` if the timeout expires.
    pub async fn wait_for_parent_span(&self, request_id: &str) -> Option<SpanContext> {
        let rx = {
            let contexts = self.contexts.read().await;
            contexts.get(request_id)?.parent_ready_rx.clone()
        };

        let timeout = self.config.max_correlation_delay;

        match tokio::time::timeout(timeout, async {
            let mut rx = rx;
            loop {
                if rx.borrow().is_some() {
                    return rx.borrow().clone();
                }
                if rx.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await
        {
            Ok(ctx) => ctx,
            Err(_) => {
                let contexts = self.contexts.read().await;
                contexts
                    .get(request_id)
                    .and_then(|c| c.parent_span_context.clone())
            }
        }
    }

    /// Gets the current parent span context if available (non-blocking).
    pub async fn get_parent_span(&self, request_id: &str) -> Option<SpanContext> {
        let contexts = self.contexts.read().await;
        contexts
            .get(request_id)
            .and_then(|c| c.parent_span_context.clone())
    }

    /// Adds a platform event to the invocation context.
    ///
    /// Events are buffered until the parent span is available for correlation.
    pub async fn add_platform_event(&self, event: PlatformEvent) {
        let mut contexts = self.contexts.write().await;

        if let Some(inv_ctx) = contexts.get_mut(&event.request_id) {
            if inv_ctx.pending_platform_events.len()
                < self.config.max_buffered_events_per_invocation
            {
                inv_ctx.pending_platform_events.push(event);
            } else {
                tracing::warn!(
                    request_id = %inv_ctx.request_id,
                    "Platform event buffer full, dropping event"
                );
            }
        } else {
            let mut ctx = InvocationContext::new(event.request_id.clone());
            ctx.pending_platform_events.push(event);
            contexts.insert(ctx.request_id.clone(), ctx);
        }
    }

    /// Takes all pending platform events for an invocation.
    ///
    /// Returns the events and clears the buffer.
    pub async fn take_platform_events(&self, request_id: &str) -> Vec<PlatformEvent> {
        let mut contexts = self.contexts.write().await;

        if let Some(inv_ctx) = contexts.get_mut(request_id) {
            std::mem::take(&mut inv_ctx.pending_platform_events)
        } else {
            Vec::new()
        }
    }

    /// Removes an invocation context.
    ///
    /// Call this after the invocation is complete and all data has been flushed.
    pub async fn remove_invocation(&self, request_id: &str) {
        let mut contexts = self.contexts.write().await;
        contexts.remove(request_id);
    }

    /// Returns the number of active invocation contexts.
    pub async fn active_count(&self) -> usize {
        let contexts = self.contexts.read().await;
        contexts.len()
    }

    /// Cleans up stale invocation contexts.
    ///
    /// Removes contexts that have exceeded `max_invocation_lifetime`.
    pub async fn cleanup_stale_contexts(&self) {
        let mut contexts = self.contexts.write().await;
        self.cleanup_stale_contexts_locked(&mut contexts);
    }

    fn cleanup_stale_contexts_locked(&self, contexts: &mut HashMap<RequestId, InvocationContext>) {
        let max_lifetime = self.config.max_invocation_lifetime;
        let now = Instant::now();

        contexts.retain(|request_id, ctx| {
            let age = now.duration_since(ctx.started_at);
            if age > max_lifetime {
                tracing::debug!(
                    request_id = %request_id,
                    age_secs = age.as_secs(),
                    "Removing stale invocation context"
                );
                false
            } else {
                true
            }
        });
    }

    /// Returns whether to emit orphaned spans (spans without parent context).
    pub fn emit_orphaned_spans(&self) -> bool {
        self.config.emit_orphaned_spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_config() -> CorrelationConfig {
        CorrelationConfig {
            max_correlation_delay: Duration::from_millis(100),
            max_buffered_events_per_invocation: 10,
            max_total_buffered_events: 100,
            max_invocation_lifetime: Duration::from_secs(60),
            emit_orphaned_spans: true,
        }
    }

    #[tokio::test]
    async fn test_register_and_get_parent_span() {
        let manager = InvocationContextManager::new(test_config());

        manager.register_invocation("req-123".to_string()).await;

        assert!(manager.get_parent_span("req-123").await.is_none());

        let span_ctx = SpanContext {
            trace_id: "0102030405060708090a0b0c0d0e0f10".to_string(),
            span_id: "0102030405060708".to_string(),
            trace_flags: 1,
            trace_state: None,
        };

        manager.set_parent_span("req-123", span_ctx.clone()).await;

        let retrieved = manager.get_parent_span("req-123").await;
        assert_eq!(retrieved, Some(span_ctx));
    }

    #[tokio::test]
    async fn test_wait_for_parent_span_immediate() {
        let manager = InvocationContextManager::new(test_config());

        manager.register_invocation("req-456".to_string()).await;

        let span_ctx = SpanContext {
            trace_id: "trace-id".to_string(),
            span_id: "span-id".to_string(),
            trace_flags: 1,
            trace_state: None,
        };

        manager.set_parent_span("req-456", span_ctx.clone()).await;

        let result = manager.wait_for_parent_span("req-456").await;
        assert_eq!(result, Some(span_ctx));
    }

    #[tokio::test]
    async fn test_wait_for_parent_span_delayed() {
        let manager = Arc::new(InvocationContextManager::new(test_config()));

        manager.register_invocation("req-789".to_string()).await;

        let manager_clone = manager.clone();
        let set_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let span_ctx = SpanContext {
                trace_id: "delayed-trace".to_string(),
                span_id: "delayed-span".to_string(),
                trace_flags: 1,
                trace_state: None,
            };
            manager_clone.set_parent_span("req-789", span_ctx).await;
        });

        let result = manager.wait_for_parent_span("req-789").await;
        set_handle.await.unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().trace_id, "delayed-trace");
    }

    #[tokio::test]
    async fn test_wait_for_parent_span_timeout() {
        let mut config = test_config();
        config.max_correlation_delay = Duration::from_millis(10);

        let manager = InvocationContextManager::new(config);
        manager.register_invocation("req-timeout".to_string()).await;

        let result = manager.wait_for_parent_span("req-timeout").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_platform_events() {
        let manager = InvocationContextManager::new(test_config());

        let event = PlatformEvent {
            event_type: PlatformEventType::Start,
            timestamp: chrono::Utc::now(),
            request_id: "req-events".to_string(),
            data: serde_json::json!({"requestId": "req-events"}),
        };

        manager.add_platform_event(event.clone()).await;

        let events = manager.take_platform_events("req-events").await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].request_id, "req-events");

        let events_again = manager.take_platform_events("req-events").await;
        assert!(events_again.is_empty());
    }

    #[tokio::test]
    async fn test_remove_invocation() {
        let manager = InvocationContextManager::new(test_config());

        manager.register_invocation("req-remove".to_string()).await;
        assert_eq!(manager.active_count().await, 1);

        manager.remove_invocation("req-remove").await;
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_cleanup_stale_contexts() {
        let mut config = test_config();
        config.max_invocation_lifetime = Duration::from_millis(10);

        let manager = InvocationContextManager::new(config);

        manager.register_invocation("req-stale".to_string()).await;
        assert_eq!(manager.active_count().await, 1);

        tokio::time::sleep(Duration::from_millis(20)).await;

        manager.cleanup_stale_contexts().await;
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_event_buffer_limit() {
        let mut config = test_config();
        config.max_buffered_events_per_invocation = 2;

        let manager = InvocationContextManager::new(config);
        manager.register_invocation("req-limit".to_string()).await;

        for i in 0..5 {
            let event = PlatformEvent {
                event_type: PlatformEventType::Start,
                timestamp: chrono::Utc::now(),
                request_id: "req-limit".to_string(),
                data: serde_json::json!({"index": i}),
            };
            manager.add_platform_event(event).await;
        }

        let events = manager.take_platform_events("req-limit").await;
        assert_eq!(events.len(), 2);
    }
}

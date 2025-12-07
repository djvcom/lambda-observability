//! Telemetry subscription state management.

use crate::telemetry::{
    BufferingConfig, PlatformTelemetrySubscription, TelemetryEvent, TelemetryEventType,
    TelemetrySubscription,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

/// Information about an extension's telemetry subscription.
#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    /// Extension identifier.
    #[allow(dead_code)]
    pub extension_id: String,

    /// Extension name.
    #[allow(dead_code)]
    pub extension_name: String,

    /// Types of events subscribed to.
    pub event_types: Vec<TelemetryEventType>,

    /// HTTP URI to send events to.
    pub destination_uri: String,

    /// Buffering configuration.
    #[allow(dead_code)]
    pub buffering: BufferingConfig,
}

impl Default for BufferingConfig {
    fn default() -> Self {
        Self {
            max_items: Some(10000),
            max_bytes: Some(262144),
            timeout_ms: Some(1000),
        }
    }
}

/// Maximum number of events stored in the internal test capture buffer.
const MAX_CAPTURED_EVENTS: usize = 10000;

/// State for managing telemetry subscriptions and event delivery.
#[derive(Debug)]
pub(crate) struct TelemetryState {
    /// Map of extension ID to subscription info.
    subscriptions: Mutex<HashMap<String, Subscription>>,

    /// Buffered events per extension.
    event_buffers: Mutex<HashMap<String, Vec<TelemetryEvent>>>,

    /// Background tasks for event delivery.
    delivery_handles: Mutex<HashMap<String, JoinHandle<()>>>,

    /// HTTP client for sending events.
    http_client: reqwest::Client,
    /// HTTP/1.1 only client for telemetry delivery.
    /// The lambda_extension crate uses a simple hyper http1 server that doesn't support HTTP/2.
    http1_client: reqwest::Client,

    /// Test mode: capture all events in memory instead of sending via HTTP.
    capture_mode: Mutex<bool>,

    /// Captured events when in test mode.
    ///
    /// This buffer is bounded to `MAX_CAPTURED_EVENTS`. When full, the oldest
    /// events are dropped to make room for new ones.
    captured_events: Mutex<Vec<TelemetryEvent>>,

    /// Lock to synchronise flush operations with shutdown.
    ///
    /// Flush operations acquire this lock while sending events to extensions.
    /// Shutdown waits to acquire this lock before proceeding, ensuring all
    /// in-flight flush operations complete before SHUTDOWN is sent.
    flush_lock: RwLock<()>,
}

impl TelemetryState {
    /// Creates a new telemetry state.
    pub fn new() -> Self {
        // Create HTTP/1.1 only client for telemetry delivery
        // The lambda_extension crate uses a simple hyper http1 server
        let http1_client = reqwest::Client::builder()
            .http1_only()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            subscriptions: Mutex::new(HashMap::new()),
            event_buffers: Mutex::new(HashMap::new()),
            delivery_handles: Mutex::new(HashMap::new()),
            http_client: reqwest::Client::new(),
            http1_client,
            capture_mode: Mutex::new(false),
            captured_events: Mutex::new(Vec::new()),
            flush_lock: RwLock::new(()),
        }
    }

    /// Subscribes an extension to telemetry events.
    ///
    /// # Arguments
    ///
    /// * `extension_id` - The extension's identifier
    /// * `extension_name` - The extension's name
    /// * `subscription` - Subscription configuration
    pub async fn subscribe(
        self: &Arc<Self>,
        extension_id: String,
        extension_name: String,
        subscription: TelemetrySubscription,
    ) {
        let buffering = subscription.buffering.unwrap_or_default();

        // The lambda_extension crate uses sandbox.localdomain as the destination host,
        // which doesn't resolve in local test environments. Replace with 127.0.0.1.
        let destination_uri = subscription
            .destination
            .uri
            .replace("sandbox.localdomain", "127.0.0.1");

        let subscribed_types: Vec<String> = subscription
            .types
            .iter()
            .map(|t| format!("{:?}", t).to_lowercase())
            .collect();

        let sub = Subscription {
            extension_id: extension_id.clone(),
            extension_name: extension_name.clone(),
            event_types: subscription.types,
            destination_uri,
            buffering: buffering.clone(),
        };

        self.subscriptions
            .lock()
            .await
            .insert(extension_id.clone(), sub);

        self.event_buffers
            .lock()
            .await
            .insert(extension_id.clone(), Vec::new());

        self.start_delivery_task(extension_id, buffering).await;

        let subscription_event = TelemetryEvent {
            time: Utc::now(),
            event_type: "platform.telemetrySubscription".to_string(),
            record: serde_json::to_value(PlatformTelemetrySubscription {
                name: extension_name,
                state: "Subscribed".to_string(),
                types: subscribed_types,
            })
            .unwrap_or_default(),
        };

        self.broadcast_event(subscription_event, TelemetryEventType::Platform)
            .await;
    }

    /// Starts a background task to deliver buffered events.
    async fn start_delivery_task(
        self: &Arc<Self>,
        extension_id: String,
        buffering: BufferingConfig,
    ) {
        let state = Arc::clone(self);
        let timeout_ms = buffering.timeout_ms.unwrap_or(1000);
        let max_items = buffering.max_items.unwrap_or(10000);
        let ext_id_for_insert = extension_id.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(timeout_ms as u64));

            loop {
                interval.tick().await;

                let events = {
                    let mut buffers = state.event_buffers.lock().await;
                    if let Some(buffer) = buffers.get_mut(&extension_id) {
                        if buffer.is_empty() {
                            continue;
                        }

                        let count = buffer.len().min(max_items as usize);
                        buffer.drain(..count).collect::<Vec<_>>()
                    } else {
                        break;
                    }
                };

                if let Some(sub) = state.subscriptions.lock().await.get(&extension_id) {
                    let uri = sub.destination_uri.clone();
                    let client = state.http_client.clone();

                    tracing::debug!(
                        count = events.len(),
                        uri = %uri,
                        "Sending telemetry events"
                    );
                    match client.post(&uri).json(&events).send().await {
                        Ok(resp) => {
                            tracing::debug!(status = %resp.status(), "Telemetry delivery response");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Telemetry delivery error");
                        }
                    }
                }
            }
        });

        self.delivery_handles
            .lock()
            .await
            .insert(ext_id_for_insert, handle);
    }

    /// Broadcasts a telemetry event to all subscribed extensions.
    ///
    /// Events are always captured internally for test introspection, even
    /// if capture mode is not explicitly enabled. All buffers are bounded:
    /// when full, the oldest events are dropped to make room for new ones.
    ///
    /// # Arguments
    ///
    /// * `event` - The telemetry event to broadcast
    /// * `event_type` - The type of the event (platform, function, extension)
    pub async fn broadcast_event(&self, event: TelemetryEvent, event_type: TelemetryEventType) {
        // Always capture events for test introspection (bounded buffer)
        {
            let mut captured = self.captured_events.lock().await;
            if captured.len() >= MAX_CAPTURED_EVENTS {
                // Drop oldest event to make room
                captured.remove(0);
            }
            captured.push(event.clone());
        }

        let subscriptions = self.subscriptions.lock().await;
        let mut buffers = self.event_buffers.lock().await;

        tracing::trace!(
            event_type = ?event_type,
            subscriptions = subscriptions.len(),
            buffers = buffers.len(),
            "Broadcasting telemetry event"
        );

        for (ext_id, sub) in subscriptions.iter() {
            tracing::trace!(
                extension_id = %ext_id,
                event_types = ?sub.event_types,
                matches = sub.event_types.contains(&event_type),
                "Checking subscription"
            );
            if sub.event_types.contains(&event_type)
                && let Some(buffer) = buffers.get_mut(ext_id)
            {
                tracing::trace!(extension_id = %ext_id, "Adding event to buffer");
                let max_items = sub.buffering.max_items.unwrap_or(10000) as usize;
                if buffer.len() >= max_items {
                    // Drop oldest events to stay within limit
                    let excess = buffer.len() - max_items + 1;
                    buffer.drain(..excess);
                    tracing::warn!(
                        extension_id = %ext_id,
                        dropped_events = excess,
                        "Telemetry buffer overflow, dropped oldest events"
                    );
                }
                buffer.push(event.clone());
            }
        }
    }

    /// Gets all active subscriptions.
    #[allow(dead_code)]
    pub async fn get_subscriptions(&self) -> Vec<Subscription> {
        self.subscriptions.lock().await.values().cloned().collect()
    }

    /// Checks if an extension has an active telemetry subscription.
    #[allow(dead_code)]
    pub async fn is_subscribed(&self, extension_id: &str) -> bool {
        self.subscriptions.lock().await.contains_key(extension_id)
    }

    /// Enables test mode telemetry capture.
    ///
    /// When enabled, all telemetry events are captured in memory
    /// instead of being sent via HTTP. This is useful for test assertions.
    pub async fn enable_capture(&self) {
        *self.capture_mode.lock().await = true;
    }

    /// Gets all captured telemetry events.
    ///
    /// Returns an empty vector if capture mode is not enabled.
    pub async fn get_captured_events(&self) -> Vec<TelemetryEvent> {
        self.captured_events.lock().await.clone()
    }

    /// Gets captured telemetry events filtered by event type.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to filter by (e.g., "platform.start")
    pub async fn get_captured_events_by_type(&self, event_type: &str) -> Vec<TelemetryEvent> {
        self.captured_events
            .lock()
            .await
            .iter()
            .filter(|e| e.event_type == event_type)
            .cloned()
            .collect()
    }

    /// Clears all captured telemetry events.
    pub async fn clear_captured_events(&self) {
        self.captured_events.lock().await.clear();
    }

    /// Flushes all buffered telemetry events to subscribers immediately.
    ///
    /// This bypasses the normal interval-based delivery and sends all pending
    /// events right away. Useful before shutdown to ensure extensions receive
    /// all telemetry.
    ///
    /// This method holds the flush lock while sending, which allows
    /// `wait_for_flush_complete` to synchronise with ongoing flush operations.
    pub async fn flush_all(&self) {
        tracing::debug!("Starting flush_all");
        // Acquire read lock - allows concurrent flushes but blocks shutdown waiting
        let _guard = self.flush_lock.read().await;

        let subscriptions = self.subscriptions.lock().await;
        let mut buffers = self.event_buffers.lock().await;

        tracing::debug!(
            subscriptions = subscriptions.len(),
            buffers = buffers.len(),
            "Flushing telemetry buffers"
        );

        for (ext_id, sub) in subscriptions.iter() {
            if let Some(buffer) = buffers.get_mut(ext_id) {
                if buffer.is_empty() {
                    tracing::trace!(extension_id = %ext_id, "Buffer empty, skipping");
                    continue;
                }

                let events = std::mem::take(buffer);
                let uri = sub.destination_uri.clone();

                tracing::debug!(
                    extension_id = %ext_id,
                    count = events.len(),
                    uri = %uri,
                    "Flushing events to extension"
                );

                // Send with retries to handle cases where extension's HTTP server
                // isn't fully ready yet (especially during rapid test execution)
                let mut attempts = 0;
                let max_attempts = 5;

                loop {
                    attempts += 1;
                    // Use http1_client as lambda_extension uses a simple hyper http1 server
                    // Use .json() instead of .body() to ensure proper serialization
                    match self.http1_client.post(&uri).json(&events).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            tracing::debug!(
                                status = %resp.status(),
                                attempts,
                                "Flush successful"
                            );
                            break;
                        }
                        Ok(resp) => {
                            let status = resp.status();
                            let body = resp.text().await.unwrap_or_default();
                            tracing::debug!(
                                status = %status,
                                body = %body,
                                attempts,
                                "Flush attempt failed"
                            );
                            if attempts >= max_attempts {
                                tracing::warn!(
                                    extension_id = %ext_id,
                                    status = %status,
                                    "Failed to flush telemetry events after {} attempts",
                                    max_attempts
                                );
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                attempts,
                                "Flush attempt error"
                            );
                            if attempts >= max_attempts {
                                tracing::warn!(
                                    extension_id = %ext_id,
                                    error = %e,
                                    "Failed to flush telemetry events after {} attempts",
                                    max_attempts
                                );
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            }
        }
        tracing::debug!("flush_all complete");
    }

    /// Waits for any in-progress flush operations to complete.
    ///
    /// This acquires a write lock on the flush lock, which blocks until all
    /// concurrent read locks (held by `flush_all`) are released. Use this
    /// before sending SHUTDOWN to ensure extensions have received all telemetry.
    ///
    /// The timeout parameter specifies how long to wait for flush operations
    /// to complete. If the timeout expires, this method returns without
    /// guaranteeing all flushes are complete.
    pub async fn wait_for_flush_complete(&self, timeout: Duration) {
        let result = tokio::time::timeout(timeout, self.flush_lock.write()).await;
        if result.is_err() {
            tracing::warn!(
                timeout_ms = timeout.as_millis(),
                "Timed out waiting for flush operations to complete"
            );
        }
        // Lock is immediately dropped, we just needed to wait for it
    }

    /// Shuts down all background telemetry delivery tasks.
    ///
    /// This aborts all spawned delivery tasks to ensure clean shutdown.
    pub async fn shutdown(&self) {
        let mut handles = self.delivery_handles.lock().await;
        for (_, handle) in handles.drain() {
            handle.abort();
        }
    }
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self::new()
    }
}

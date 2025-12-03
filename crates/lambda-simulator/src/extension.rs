//! Lambda Extensions API types and state management.
//!
//! Implements the Lambda Extensions API as documented at:
//! <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html>

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

/// Extension identifier returned upon registration.
pub type ExtensionId = String;

/// Lifecycle events that extensions can subscribe to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum EventType {
    /// Invocation event - sent when a new invocation starts.
    Invoke,

    /// Shutdown event - sent when the Lambda environment is shutting down.
    Shutdown,
}

/// Reasons for a shutdown event.
///
/// Serializes to lowercase (`spindown`, `timeout`, `failure`) per the Extensions API spec.
/// Deserializes case-insensitively to handle variations like `SPINDOWN` or `Spindown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ShutdownReason {
    /// Normal spindown of the Lambda environment.
    #[serde(rename = "spindown")]
    Spindown,

    /// Timeout occurred during execution.
    #[serde(rename = "timeout")]
    Timeout,

    /// Failure in the Lambda environment.
    #[serde(rename = "failure")]
    Failure,
}

impl<'de> Deserialize<'de> for ShutdownReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "spindown" => Ok(ShutdownReason::Spindown),
            "timeout" => Ok(ShutdownReason::Timeout),
            "failure" => Ok(ShutdownReason::Failure),
            _ => Err(serde::de::Error::unknown_variant(
                &s,
                &["spindown", "timeout", "failure"],
            )),
        }
    }
}

/// A lifecycle event sent to extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "eventType")]
pub enum LifecycleEvent {
    /// Invocation event with request ID.
    #[serde(rename = "INVOKE")]
    Invoke {
        /// Timestamp when the invocation was received.
        #[serde(rename = "deadlineMs")]
        deadline_ms: i64,

        /// Request ID for this invocation.
        #[serde(rename = "requestId")]
        request_id: String,

        /// ARN of the invoked function.
        #[serde(rename = "invokedFunctionArn")]
        invoked_function_arn: String,

        /// X-Ray tracing ID.
        #[serde(rename = "tracing")]
        tracing: TracingInfo,
    },

    /// Shutdown event with reason.
    #[serde(rename = "SHUTDOWN")]
    Shutdown {
        /// Reason for the shutdown.
        #[serde(rename = "shutdownReason")]
        shutdown_reason: ShutdownReason,

        /// Timestamp of the shutdown event.
        #[serde(rename = "deadlineMs")]
        deadline_ms: i64,
    },
}

/// X-Ray tracing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingInfo {
    /// X-Ray trace ID type (always "X-Amzn-Trace-Id" for AWS).
    #[serde(rename = "type")]
    pub trace_type: String,

    /// The actual trace ID value.
    pub value: String,
}

/// Registration request from an extension.
#[derive(Debug, Clone, Deserialize)]
pub struct RegisterRequest {
    /// Events the extension wants to receive.
    pub events: Vec<EventType>,
}

/// Information about a registered extension.
#[derive(Debug, Clone)]
pub struct RegisteredExtension {
    /// Unique identifier for this extension.
    pub id: ExtensionId,

    /// Name of the extension.
    pub name: String,

    /// Events this extension subscribed to.
    pub events: Vec<EventType>,

    /// Timestamp when the extension registered.
    pub registered_at: DateTime<Utc>,
}

impl RegisteredExtension {
    /// Creates a new registered extension.
    pub fn new(name: String, events: Vec<EventType>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            events,
            registered_at: Utc::now(),
        }
    }

    /// Checks if this extension is subscribed to a specific event type.
    pub fn is_subscribed_to(&self, event_type: &EventType) -> bool {
        self.events.contains(event_type)
    }
}

/// Shared state for extension management.
///
/// This manages all registered extensions, their event queues, and
/// coordination for event delivery.
#[derive(Debug)]
pub struct ExtensionState {
    /// Map of extension IDs to their registration information.
    extensions: Mutex<HashMap<ExtensionId, RegisteredExtension>>,

    /// Event queues for each extension (extension_id -> events).
    event_queues: Mutex<HashMap<ExtensionId, VecDeque<LifecycleEvent>>>,

    /// Notifiers for each extension to signal new events.
    event_notifiers: Mutex<HashMap<ExtensionId, std::sync::Arc<Notify>>>,

    /// Extensions that have acknowledged shutdown by polling /next after receiving SHUTDOWN.
    shutdown_acknowledged: Mutex<std::collections::HashSet<ExtensionId>>,

    /// Notifier for shutdown acknowledgment changes.
    shutdown_notify: Notify,
}

impl ExtensionState {
    /// Creates a new extension state.
    pub fn new() -> Self {
        Self {
            extensions: Mutex::new(HashMap::new()),
            event_queues: Mutex::new(HashMap::new()),
            event_notifiers: Mutex::new(HashMap::new()),
            shutdown_acknowledged: Mutex::new(std::collections::HashSet::new()),
            shutdown_notify: Notify::new(),
        }
    }

    /// Registers a new extension.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the extension (from Lambda-Extension-Name header)
    /// * `events` - List of events the extension wants to receive
    ///
    /// # Returns
    ///
    /// The registered extension information including its ID.
    pub async fn register(&self, name: String, events: Vec<EventType>) -> RegisteredExtension {
        let extension = RegisteredExtension::new(name, events);
        let id = extension.id.clone();

        self.extensions
            .lock()
            .await
            .insert(id.clone(), extension.clone());
        self.event_queues
            .lock()
            .await
            .insert(id.clone(), VecDeque::new());
        self.event_notifiers
            .lock()
            .await
            .insert(id.clone(), std::sync::Arc::new(Notify::new()));

        extension
    }

    /// Broadcasts an event to all extensions subscribed to that event type.
    ///
    /// # Arguments
    ///
    /// * `event` - The lifecycle event to broadcast
    pub async fn broadcast_event(&self, event: LifecycleEvent) {
        let event_type = match &event {
            LifecycleEvent::Invoke { .. } => EventType::Invoke,
            LifecycleEvent::Shutdown { .. } => EventType::Shutdown,
        };

        let extensions = self.extensions.lock().await;
        let mut queues = self.event_queues.lock().await;
        let notifiers = self.event_notifiers.lock().await;

        for (id, ext) in extensions.iter() {
            if ext.is_subscribed_to(&event_type) {
                if let Some(queue) = queues.get_mut(id) {
                    queue.push_back(event.clone());
                }
                if let Some(notifier) = notifiers.get(id) {
                    notifier.notify_one();
                }
            }
        }
    }

    /// Waits for and retrieves the next event for a specific extension.
    ///
    /// This is a long-poll operation that blocks until an event is available.
    ///
    /// # Arguments
    ///
    /// * `extension_id` - The ID of the extension requesting the next event
    ///
    /// # Returns
    ///
    /// The next lifecycle event for this extension.
    pub async fn next_event(&self, extension_id: &str) -> Option<LifecycleEvent> {
        loop {
            {
                let mut queues = self.event_queues.lock().await;
                if let Some(queue) = queues.get_mut(extension_id) {
                    if let Some(event) = queue.pop_front() {
                        return Some(event);
                    }
                } else {
                    return None;
                }
            }

            let notifiers = self.event_notifiers.lock().await;
            if let Some(notifier) = notifiers.get(extension_id) {
                let notifier = std::sync::Arc::clone(notifier);
                drop(notifiers);
                notifier.notified().await;
            } else {
                return None;
            }
        }
    }

    /// Gets information about a registered extension.
    ///
    /// # Arguments
    ///
    /// * `extension_id` - The ID of the extension
    ///
    /// # Returns
    ///
    /// The extension information if it exists.
    pub async fn get_extension(&self, extension_id: &str) -> Option<RegisteredExtension> {
        self.extensions.lock().await.get(extension_id).cloned()
    }

    /// Gets all registered extensions.
    pub async fn get_all_extensions(&self) -> Vec<RegisteredExtension> {
        self.extensions.lock().await.values().cloned().collect()
    }

    /// Returns the number of registered extensions.
    pub async fn extension_count(&self) -> usize {
        self.extensions.lock().await.len()
    }

    /// Returns the IDs of all extensions subscribed to INVOKE events.
    ///
    /// This is used to determine which extensions need to signal readiness
    /// after each invocation.
    pub async fn get_invoke_subscribers(&self) -> Vec<ExtensionId> {
        self.extensions
            .lock()
            .await
            .values()
            .filter(|ext| ext.is_subscribed_to(&EventType::Invoke))
            .map(|ext| ext.id.clone())
            .collect()
    }

    /// Returns the IDs of all extensions subscribed to SHUTDOWN events.
    ///
    /// This is used to determine which extensions need to receive the
    /// SHUTDOWN event during graceful shutdown.
    pub async fn get_shutdown_subscribers(&self) -> Vec<ExtensionId> {
        self.extensions
            .lock()
            .await
            .values()
            .filter(|ext| ext.is_subscribed_to(&EventType::Shutdown))
            .map(|ext| ext.id.clone())
            .collect()
    }

    /// Wakes all extensions waiting on /next.
    ///
    /// This is used during shutdown to unblock extensions waiting for events.
    pub async fn wake_all_extensions(&self) {
        let notifiers = self.event_notifiers.lock().await;
        for notifier in notifiers.values() {
            notifier.notify_one();
        }
    }

    /// Checks if an extension's event queue is empty.
    ///
    /// This is used during shutdown to determine if an extension has
    /// consumed all events (including the SHUTDOWN event).
    #[allow(dead_code)]
    pub async fn is_queue_empty(&self, extension_id: &str) -> bool {
        let queues = self.event_queues.lock().await;
        queues
            .get(extension_id)
            .is_none_or(|queue| queue.is_empty())
    }

    /// Marks an extension as having acknowledged shutdown.
    ///
    /// This is called when an extension polls `/next` after receiving the
    /// SHUTDOWN event, signaling it has completed its cleanup work.
    pub async fn mark_shutdown_acknowledged(&self, extension_id: &str) {
        self.shutdown_acknowledged
            .lock()
            .await
            .insert(extension_id.to_string());
        self.shutdown_notify.notify_waiters();
    }

    /// Checks if an extension has acknowledged shutdown.
    pub async fn is_shutdown_acknowledged(&self, extension_id: &str) -> bool {
        self.shutdown_acknowledged
            .lock()
            .await
            .contains(extension_id)
    }

    /// Waits for all specified extensions to acknowledge shutdown.
    ///
    /// Returns when all extensions have polled `/next` after receiving SHUTDOWN.
    pub async fn wait_for_shutdown_acknowledged(&self, extension_ids: &[String]) {
        loop {
            let acknowledged = self.shutdown_acknowledged.lock().await;
            if extension_ids.iter().all(|id| acknowledged.contains(id)) {
                return;
            }
            drop(acknowledged);
            self.shutdown_notify.notified().await;
        }
    }

    /// Clears shutdown acknowledgment state.
    ///
    /// Called when starting a new shutdown sequence.
    pub async fn clear_shutdown_acknowledged(&self) {
        self.shutdown_acknowledged.lock().await.clear();
    }
}

impl Default for ExtensionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_reason_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ShutdownReason::Spindown).unwrap(),
            "\"spindown\""
        );
        assert_eq!(
            serde_json::to_string(&ShutdownReason::Timeout).unwrap(),
            "\"timeout\""
        );
        assert_eq!(
            serde_json::to_string(&ShutdownReason::Failure).unwrap(),
            "\"failure\""
        );
    }

    #[test]
    fn test_shutdown_reason_deserializes_case_insensitive() {
        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"spindown\"").unwrap(),
            ShutdownReason::Spindown
        );
        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"SPINDOWN\"").unwrap(),
            ShutdownReason::Spindown
        );
        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"Spindown\"").unwrap(),
            ShutdownReason::Spindown
        );
        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"SpInDoWn\"").unwrap(),
            ShutdownReason::Spindown
        );

        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"timeout\"").unwrap(),
            ShutdownReason::Timeout
        );
        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"TIMEOUT\"").unwrap(),
            ShutdownReason::Timeout
        );

        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"failure\"").unwrap(),
            ShutdownReason::Failure
        );
        assert_eq!(
            serde_json::from_str::<ShutdownReason>("\"FAILURE\"").unwrap(),
            ShutdownReason::Failure
        );
    }

    #[test]
    fn test_shutdown_reason_deserialize_invalid() {
        let result = serde_json::from_str::<ShutdownReason>("\"invalid\"");
        assert!(result.is_err());
    }
}

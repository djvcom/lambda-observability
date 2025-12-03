//! Extension readiness tracking for Lambda lifecycle coordination.
//!
//! This module tracks when extensions have completed their post-invocation work
//! and are ready for the next invocation. This is critical for accurate lifecycle
//! simulation because Lambda waits for all extensions to be ready before:
//! - Emitting `platform.report` telemetry
//! - Freezing the process
//!
//! The "extension overhead" time is the duration between when the runtime completes
//! its response and when all extensions signal they are ready by polling `/next`.

use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// Tracks extension readiness for a specific invocation.
#[derive(Debug)]
struct InvocationReadiness {
    /// Extensions that are subscribed to INVOKE events for this invocation.
    expected_extensions: HashSet<String>,

    /// Extensions that have signalled readiness by polling /next.
    ready_extensions: HashSet<String>,

    /// Extensions that have called /next but before runtime was done.
    /// These will be promoted to ready_extensions when runtime finishes.
    pending_ready: HashSet<String>,

    /// When the runtime completed its response.
    runtime_done_at: Option<DateTime<Utc>>,

    /// When all extensions became ready.
    extensions_ready_at: Option<DateTime<Utc>>,
}

impl InvocationReadiness {
    fn new(expected_extensions: HashSet<String>) -> Self {
        Self {
            expected_extensions,
            ready_extensions: HashSet::new(),
            pending_ready: HashSet::new(),
            runtime_done_at: None,
            extensions_ready_at: None,
        }
    }

    fn all_ready(&self) -> bool {
        self.expected_extensions.is_subset(&self.ready_extensions)
    }

    fn extension_overhead_ms(&self) -> Option<f64> {
        match (self.runtime_done_at, self.extensions_ready_at) {
            (Some(done), Some(ready)) => Some((ready - done).num_milliseconds() as f64),
            _ => None,
        }
    }
}

/// Tracks extension readiness across all invocations.
///
/// This tracker coordinates between the runtime API (which knows when invocations
/// complete) and the extensions API (which knows when extensions poll /next).
///
/// # Lifecycle Flow
///
/// 1. When an invocation is enqueued, `start_invocation()` is called with the
///    list of extension IDs that subscribed to INVOKE events.
/// 2. When the runtime posts its response, `mark_runtime_done()` is called.
/// 3. Each extension has an "overhead window" to do post-invocation work.
/// 4. When an extension polls `/next`, `mark_extension_ready()` is called.
/// 5. When all extensions are ready, `wait_for_all_ready()` returns.
///
/// If no extensions are registered for INVOKE events, the invocation is
/// immediately considered ready.
#[derive(Debug)]
pub struct ExtensionReadinessTracker {
    /// Per-invocation readiness tracking.
    invocations: Mutex<HashMap<String, InvocationReadiness>>,

    /// Notifier for readiness state changes.
    readiness_changed: Notify,

    /// The currently in-progress request ID.
    /// Set when invocation starts, used to track extensions that call /next before runtime done.
    current_request: Mutex<Option<String>>,

    /// The most recently completed request ID.
    /// Used to determine if an extension polling /next is signalling readiness
    /// for a completed invocation or waiting for a new one.
    last_completed_request: Mutex<Option<String>>,
}

impl ExtensionReadinessTracker {
    /// Creates a new extension readiness tracker.
    pub fn new() -> Self {
        Self {
            invocations: Mutex::new(HashMap::new()),
            readiness_changed: Notify::new(),
            current_request: Mutex::new(None),
            last_completed_request: Mutex::new(None),
        }
    }

    /// Creates a new extension readiness tracker wrapped in an `Arc`.
    ///
    /// This is a convenience method for cases where shared ownership is needed.
    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Starts tracking a new invocation.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID of the invocation
    /// * `invoke_extension_ids` - Extension IDs that subscribed to INVOKE events
    pub async fn start_invocation(&self, request_id: &str, invoke_extension_ids: Vec<String>) {
        let expected: HashSet<String> = invoke_extension_ids.into_iter().collect();
        let readiness = InvocationReadiness::new(expected);

        self.invocations
            .lock()
            .await
            .insert(request_id.to_string(), readiness);

        *self.current_request.lock().await = Some(request_id.to_string());
    }

    /// Marks the runtime as done processing an invocation.
    ///
    /// This is called when the runtime posts its response or error. After this,
    /// the "extension overhead" window begins.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID of the completed invocation
    pub async fn mark_runtime_done(&self, request_id: &str) {
        let mut invocations = self.invocations.lock().await;
        if let Some(readiness) = invocations.get_mut(request_id) {
            readiness.runtime_done_at = Some(Utc::now());

            // Promote any extensions that called /next before runtime was done
            for ext in readiness.pending_ready.drain() {
                readiness.ready_extensions.insert(ext);
            }

            if readiness.all_ready() {
                readiness.extensions_ready_at = Some(Utc::now());
                self.readiness_changed.notify_waiters();
            }
        }

        *self.last_completed_request.lock().await = Some(request_id.to_string());
        self.readiness_changed.notify_waiters();
    }

    /// Marks an extension as ready for the next invocation.
    ///
    /// This is called when an extension polls `/next` after the runtime has
    /// completed its response. It signals that the extension has finished its
    /// post-invocation work.
    ///
    /// # Arguments
    ///
    /// * `extension_id` - The ID of the extension signalling readiness
    ///
    /// # Returns
    ///
    /// `true` if the extension was marked ready for a pending invocation,
    /// `false` if there was no pending invocation to mark ready for.
    pub async fn mark_extension_ready(&self, extension_id: &str) -> bool {
        let current_request = self.current_request.lock().await.clone();
        let last_request = self.last_completed_request.lock().await.clone();

        let mut invocations = self.invocations.lock().await;

        // First, check if there's a completed request waiting for readiness
        if let Some(ref request_id) = last_request
            && let Some(readiness) = invocations.get_mut(request_id)
            && readiness.runtime_done_at.is_some()
            && !readiness.all_ready()
        {
            readiness.ready_extensions.insert(extension_id.to_string());

            if readiness.all_ready() {
                readiness.extensions_ready_at = Some(Utc::now());
                self.readiness_changed.notify_waiters();
            }
            return true;
        }

        // If no completed request or already ready, check if there's a current
        // in-progress request where we should track this as pending ready
        if let Some(ref request_id) = current_request
            && let Some(readiness) = invocations.get_mut(request_id)
            && readiness.runtime_done_at.is_none()
            && readiness.expected_extensions.contains(extension_id)
        {
            // Runtime not done yet, but extension called /next - mark as pending
            readiness.pending_ready.insert(extension_id.to_string());
            return true;
        }

        false
    }

    /// Checks if all extensions are ready for a specific invocation.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to check
    ///
    /// # Returns
    ///
    /// `true` if all expected extensions have signalled readiness, or if no
    /// extensions were subscribed to INVOKE events.
    pub async fn is_all_ready(&self, request_id: &str) -> bool {
        let invocations = self.invocations.lock().await;
        invocations.get(request_id).is_none_or(|r| r.all_ready())
    }

    /// Waits for all extensions to be ready for a specific invocation.
    ///
    /// This method blocks until all extensions that subscribed to INVOKE events
    /// have polled `/next`, signalling they are ready for the next invocation.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to wait for
    pub async fn wait_for_all_ready(&self, request_id: &str) {
        loop {
            if self.is_all_ready(request_id).await {
                return;
            }
            self.readiness_changed.notified().await;
        }
    }

    /// Gets the extension overhead time for an invocation.
    ///
    /// The overhead is the time between when the runtime completed its response
    /// and when all extensions signalled readiness.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to get overhead for
    ///
    /// # Returns
    ///
    /// The overhead in milliseconds, or `None` if the invocation is not complete
    /// or extensions are not yet ready.
    pub async fn get_extension_overhead_ms(&self, request_id: &str) -> Option<f64> {
        let invocations = self.invocations.lock().await;
        invocations
            .get(request_id)
            .and_then(|r| r.extension_overhead_ms())
    }

    /// Gets the list of extensions that have signalled readiness for an invocation.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to check
    #[allow(dead_code)]
    pub async fn get_ready_extensions(&self, request_id: &str) -> Vec<String> {
        let invocations = self.invocations.lock().await;
        invocations
            .get(request_id)
            .map(|r| r.ready_extensions.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Gets the list of extensions still pending readiness for an invocation.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to check
    #[allow(dead_code)]
    pub async fn get_pending_extensions(&self, request_id: &str) -> Vec<String> {
        let invocations = self.invocations.lock().await;
        invocations
            .get(request_id)
            .map(|r| {
                r.expected_extensions
                    .difference(&r.ready_extensions)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Cleans up completed invocation tracking data.
    ///
    /// Call this after an invocation is fully complete to free memory.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to clean up
    pub async fn cleanup_invocation(&self, request_id: &str) {
        self.invocations.lock().await.remove(request_id);
    }

    /// Notifies waiters that readiness state has changed.
    ///
    /// This is used internally when state changes occur.
    #[allow(dead_code)]
    pub(crate) fn notify_readiness_changed(&self) {
        self.readiness_changed.notify_waiters();
    }
}

impl Default for ExtensionReadinessTracker {
    fn default() -> Self {
        Self {
            invocations: Mutex::new(HashMap::new()),
            readiness_changed: Notify::new(),
            current_request: Mutex::new(None),
            last_completed_request: Mutex::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_no_extensions_is_immediately_ready() {
        let tracker = ExtensionReadinessTracker::new();

        tracker.start_invocation("req-1", vec![]).await;
        tracker.mark_runtime_done("req-1").await;

        assert!(tracker.is_all_ready("req-1").await);
    }

    #[tokio::test]
    async fn test_wait_for_all_ready_returns_immediately_with_no_extensions() {
        let tracker = ExtensionReadinessTracker::new();

        tracker.start_invocation("req-1", vec![]).await;
        tracker.mark_runtime_done("req-1").await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), async {
            tracker.wait_for_all_ready("req-1").await;
        })
        .await;

        assert!(
            result.is_ok(),
            "wait_for_all_ready should return immediately with no extensions"
        );
    }

    #[tokio::test]
    async fn test_single_extension_readiness() {
        let tracker = ExtensionReadinessTracker::new();

        tracker
            .start_invocation("req-1", vec!["ext-1".to_string()])
            .await;
        tracker.mark_runtime_done("req-1").await;

        assert!(!tracker.is_all_ready("req-1").await);

        tracker.mark_extension_ready("ext-1").await;

        assert!(tracker.is_all_ready("req-1").await);
    }

    #[tokio::test]
    async fn test_multiple_extensions_readiness() {
        let tracker = ExtensionReadinessTracker::new();

        tracker
            .start_invocation(
                "req-1",
                vec![
                    "ext-1".to_string(),
                    "ext-2".to_string(),
                    "ext-3".to_string(),
                ],
            )
            .await;
        tracker.mark_runtime_done("req-1").await;

        assert!(!tracker.is_all_ready("req-1").await);

        tracker.mark_extension_ready("ext-1").await;
        assert!(!tracker.is_all_ready("req-1").await);

        tracker.mark_extension_ready("ext-2").await;
        assert!(!tracker.is_all_ready("req-1").await);

        tracker.mark_extension_ready("ext-3").await;
        assert!(tracker.is_all_ready("req-1").await);
    }

    #[tokio::test]
    async fn test_pending_extensions() {
        let tracker = ExtensionReadinessTracker::new();

        tracker
            .start_invocation("req-1", vec!["ext-1".to_string(), "ext-2".to_string()])
            .await;
        tracker.mark_runtime_done("req-1").await;

        let pending = tracker.get_pending_extensions("req-1").await;
        assert_eq!(pending.len(), 2);
        assert!(pending.contains(&"ext-1".to_string()));
        assert!(pending.contains(&"ext-2".to_string()));

        tracker.mark_extension_ready("ext-1").await;

        let pending = tracker.get_pending_extensions("req-1").await;
        assert_eq!(pending.len(), 1);
        assert!(pending.contains(&"ext-2".to_string()));

        let ready = tracker.get_ready_extensions("req-1").await;
        assert_eq!(ready.len(), 1);
        assert!(ready.contains(&"ext-1".to_string()));
    }

    #[tokio::test]
    async fn test_extension_overhead_calculation() {
        let tracker = ExtensionReadinessTracker::new();

        tracker
            .start_invocation("req-1", vec!["ext-1".to_string()])
            .await;
        tracker.mark_runtime_done("req-1").await;

        assert!(tracker.get_extension_overhead_ms("req-1").await.is_none());

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        tracker.mark_extension_ready("ext-1").await;

        let overhead = tracker.get_extension_overhead_ms("req-1").await;
        assert!(overhead.is_some());
        assert!(overhead.unwrap() >= 10.0);
    }

    #[tokio::test]
    async fn test_cleanup_invocation() {
        let tracker = ExtensionReadinessTracker::new();

        tracker
            .start_invocation("req-1", vec!["ext-1".to_string()])
            .await;

        assert!(!tracker.is_all_ready("req-1").await);

        tracker.cleanup_invocation("req-1").await;

        assert!(tracker.is_all_ready("req-1").await);
    }

    #[tokio::test]
    async fn test_unknown_request_is_ready() {
        let tracker = ExtensionReadinessTracker::new();

        assert!(tracker.is_all_ready("nonexistent").await);
    }
}

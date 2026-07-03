//! Invocation completion tracking.
//!
//! This module provides the coordination point between the extension's
//! lifecycle handler and the sources that know when an invocation has
//! finished. The lifecycle handler holds the `/next` poll open until
//! completion is signalled, which keeps the execution environment thawed
//! and gives the extension a guaranteed post-invocation window in which to
//! export telemetry — Lambda only freezes the environment once the runtime
//! has responded *and* every extension is parked on `/next`.
//!
//! Completion is signalled by (in order of preference):
//!
//! 1. A handler wrapper calling `POST /invocation/complete` on the OTLP
//!    receiver ([`CompletionSource::Wrapper`]).
//! 2. The `platform.runtimeDone` event from the Telemetry API
//!    ([`CompletionSource::RuntimeDone`]).
//! 3. A deadline-based fallback when neither signal arrives in time.
//!
//! The tracker records whether completion signals actually arrive. After a
//! hold times out, holding is disabled until a signal is next observed
//! within its invocation's window, so functions with no wrapper and
//! unreliable Telemetry API delivery pay the hold cost at most once per
//! degradation rather than on every invocation.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::watch;

/// Maximum number of completed request IDs remembered for duplicate and
/// early-signal detection.
const RECENTLY_COMPLETED_CAPACITY: usize = 16;

/// The source that signalled invocation completion.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionSource {
    /// A handler wrapper called `POST /invocation/complete`.
    Wrapper,
    /// The `platform.runtimeDone` telemetry event was processed.
    RuntimeDone,
}

/// The outcome of waiting for invocation completion.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    /// A completion signal arrived before the hold deadline.
    Completed(CompletionSource),
    /// No completion signal arrived before the hold deadline.
    DeadlineExpired,
}

struct CurrentInvocation {
    request_id: String,
    hold_deadline: Instant,
    tx: watch::Sender<Option<CompletionSource>>,
}

struct TrackerState {
    current: Option<CurrentInvocation>,
    recently_completed: VecDeque<(String, CompletionSource)>,
    consecutive_hold_timeouts: u32,
}

impl TrackerState {
    fn remember_completed(&mut self, request_id: String, source: CompletionSource) {
        if self
            .recently_completed
            .iter()
            .any(|(id, _)| id == &request_id)
        {
            return;
        }
        if self.recently_completed.len() >= RECENTLY_COMPLETED_CAPACITY {
            self.recently_completed.pop_front();
        }
        self.recently_completed.push_back((request_id, source));
    }
}

/// Tracks the in-flight invocation and its completion signals.
///
/// Lambda guarantees a single in-flight invocation per execution
/// environment, so the tracker holds one current invocation slot plus a
/// small memory of recently completed request IDs to absorb signals that
/// race ahead of the INVOKE event or arrive twice.
pub struct CompletionTracker {
    state: Mutex<TrackerState>,
}

impl CompletionTracker {
    /// Creates a new tracker with no invocation in flight.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TrackerState {
                current: None,
                recently_completed: VecDeque::new(),
                consecutive_hold_timeouts: 0,
            }),
        }
    }

    /// Begins tracking an invocation.
    ///
    /// Call this when the INVOKE event is received. `hold_deadline` is the
    /// latest instant the caller is prepared to wait for completion; it is
    /// also used to judge whether later signals arrived within their
    /// invocation's window when updating signal health.
    ///
    /// If a completion signal for this request ID already arrived (a
    /// wrapper can beat the INVOKE event for sub-millisecond handlers), the
    /// invocation starts already complete.
    pub fn begin(&self, request_id: impl Into<String>, hold_deadline: Instant) {
        let request_id = request_id.into();
        let mut state = self.state.lock().expect("completion tracker poisoned");

        let already_completed = state
            .recently_completed
            .iter()
            .find(|(id, _)| id == &request_id)
            .map(|(_, source)| *source);

        let (tx, _rx) = watch::channel(already_completed);
        state.current = Some(CurrentInvocation {
            request_id,
            hold_deadline,
            tx,
        });
    }

    /// Records a completion signal.
    ///
    /// A `request_id` of `None` (a wrapper POST without the request ID
    /// header) completes the current invocation. A request ID that matches
    /// neither the current invocation nor a recently completed one is
    /// remembered so a subsequent [`begin`](Self::begin) for it starts
    /// already complete. Duplicate signals are idempotent; the first source
    /// wins.
    pub fn complete(&self, request_id: Option<&str>, source: CompletionSource) {
        let mut state = self.state.lock().expect("completion tracker poisoned");
        let now = Instant::now();

        let matches_current = match (&state.current, request_id) {
            (Some(_), None) => true,
            (Some(current), Some(id)) => current.request_id == id,
            (None, _) => false,
        };

        if matches_current {
            let current = state.current.as_ref().expect("checked above");
            if current.tx.borrow().is_some() {
                tracing::trace!(?source, "Duplicate completion signal ignored");
                return;
            }
            let timely = now < current.hold_deadline;
            let request_id = current.request_id.clone();
            // send_replace updates the value even when no waiter is
            // subscribed yet, unlike send which fails without receivers.
            current.tx.send_replace(Some(source));
            if timely {
                state.consecutive_hold_timeouts = 0;
            }
            state.remember_completed(request_id, source);
            tracing::debug!(?source, timely, "Invocation completion signalled");
            return;
        }

        match request_id {
            Some(id) => {
                if state.recently_completed.iter().any(|(rid, _)| rid == id) {
                    tracing::trace!(
                        request_id = %id,
                        ?source,
                        "Completion signal for already completed invocation ignored"
                    );
                } else {
                    tracing::debug!(
                        request_id = %id,
                        ?source,
                        "Completion signal ahead of INVOKE event, remembering"
                    );
                    state.remember_completed(id.to_string(), source);
                }
            }
            None => {
                tracing::debug!(
                    ?source,
                    "Completion signal without request ID and no invocation in flight, ignoring"
                );
            }
        }
    }

    /// Waits for the current invocation to complete or its hold deadline to
    /// expire.
    ///
    /// Returns immediately if the invocation is already complete or no
    /// invocation is being tracked. A deadline expiry is recorded against
    /// signal health (see [`should_hold`](Self::should_hold)).
    pub async fn wait_for_completion(&self) -> CompletionOutcome {
        let (mut rx, deadline) = {
            let state = self.state.lock().expect("completion tracker poisoned");
            match &state.current {
                Some(current) => (current.tx.subscribe(), current.hold_deadline),
                None => {
                    tracing::warn!("wait_for_completion called with no invocation in flight");
                    return CompletionOutcome::DeadlineExpired;
                }
            }
        };

        let wait = rx.wait_for(|value| value.is_some());
        match tokio::time::timeout_at(deadline.into(), wait).await {
            Ok(Ok(value)) => {
                let source = value.expect("wait_for guarantees Some");
                let mut state = self.state.lock().expect("completion tracker poisoned");
                state.consecutive_hold_timeouts = 0;
                CompletionOutcome::Completed(source)
            }
            Ok(Err(_closed)) => {
                // The current invocation was replaced mid-wait; treat as
                // expired without penalising signal health.
                CompletionOutcome::DeadlineExpired
            }
            Err(_elapsed) => {
                let mut state = self.state.lock().expect("completion tracker poisoned");
                state.consecutive_hold_timeouts += 1;
                tracing::warn!(
                    consecutive = state.consecutive_hold_timeouts,
                    "No completion signal before hold deadline"
                );
                CompletionOutcome::DeadlineExpired
            }
        }
    }

    /// Returns whether the lifecycle handler should hold `/next` waiting
    /// for completion signals.
    ///
    /// Holding is disabled after a hold deadline expires and re-enabled
    /// once a completion signal is observed within its invocation's window,
    /// so degraded environments pay the hold cost at most once.
    pub fn should_hold(&self) -> bool {
        let state = self.state.lock().expect("completion tracker poisoned");
        state.consecutive_hold_timeouts == 0
    }
}

impl Default for CompletionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn deadline_in(duration: Duration) -> Instant {
        Instant::now() + duration
    }

    #[tokio::test]
    async fn completes_when_signal_arrives_during_wait() {
        let tracker = std::sync::Arc::new(CompletionTracker::new());
        tracker.begin("req-1", deadline_in(Duration::from_secs(5)));

        let waiter = {
            let tracker = std::sync::Arc::clone(&tracker);
            tokio::spawn(async move { tracker.wait_for_completion().await })
        };

        tokio::time::sleep(Duration::from_millis(20)).await;
        tracker.complete(Some("req-1"), CompletionSource::Wrapper);

        let outcome = waiter.await.unwrap();
        assert_eq!(
            outcome,
            CompletionOutcome::Completed(CompletionSource::Wrapper)
        );
        assert!(tracker.should_hold());
    }

    #[tokio::test]
    async fn signal_before_begin_completes_immediately() {
        let tracker = CompletionTracker::new();

        tracker.complete(Some("req-early"), CompletionSource::Wrapper);
        tracker.begin("req-early", deadline_in(Duration::from_secs(5)));

        let outcome = tracker.wait_for_completion().await;
        assert_eq!(
            outcome,
            CompletionOutcome::Completed(CompletionSource::Wrapper)
        );
    }

    #[tokio::test]
    async fn completion_without_request_id_completes_current() {
        let tracker = CompletionTracker::new();
        tracker.begin("req-2", deadline_in(Duration::from_secs(5)));

        tracker.complete(None, CompletionSource::Wrapper);

        let outcome = tracker.wait_for_completion().await;
        assert_eq!(
            outcome,
            CompletionOutcome::Completed(CompletionSource::Wrapper)
        );
    }

    #[tokio::test]
    async fn unknown_request_id_does_not_complete_current() {
        let tracker = CompletionTracker::new();
        tracker.begin("req-3", deadline_in(Duration::from_millis(50)));

        tracker.complete(Some("req-other"), CompletionSource::Wrapper);

        let outcome = tracker.wait_for_completion().await;
        assert_eq!(outcome, CompletionOutcome::DeadlineExpired);
    }

    #[tokio::test]
    async fn duplicate_signals_are_idempotent_first_source_wins() {
        let tracker = CompletionTracker::new();
        tracker.begin("req-4", deadline_in(Duration::from_secs(5)));

        tracker.complete(Some("req-4"), CompletionSource::RuntimeDone);
        tracker.complete(Some("req-4"), CompletionSource::Wrapper);

        let outcome = tracker.wait_for_completion().await;
        assert_eq!(
            outcome,
            CompletionOutcome::Completed(CompletionSource::RuntimeDone)
        );
    }

    #[tokio::test]
    async fn deadline_expiry_disables_holding() {
        let tracker = CompletionTracker::new();
        tracker.begin("req-5", deadline_in(Duration::from_millis(20)));

        let outcome = tracker.wait_for_completion().await;
        assert_eq!(outcome, CompletionOutcome::DeadlineExpired);
        assert!(
            !tracker.should_hold(),
            "Holding must be disabled after a hold timeout"
        );
    }

    #[tokio::test]
    async fn timely_signal_restores_holding() {
        let tracker = CompletionTracker::new();

        tracker.begin("req-6", deadline_in(Duration::from_millis(20)));
        assert_eq!(
            tracker.wait_for_completion().await,
            CompletionOutcome::DeadlineExpired
        );
        assert!(!tracker.should_hold());

        // A signal within the next invocation's window restores health,
        // even though the handler was not holding at the time.
        tracker.begin("req-7", deadline_in(Duration::from_secs(5)));
        tracker.complete(Some("req-7"), CompletionSource::RuntimeDone);
        assert!(tracker.should_hold());
    }

    #[tokio::test]
    async fn late_signal_does_not_restore_holding() {
        let tracker = CompletionTracker::new();

        tracker.begin("req-8", deadline_in(Duration::from_millis(20)));
        assert_eq!(
            tracker.wait_for_completion().await,
            CompletionOutcome::DeadlineExpired
        );

        // The signal arrives after req-8's window has already expired.
        tracker.complete(Some("req-8"), CompletionSource::RuntimeDone);
        assert!(
            !tracker.should_hold(),
            "A signal after its invocation's window must not restore holding"
        );
    }

    #[tokio::test]
    async fn wait_without_begin_returns_expired() {
        let tracker = CompletionTracker::new();
        let outcome = tracker.wait_for_completion().await;
        assert_eq!(outcome, CompletionOutcome::DeadlineExpired);
    }

    #[test]
    fn recently_completed_is_bounded() {
        let tracker = CompletionTracker::new();
        for i in 0..(RECENTLY_COMPLETED_CAPACITY + 10) {
            tracker.complete(Some(&format!("req-{i}")), CompletionSource::Wrapper);
        }
        let state = tracker.state.lock().unwrap();
        assert_eq!(state.recently_completed.len(), RECENTLY_COMPLETED_CAPACITY);
    }
}

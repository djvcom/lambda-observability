//! Shared state management for the Lambda runtime simulator.

use crate::invocation::{Invocation, InvocationError, InvocationResponse, InvocationStatus};
use crate::simulator::SimulatorPhase;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Notify};

/// Tracks the state of a single invocation.
#[derive(Debug, Clone)]
pub struct InvocationState {
    /// The invocation data.
    pub invocation: Invocation,

    /// Current status of the invocation.
    pub status: InvocationStatus,

    /// When the runtime started processing this invocation.
    pub started_at: Option<DateTime<Utc>>,

    /// Response if completed successfully.
    pub response: Option<InvocationResponse>,

    /// Error if failed.
    pub error: Option<InvocationError>,
}

/// Shared state for the runtime simulator.
///
/// This holds all the invocations, their states, and provides synchronization
/// primitives for coordinating between the HTTP handlers and test code.
///
/// This type is internal to the simulator and not exposed in the public API.
/// Users interact with the simulator through the `Simulator` type.
#[derive(Debug)]
pub(crate) struct RuntimeState {
    /// Queue of pending invocations waiting to be processed.
    pending_invocations: Mutex<VecDeque<Invocation>>,

    /// Map of request IDs to their current state.
    invocation_states: Mutex<HashMap<String, InvocationState>>,

    /// Notifier for when a new invocation is enqueued.
    invocation_available: Notify,

    /// Notifier for when invocation state changes.
    state_changed: Notify,

    /// Current lifecycle phase.
    phase: Mutex<SimulatorPhase>,

    /// Notifier for when phase changes.
    phase_changed: Notify,

    /// Whether an initialization error has occurred.
    init_error: Mutex<Option<String>>,

    /// When the runtime was created (for init duration tracking).
    init_started_at: DateTime<Utc>,

    /// Whether init telemetry has already been emitted.
    init_telemetry_emitted: AtomicBool,
}

impl RuntimeState {
    /// Creates a new runtime state.
    pub fn new() -> Self {
        Self {
            pending_invocations: Mutex::new(VecDeque::new()),
            invocation_states: Mutex::new(HashMap::new()),
            invocation_available: Notify::new(),
            state_changed: Notify::new(),
            phase: Mutex::new(SimulatorPhase::Initializing),
            phase_changed: Notify::new(),
            init_error: Mutex::new(None),
            init_started_at: Utc::now(),
            init_telemetry_emitted: AtomicBool::new(false),
        }
    }

    /// Creates a new runtime state wrapped in an `Arc`.
    ///
    /// This is a convenience method for cases where shared ownership is needed.
    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Gets when the runtime state was created (init start time).
    pub fn init_started_at(&self) -> DateTime<Utc> {
        self.init_started_at
    }

    /// Marks init telemetry as emitted and returns whether it was already emitted.
    ///
    /// Returns `false` if this is the first call (init telemetry should be emitted),
    /// or `true` if it was already emitted (skip emission).
    pub fn mark_init_telemetry_emitted(&self) -> bool {
        self.init_telemetry_emitted.swap(true, Ordering::SeqCst)
    }

    /// Enqueues a new invocation.
    ///
    /// # Arguments
    ///
    /// * `invocation` - The invocation to enqueue
    pub(crate) async fn enqueue_invocation(&self, invocation: Invocation) {
        let request_id = invocation.request_id.clone();

        let state = InvocationState {
            invocation: invocation.clone(),
            status: InvocationStatus::Pending,
            started_at: None,
            response: None,
            error: None,
        };

        self.invocation_states
            .lock()
            .await
            .insert(request_id, state);

        self.pending_invocations.lock().await.push_back(invocation);
        self.invocation_available.notify_one();
    }

    /// Waits for and dequeues the next invocation.
    ///
    /// This will block until an invocation is available.
    ///
    /// # Returns
    ///
    /// The next invocation to process.
    pub async fn next_invocation(&self) -> Invocation {
        loop {
            {
                let mut queue = self.pending_invocations.lock().await;
                if let Some(invocation) = queue.pop_front() {
                    if let Some(state) = self
                        .invocation_states
                        .lock()
                        .await
                        .get_mut(&invocation.request_id)
                    {
                        state.status = InvocationStatus::InProgress;
                        state.started_at = Some(Utc::now());
                    }
                    return invocation;
                }
            }

            self.invocation_available.notified().await;
        }
    }

    /// Records a successful invocation response.
    ///
    /// Only records if the invocation is still in `InProgress` status.
    /// This implements "first wins" semantics - subsequent responses are ignored.
    ///
    /// # Arguments
    ///
    /// * `response` - The invocation response
    ///
    /// # Returns
    ///
    /// Returns `true` if the response was recorded, `false` if it was already
    /// completed (response or error already recorded).
    pub async fn record_response(&self, response: InvocationResponse) -> bool {
        if let Some(state) = self
            .invocation_states
            .lock()
            .await
            .get_mut(&response.request_id)
        {
            // Only accept response if still in progress
            if state.status != InvocationStatus::InProgress {
                return false;
            }
            state.status = InvocationStatus::Success;
            state.response = Some(response);
            self.state_changed.notify_waiters();
            return true;
        }
        false
    }

    /// Records an invocation error.
    ///
    /// Only records if the invocation is still in `InProgress` status.
    /// This implements "first wins" semantics - subsequent errors are ignored.
    ///
    /// # Arguments
    ///
    /// * `error` - The invocation error
    ///
    /// # Returns
    ///
    /// Returns `true` if the error was recorded, `false` if it was already
    /// completed (response or error already recorded).
    pub async fn record_error(&self, error: InvocationError) -> bool {
        if let Some(state) = self
            .invocation_states
            .lock()
            .await
            .get_mut(&error.request_id)
        {
            // Only accept error if still in progress
            if state.status != InvocationStatus::InProgress {
                return false;
            }
            state.status = InvocationStatus::Error;
            state.error = Some(error);
            self.state_changed.notify_waiters();
            return true;
        }
        false
    }

    /// Marks the runtime as initialized and transitions to Ready phase.
    pub async fn mark_initialized(&self) {
        *self.phase.lock().await = SimulatorPhase::Ready;
        self.phase_changed.notify_waiters();
    }

    /// Marks the runtime as shutting down.
    pub async fn mark_shutting_down(&self) {
        *self.phase.lock().await = SimulatorPhase::ShuttingDown;
        self.phase_changed.notify_waiters();
    }

    /// Checks if the runtime has been initialized.
    pub async fn is_initialized(&self) -> bool {
        matches!(
            *self.phase.lock().await,
            SimulatorPhase::Ready | SimulatorPhase::ShuttingDown
        )
    }

    /// Gets the current lifecycle phase.
    pub async fn get_phase(&self) -> SimulatorPhase {
        *self.phase.lock().await
    }

    /// Waits for the simulator to reach a specific phase.
    ///
    /// # Arguments
    ///
    /// * `target_phase` - The phase to wait for
    pub(crate) async fn wait_for_phase(&self, target_phase: SimulatorPhase) {
        loop {
            if *self.phase.lock().await == target_phase {
                return;
            }
            self.phase_changed.notified().await;
        }
    }

    /// Records an initialization error.
    ///
    /// # Arguments
    ///
    /// * `error` - The error message
    pub async fn record_init_error(&self, error: String) {
        *self.init_error.lock().await = Some(error);
    }

    /// Gets the initialization error if one occurred.
    pub async fn get_init_error(&self) -> Option<String> {
        self.init_error.lock().await.clone()
    }

    /// Waits for an invocation state change notification.
    ///
    /// This method blocks until any invocation state changes (response, error, or timeout).
    /// It's used internally by wait helpers to efficiently wait for state transitions
    /// without polling.
    pub(crate) async fn wait_for_state_change(&self) {
        self.state_changed.notified().await;
    }

    /// Gets the state of an invocation by request ID.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to look up
    ///
    /// # Returns
    ///
    /// The invocation state if found.
    pub async fn get_invocation_state(&self, request_id: &str) -> Option<InvocationState> {
        self.invocation_states.lock().await.get(request_id).cloned()
    }

    /// Gets all invocation states.
    pub async fn get_all_states(&self) -> Vec<InvocationState> {
        self.invocation_states
            .lock()
            .await
            .values()
            .cloned()
            .collect()
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            pending_invocations: Mutex::new(VecDeque::new()),
            invocation_states: Mutex::new(HashMap::new()),
            invocation_available: Notify::new(),
            state_changed: Notify::new(),
            phase: Mutex::new(SimulatorPhase::Initializing),
            phase_changed: Notify::new(),
            init_error: Mutex::new(None),
            init_started_at: Utc::now(),
            init_telemetry_emitted: AtomicBool::new(false),
        }
    }
}

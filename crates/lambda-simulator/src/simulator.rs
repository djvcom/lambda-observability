//! Main simulator orchestration and builder.

use crate::error::{SimulatorError, SimulatorResult};
use crate::extension::{
    ExtensionState, LifecycleEvent, RegisteredExtension, ShutdownReason, TracingInfo,
};
use crate::extension_readiness::ExtensionReadinessTracker;
use crate::extensions_api::{ExtensionsApiState, create_extensions_api_router};
use crate::freeze::{FreezeMode, FreezeState};
use crate::invocation::Invocation;
use crate::invocation::InvocationStatus;
use crate::runtime_api::{RuntimeApiState, create_runtime_api_router};
use crate::state::{InvocationState, RuntimeState};
use crate::telemetry::{
    InitializationType, Phase, PlatformInitStart, TelemetryEvent, TelemetryEventType,
};
use crate::telemetry_api::{TelemetryApiState, create_telemetry_api_router};
use crate::telemetry_state::TelemetryState;
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The lifecycle phase of the simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatorPhase {
    /// Extensions can register, waiting for runtime to initialize.
    Initializing,
    /// Runtime is initialized, ready for invocations.
    Ready,
    /// Shutting down.
    ShuttingDown,
}

/// Configuration for the Lambda runtime simulator.
///
/// # Timeout Limitations (Phase 1)
///
/// Currently, timeout values are used only for calculating invocation deadlines
/// (the `Lambda-Runtime-Deadline-Ms` header). Actual timeout enforcement is not
/// yet implemented in Phase 1. Invocations will not be automatically terminated
/// when they exceed their configured timeout.
///
/// Full timeout enforcement will be added in Phase 4 of development.
#[derive(Debug, Clone)]
pub struct SimulatorConfig {
    /// Default timeout for invocations in milliseconds.
    ///
    /// **Note:** Used for deadline calculation only. Not enforced in Phase 1.
    pub invocation_timeout_ms: u64,

    /// Timeout for initialization phase in milliseconds.
    ///
    /// **Note:** Not enforced in Phase 1.
    pub init_timeout_ms: u64,

    /// Function name.
    pub function_name: String,

    /// Function version.
    pub function_version: String,

    /// Function memory size in MB.
    pub memory_size_mb: u32,

    /// Log group name.
    pub log_group_name: String,

    /// Log stream name.
    pub log_stream_name: String,

    /// Timeout for waiting for extensions to be ready in milliseconds.
    ///
    /// After the runtime completes an invocation, this is the maximum time
    /// to wait for all extensions to signal readiness before proceeding
    /// with platform.report emission and process freezing.
    pub extension_ready_timeout_ms: u64,

    /// Timeout for graceful shutdown in milliseconds.
    ///
    /// During graceful shutdown, this is the maximum time to wait for
    /// extensions subscribed to SHUTDOWN events to complete their cleanup
    /// work by polling `/next` again.
    pub shutdown_timeout_ms: u64,

    /// Unique instance identifier for this simulator run.
    ///
    /// Used in telemetry events to identify the Lambda execution environment.
    pub instance_id: String,

    /// Function handler name (e.g., "index.handler").
    ///
    /// Used in extension registration responses.
    pub handler: Option<String>,

    /// AWS account ID.
    ///
    /// Used in extension registration responses and ARN construction.
    pub account_id: Option<String>,

    /// AWS region.
    ///
    /// Used for environment variables and ARN construction.
    /// Defaults to "us-east-1".
    pub region: String,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            invocation_timeout_ms: 3000,
            init_timeout_ms: 10000,
            function_name: "test-function".to_string(),
            function_version: "$LATEST".to_string(),
            memory_size_mb: 128,
            log_group_name: "/aws/lambda/test-function".to_string(),
            log_stream_name: "2024/01/01/[$LATEST]test".to_string(),
            extension_ready_timeout_ms: 2000,
            shutdown_timeout_ms: 2000,
            instance_id: uuid::Uuid::new_v4().to_string(),
            handler: None,
            account_id: None,
            region: "us-east-1".to_string(),
        }
    }
}

/// Builder for creating a Lambda runtime simulator.
///
/// # Examples
///
/// ```no_run
/// use lambda_simulator::Simulator;
/// use std::time::Duration;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let simulator = Simulator::builder()
///     .invocation_timeout(Duration::from_secs(30))
///     .function_name("my-function")
///     .build()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
#[must_use = "builders do nothing unless .build() is called"]
pub struct SimulatorBuilder {
    config: SimulatorConfig,
    port: Option<u16>,
    freeze_mode: FreezeMode,
    runtime_pid: Option<u32>,
    extension_pids: Vec<u32>,
}

impl SimulatorBuilder {
    /// Creates a new simulator builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the invocation timeout.
    ///
    /// This timeout is used to calculate the `Lambda-Runtime-Deadline-Ms` header
    /// sent to the runtime. However, timeout enforcement is not yet implemented
    /// in Phase 1 - invocations will not be automatically terminated.
    pub fn invocation_timeout(mut self, timeout: Duration) -> Self {
        self.config.invocation_timeout_ms = timeout.as_millis() as u64;
        self
    }

    /// Sets the initialization timeout.
    ///
    /// **Note:** Timeout enforcement is not yet implemented in Phase 1.
    pub fn init_timeout(mut self, timeout: Duration) -> Self {
        self.config.init_timeout_ms = timeout.as_millis() as u64;
        self
    }

    /// Sets the function name.
    pub fn function_name(mut self, name: impl Into<String>) -> Self {
        self.config.function_name = name.into();
        self
    }

    /// Sets the function version.
    pub fn function_version(mut self, version: impl Into<String>) -> Self {
        self.config.function_version = version.into();
        self
    }

    /// Sets the function memory size in MB.
    pub fn memory_size_mb(mut self, memory: u32) -> Self {
        self.config.memory_size_mb = memory;
        self
    }

    /// Sets the function handler name.
    ///
    /// This is used in extension registration responses and the `_HANDLER`
    /// environment variable.
    pub fn handler(mut self, handler: impl Into<String>) -> Self {
        self.config.handler = Some(handler.into());
        self
    }

    /// Sets the AWS region.
    ///
    /// This is used for `AWS_REGION` and `AWS_DEFAULT_REGION` environment
    /// variables, as well as ARN construction.
    ///
    /// Default: "us-east-1"
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.config.region = region.into();
        self
    }

    /// Sets the AWS account ID.
    ///
    /// This is used in extension registration responses and ARN construction.
    pub fn account_id(mut self, account_id: impl Into<String>) -> Self {
        self.config.account_id = Some(account_id.into());
        self
    }

    /// Sets the port to bind to. If not specified, a random available port will be used.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Sets the timeout for waiting for extensions to be ready.
    ///
    /// After the runtime completes an invocation, the simulator will wait up to
    /// this duration for all extensions to signal readiness by polling `/next`.
    /// If the timeout expires, the simulator proceeds anyway.
    ///
    /// Default: 2 seconds
    pub fn extension_ready_timeout(mut self, timeout: Duration) -> Self {
        self.config.extension_ready_timeout_ms = timeout.as_millis() as u64;
        self
    }

    /// Sets the timeout for graceful shutdown.
    ///
    /// During graceful shutdown, extensions subscribed to SHUTDOWN events have
    /// this amount of time to complete their cleanup work and signal readiness.
    /// If the timeout expires, shutdown proceeds anyway.
    ///
    /// Default: 2 seconds
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout_ms = timeout.as_millis() as u64;
        self
    }

    /// Sets the freeze mode for process freezing simulation.
    ///
    /// When set to `FreezeMode::Process`, the simulator will send SIGSTOP/SIGCONT
    /// signals to simulate Lambda's freeze/thaw behaviour. This requires a runtime
    /// PID to be configured via `runtime_pid()`.
    ///
    /// # Platform Support
    ///
    /// Process freezing is only supported on Unix platforms. On other platforms,
    /// `FreezeMode::Process` will fail at build time.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, FreezeMode};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .freeze_mode(FreezeMode::Process)
    ///     .runtime_pid(12345)
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn freeze_mode(mut self, mode: FreezeMode) -> Self {
        self.freeze_mode = mode;
        self
    }

    /// Sets the PID of the runtime process for freeze/thaw simulation.
    ///
    /// This is required when using `FreezeMode::Process`. The simulator will
    /// send SIGSTOP to this process after each invocation response is fully
    /// sent, and SIGCONT when a new invocation is enqueued.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, FreezeMode};
    /// use std::process;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Use current process PID (for testing)
    /// let simulator = Simulator::builder()
    ///     .freeze_mode(FreezeMode::Process)
    ///     .runtime_pid(process::id())
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn runtime_pid(mut self, pid: u32) -> Self {
        self.runtime_pid = Some(pid);
        self
    }

    /// Sets the PIDs of extension processes for freeze/thaw simulation.
    ///
    /// In real AWS Lambda, the entire execution environment is frozen between
    /// invocations, including all extension processes. Use this method to
    /// register extension PIDs that should be frozen along with the runtime.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, FreezeMode};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .freeze_mode(FreezeMode::Process)
    ///     .runtime_pid(12345)
    ///     .extension_pids(vec![12346, 12347])
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn extension_pids(mut self, pids: Vec<u32>) -> Self {
        self.extension_pids = pids;
        self
    }

    /// Builds and starts the simulator.
    ///
    /// # Returns
    ///
    /// A running simulator instance.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The server fails to start or bind to the specified address.
    /// - `FreezeMode::Process` is used on a non-Unix platform.
    pub async fn build(self) -> SimulatorResult<Simulator> {
        // Note: FreezeMode::Process no longer requires runtime_pid at build time.
        // PIDs can be registered dynamically using register_freeze_pid() after
        // spawning processes. This supports the realistic workflow where the
        // simulator must be running before processes can connect to it.

        #[cfg(not(unix))]
        if self.freeze_mode == FreezeMode::Process {
            return Err(SimulatorError::InvalidConfiguration(
                "FreezeMode::Process is only supported on Unix platforms".to_string(),
            ));
        }

        let all_pids: Vec<u32> = self
            .runtime_pid
            .into_iter()
            .chain(self.extension_pids)
            .collect();
        let freeze_state = FreezeState::with_pids_shared(self.freeze_mode, all_pids);
        let runtime_state = RuntimeState::new_shared();
        let extension_state = Arc::new(ExtensionState::new());
        let telemetry_state = TelemetryState::new_shared();
        let readiness_tracker = ExtensionReadinessTracker::new_shared();
        let config = Arc::new(self.config);

        let runtime_api_state = RuntimeApiState {
            runtime: runtime_state.clone(),
            telemetry: telemetry_state.clone(),
            freeze: freeze_state.clone(),
            readiness: readiness_tracker.clone(),
            config: config.clone(),
        };
        let runtime_router = create_runtime_api_router(runtime_api_state);

        let extensions_api_state = ExtensionsApiState {
            extensions: extension_state.clone(),
            readiness: readiness_tracker.clone(),
            config: config.clone(),
            runtime: runtime_state.clone(),
        };
        let extensions_router = create_extensions_api_router(extensions_api_state);

        let telemetry_api_state = TelemetryApiState {
            telemetry: telemetry_state.clone(),
        };
        let telemetry_router = create_telemetry_api_router(telemetry_api_state);

        let combined_router = runtime_router
            .merge(extensions_router)
            .merge(telemetry_router)
            .fallback(|req: axum::extract::Request| async move {
                tracing::warn!(
                    method = %req.method(),
                    uri = %req.uri(),
                    "Unhandled request"
                );
                axum::http::StatusCode::NOT_FOUND
            });

        let addr: SocketAddr = if let Some(port) = self.port {
            ([127, 0, 0, 1], port).into()
        } else {
            ([127, 0, 0, 1], 0).into()
        };

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| SimulatorError::BindError(e.to_string()))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| SimulatorError::ServerStart(e.to_string()))?;

        let server_handle = tokio::spawn(async move {
            axum::serve(listener, combined_router)
                .await
                .map_err(|e| SimulatorError::ServerStart(e.to_string()))
        });

        // Emit platform.initStart telemetry event
        let init_start = PlatformInitStart {
            initialization_type: InitializationType::OnDemand,
            phase: Phase::Init,
            function_name: Some(config.function_name.clone()),
            function_version: Some(config.function_version.clone()),
            instance_id: Some(config.instance_id.clone()),
            instance_max_memory: Some(config.memory_size_mb),
            runtime_version: None,
            runtime_version_arn: None,
            tracing: None,
        };

        let init_start_event = TelemetryEvent {
            time: runtime_state.init_started_at(),
            event_type: "platform.initStart".to_string(),
            record: json!(init_start),
        };

        telemetry_state
            .broadcast_event(init_start_event, TelemetryEventType::Platform)
            .await;

        tracing::info!(target: "lambda_lifecycle", "");
        tracing::info!(target: "lambda_lifecycle", "═══════════════════════════════════════════════════════════");
        tracing::info!(target: "lambda_lifecycle", "  INIT PHASE");
        tracing::info!(target: "lambda_lifecycle", "═══════════════════════════════════════════════════════════");
        tracing::info!(target: "lambda_lifecycle", "📋 platform.initStart (function: {})", config.function_name);

        Ok(Simulator {
            runtime_state,
            extension_state,
            telemetry_state,
            freeze_state,
            readiness_tracker,
            config,
            addr: local_addr,
            server_handle,
        })
    }
}

/// A running Lambda runtime simulator.
///
/// The simulator provides an HTTP server that implements both the Lambda Runtime API
/// and Extensions API, allowing Lambda runtimes and extensions to be tested locally.
pub struct Simulator {
    runtime_state: Arc<RuntimeState>,
    extension_state: Arc<ExtensionState>,
    telemetry_state: Arc<TelemetryState>,
    freeze_state: Arc<FreezeState>,
    readiness_tracker: Arc<ExtensionReadinessTracker>,
    config: Arc<SimulatorConfig>,
    addr: SocketAddr,
    server_handle: JoinHandle<SimulatorResult<()>>,
}

impl Simulator {
    /// Creates a new simulator builder.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .function_name("my-function")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> SimulatorBuilder {
        SimulatorBuilder::new()
    }

    /// Returns the base URL for the Runtime API.
    ///
    /// This should be set as the `AWS_LAMBDA_RUNTIME_API` environment variable
    /// for Lambda runtimes.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// let runtime_api_url = simulator.runtime_api_url();
    /// println!("Set AWS_LAMBDA_RUNTIME_API={}", runtime_api_url);
    /// # Ok(())
    /// # }
    /// ```
    pub fn runtime_api_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Returns the socket address the simulator is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Enqueues an invocation for processing.
    ///
    /// If process freezing is enabled and the process is currently frozen,
    /// this method will unfreeze (SIGCONT) the process before enqueuing
    /// the invocation.
    ///
    /// # Arguments
    ///
    /// * `invocation` - The invocation to enqueue
    ///
    /// # Returns
    ///
    /// The request ID of the enqueued invocation.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, InvocationBuilder};
    /// use serde_json::json;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    ///
    /// let invocation = InvocationBuilder::new()
    ///     .payload(json!({"key": "value"}))
    ///     .build()?;
    ///
    /// let request_id = simulator.enqueue(invocation).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn enqueue(&self, invocation: Invocation) -> String {
        if self.freeze_state.is_frozen() {
            tracing::info!(target: "lambda_lifecycle", "🔥 Thawing environment (SIGCONT)");
        }
        if let Err(e) = self.freeze_state.unfreeze() {
            tracing::warn!("Failed to unfreeze processes before invocation: {}", e);
        }

        let request_id = invocation.request_id.clone();

        tracing::info!(target: "lambda_lifecycle", "");
        tracing::info!(target: "lambda_lifecycle", "═══════════════════════════════════════════════════════════");
        tracing::info!(target: "lambda_lifecycle", "  INVOKE PHASE");
        tracing::info!(target: "lambda_lifecycle", "═══════════════════════════════════════════════════════════");

        let invoke_subscribers = self.extension_state.get_invoke_subscribers().await;
        self.readiness_tracker
            .start_invocation(&request_id, invoke_subscribers)
            .await;

        let event = LifecycleEvent::Invoke {
            deadline_ms: invocation.deadline_ms(),
            request_id: invocation.request_id.clone(),
            invoked_function_arn: invocation.invoked_function_arn.clone(),
            tracing: TracingInfo {
                trace_type: "X-Amzn-Trace-Id".to_string(),
                value: invocation.trace_id.clone(),
            },
        };

        self.extension_state.broadcast_event(event).await;

        self.runtime_state.enqueue_invocation(invocation).await;

        request_id
    }

    /// Enqueues a simple invocation with just a payload.
    ///
    /// # Arguments
    ///
    /// * `payload` - The JSON payload for the invocation
    ///
    /// # Returns
    ///
    /// The request ID of the enqueued invocation.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use serde_json::json;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// let request_id = simulator.enqueue_payload(json!({"key": "value"})).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn enqueue_payload(&self, payload: Value) -> String {
        let invocation = Invocation::new(payload, self.config.invocation_timeout_ms);
        self.enqueue(invocation).await
    }

    /// Gets the state of a specific invocation.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to look up
    ///
    /// # Returns
    ///
    /// The invocation state if found.
    pub async fn get_invocation_state(&self, request_id: &str) -> Option<InvocationState> {
        self.runtime_state.get_invocation_state(request_id).await
    }

    /// Gets all invocation states.
    pub async fn get_all_invocation_states(&self) -> Vec<InvocationState> {
        self.runtime_state.get_all_states().await
    }

    /// Checks if the runtime has been initialized.
    pub async fn is_initialized(&self) -> bool {
        self.runtime_state.is_initialized().await
    }

    /// Gets the initialization error if one occurred.
    pub async fn get_init_error(&self) -> Option<String> {
        self.runtime_state.get_init_error().await
    }

    /// Gets all registered extensions.
    ///
    /// # Returns
    ///
    /// A list of all extensions that have registered with the simulator.
    pub async fn get_registered_extensions(&self) -> Vec<RegisteredExtension> {
        self.extension_state.get_all_extensions().await
    }

    /// Gets the number of registered extensions.
    pub async fn extension_count(&self) -> usize {
        self.extension_state.extension_count().await
    }

    /// Shuts down the simulator immediately without waiting for extensions.
    ///
    /// This will unfreeze any frozen process, abort the HTTP server,
    /// clean up background telemetry tasks, and wait for everything to finish.
    ///
    /// For graceful shutdown that allows extensions to complete cleanup work,
    /// use [`graceful_shutdown`](Self::graceful_shutdown) instead.
    pub async fn shutdown(self) {
        self.freeze_state.force_unfreeze();

        self.telemetry_state.shutdown().await;

        self.server_handle.abort();
        let _ = self.server_handle.await;
    }

    /// Gracefully shuts down the simulator, allowing extensions to complete cleanup.
    ///
    /// This method performs a graceful shutdown sequence that matches Lambda's
    /// actual behavior:
    ///
    /// 1. Unfreezes the runtime process (if frozen)
    /// 2. Transitions to `ShuttingDown` phase
    /// 3. Broadcasts a `SHUTDOWN` event to all subscribed extensions
    /// 4. Waits for extensions to acknowledge by polling `/next`
    /// 5. Cleans up resources and stops the HTTP server
    ///
    /// # Arguments
    ///
    /// * `reason` - The reason for shutdown (Spindown, Timeout, or Failure)
    ///
    /// # Extension Behavior
    ///
    /// Extensions subscribed to SHUTDOWN events will receive the event when they
    /// poll `/next`. After receiving the SHUTDOWN event, extensions should perform
    /// their cleanup work (e.g., flushing buffers, sending final telemetry batches)
    /// and then poll `/next` again to signal completion.
    ///
    /// If extensions don't complete within the configured `shutdown_timeout`,
    /// the simulator proceeds with shutdown anyway.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, ShutdownReason};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    ///
    /// // ... run invocations ...
    ///
    /// // Graceful shutdown with SPINDOWN reason
    /// simulator.graceful_shutdown(ShutdownReason::Spindown).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn graceful_shutdown(self, reason: ShutdownReason) {
        tracing::info!(target: "lambda_lifecycle", "");
        tracing::info!(target: "lambda_lifecycle", "═══════════════════════════════════════════════════════════");
        tracing::info!(target: "lambda_lifecycle", "  SHUTDOWN PHASE");
        tracing::info!(target: "lambda_lifecycle", "═══════════════════════════════════════════════════════════");
        tracing::info!(target: "lambda_lifecycle", "⏩ Fast-forwarding to shutdown (reason: {:?})", reason);

        self.freeze_state.force_unfreeze();

        self.runtime_state.mark_shutting_down().await;

        // Flush any buffered telemetry events before sending SHUTDOWN.
        // The telemetry delivery task uses an interval timer, so events like
        // platform.report may still be buffered. Flushing ensures extensions
        // receive all telemetry before they begin shutdown processing.
        self.telemetry_state.flush_all().await;

        // Wait for the flush operation to fully complete.
        // This ensures extensions have received and processed all telemetry
        // before we send the SHUTDOWN event.
        self.telemetry_state
            .wait_for_flush_complete(Duration::from_millis(100))
            .await;

        let shutdown_subscribers = self.extension_state.get_shutdown_subscribers().await;
        let has_shutdown_subscribers = !shutdown_subscribers.is_empty();

        if has_shutdown_subscribers {
            self.extension_state.clear_shutdown_acknowledged().await;

            let deadline_ms = Utc::now()
                .timestamp_millis()
                .saturating_add(self.config.shutdown_timeout_ms as i64);

            let shutdown_event = LifecycleEvent::Shutdown {
                shutdown_reason: reason,
                deadline_ms,
            };

            self.extension_state.broadcast_event(shutdown_event).await;

            let timeout = Duration::from_millis(self.config.shutdown_timeout_ms);
            let _ = tokio::time::timeout(
                timeout,
                self.extension_state
                    .wait_for_shutdown_acknowledged(&shutdown_subscribers),
            )
            .await;
        }

        self.telemetry_state.shutdown().await;

        self.server_handle.abort();
        let _ = self.server_handle.await;
    }

    /// Waits for an invocation to reach a terminal state (Success, Error, or Timeout).
    ///
    /// This method uses efficient event-driven waiting instead of polling,
    /// making tests more reliable and faster.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to wait for
    /// * `timeout` - Maximum duration to wait
    ///
    /// # Returns
    ///
    /// The final invocation state
    ///
    /// # Errors
    ///
    /// Returns `SimulatorError::Timeout` if the invocation doesn't complete within the timeout,
    /// or `SimulatorError::InvocationNotFound` if the request ID doesn't exist.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use serde_json::json;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// let request_id = simulator.enqueue_payload(json!({"test": "data"})).await;
    ///
    /// let state = simulator.wait_for_invocation_complete(&request_id, Duration::from_secs(5)).await?;
    /// assert_eq!(state.status, lambda_simulator::InvocationStatus::Success);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_invocation_complete(
        &self,
        request_id: &str,
        timeout: Duration,
    ) -> SimulatorResult<InvocationState> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if let Some(state) = self.runtime_state.get_invocation_state(request_id).await {
                match state.status {
                    InvocationStatus::Success
                    | InvocationStatus::Error
                    | InvocationStatus::Timeout => {
                        return Ok(state);
                    }
                    _ => {}
                }
            } else {
                return Err(SimulatorError::InvocationNotFound(request_id.to_string()));
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(SimulatorError::Timeout(format!(
                    "Invocation {} did not complete within {:?}",
                    request_id, timeout
                )));
            }

            tokio::select! {
                _ = self.runtime_state.wait_for_state_change() => {},
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(SimulatorError::Timeout(format!(
                        "Invocation {} did not complete within {:?}",
                        request_id, timeout
                    )));
                }
            }
        }
    }

    /// Waits for a condition to become true.
    ///
    /// This is a general-purpose helper that polls a condition function.
    /// For common conditions, use specific helpers like `wait_for_invocation_complete`.
    ///
    /// # Arguments
    ///
    /// * `condition` - Async function that returns true when the condition is met
    /// * `timeout` - Maximum duration to wait
    ///
    /// # Errors
    ///
    /// Returns `SimulatorError::Timeout` if the condition doesn't become true within the timeout.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    ///
    /// // Wait for 3 extensions to register
    /// simulator.wait_for(
    ///     || async { simulator.extension_count().await >= 3 },
    ///     Duration::from_secs(5)
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for<F, Fut>(&self, condition: F, timeout: Duration) -> SimulatorResult<()>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        let poll_interval = Duration::from_millis(10);

        loop {
            if condition().await {
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(SimulatorError::Timeout(format!(
                    "Condition did not become true within {:?}",
                    timeout
                )));
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Enables test mode telemetry capture.
    ///
    /// When enabled, all telemetry events are captured in memory,
    /// avoiding the need to set up HTTP servers in tests.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use serde_json::json;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// simulator.enable_telemetry_capture().await;
    ///
    /// simulator.enqueue_payload(json!({"test": "data"})).await;
    ///
    /// let events = simulator.get_telemetry_events().await;
    /// assert!(!events.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn enable_telemetry_capture(&self) {
        self.telemetry_state.enable_capture().await;
    }

    /// Gets all captured telemetry events.
    ///
    /// Returns an empty vector if telemetry capture is not enabled.
    pub async fn get_telemetry_events(&self) -> Vec<TelemetryEvent> {
        self.telemetry_state.get_captured_events().await
    }

    /// Gets captured telemetry events filtered by event type.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to filter by (e.g., "platform.start")
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use serde_json::json;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// simulator.enable_telemetry_capture().await;
    ///
    /// simulator.enqueue_payload(json!({"test": "data"})).await;
    ///
    /// let start_events = simulator.get_telemetry_events_by_type("platform.start").await;
    /// assert_eq!(start_events.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_telemetry_events_by_type(&self, event_type: &str) -> Vec<TelemetryEvent> {
        self.telemetry_state
            .get_captured_events_by_type(event_type)
            .await
    }

    /// Clears all captured telemetry events.
    pub async fn clear_telemetry_events(&self) {
        self.telemetry_state.clear_captured_events().await;
    }

    /// Gets the current lifecycle phase of the simulator.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, SimulatorPhase};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// assert_eq!(simulator.phase().await, SimulatorPhase::Initializing);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn phase(&self) -> SimulatorPhase {
        self.runtime_state.get_phase().await
    }

    /// Waits for the simulator to reach a specific phase.
    ///
    /// # Arguments
    ///
    /// * `target_phase` - The phase to wait for
    /// * `timeout` - Maximum duration to wait
    ///
    /// # Errors
    ///
    /// Returns `SimulatorError::Timeout` if the phase is not reached within the timeout.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, SimulatorPhase};
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    ///
    /// // Wait for runtime to initialize
    /// simulator.wait_for_phase(SimulatorPhase::Ready, Duration::from_secs(5)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_phase(
        &self,
        target_phase: SimulatorPhase,
        timeout: Duration,
    ) -> SimulatorResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;

        tokio::select! {
            _ = self.runtime_state.wait_for_phase(target_phase) => Ok(()),
            _ = tokio::time::sleep_until(deadline) => {
                Err(SimulatorError::Timeout(format!(
                    "Simulator did not reach phase {:?} within {:?}",
                    target_phase, timeout
                )))
            }
        }
    }

    /// Marks the runtime as initialized and transitions to Ready phase.
    ///
    /// This is typically called internally when the first runtime polls for an invocation,
    /// but can be called explicitly in tests.
    pub async fn mark_initialized(&self) {
        self.runtime_state.mark_initialized().await;
    }

    /// Returns whether the runtime process is currently frozen.
    ///
    /// This is useful in tests to verify freeze behaviour.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, FreezeMode};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .freeze_mode(FreezeMode::Process)
    ///     .runtime_pid(12345)
    ///     .build()
    ///     .await?;
    ///
    /// assert!(!simulator.is_frozen());
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_frozen(&self) -> bool {
        self.freeze_state.is_frozen()
    }

    /// Waits for the process to become frozen.
    ///
    /// This is useful in tests to verify freeze behaviour after sending
    /// an invocation response.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum duration to wait
    ///
    /// # Errors
    ///
    /// Returns `SimulatorError::Timeout` if the process doesn't freeze within the timeout.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, FreezeMode};
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .freeze_mode(FreezeMode::Process)
    ///     .runtime_pid(12345)
    ///     .build()
    ///     .await?;
    ///
    /// // After an invocation completes...
    /// simulator.wait_for_frozen(Duration::from_secs(5)).await?;
    /// assert!(simulator.is_frozen());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_frozen(&self, timeout: Duration) -> SimulatorResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if self.freeze_state.is_frozen() {
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(SimulatorError::Timeout(
                    "Process did not freeze within timeout".to_string(),
                ));
            }

            tokio::select! {
                _ = self.freeze_state.wait_for_state_change() => {},
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(SimulatorError::Timeout(
                        "Process did not freeze within timeout".to_string(),
                    ));
                }
            }
        }
    }

    /// Returns the current freeze mode.
    pub fn freeze_mode(&self) -> FreezeMode {
        self.freeze_state.mode()
    }

    /// Returns the current freeze epoch.
    ///
    /// The epoch increments on each unfreeze operation. This can be used
    /// in tests to verify freeze/thaw cycles.
    pub fn freeze_epoch(&self) -> u64 {
        self.freeze_state.current_epoch()
    }

    /// Registers a process ID for freeze/thaw operations.
    ///
    /// In real AWS Lambda, the entire execution environment is frozen between
    /// invocations - this includes the runtime and all extension processes.
    /// Use this method to register PIDs after spawning processes, so they
    /// will be included in freeze/thaw cycles.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::{Simulator, FreezeMode};
    /// use std::process::Command;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .freeze_mode(FreezeMode::Process)
    ///     .build()
    ///     .await?;
    ///
    /// // Spawn runtime and extension processes
    /// let runtime = Command::new("my-runtime")
    ///     .env("AWS_LAMBDA_RUNTIME_API", simulator.runtime_api_url().replace("http://", ""))
    ///     .spawn()?;
    ///
    /// let extension = Command::new("my-extension")
    ///     .env("AWS_LAMBDA_RUNTIME_API", simulator.runtime_api_url().replace("http://", ""))
    ///     .spawn()?;
    ///
    /// // Register PIDs for freezing
    /// simulator.register_freeze_pid(runtime.id());
    /// simulator.register_freeze_pid(extension.id());
    /// # Ok(())
    /// # }
    /// ```
    pub fn register_freeze_pid(&self, pid: u32) {
        self.freeze_state.register_pid(pid);
    }

    /// Waits for all extensions to signal readiness for a specific invocation.
    ///
    /// Extensions signal readiness by polling the `/next` endpoint after
    /// the runtime has submitted its response. This method blocks until
    /// all extensions subscribed to INVOKE events have signalled readiness.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to wait for
    /// * `timeout` - Maximum duration to wait
    ///
    /// # Errors
    ///
    /// Returns `SimulatorError::Timeout` if not all extensions become ready within the timeout.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use serde_json::json;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// let request_id = simulator.enqueue_payload(json!({"test": "data"})).await;
    ///
    /// // Wait for extensions to be ready
    /// simulator.wait_for_extensions_ready(&request_id, Duration::from_secs(5)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_extensions_ready(
        &self,
        request_id: &str,
        timeout: Duration,
    ) -> SimulatorResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;

        tokio::select! {
            _ = self.readiness_tracker.wait_for_all_ready(request_id) => Ok(()),
            _ = tokio::time::sleep_until(deadline) => {
                Err(SimulatorError::Timeout(format!(
                    "Extensions did not become ready for {} within {:?}",
                    request_id, timeout
                )))
            }
        }
    }

    /// Checks if all extensions are ready for a specific invocation.
    ///
    /// This is a non-blocking check that returns immediately.
    ///
    /// # Arguments
    ///
    /// * `request_id` - The request ID to check
    ///
    /// # Returns
    ///
    /// `true` if all expected extensions have signalled readiness.
    pub async fn are_extensions_ready(&self, request_id: &str) -> bool {
        self.readiness_tracker.is_all_ready(request_id).await
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
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use serde_json::json;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder().build().await?;
    /// let request_id = simulator.enqueue_payload(json!({"test": "data"})).await;
    ///
    /// // After invocation completes...
    /// if let Some(overhead_ms) = simulator.get_extension_overhead_ms(&request_id).await {
    ///     println!("Extension overhead: {:.2}ms", overhead_ms);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_extension_overhead_ms(&self, request_id: &str) -> Option<f64> {
        self.readiness_tracker
            .get_extension_overhead_ms(request_id)
            .await
    }

    /// Generates a map of standard AWS Lambda environment variables.
    ///
    /// This method creates a `HashMap` containing all the environment variables
    /// that AWS Lambda sets for function execution. These can be used when
    /// spawning a Lambda runtime process to simulate the real Lambda environment.
    ///
    /// # Environment Variables Included
    ///
    /// - `AWS_LAMBDA_FUNCTION_NAME` - Function name
    /// - `AWS_LAMBDA_FUNCTION_VERSION` - Function version
    /// - `AWS_LAMBDA_FUNCTION_MEMORY_SIZE` - Memory allocation in MB
    /// - `AWS_LAMBDA_LOG_GROUP_NAME` - CloudWatch log group name
    /// - `AWS_LAMBDA_LOG_STREAM_NAME` - CloudWatch log stream name
    /// - `AWS_LAMBDA_RUNTIME_API` - Runtime API endpoint (host:port)
    /// - `AWS_LAMBDA_INITIALIZATION_TYPE` - Always "on-demand" for simulator
    /// - `AWS_REGION` / `AWS_DEFAULT_REGION` - AWS region (extracted from config)
    /// - `AWS_EXECUTION_ENV` - Runtime identifier
    /// - `LAMBDA_TASK_ROOT` - Path to function code (/var/task)
    /// - `LAMBDA_RUNTIME_DIR` - Path to runtime libraries (/var/runtime)
    /// - `_HANDLER` - Handler identifier (if configured)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use lambda_simulator::Simulator;
    /// use std::process::Command;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let simulator = Simulator::builder()
    ///     .function_name("my-function")
    ///     .handler("bootstrap")
    ///     .build()
    ///     .await?;
    ///
    /// let env_vars = simulator.lambda_env_vars();
    ///
    /// let mut cmd = Command::new("./bootstrap");
    /// for (key, value) in &env_vars {
    ///     cmd.env(key, value);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn lambda_env_vars(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        let config = &self.config;

        env.insert(
            "AWS_LAMBDA_FUNCTION_NAME".to_string(),
            config.function_name.clone(),
        );
        env.insert(
            "AWS_LAMBDA_FUNCTION_VERSION".to_string(),
            config.function_version.clone(),
        );
        env.insert(
            "AWS_LAMBDA_FUNCTION_MEMORY_SIZE".to_string(),
            config.memory_size_mb.to_string(),
        );
        env.insert(
            "AWS_LAMBDA_LOG_GROUP_NAME".to_string(),
            config.log_group_name.clone(),
        );
        env.insert(
            "AWS_LAMBDA_LOG_STREAM_NAME".to_string(),
            config.log_stream_name.clone(),
        );

        let host_port = format!("127.0.0.1:{}", self.addr.port());
        env.insert("AWS_LAMBDA_RUNTIME_API".to_string(), host_port);

        env.insert(
            "AWS_LAMBDA_INITIALIZATION_TYPE".to_string(),
            "on-demand".to_string(),
        );

        env.insert("AWS_REGION".to_string(), config.region.clone());
        env.insert("AWS_DEFAULT_REGION".to_string(), config.region.clone());

        env.insert(
            "AWS_EXECUTION_ENV".to_string(),
            "AWS_Lambda_provided.al2".to_string(),
        );

        env.insert("LAMBDA_TASK_ROOT".to_string(), "/var/task".to_string());
        env.insert("LAMBDA_RUNTIME_DIR".to_string(), "/var/runtime".to_string());

        if let Some(handler) = &config.handler {
            env.insert("_HANDLER".to_string(), handler.clone());
        }

        if let Some(account_id) = &config.account_id {
            env.insert("AWS_ACCOUNT_ID".to_string(), account_id.clone());
        }

        env.insert("TZ".to_string(), ":UTC".to_string());
        env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        env.insert(
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin:/opt/bin".to_string(),
        );
        env.insert(
            "LD_LIBRARY_PATH".to_string(),
            "/var/lang/lib:/lib64:/usr/lib64:/var/runtime:/var/runtime/lib:/var/task:/var/task/lib:/opt/lib".to_string(),
        );

        env
    }
}

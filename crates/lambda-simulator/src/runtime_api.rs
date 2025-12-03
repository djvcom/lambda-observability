//! Lambda Runtime API HTTP endpoints implementation.
//!
//! Implements the Lambda Runtime API as documented at:
//! <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-api.html>

use crate::extension_readiness::ExtensionReadinessTracker;
use crate::freeze::FreezeState;
use crate::invocation::{InvocationError, InvocationResponse};
use crate::simulator::SimulatorConfig;
use crate::state::RuntimeState;
use crate::telemetry::{
    InitReportMetrics, InitializationType, Phase, PlatformInitReport, PlatformInitRuntimeDone,
    PlatformReport, PlatformRuntimeDone, PlatformStart, ReportMetrics, RuntimeDoneMetrics,
    RuntimeStatus, TelemetryEvent, TelemetryEventType, TraceContext,
};
use crate::telemetry_state::TelemetryState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;

/// Shared state for Runtime API endpoints.
#[derive(Clone)]
pub(crate) struct RuntimeApiState {
    pub runtime: Arc<RuntimeState>,
    pub telemetry: Arc<TelemetryState>,
    pub freeze: Arc<FreezeState>,
    pub readiness: Arc<ExtensionReadinessTracker>,
    pub config: Arc<SimulatorConfig>,
}

/// Creates the Runtime API router.
///
/// # Arguments
///
/// * `state` - Shared runtime API state
///
/// # Returns
///
/// An axum router configured with all Runtime API endpoints.
pub(crate) fn create_runtime_api_router(state: RuntimeApiState) -> Router {
    Router::new()
        .route("/2018-06-01/runtime/invocation/next", get(next_invocation))
        .route(
            "/2018-06-01/runtime/invocation/{request_id}/response",
            post(invocation_response),
        )
        .route(
            "/2018-06-01/runtime/invocation/{request_id}/error",
            post(invocation_error),
        )
        .route("/2018-06-01/runtime/init/error", post(init_error))
        .with_state(state)
}

/// Helper function to safely insert a header value.
#[allow(clippy::result_large_err)]
fn safe_header_insert(
    headers: &mut HeaderMap,
    name: &'static str,
    value: impl AsRef<str>,
) -> Result<(), Response> {
    match HeaderValue::from_str(value.as_ref()) {
        Ok(header_value) => {
            headers.insert(name, header_value);
            Ok(())
        }
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create header {}", name),
        )
            .into_response()),
    }
}

/// GET /2018-06-01/runtime/invocation/next
///
/// Retrieves the next invocation. This is a long-poll endpoint that blocks
/// until an invocation is available.
///
/// On the first call, this endpoint:
/// - Marks the runtime as initialized, ending the extension registration phase
/// - Emits `platform.initRuntimeDone` and `platform.initReport` telemetry events
///
/// Process freezing happens after all extensions signal readiness (via polling
/// their /next endpoint) following the runtime's response submission.
async fn next_invocation(State(state): State<RuntimeApiState>) -> Response {
    // Mark initialized on first call to /next - this ends the extension registration phase
    state.runtime.mark_initialized().await;

    // Emit init telemetry on first call to /next
    if !state.runtime.mark_init_telemetry_emitted() {
        let now = Utc::now();
        let init_started_at = state.runtime.init_started_at();
        let init_duration_ms = (now - init_started_at).num_milliseconds() as f64;

        // Emit platform.initRuntimeDone
        let init_runtime_done = PlatformInitRuntimeDone {
            initialization_type: InitializationType::OnDemand,
            phase: Phase::Init,
            status: RuntimeStatus::Success,
            spans: None,
            tracing: None,
        };

        let init_runtime_done_event = TelemetryEvent {
            time: now,
            event_type: "platform.initRuntimeDone".to_string(),
            record: serde_json::json!(init_runtime_done),
        };

        state
            .telemetry
            .broadcast_event(init_runtime_done_event, TelemetryEventType::Platform)
            .await;

        // Emit platform.initReport
        let init_report = PlatformInitReport {
            initialization_type: InitializationType::OnDemand,
            phase: Phase::Init,
            status: RuntimeStatus::Success,
            metrics: InitReportMetrics {
                duration_ms: init_duration_ms,
            },
            spans: None,
            tracing: None,
        };

        let init_report_event = TelemetryEvent {
            time: now,
            event_type: "platform.initReport".to_string(),
            record: serde_json::json!(init_report),
        };

        state
            .telemetry
            .broadcast_event(init_report_event, TelemetryEventType::Platform)
            .await;

        tracing::debug!("Emitted init telemetry: duration={:.2}ms", init_duration_ms);
    }

    let invocation = state.runtime.next_invocation().await;

    // Emit platform.start when the runtime receives an invocation
    let trace_context = TraceContext {
        trace_type: "X-Amzn-Trace-Id".to_string(),
        value: invocation.trace_id.clone(),
        span_id: None,
    };

    let platform_start = PlatformStart {
        request_id: invocation.aws_request_id.clone(),
        version: Some(state.config.function_version.clone()),
        tracing: Some(trace_context),
    };

    let platform_start_event = TelemetryEvent {
        time: Utc::now(),
        event_type: "platform.start".to_string(),
        record: serde_json::json!(platform_start),
    };

    state
        .telemetry
        .broadcast_event(platform_start_event, TelemetryEventType::Platform)
        .await;

    tracing::debug!(
        "Emitted platform.start for request_id={}",
        invocation.aws_request_id
    );

    let mut headers = HeaderMap::new();

    if let Err(e) = safe_header_insert(
        &mut headers,
        "Lambda-Runtime-Aws-Request-Id",
        &invocation.aws_request_id,
    ) {
        return e;
    }

    if let Err(e) = safe_header_insert(
        &mut headers,
        "Lambda-Runtime-Deadline-Ms",
        invocation.deadline_ms().to_string(),
    ) {
        return e;
    }

    if let Err(e) = safe_header_insert(
        &mut headers,
        "Lambda-Runtime-Invoked-Function-Arn",
        &invocation.invoked_function_arn,
    ) {
        return e;
    }

    if let Err(e) = safe_header_insert(
        &mut headers,
        "Lambda-Runtime-Trace-Id",
        &invocation.trace_id,
    ) {
        return e;
    }

    if let Some(client_context) = &invocation.client_context
        && let Err(e) = safe_header_insert(
            &mut headers,
            "Lambda-Runtime-Client-Context",
            client_context,
        )
    {
        return e;
    }

    if let Some(cognito_identity) = &invocation.cognito_identity
        && let Err(e) = safe_header_insert(
            &mut headers,
            "Lambda-Runtime-Cognito-Identity",
            cognito_identity,
        )
    {
        return e;
    }

    let body_str = match serde_json::to_string(&invocation.payload) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to serialize invocation payload: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to serialize invocation payload",
            )
                .into_response();
        }
    };

    (StatusCode::OK, headers, body_str).into_response()
}

/// POST /2018-06-01/runtime/invocation/:request_id/response
///
/// Reports a successful invocation response.
///
/// After recording the response and emitting `platform.runtimeDone`, this
/// spawns a background task to wait for all extensions to signal readiness
/// before emitting `platform.report`. The HTTP response is returned immediately.
///
/// Returns 404 if the request ID is not found.
/// Returns 400 if a response or error has already been recorded for this invocation.
async fn invocation_response(
    State(state): State<RuntimeApiState>,
    Path(request_id): Path<String>,
    body: String,
) -> Response {
    // Check if the invocation exists
    let inv_state = match state.runtime.get_invocation_state(&request_id).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                format!("Unknown request ID: {}", request_id),
            )
                .into_response();
        }
    };

    let payload: Value = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid JSON payload: {}", e),
            )
                .into_response();
        }
    };

    let received_at = Utc::now();
    let response = InvocationResponse {
        request_id: request_id.clone(),
        payload,
        received_at,
    };

    // Record the response - returns false if already completed
    if !state.runtime.record_response(response).await {
        return (
            StatusCode::BAD_REQUEST,
            "Response already submitted for this invocation",
        )
            .into_response();
    }

    // Proceed with telemetry emission since we successfully recorded
    {
        let duration_ms = if let Some(started_at) = inv_state.started_at {
            (received_at - started_at).num_milliseconds() as f64
        } else {
            0.0
        };

        let trace_context = TraceContext {
            trace_type: "X-Amzn-Trace-Id".to_string(),
            value: inv_state.invocation.trace_id.clone(),
            span_id: None,
        };

        let runtime_done = PlatformRuntimeDone {
            request_id: request_id.clone(),
            status: RuntimeStatus::Success,
            metrics: Some(RuntimeDoneMetrics {
                duration_ms,
                produced_bytes: None,
            }),
            spans: None,
            tracing: Some(trace_context.clone()),
        };

        let runtime_done_event = TelemetryEvent {
            time: Utc::now(),
            event_type: "platform.runtimeDone".to_string(),
            record: json!(runtime_done),
        };

        state
            .telemetry
            .broadcast_event(runtime_done_event, TelemetryEventType::Platform)
            .await;

        state.readiness.mark_runtime_done(&request_id).await;

        spawn_report_task(
            state.clone(),
            request_id.clone(),
            inv_state.invocation.created_at,
            received_at,
            RuntimeStatus::Success,
            trace_context,
        );
    }

    StatusCode::ACCEPTED.into_response()
}

/// Spawns a background task to wait for extension readiness, emit platform.report,
/// and freeze the process.
fn spawn_report_task(
    state: RuntimeApiState,
    request_id: String,
    invocation_created_at: chrono::DateTime<Utc>,
    runtime_done_at: chrono::DateTime<Utc>,
    status: RuntimeStatus,
    trace_context: TraceContext,
) {
    let timeout_ms = state.config.extension_ready_timeout_ms;
    let freeze_epoch = state.freeze.current_epoch();

    tokio::spawn(async move {
        let timeout = std::time::Duration::from_millis(timeout_ms);

        tokio::select! {
            _ = state.readiness.wait_for_all_ready(&request_id) => {
                tracing::debug!("All extensions ready for {}", request_id);
            }
            _ = tokio::time::sleep(timeout) => {
                tracing::warn!(
                    "Extension readiness timeout for {}; proceeding with report",
                    request_id
                );
            }
        }

        let extensions_ready_at = Utc::now();
        let extension_overhead_ms = state
            .readiness
            .get_extension_overhead_ms(&request_id)
            .await
            .unwrap_or_else(|| (extensions_ready_at - runtime_done_at).num_milliseconds() as f64);

        let total_duration_ms =
            (extensions_ready_at - invocation_created_at).num_milliseconds() as f64;
        let billed_duration_ms = ((total_duration_ms / 100.0).ceil() * 100.0) as u64;

        let report = PlatformReport {
            request_id: request_id.clone(),
            status,
            metrics: ReportMetrics {
                duration_ms: total_duration_ms,
                billed_duration_ms,
                memory_size_mb: state.config.memory_size_mb as u64,
                max_memory_used_mb: (state.config.memory_size_mb / 2) as u64,
                init_duration_ms: None,
                restore_duration_ms: None,
                billed_restore_duration_ms: None,
            },
            spans: None,
            tracing: Some(trace_context),
        };

        tracing::debug!(
            "Emitting platform.report for {} with extension_overhead_ms={:.2}",
            request_id,
            extension_overhead_ms
        );

        let report_event = TelemetryEvent {
            time: Utc::now(),
            event_type: "platform.report".to_string(),
            record: json!(report),
        };

        state
            .telemetry
            .broadcast_event(report_event, TelemetryEventType::Platform)
            .await;

        state.readiness.cleanup_invocation(&request_id).await;

        let _ = state.freeze.freeze_at_epoch(freeze_epoch);
    });
}

/// POST /2018-06-01/runtime/invocation/:request_id/error
///
/// Reports an invocation error.
///
/// After recording the error and emitting `platform.runtimeDone`, this
/// spawns a background task to wait for all extensions to signal readiness
/// before emitting `platform.report`. The HTTP response is returned immediately.
///
/// Returns 404 if the request ID is not found.
/// Returns 400 if a response or error has already been recorded for this invocation.
async fn invocation_error(
    State(state): State<RuntimeApiState>,
    Path(request_id): Path<String>,
    body: String,
) -> Response {
    // Parse the error payload manually since lambda_runtime doesn't send Content-Type header
    let error_payload: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)).into_response();
        }
    };
    // Check if the invocation exists
    let inv_state = match state.runtime.get_invocation_state(&request_id).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                format!("Unknown request ID: {}", request_id),
            )
                .into_response();
        }
    };

    let error_type = error_payload
        .get("errorType")
        .and_then(|v| v.as_str())
        .unwrap_or("UnknownError")
        .to_string();

    let error_message = error_payload
        .get("errorMessage")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown error")
        .to_string();

    let stack_trace = error_payload
        .get("stackTrace")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        });

    let received_at = Utc::now();
    let error = InvocationError {
        request_id: request_id.clone(),
        error_type: error_type.clone(),
        error_message,
        stack_trace,
        received_at,
    };

    // Record the error - returns false if already completed
    if !state.runtime.record_error(error).await {
        return (
            StatusCode::BAD_REQUEST,
            "Response already submitted for this invocation",
        )
            .into_response();
    }

    // Proceed with telemetry emission since we successfully recorded
    {
        let duration_ms = if let Some(started_at) = inv_state.started_at {
            (received_at - started_at).num_milliseconds() as f64
        } else {
            0.0
        };

        let trace_context = TraceContext {
            trace_type: "X-Amzn-Trace-Id".to_string(),
            value: inv_state.invocation.trace_id.clone(),
            span_id: None,
        };

        let runtime_done = PlatformRuntimeDone {
            request_id: request_id.clone(),
            status: RuntimeStatus::Error,
            metrics: Some(RuntimeDoneMetrics {
                duration_ms,
                produced_bytes: None,
            }),
            spans: None,
            tracing: Some(trace_context.clone()),
        };

        let runtime_done_event = TelemetryEvent {
            time: Utc::now(),
            event_type: "platform.runtimeDone".to_string(),
            record: json!(runtime_done),
        };

        state
            .telemetry
            .broadcast_event(runtime_done_event, TelemetryEventType::Platform)
            .await;

        state.readiness.mark_runtime_done(&request_id).await;

        spawn_report_task(
            state.clone(),
            request_id.clone(),
            inv_state.invocation.created_at,
            received_at,
            RuntimeStatus::Error,
            trace_context,
        );
    }

    StatusCode::ACCEPTED.into_response()
}

/// POST /2018-06-01/runtime/init/error
///
/// Reports an initialization error.
async fn init_error(
    State(state): State<RuntimeApiState>,
    Json(error_payload): Json<Value>,
) -> Response {
    let error_type = error_payload
        .get("errorType")
        .and_then(|v| v.as_str())
        .unwrap_or("UnknownError");

    let error_message = error_payload
        .get("errorMessage")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown error");

    let error_string = format!("{}: {}", error_type, error_message);
    state.runtime.record_init_error(error_string).await;

    StatusCode::ACCEPTED.into_response()
}

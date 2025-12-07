//! Lambda Extensions API HTTP endpoints implementation.
//!
//! Implements the Lambda Extensions API as documented at:
//! <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html>

use crate::extension::{ExtensionState, LifecycleEvent, RegisterRequest};
use crate::extension_readiness::ExtensionReadinessTracker;
use crate::simulator::SimulatorConfig;
use crate::state::RuntimeState;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::Arc;

/// Shared state for Extensions API handlers.
#[derive(Clone)]
pub(crate) struct ExtensionsApiState {
    pub extensions: Arc<ExtensionState>,
    pub readiness: Arc<ExtensionReadinessTracker>,
    pub config: Arc<SimulatorConfig>,
    pub runtime: Arc<RuntimeState>,
}

/// Creates the Extensions API router.
///
/// # Arguments
///
/// * `state` - Shared extensions API state
///
/// # Returns
///
/// An axum router configured with all Extensions API endpoints.
pub(crate) fn create_extensions_api_router(state: ExtensionsApiState) -> Router {
    Router::new()
        .route("/2020-01-01/extension/register", post(register_extension))
        .route("/2020-01-01/extension/event/next", get(next_event))
        .with_state(state)
}

/// POST /2020-01-01/extension/register
///
/// Registers an extension with the Lambda environment.
///
/// Per the Lambda Extensions API specification, extensions can only register
/// during the initialization phase. Any registration attempts after the runtime
/// has called `/next` for the first time will be rejected.
async fn register_extension(
    State(state): State<ExtensionsApiState>,
    headers: HeaderMap,
    Json(request): Json<RegisterRequest>,
) -> Response {
    if state.runtime.is_initialized().await {
        return (
            StatusCode::FORBIDDEN,
            "Extension registration is only allowed during initialization phase",
        )
            .into_response();
    }

    let extension_name = match headers.get("Lambda-Extension-Name") {
        Some(name) => match name.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    "Invalid Lambda-Extension-Name header",
                )
                    .into_response();
            }
        },
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Missing Lambda-Extension-Name header",
            )
                .into_response();
        }
    };

    let extension = state
        .extensions
        .register(extension_name.clone(), request.events.clone())
        .await;

    let events_str = request
        .events
        .iter()
        .map(|e| format!("{:?}", e))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(target: "lambda_lifecycle", "🔌 Extension registered: {} (events: {})", extension_name, events_str);

    let mut response_headers = HeaderMap::new();

    if let Ok(id) = HeaderValue::from_str(&extension.id) {
        response_headers.insert("Lambda-Extension-Identifier", id);
    }

    if let Ok(name) = HeaderValue::from_str(&state.config.function_name) {
        response_headers.insert("Lambda-Extension-Function-Name", name);
    }

    if let Ok(version) = HeaderValue::from_str(&state.config.function_version) {
        response_headers.insert("Lambda-Extension-Function-Version", version);
    }

    // The lambda_extension crate expects these fields in the response body
    let response_body = serde_json::json!({
        "functionName": state.config.function_name,
        "functionVersion": state.config.function_version,
        "handler": state.config.handler.clone().unwrap_or_else(|| "handler".to_string()),
        "accountId": state.config.account_id.clone().unwrap_or_else(|| "123456789012".to_string()),
        "logGroupName": state.config.log_group_name,
        "logStreamName": state.config.log_stream_name,
    });

    (StatusCode::OK, response_headers, Json(response_body)).into_response()
}

/// GET /2020-01-01/extension/event/next
///
/// Retrieves the next lifecycle event for an extension.
/// This is a long-poll endpoint that blocks until an event is available.
///
/// When an extension polls this endpoint, it signals that the extension has
/// completed its post-invocation work and is ready for the next event.
/// This is used to track extension readiness for the lifecycle coordination.
///
/// During shutdown, if an extension has already received the SHUTDOWN event,
/// polling this endpoint again signals that the extension has completed its
/// cleanup work (shutdown acknowledgment).
async fn next_event(State(state): State<ExtensionsApiState>, headers: HeaderMap) -> Response {
    use crate::simulator::SimulatorPhase;

    let extension_id = match headers.get("Lambda-Extension-Identifier") {
        Some(id) => match id.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    "Invalid Lambda-Extension-Identifier header",
                )
                    .into_response();
            }
        },
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Missing Lambda-Extension-Identifier header",
            )
                .into_response();
        }
    };

    match state.extensions.get_extension(&extension_id).await {
        Some(ext) => {
            state.readiness.mark_extension_ready(&extension_id).await;
            tracing::info!(target: "lambda_lifecycle", "⏳ Extension polling /next: {} (waiting)", ext.name);

            match state.extensions.next_event(&extension_id).await {
                Some(event) => {
                    let is_shutdown = matches!(event, LifecycleEvent::Shutdown { .. });
                    let is_invoke = matches!(event, LifecycleEvent::Invoke { .. });

                    if is_invoke {
                        tracing::info!(target: "lambda_lifecycle", "📨 Extension received INVOKE: {}", ext.name);
                    } else if is_shutdown {
                        tracing::info!(target: "lambda_lifecycle", "🛑 Extension received SHUTDOWN: {}", ext.name);
                        state
                            .extensions
                            .mark_shutdown_acknowledged(&extension_id)
                            .await;
                    }

                    Json(&event).into_response()
                }
                None => {
                    if state.runtime.get_phase().await == SimulatorPhase::ShuttingDown {
                        state
                            .extensions
                            .mark_shutdown_acknowledged(&extension_id)
                            .await;
                    }
                    (StatusCode::INTERNAL_SERVER_ERROR, "Extension not found").into_response()
                }
            }
        }
        None => (StatusCode::FORBIDDEN, "Extension not registered").into_response(),
    }
}

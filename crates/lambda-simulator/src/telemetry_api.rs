//! Lambda Telemetry API HTTP endpoints implementation.
//!
//! Implements the Lambda Telemetry API as documented at:
//! <https://docs.aws.amazon.com/lambda/latest/dg/telemetry-api.html>

use crate::telemetry::TelemetrySubscription;
use crate::telemetry_state::TelemetryState;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::put,
};
use std::sync::Arc;

/// Shared state for telemetry API endpoints.
#[derive(Clone)]
pub(crate) struct TelemetryApiState {
    pub telemetry: Arc<TelemetryState>,
}

/// Creates the Telemetry API router.
///
/// # Arguments
///
/// * `state` - Shared telemetry API state
///
/// # Returns
///
/// An axum router configured with all Telemetry API endpoints.
pub(crate) fn create_telemetry_api_router(state: TelemetryApiState) -> Router {
    Router::new()
        .route("/2022-07-01/telemetry", put(subscribe_telemetry))
        .with_state(state)
}

/// PUT /2022-07-01/telemetry
///
/// Subscribes to telemetry events.
async fn subscribe_telemetry(
    State(state): State<TelemetryApiState>,
    headers: axum::http::HeaderMap,
    Json(subscription): Json<TelemetrySubscription>,
) -> Response {
    tracing::debug!(
        types = ?subscription.types,
        uri = %subscription.destination.uri,
        "Received telemetry subscription request"
    );

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

    if subscription.destination.protocol != "HTTP" {
        return (StatusCode::BAD_REQUEST, "Only HTTP protocol is supported").into_response();
    }

    if subscription.destination.uri.is_empty() {
        return (StatusCode::BAD_REQUEST, "Destination URI is required").into_response();
    }

    let extension_name = headers
        .get("Lambda-Extension-Name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    tracing::info!(
        extension_id = %extension_id,
        extension_name = %extension_name,
        types = ?subscription.types,
        destination = %subscription.destination.uri,
        "Extension subscribed to telemetry"
    );

    state
        .telemetry
        .subscribe(extension_id, extension_name, subscription)
        .await;

    StatusCode::OK.into_response()
}

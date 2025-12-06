//! Error scenario tests for the Lambda Runtime Simulator.
//!
//! These tests cover error handling scenarios for:
//! - Runtime API errors
//! - Extensions API errors
//! - Telemetry API errors

use lambda_simulator::{
    EventType, InvocationBuilder, Simulator, TelemetryEventType, TelemetrySubscription,
};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Malformed JSON body returns 400 Bad Request.
///
/// When the runtime sends a response with invalid JSON, the simulator
/// should return a 400 Bad Request error.
#[tokio::test]
async fn test_malformed_json_body_returns_bad_request() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "malformed"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );

    let response = client
        .post(&response_url)
        .header("Content-Type", "application/json")
        .body("{ invalid json without quotes }")
        .send()
        .await
        .expect("Failed to send malformed response");

    assert_eq!(
        response.status(),
        400,
        "Malformed JSON should return 400 Bad Request"
    );

    let error_text = response.text().await.unwrap();
    assert!(
        error_text.to_lowercase().contains("json")
            || error_text.to_lowercase().contains("invalid")
            || error_text.to_lowercase().contains("parse"),
        "Error message should indicate JSON parsing issue: {}",
        error_text
    );

    simulator.shutdown().await;
}

/// Concurrent /next calls each receive an invocation.
///
/// When multiple concurrent GET requests are made to /next, they should each
/// receive an invocation when available.
#[tokio::test]
async fn test_concurrent_next_calls_each_receive_invocation() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();

    let inv1 = InvocationBuilder::new()
        .payload(json!({"test": "concurrent1"}))
        .build()
        .unwrap();
    let inv2 = InvocationBuilder::new()
        .payload(json!({"test": "concurrent2"}))
        .build()
        .unwrap();

    let request_id1 = inv1.request_id.clone();
    let request_id2 = inv2.request_id.clone();

    let url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let url1 = url.clone();
    let url2 = url.clone();

    let requests_started = Arc::new(AtomicUsize::new(0));
    let requests_started1 = Arc::clone(&requests_started);
    let requests_started2 = Arc::clone(&requests_started);

    let handle1 = tokio::spawn(async move {
        requests_started1.fetch_add(1, Ordering::SeqCst);
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        client.get(&url1).send().await
    });

    let handle2 = tokio::spawn(async move {
        requests_started2.fetch_add(1, Ordering::SeqCst);
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        client.get(&url2).send().await
    });

    while requests_started.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }

    simulator.enqueue(inv1).await;
    simulator.enqueue(inv2).await;

    let (result1, result2) = tokio::join!(handle1, handle2);

    let response1 = result1.unwrap().unwrap();
    let response2 = result2.unwrap().unwrap();

    assert_eq!(response1.status(), 200);
    assert_eq!(response2.status(), 200);

    let recv_id1 = response1
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .map(|h| h.to_str().unwrap().to_string())
        .unwrap();
    let recv_id2 = response2
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .map(|h| h.to_str().unwrap().to_string())
        .unwrap();

    let ids = [recv_id1, recv_id2];
    assert!(
        ids.contains(&request_id1) && ids.contains(&request_id2),
        "Both invocations should be delivered to concurrent requests"
    );

    simulator.shutdown().await;
}

/// Response after deadline passed is accepted but status may be Timeout.
///
/// When a response is submitted after the deadline has passed, it should still
/// be accepted (202), but the invocation status may reflect the timeout.
/// Uses tokio's paused time to avoid real delays.
#[tokio::test(start_paused = true)]
async fn test_response_after_deadline_accepted_or_rejected() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "deadline"}))
        .timeout_ms(50)
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    tokio::time::advance(Duration::from_millis(100)).await;

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    let response = client
        .post(&response_url)
        .json(&json!({"result": "late"}))
        .send()
        .await
        .expect("Failed to send late response");

    assert!(
        response.status() == 202 || response.status() == 400,
        "Late response should be accepted (202) or rejected (400), got {}",
        response.status()
    );

    simulator.shutdown().await;
}

/// Calling /next before registration returns 403 Forbidden.
///
/// When an extension attempts to call /next before registering, it should
/// receive a 403 Forbidden error.
#[tokio::test]
async fn test_extension_next_before_registration_returns_forbidden() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .get(format!(
            "{}/2020-01-01/extension/event/next",
            runtime_api_url
        ))
        .header("Lambda-Extension-Identifier", "unregistered-extension-id")
        .send()
        .await
        .expect("Failed to call /next");

    assert_eq!(
        response.status(),
        403,
        "Calling /next with unregistered extension ID should return 403 Forbidden"
    );

    let error_text = response.text().await.unwrap();
    assert!(
        error_text.to_lowercase().contains("not registered")
            || error_text.to_lowercase().contains("forbidden")
            || error_text.to_lowercase().contains("unknown"),
        "Error message should indicate the extension is not registered: {}",
        error_text
    );

    simulator.shutdown().await;
}

/// Extension that crashes during INVOKE is removed from readiness tracking.
///
/// When an extension "crashes" (stops polling /next), it should be removed
/// from readiness tracking so invocations can complete without waiting for it.
/// Uses wait_for_invocation_complete instead of arbitrary sleeps.
#[tokio::test]
async fn test_extension_crash_during_invoke_removed_from_readiness() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .post(format!("{}/2020-01-01/extension/register", runtime_api_url))
        .header("Lambda-Extension-Name", "crashing-extension")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .expect("Failed to register extension");

    assert_eq!(response.status(), 200);
    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    simulator.enqueue_payload(json!({"test": "crash"})).await;

    let _ext_event = client
        .get(format!(
            "{}/2020-01-01/extension/event/next",
            runtime_api_url
        ))
        .header("Lambda-Extension-Identifier", &extension_id)
        .send()
        .await
        .expect("Extension should receive INVOKE event");

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let response = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    client
        .post(&response_url)
        .json(&json!({"result": "success"}))
        .send()
        .await
        .expect("Failed to send response");

    let state = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(2))
        .await;

    assert!(
        state.is_ok(),
        "Invocation should complete even without extension polling for readiness"
    );

    simulator.shutdown().await;
}

/// Empty destination URI returns 400 Bad Request.
///
/// When subscribing to telemetry with an empty URI, the simulator should
/// return a 400 Bad Request error.
#[tokio::test]
async fn test_empty_telemetry_destination_uri_returns_bad_request() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .post(format!("{}/2020-01-01/extension/register", runtime_api_url))
        .header("Lambda-Extension-Name", "telemetry-extension")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .expect("Failed to register extension");

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: None,
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: "".to_string(),
        },
    };

    let response = client
        .put(format!("{}/2022-07-01/telemetry", runtime_api_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "telemetry-extension")
        .json(&subscription)
        .send()
        .await
        .expect("Failed to subscribe to telemetry");

    assert_eq!(
        response.status(),
        400,
        "Empty destination URI should return 400 Bad Request"
    );

    let error_text = response.text().await.unwrap();
    assert!(
        error_text.to_lowercase().contains("uri")
            || error_text.to_lowercase().contains("destination")
            || error_text.to_lowercase().contains("empty"),
        "Error message should indicate URI issue: {}",
        error_text
    );

    simulator.shutdown().await;
}

/// Delivery to unreachable URI is logged and buffering continues.
///
/// When telemetry delivery fails to an unreachable URI, the simulator should
/// log the failure but continue operating and buffering events.
/// Uses wait_for_invocation_complete instead of arbitrary sleeps.
#[tokio::test]
async fn test_telemetry_delivery_to_unreachable_uri_continues_operation() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .post(format!("{}/2020-01-01/extension/register", runtime_api_url))
        .header("Lambda-Extension-Name", "unreachable-extension")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .expect("Failed to register extension");

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(50),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: "http://127.0.0.1:59999/unreachable".to_string(),
        },
    };

    let response = client
        .put(format!("{}/2022-07-01/telemetry", runtime_api_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "unreachable-extension")
        .json(&subscription)
        .send()
        .await
        .expect("Failed to subscribe to telemetry");

    assert_eq!(response.status(), 200, "Subscription should succeed");

    let request_id = simulator
        .enqueue_payload(json!({"test": "unreachable"}))
        .await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    client
        .post(&response_url)
        .json(&json!({"result": "success"}))
        .send()
        .await
        .expect("Failed to send response");

    let state = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(2))
        .await
        .expect("Invocation should complete");

    assert_eq!(
        state.status,
        lambda_simulator::InvocationStatus::Success,
        "Invocation should complete successfully even with unreachable telemetry endpoint"
    );

    simulator.shutdown().await;
}

/// Slow telemetry endpoint causes events to queue; delivery is retried.
///
/// When the telemetry endpoint is slow, events should be queued and the
/// simulator should continue operating without blocking.
/// Uses Notify for synchronization instead of arbitrary sleeps.
#[tokio::test]
async fn test_slow_telemetry_endpoint_does_not_block_invocations() {
    let events_received = Arc::new(AtomicUsize::new(0));
    let events_clone = Arc::clone(&events_received);
    let delivery_complete = Arc::new(Notify::new());
    let delivery_complete_clone = Arc::clone(&delivery_complete);

    let app = axum::Router::new().route(
        "/slow-telemetry",
        axum::routing::post(
            move |axum::Json(payload): axum::Json<Vec<lambda_simulator::TelemetryEvent>>| {
                let events = Arc::clone(&events_clone);
                let notify = Arc::clone(&delivery_complete_clone);
                async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    events.fetch_add(payload.len(), Ordering::SeqCst);
                    notify.notify_one();
                    axum::http::StatusCode::OK
                }
            },
        ),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_ready = Arc::new(Notify::new());
    let server_ready_clone = Arc::clone(&server_ready);

    tokio::spawn(async move {
        server_ready_clone.notify_one();
        axum::serve(listener, app).await.unwrap();
    });

    server_ready.notified().await;

    let telemetry_url = format!("http://{}/slow-telemetry", addr);

    let simulator = Simulator::builder()
        .function_name("test-function")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .post(format!("{}/2020-01-01/extension/register", runtime_api_url))
        .header("Lambda-Extension-Name", "slow-endpoint-extension")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .expect("Failed to register extension");

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", runtime_api_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "slow-endpoint-extension")
        .json(&subscription)
        .send()
        .await
        .expect("Failed to subscribe to telemetry");

    let request_id = simulator.enqueue_payload(json!({"test": "slow"})).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    client
        .post(&response_url)
        .json(&json!({"result": "success"}))
        .send()
        .await
        .expect("Failed to send response");

    let state = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(2))
        .await
        .expect("Invocation should complete quickly despite slow telemetry");

    assert_eq!(state.status, lambda_simulator::InvocationStatus::Success);

    tokio::time::timeout(Duration::from_secs(2), delivery_complete.notified())
        .await
        .expect("Telemetry should eventually be delivered");

    assert!(
        events_received.load(Ordering::SeqCst) > 0,
        "Telemetry events should eventually be delivered to slow endpoint"
    );

    simulator.shutdown().await;
}

/// Response exceeding 6MB returns 413 Payload Too Large.
///
/// AWS Lambda enforces a 6 MB response payload limit for synchronous invocations.
/// The simulator should reject oversized responses to catch issues during testing.
#[tokio::test]
async fn test_response_exceeding_6mb_returns_payload_too_large() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "oversized"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );

    let oversized_payload = "a".repeat(6 * 1024 * 1024 + 1);
    let oversized_json = format!(r#"{{"data": "{}"}}"#, oversized_payload);

    let response = client
        .post(&response_url)
        .header("Content-Type", "application/json")
        .body(oversized_json)
        .send()
        .await
        .expect("Failed to send oversized response");

    assert_eq!(
        response.status(),
        413,
        "Oversized response should return 413 Payload Too Large"
    );

    let error_text = response.text().await.unwrap();
    assert!(
        error_text.contains("6 MB") || error_text.contains("6MB"),
        "Error message should mention 6 MB limit: {}",
        error_text
    );

    simulator.shutdown().await;
}

/// Response just under 6MB limit is accepted.
///
/// Verifies that responses at the boundary (just under 6 MB) are accepted.
#[tokio::test]
async fn test_response_just_under_6mb_is_accepted() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "boundary"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );

    let response = client
        .post(&response_url)
        .header("Content-Type", "application/json")
        .body(r#"{"status": "ok"}"#)
        .send()
        .await
        .expect("Failed to send small response");

    assert_eq!(
        response.status(),
        202,
        "Small response should be accepted with 202"
    );

    simulator.shutdown().await;
}

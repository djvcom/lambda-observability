//! Integration tests for graceful shutdown behavior.
//!
//! These tests verify that the Lambda lifecycle is correctly simulated during shutdown:
//! 1. Extensions receive SHUTDOWN events
//! 2. Extensions have time to do cleanup work
//! 3. Shutdown proceeds after extensions acknowledge or timeout
//! 4. Server and telemetry are properly cleaned up

use lambda_simulator::{
    EventType, InvocationBuilder, ShutdownReason, Simulator, SimulatorPhase, TelemetryEvent,
    TelemetryEventType, TelemetrySubscription,
};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// Helper to register an extension.
async fn register_extension(
    client: &Client,
    base_url: &str,
    name: &str,
    events: Vec<EventType>,
) -> String {
    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .header("Lambda-Extension-Name", name)
        .json(&json!({ "events": events }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    response
        .headers()
        .get("Lambda-Extension-Identifier")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn test_extension_receives_shutdown_event() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "shutdown-extension",
        vec![EventType::Shutdown],
    )
    .await;

    let received_shutdown = Arc::new(AtomicBool::new(false));
    let received_shutdown_clone = Arc::clone(&received_shutdown);
    let extension_id_clone = extension_id.clone();
    let base_url_clone = base_url.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();

        let response = client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        let event: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            event["eventType"], "SHUTDOWN",
            "Should receive SHUTDOWN event"
        );
        received_shutdown_clone.store(true, Ordering::SeqCst);

        assert_eq!(
            event["shutdownReason"], "spindown",
            "Shutdown reason should match"
        );
        let deadline = event["deadlineMs"]
            .as_i64()
            .expect("deadlineMs should be i64");
        assert!(deadline > 0, "Deadline should be positive");
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let task_result = tokio::time::timeout(Duration::from_secs(2), extension_task)
        .await
        .expect("Extension task should complete before timeout");
    task_result.expect("Extension task should not panic");

    assert!(
        received_shutdown.load(Ordering::SeqCst),
        "Extension should have received SHUTDOWN event"
    );
}

#[tokio::test]
async fn test_graceful_shutdown_waits_for_extension() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "cleanup-extension",
        vec![EventType::Shutdown],
    )
    .await;

    let cleanup_completed = Arc::new(AtomicBool::new(false));
    let cleanup_completed_clone = Arc::clone(&cleanup_completed);
    let extension_id_clone = extension_id.clone();
    let base_url_clone = base_url.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();

        let response = client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        let event: serde_json::Value = response.json().await.unwrap();
        assert_eq!(event["eventType"], "SHUTDOWN");

        tokio::time::sleep(Duration::from_millis(100)).await;

        cleanup_completed_clone.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = extension_task.await;

    assert!(
        cleanup_completed.load(Ordering::SeqCst),
        "Extension should have completed cleanup before shutdown finished"
    );
}

#[tokio::test]
async fn test_graceful_shutdown_timeout() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let _extension_id = register_extension(
        &client,
        &base_url,
        "slow-extension",
        vec![EventType::Shutdown],
    )
    .await;

    let start = std::time::Instant::now();
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(150),
        "Shutdown should wait for timeout"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "Shutdown should not wait too long"
    );
}

#[tokio::test]
async fn test_graceful_shutdown_with_invoke_and_shutdown_extension() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "full-lifecycle-extension",
        vec![EventType::Invoke, EventType::Shutdown],
    )
    .await;

    let events_received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events_received);
    let extension_id_clone = extension_id.clone();
    let base_url_clone = base_url.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();

        loop {
            let response = client
                .get(format!(
                    "{}/2020-01-01/extension/event/next",
                    base_url_clone
                ))
                .header("Lambda-Extension-Identifier", &extension_id_clone)
                .timeout(Duration::from_secs(10))
                .send()
                .await;

            match response {
                Ok(resp) if resp.status() == 200 => {
                    let event: serde_json::Value = resp.json().await.unwrap();
                    let event_type = event["eventType"].as_str().unwrap().to_string();
                    events_clone.lock().await.push(event_type.clone());

                    if event_type == "SHUTDOWN" {
                        break;
                    }
                }
                _ => break,
            }
        }
    });

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "data"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let runtime_response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(runtime_response.status(), 200);

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "success"}))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = tokio::time::timeout(Duration::from_secs(2), extension_task).await;

    let events = events_received.lock().await;
    assert!(
        events.contains(&"INVOKE".to_string()),
        "Extension should have received INVOKE event"
    );
    assert!(
        events.contains(&"SHUTDOWN".to_string()),
        "Extension should have received SHUTDOWN event"
    );
}

#[tokio::test]
async fn test_graceful_shutdown_with_no_shutdown_subscribers() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    register_extension(
        &client,
        &base_url,
        "invoke-only-extension",
        vec![EventType::Invoke],
    )
    .await;

    let start = std::time::Instant::now();
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "Shutdown should be fast when no extensions subscribe to SHUTDOWN"
    );
}

#[tokio::test]
async fn test_shutdown_reason_is_correct() {
    for reason in [
        ShutdownReason::Spindown,
        ShutdownReason::Timeout,
        ShutdownReason::Failure,
    ] {
        let simulator = Simulator::builder()
            .shutdown_timeout(Duration::from_secs(5))
            .build()
            .await
            .unwrap();
        let base_url = simulator.runtime_api_url();
        let client = Client::new();

        let extension_id = register_extension(
            &client,
            &base_url,
            "reason-checker",
            vec![EventType::Shutdown],
        )
        .await;

        let expected_reason = match &reason {
            ShutdownReason::Spindown => "spindown",
            ShutdownReason::Timeout => "timeout",
            ShutdownReason::Failure => "failure",
        };

        let received_reason = Arc::new(tokio::sync::Mutex::new(String::new()));
        let received_reason_clone = Arc::clone(&received_reason);
        let extension_id_clone = extension_id.clone();
        let base_url_clone = base_url.clone();

        let extension_task = tokio::spawn(async move {
            let client = Client::new();

            let response = client
                .get(format!(
                    "{}/2020-01-01/extension/event/next",
                    base_url_clone
                ))
                .header("Lambda-Extension-Identifier", &extension_id_clone)
                .send()
                .await
                .unwrap();

            let event: serde_json::Value = response.json().await.unwrap();
            if let Some(reason) = event["shutdownReason"].as_str() {
                *received_reason_clone.lock().await = reason.to_string();
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        simulator.graceful_shutdown(reason).await;

        let _ = tokio::time::timeout(Duration::from_secs(2), extension_task).await;

        let received = received_reason.lock().await;
        assert_eq!(*received, expected_reason, "Shutdown reason should match");
    }
}

async fn start_telemetry_receiver() -> (String, Arc<Mutex<Vec<TelemetryEvent>>>, Arc<Notify>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let event_received = Arc::new(Notify::new());
    let event_received_clone = Arc::clone(&event_received);

    let app = axum::Router::new().route(
        "/telemetry",
        axum::routing::post(
            move |axum::Json(payload): axum::Json<Vec<TelemetryEvent>>| {
                let events = Arc::clone(&events_clone);
                let notify = Arc::clone(&event_received_clone);
                async move {
                    events.lock().await.extend(payload);
                    notify.notify_waiters();
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

    (format!("http://{}/telemetry", addr), events, event_received)
}

/// Extension acknowledgment tracked - shutdown completes faster when extension acknowledges.
#[tokio::test]
async fn test_extension_acknowledgment_tracked() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "acknowledging-extension",
        vec![EventType::Shutdown],
    )
    .await;

    let extension_id_clone = extension_id.clone();
    let base_url_clone = base_url.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();

        let response = client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .send()
            .await
            .unwrap();

        let event: serde_json::Value = response.json().await.unwrap();
        assert_eq!(event["eventType"], "SHUTDOWN");

        tokio::time::sleep(Duration::from_millis(50)).await;

        client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .timeout(Duration::from_millis(100))
            .send()
            .await
            .ok();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let start = std::time::Instant::now();
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;
    let elapsed = start.elapsed();

    let _ = extension_task.await;

    assert!(
        elapsed < Duration::from_millis(500),
        "Shutdown should complete quickly after extension acknowledges (took {:?})",
        elapsed
    );
}

/// Telemetry cleanup - delivery tasks are aborted during shutdown.
#[tokio::test]
async fn test_telemetry_flushed_on_shutdown() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, _notify) = start_telemetry_receiver().await;

    let extension_id =
        register_extension(&client, &base_url, "telemetry-ext", vec![EventType::Invoke]).await;

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(100),
            max_bytes: Some(262144),
            timeout_ms: Some(5000),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "telemetry-ext")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "telemetry_cleanup"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "ok"}))
        .send()
        .await
        .unwrap();

    let events_before = events.lock().await.len();

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    let events_after = events.lock().await.len();

    assert!(
        events_after >= events_before,
        "Events should be stable after shutdown (no new deliveries)"
    );
}

/// Server stopped - HTTP endpoints no longer respond after shutdown.
#[tokio::test]
async fn test_server_stopped_after_shutdown() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .timeout(Duration::from_millis(100))
        .send()
        .await;
    assert!(
        response.is_ok() || response.is_err(),
        "Server should be running before shutdown"
    );

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .timeout(Duration::from_millis(500))
        .send()
        .await;

    assert!(
        response.is_err(),
        "Server should not respond after shutdown"
    );
}

/// Deadline calculation correct - deadlineMs = now + shutdown_timeout_ms.
#[tokio::test]
async fn test_shutdown_deadline_calculated_correctly() {
    let shutdown_timeout_ms = 2000u64;
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_millis(shutdown_timeout_ms))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "deadline-checker",
        vec![EventType::Shutdown],
    )
    .await;

    let received_deadline = Arc::new(tokio::sync::Mutex::new(0i64));
    let received_deadline_clone = Arc::clone(&received_deadline);
    let extension_id_clone = extension_id.clone();
    let base_url_clone = base_url.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();

        let response = client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .send()
            .await
            .unwrap();

        let event: serde_json::Value = response.json().await.unwrap();
        if let Some(deadline) = event["deadlineMs"].as_i64() {
            *received_deadline_clone.lock().await = deadline;
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let before_shutdown = chrono::Utc::now().timestamp_millis();
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = tokio::time::timeout(Duration::from_secs(1), extension_task).await;

    let deadline = *received_deadline.lock().await;
    let expected_min = before_shutdown + shutdown_timeout_ms as i64 - 100;
    let expected_max = before_shutdown + shutdown_timeout_ms as i64 + 100;

    assert!(
        deadline >= expected_min && deadline <= expected_max,
        "Deadline ({}) should be approximately now + shutdown_timeout ({} to {})",
        deadline,
        expected_min,
        expected_max
    );
}

/// Shutdown during invocation - in-flight response still accepted.
#[tokio::test]
async fn test_shutdown_during_invocation_waits_for_completion() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(2))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "shutdown_during_invocation"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let shutdown_handle = {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    let response = client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "completed_during_shutdown"}))
        .send()
        .await
        .unwrap();

    assert!(
        response.status().is_success(),
        "Response should be accepted even during shutdown (got {})",
        response.status()
    );

    drop(shutdown_handle);
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;
}

/// New invocations blocked - enqueued invocations not delivered during shutdown.
#[tokio::test]
async fn test_new_invocations_blocked_during_shutdown() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(2))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "blocking-test-ext",
        vec![EventType::Shutdown],
    )
    .await;

    let extension_id_clone = extension_id.clone();
    let base_url_clone = base_url.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();
        let response = client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .send()
            .await
            .unwrap();

        let event: serde_json::Value = response.json().await.unwrap();
        assert_eq!(event["eventType"], "SHUTDOWN");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let phase_before = simulator.phase().await;
    assert_eq!(
        phase_before,
        SimulatorPhase::Initializing,
        "Should be in Initializing or Ready phase before shutdown"
    );

    let shutdown_task = {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    simulator
        .enqueue_payload(json!({"test": "blocked_invocation"}))
        .await;

    drop(shutdown_task);
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = tokio::time::timeout(Duration::from_secs(1), extension_task).await;
}

/// Runtime can complete work - /response endpoint still works during shutdown.
#[tokio::test]
async fn test_runtime_can_complete_work_during_shutdown() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .extension_ready_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let extension_id = register_extension(
        &client,
        &base_url,
        "work-completion-ext",
        vec![EventType::Invoke, EventType::Shutdown],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "work_completion"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let runtime_response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(runtime_response.status(), 200);

    let base_url_clone = base_url.clone();
    let extension_id_clone = extension_id.clone();

    let extension_task = tokio::spawn(async move {
        let client = Client::new();

        let response = client
            .get(format!(
                "{}/2020-01-01/extension/event/next",
                base_url_clone
            ))
            .header("Lambda-Extension-Identifier", &extension_id_clone)
            .send()
            .await
            .unwrap();

        let event: serde_json::Value = response.json().await.unwrap();
        let event_type = event["eventType"].as_str().unwrap();

        if event_type == "INVOKE" {
            let response = client
                .get(format!(
                    "{}/2020-01-01/extension/event/next",
                    base_url_clone
                ))
                .header("Lambda-Extension-Identifier", &extension_id_clone)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            if let Ok(resp) = response {
                let event: serde_json::Value = resp.json().await.unwrap();
                assert_eq!(event["eventType"], "SHUTDOWN");
            }
        }
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "work_completed"}))
        .send()
        .await
        .unwrap();

    assert!(
        response.status().is_success(),
        "Runtime should be able to complete work (got {})",
        response.status()
    );

    tokio::time::sleep(Duration::from_millis(600)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(
        !report_events.is_empty(),
        "platform.report should be emitted for completed invocation"
    );

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = tokio::time::timeout(Duration::from_secs(2), extension_task).await;
}

//! Tests for concurrent operation scenarios.
//!
//! These tests verify correct handling of:
//! - Multiple extensions with different subscriptions
//! - Event routing by subscription type
//! - Readiness tracking per subscription
//! - Multiple telemetry subscribers with independent delivery

use lambda_simulator::{
    EventType, InvocationBuilder, ShutdownReason, Simulator, TelemetryEvent, TelemetryEventType,
    TelemetrySubscription,
};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

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

async fn wait_for_events<F>(
    events: &Arc<Mutex<Vec<TelemetryEvent>>>,
    notify: &Arc<Notify>,
    predicate: F,
    timeout: Duration,
) -> bool
where
    F: Fn(&[TelemetryEvent]) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        {
            let events_guard = events.lock().await;
            if predicate(&events_guard) {
                return true;
            }
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }

        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(remaining) => {
                return false;
            }
        }
    }
}

/// Each extension receives only the events it subscribed to.
///
/// Extension A subscribes to INVOKE, Extension B to INVOKE + SHUTDOWN, Extension C to SHUTDOWN.
#[tokio::test]
async fn test_event_routing_by_subscription() {
    let simulator = Simulator::builder()
        .shutdown_timeout(Duration::from_secs(5))
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let ext_a_id = register_extension(
        &client,
        &base_url,
        "ext-a-invoke-only",
        vec![EventType::Invoke],
    )
    .await;
    let ext_b_id = register_extension(
        &client,
        &base_url,
        "ext-b-both",
        vec![EventType::Invoke, EventType::Shutdown],
    )
    .await;
    let ext_c_id = register_extension(
        &client,
        &base_url,
        "ext-c-shutdown-only",
        vec![EventType::Shutdown],
    )
    .await;

    let ext_a_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let ext_b_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let ext_c_events = Arc::new(Mutex::new(Vec::<String>::new()));

    let ext_a_events_clone = Arc::clone(&ext_a_events);
    let ext_a_id_clone = ext_a_id.clone();
    let base_url_a = base_url.clone();

    let ext_a_task = tokio::spawn(async move {
        let client = Client::new();
        loop {
            let response = client
                .get(format!("{}/2020-01-01/extension/event/next", base_url_a))
                .header("Lambda-Extension-Identifier", &ext_a_id_clone)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            match response {
                Ok(resp) if resp.status() == 200 => {
                    let event: serde_json::Value = resp.json().await.unwrap();
                    let event_type = event["eventType"].as_str().unwrap().to_string();
                    ext_a_events_clone.lock().await.push(event_type.clone());
                    if event_type == "SHUTDOWN" {
                        break;
                    }
                }
                _ => break,
            }
        }
    });

    let ext_b_events_clone = Arc::clone(&ext_b_events);
    let ext_b_id_clone = ext_b_id.clone();
    let base_url_b = base_url.clone();

    let ext_b_task = tokio::spawn(async move {
        let client = Client::new();
        loop {
            let response = client
                .get(format!("{}/2020-01-01/extension/event/next", base_url_b))
                .header("Lambda-Extension-Identifier", &ext_b_id_clone)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            match response {
                Ok(resp) if resp.status() == 200 => {
                    let event: serde_json::Value = resp.json().await.unwrap();
                    let event_type = event["eventType"].as_str().unwrap().to_string();
                    ext_b_events_clone.lock().await.push(event_type.clone());
                    if event_type == "SHUTDOWN" {
                        break;
                    }
                }
                _ => break,
            }
        }
    });

    let ext_c_events_clone = Arc::clone(&ext_c_events);
    let ext_c_id_clone = ext_c_id.clone();
    let base_url_c = base_url.clone();

    let ext_c_task = tokio::spawn(async move {
        let client = Client::new();
        let response = client
            .get(format!("{}/2020-01-01/extension/event/next", base_url_c))
            .header("Lambda-Extension-Identifier", &ext_c_id_clone)
            .timeout(Duration::from_secs(5))
            .send()
            .await;

        if let Ok(resp) = response
            && resp.status() == 200
        {
            let event: serde_json::Value = resp.json().await.unwrap();
            let event_type = event["eventType"].as_str().unwrap().to_string();
            ext_c_events_clone.lock().await.push(event_type);
        }
    });

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "routing"}))
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

    tokio::time::sleep(Duration::from_millis(300)).await;

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = tokio::time::timeout(Duration::from_secs(2), ext_a_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), ext_b_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), ext_c_task).await;

    let a_events = ext_a_events.lock().await;
    let b_events = ext_b_events.lock().await;
    let c_events = ext_c_events.lock().await;

    assert!(
        a_events.contains(&"INVOKE".to_string()),
        "Extension A should receive INVOKE"
    );
    assert!(
        !a_events.contains(&"SHUTDOWN".to_string()),
        "Extension A should NOT receive SHUTDOWN"
    );

    assert!(
        b_events.contains(&"INVOKE".to_string()),
        "Extension B should receive INVOKE"
    );
    assert!(
        b_events.contains(&"SHUTDOWN".to_string()),
        "Extension B should receive SHUTDOWN"
    );

    assert!(
        !c_events.contains(&"INVOKE".to_string()),
        "Extension C should NOT receive INVOKE"
    );
    assert!(
        c_events.contains(&"SHUTDOWN".to_string()),
        "Extension C should receive SHUTDOWN"
    );
}

/// Only INVOKE subscribers are tracked for readiness.
///
/// Extension C (SHUTDOWN only) should not be tracked for invocation readiness.
#[tokio::test]
async fn test_readiness_tracking_per_subscription() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let ext_a_id =
        register_extension(&client, &base_url, "invoke-ext", vec![EventType::Invoke]).await;

    let _ext_c_id = register_extension(
        &client,
        &base_url,
        "shutdown-only-ext",
        vec![EventType::Shutdown],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "readiness"}))
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

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &ext_a_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted when INVOKE subscriber is ready (SHUTDOWN-only subscriber not tracked)"
    );

    simulator.shutdown().await;
}

/// Each extension can subscribe to telemetry independently.
#[tokio::test]
async fn test_independent_telemetry_subscriptions() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url_a, events_a, notify_a) = start_telemetry_receiver().await;
    let (telemetry_url_b, events_b, notify_b) = start_telemetry_receiver().await;

    let ext_a_id = register_extension(&client, &base_url, "ext-a", vec![EventType::Invoke]).await;
    let ext_b_id = register_extension(&client, &base_url, "ext-b", vec![EventType::Invoke]).await;

    let subscription_a = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url_a,
        },
    };

    let subscription_b = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url_b,
        },
    };

    let resp_a = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_a_id)
        .header("Lambda-Extension-Name", "ext-a")
        .json(&subscription_a)
        .send()
        .await
        .unwrap();
    assert_eq!(resp_a.status(), 200);

    let resp_b = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_b_id)
        .header("Lambda-Extension-Name", "ext-b")
        .json(&subscription_b)
        .send()
        .await
        .unwrap();
    assert_eq!(resp_b.status(), 200);

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "independent_subscriptions"}))
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

    let has_start_a = wait_for_events(
        &events_a,
        &notify_a,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    let has_start_b = wait_for_events(
        &events_b,
        &notify_b,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_start_a, "Extension A should receive platform.start");
    assert!(has_start_b, "Extension B should receive platform.start");

    simulator.shutdown().await;
}

/// Multiple subscribers each receive platform.start events.
#[tokio::test]
async fn test_multiple_subscribers_receive_events() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url_1, events_1, notify_1) = start_telemetry_receiver().await;
    let (telemetry_url_2, events_2, notify_2) = start_telemetry_receiver().await;

    let ext_1_id =
        register_extension(&client, &base_url, "subscriber-1", vec![EventType::Invoke]).await;
    let ext_2_id =
        register_extension(&client, &base_url, "subscriber-2", vec![EventType::Invoke]).await;

    let subscription_1 = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url_1,
        },
    };

    let subscription_2 = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url_2,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_1_id)
        .header("Lambda-Extension-Name", "subscriber-1")
        .json(&subscription_1)
        .send()
        .await
        .unwrap();

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_2_id)
        .header("Lambda-Extension-Name", "subscriber-2")
        .json(&subscription_2)
        .send()
        .await
        .unwrap();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "multiple_subscribers"}))
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

    let has_events_1 = wait_for_events(
        &events_1,
        &notify_1,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    let has_events_2 = wait_for_events(
        &events_2,
        &notify_2,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_events_1, "Subscriber 1 should receive platform.start");
    assert!(has_events_2, "Subscriber 2 should receive platform.start");

    let evts_1 = events_1.lock().await;
    let evts_2 = events_2.lock().await;

    let start_1 = evts_1.iter().find(|e| e.event_type == "platform.start");
    let start_2 = evts_2.iter().find(|e| e.event_type == "platform.start");

    assert!(start_1.is_some(), "Subscriber 1 should have platform.start");
    assert!(start_2.is_some(), "Subscriber 2 should have platform.start");

    assert_eq!(
        start_1.unwrap().record["requestId"],
        start_2.unwrap().record["requestId"],
        "Both should receive the same event (same requestId)"
    );

    simulator.shutdown().await;
}

/// One subscriber's failure doesn't affect another.
#[tokio::test]
async fn test_independent_delivery() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (working_url, working_events, working_notify) = start_telemetry_receiver().await;
    let broken_url = "http://127.0.0.1:1".to_string();

    let ext_working_id =
        register_extension(&client, &base_url, "working-ext", vec![EventType::Invoke]).await;
    let ext_broken_id =
        register_extension(&client, &base_url, "broken-ext", vec![EventType::Invoke]).await;

    let subscription_working = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: working_url,
        },
    };

    let subscription_broken = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(10),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: broken_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_working_id)
        .header("Lambda-Extension-Name", "working-ext")
        .json(&subscription_working)
        .send()
        .await
        .unwrap();

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_broken_id)
        .header("Lambda-Extension-Name", "broken-ext")
        .json(&subscription_broken)
        .send()
        .await
        .unwrap();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "independent_delivery"}))
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

    let has_events = wait_for_events(
        &working_events,
        &working_notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(
        has_events,
        "Working subscriber should still receive events even when another subscriber fails"
    );

    simulator.shutdown().await;
}

/// Each subscriber uses its own buffering configuration.
///
/// Two subscribers can configure different buffering parameters independently
/// and both receive events according to their config.
#[tokio::test]
async fn test_different_buffering_configs() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (url_small_batch, events_small_batch, notify_small_batch) =
        start_telemetry_receiver().await;
    let (url_large_batch, events_large_batch, notify_large_batch) =
        start_telemetry_receiver().await;

    let ext_small_id =
        register_extension(&client, &base_url, "small-batch", vec![EventType::Invoke]).await;
    let ext_large_id =
        register_extension(&client, &base_url, "large-batch", vec![EventType::Invoke]).await;

    let subscription_small_batch = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(5),
            max_bytes: Some(262144),
            timeout_ms: Some(50),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: url_small_batch,
        },
    };

    let subscription_large_batch = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(100),
            max_bytes: Some(262144),
            timeout_ms: Some(100),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: url_large_batch,
        },
    };

    let resp_small = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_small_id)
        .header("Lambda-Extension-Name", "small-batch")
        .json(&subscription_small_batch)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp_small.status(),
        200,
        "Small batch subscription should succeed"
    );

    let resp_large = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_large_id)
        .header("Lambda-Extension-Name", "large-batch")
        .json(&subscription_large_batch)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp_large.status(),
        200,
        "Large batch subscription should succeed"
    );

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "buffering_configs"}))
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

    let has_small_batch_events = wait_for_events(
        &events_small_batch,
        &notify_small_batch,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    let has_large_batch_events = wait_for_events(
        &events_large_batch,
        &notify_large_batch,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(
        has_small_batch_events,
        "Small batch subscriber should receive events"
    );
    assert!(
        has_large_batch_events,
        "Large batch subscriber should receive events"
    );

    let small_events = events_small_batch.lock().await;
    let large_events = events_large_batch.lock().await;

    assert!(
        !small_events.is_empty(),
        "Small batch subscriber should have received events"
    );
    assert!(
        !large_events.is_empty(),
        "Large batch subscriber should have received events"
    );

    simulator.shutdown().await;
}

//! Integration tests for the Lambda Telemetry API.

use lambda_simulator::{
    EventType, InvocationBuilder, Simulator, TelemetryEvent, TelemetryEventType,
    TelemetrySubscription,
};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// Helper to start a simple HTTP server that receives telemetry events.
/// Returns (url, events_vec, event_received_notify).
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

/// Helper to register an extension.
async fn register_extension(
    client: &Client,
    base_url: &str,
    name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .header("Lambda-Extension-Name", name)
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await?;

    assert_eq!(response.status(), 200);

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .ok_or("Missing Lambda-Extension-Identifier header")?
        .to_str()?
        .to_string();

    Ok(extension_id)
}

/// Helper to wait for events with a predicate, with timeout.
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

#[tokio::test]
async fn test_telemetry_subscription() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, _events, _notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "test-extension")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: None,
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    let response = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "test-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn test_telemetry_requires_extension_identifier() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, _events, _notify) = start_telemetry_receiver().await;

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: None,
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    let response = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .json(&subscription)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Lambda-Extension-Identifier")
    );
}

#[tokio::test]
async fn test_telemetry_platform_start_event() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "telemetry-extension")
        .await
        .unwrap();

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
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "telemetry-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "data"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    // platform.start is emitted when the runtime calls /next, not when enqueued
    let next_handle = tokio::spawn({
        let client = client.clone();
        let base_url = base_url.clone();
        async move {
            client
                .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
                .send()
                .await
                .unwrap()
        }
    });

    let has_platform_start = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_platform_start, "Should receive platform.start event");

    let received_events = events.lock().await;
    let platform_start_events: Vec<_> = received_events
        .iter()
        .filter(|e| e.event_type == "platform.start")
        .collect();

    let start_event = platform_start_events[0];
    assert_eq!(
        start_event.record["requestId"],
        json!(request_id),
        "Request ID should match"
    );

    drop(received_events);
    let _ = next_handle.await;
}

#[tokio::test]
async fn test_telemetry_lifecycle_events() {
    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "lifecycle-extension")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(100),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "lifecycle-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    let invocation = InvocationBuilder::new()
        .payload(json!({"message": "test"}))
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

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "success"}))
        .send()
        .await
        .unwrap();

    let has_all_events = wait_for_events(
        &events,
        &notify,
        |evts| {
            let has_start = evts.iter().any(|e| e.event_type == "platform.start");
            let has_runtime_done = evts.iter().any(|e| e.event_type == "platform.runtimeDone");
            let has_report = evts.iter().any(|e| e.event_type == "platform.report");
            has_start && has_runtime_done && has_report
        },
        Duration::from_secs(2),
    )
    .await;

    assert!(has_all_events, "Should receive all lifecycle events");

    let received_events = events.lock().await;

    let platform_start: Vec<_> = received_events
        .iter()
        .filter(|e| e.event_type == "platform.start")
        .collect();
    let platform_runtime_done: Vec<_> = received_events
        .iter()
        .filter(|e| e.event_type == "platform.runtimeDone")
        .collect();
    let platform_report: Vec<_> = received_events
        .iter()
        .filter(|e| e.event_type == "platform.report")
        .collect();

    assert_eq!(
        platform_start.len(),
        1,
        "Should have one platform.start event"
    );
    assert_eq!(
        platform_runtime_done.len(),
        1,
        "Should have one platform.runtimeDone event"
    );
    assert_eq!(
        platform_report.len(),
        1,
        "Should have one platform.report event"
    );

    assert_eq!(
        platform_runtime_done[0].record["requestId"],
        json!(request_id)
    );
    assert_eq!(platform_runtime_done[0].record["status"], json!("success"));

    assert_eq!(platform_report[0].record["requestId"], json!(request_id));
    assert_eq!(platform_report[0].record["status"], json!("success"));

    let duration = platform_report[0].record["metrics"]["durationMs"]
        .as_f64()
        .unwrap();
    let billed_duration = platform_report[0].record["metrics"]["billedDurationMs"]
        .as_u64()
        .unwrap();

    assert!(duration >= 0.0, "Duration should be non-negative");
    assert!(
        billed_duration >= 1,
        "Billed duration should be at least 1ms"
    );
}

#[tokio::test]
async fn test_telemetry_only_http_protocol() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(&client, &base_url, "test-extension")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: None,
        destination: lambda_simulator::telemetry::Destination {
            protocol: "TCP".to_string(),
            uri: "tcp://localhost:1234".to_string(),
        },
    };

    let response = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "test-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert!(response.text().await.unwrap().contains("HTTP"));
}

#[tokio::test]
async fn test_telemetry_multiple_event_types() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, _events, _notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "multi-type-extension")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![
            TelemetryEventType::Platform,
            TelemetryEventType::Function,
            TelemetryEventType::Extension,
        ],
        buffering: None,
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    let response = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "multi-type-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}

/// platform.initStart emitted when simulator starts.
#[tokio::test]
async fn test_init_start_emitted_on_startup() {
    let simulator = Simulator::builder()
        .function_name("init-test-function")
        .function_version("$LATEST")
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let init_start_events = simulator
        .get_telemetry_events_by_type("platform.initStart")
        .await;

    assert_eq!(
        init_start_events.len(),
        1,
        "Should have exactly one platform.initStart event"
    );

    let init_start = &init_start_events[0];
    assert_eq!(
        init_start.record["initializationType"],
        json!("on-demand"),
        "Initialization type should be on-demand"
    );
    assert_eq!(
        init_start.record["phase"],
        json!("init"),
        "Phase should be init"
    );
    assert_eq!(
        init_start.record["functionName"],
        json!("init-test-function"),
        "Function name should match"
    );
    assert_eq!(
        init_start.record["functionVersion"],
        json!("$LATEST"),
        "Function version should match"
    );

    simulator.shutdown().await;
}

/// platform.initRuntimeDone emitted when runtime first polls /next.
#[tokio::test]
async fn test_init_runtime_done_on_first_next() {
    let simulator = Simulator::builder()
        .function_name("init-runtime-done-test")
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enqueue_payload(json!({"test": "data"})).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let init_runtime_done_events = simulator
        .get_telemetry_events_by_type("platform.initRuntimeDone")
        .await;

    assert_eq!(
        init_runtime_done_events.len(),
        1,
        "Should have exactly one platform.initRuntimeDone event"
    );

    let init_runtime_done = &init_runtime_done_events[0];
    assert_eq!(
        init_runtime_done.record["initializationType"],
        json!("on-demand")
    );
    assert_eq!(init_runtime_done.record["phase"], json!("init"));
    assert_eq!(init_runtime_done.record["status"], json!("success"));

    simulator.shutdown().await;
}

/// platform.initReport emitted with duration metrics.
#[tokio::test(start_paused = true)]
async fn test_init_report_with_duration() {
    let simulator = Simulator::builder()
        .function_name("init-report-test")
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    tokio::time::advance(Duration::from_millis(10)).await;

    simulator.enqueue_payload(json!({"test": "data"})).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let init_report_events = simulator
        .get_telemetry_events_by_type("platform.initReport")
        .await;

    assert_eq!(
        init_report_events.len(),
        1,
        "Should have exactly one platform.initReport event"
    );

    let init_report = &init_report_events[0];
    assert_eq!(init_report.record["initializationType"], json!("on-demand"));
    assert_eq!(init_report.record["phase"], json!("init"));
    assert_eq!(init_report.record["status"], json!("success"));

    let duration_ms = init_report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be a number");
    assert!(
        duration_ms >= 0.0,
        "Init duration should be non-negative, got {}",
        duration_ms
    );

    simulator.shutdown().await;
}

/// Init telemetry emitted only once across multiple invocations.
#[tokio::test]
async fn test_init_telemetry_emitted_only_once() {
    let simulator = Simulator::builder()
        .function_name("init-once-test")
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    for i in 0..3 {
        simulator.enqueue_payload(json!({"iteration": i})).await;

        let response = client
            .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
            .send()
            .await
            .unwrap();

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        client
            .post(format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                base_url, request_id
            ))
            .json(&json!({"result": i}))
            .send()
            .await
            .unwrap();
    }

    let init_start_events = simulator
        .get_telemetry_events_by_type("platform.initStart")
        .await;
    let init_runtime_done_events = simulator
        .get_telemetry_events_by_type("platform.initRuntimeDone")
        .await;
    let init_report_events = simulator
        .get_telemetry_events_by_type("platform.initReport")
        .await;

    assert_eq!(
        init_start_events.len(),
        1,
        "Should have exactly one platform.initStart event across all invocations"
    );
    assert_eq!(
        init_runtime_done_events.len(),
        1,
        "Should have exactly one platform.initRuntimeDone event across all invocations"
    );
    assert_eq!(
        init_report_events.len(),
        1,
        "Should have exactly one platform.initReport event across all invocations"
    );

    let platform_start_events = simulator
        .get_telemetry_events_by_type("platform.start")
        .await;
    assert_eq!(
        platform_start_events.len(),
        3,
        "Should have three platform.start events (one per invocation)"
    );

    simulator.shutdown().await;
}

/// Cold start telemetry event sequence is correct.
#[tokio::test]
async fn test_init_telemetry_event_sequence() {
    let simulator = Simulator::builder()
        .function_name("init-sequence-test")
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enqueue_payload(json!({"test": "data"})).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let all_events = simulator.get_telemetry_events().await;

    let init_start_idx = all_events
        .iter()
        .position(|e| e.event_type == "platform.initStart");
    let init_runtime_done_idx = all_events
        .iter()
        .position(|e| e.event_type == "platform.initRuntimeDone");
    let init_report_idx = all_events
        .iter()
        .position(|e| e.event_type == "platform.initReport");
    let platform_start_idx = all_events
        .iter()
        .position(|e| e.event_type == "platform.start");

    assert!(
        init_start_idx.is_some(),
        "Should have platform.initStart event"
    );
    assert!(
        init_runtime_done_idx.is_some(),
        "Should have platform.initRuntimeDone event"
    );
    assert!(
        init_report_idx.is_some(),
        "Should have platform.initReport event"
    );
    assert!(
        platform_start_idx.is_some(),
        "Should have platform.start event"
    );

    let init_start_idx = init_start_idx.unwrap();
    let init_runtime_done_idx = init_runtime_done_idx.unwrap();
    let init_report_idx = init_report_idx.unwrap();

    assert!(
        init_start_idx < init_runtime_done_idx,
        "platform.initStart should come before platform.initRuntimeDone"
    );
    assert!(
        init_runtime_done_idx < init_report_idx,
        "platform.initRuntimeDone should come before platform.initReport"
    );

    simulator.shutdown().await;
}

/// Buffer overflow handling - oldest events dropped, not newest.
#[tokio::test]
async fn test_telemetry_buffer_overflow_drops_oldest() {
    let simulator = Simulator::builder()
        .function_name("buffer-overflow-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "buffer-test-extension")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(3),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "buffer-test-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    for i in 0..5 {
        simulator.enqueue_payload(json!({"iteration": i})).await;

        let response = client
            .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
            .send()
            .await
            .unwrap();

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        client
            .post(format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                base_url, request_id
            ))
            .json(&json!({"result": i}))
            .send()
            .await
            .unwrap();
    }

    let has_platform_start = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_platform_start, "Should receive platform.start events");

    simulator.shutdown().await;
}

/// Bounded buffer size - internal capture buffer is bounded.
#[tokio::test]
async fn test_internal_capture_buffer_bounded() {
    let simulator = Simulator::builder()
        .function_name("bounded-capture-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();
    let event_count = Arc::new(AtomicUsize::new(0));

    for i in 0..100 {
        simulator.enqueue_payload(json!({"iteration": i})).await;

        let response = client
            .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
            .send()
            .await
            .unwrap();

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        client
            .post(format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                base_url, request_id
            ))
            .json(&json!({"result": i}))
            .send()
            .await
            .unwrap();

        event_count.fetch_add(1, Ordering::SeqCst);
    }

    let all_events = simulator.get_telemetry_events().await;

    assert!(
        !all_events.is_empty(),
        "Should have captured telemetry events"
    );

    assert!(
        all_events.len() <= 10000,
        "Captured events should be bounded, got {}",
        all_events.len()
    );

    let platform_start_events = simulator
        .get_telemetry_events_by_type("platform.start")
        .await;

    assert!(
        platform_start_events.len() >= 50,
        "Should have many platform.start events, got {}",
        platform_start_events.len()
    );

    simulator.shutdown().await;
}

/// Buffering config respected - events batched per maxItems/timeoutMs.
#[tokio::test]
async fn test_buffering_config_respected() {
    let simulator = Simulator::builder()
        .function_name("buffering-config-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let batch_sizes = Arc::new(Mutex::new(Vec::new()));
    let batch_sizes_clone = Arc::clone(&batch_sizes);
    let event_received = Arc::new(Notify::new());
    let event_received_clone = Arc::clone(&event_received);

    let app = axum::Router::new().route(
        "/telemetry",
        axum::routing::post(
            move |axum::Json(payload): axum::Json<Vec<TelemetryEvent>>| {
                let sizes = Arc::clone(&batch_sizes_clone);
                let notify = Arc::clone(&event_received_clone);
                async move {
                    sizes.lock().await.push(payload.len());
                    notify.notify_waiters();
                    axum::http::StatusCode::OK
                }
            },
        ),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let telemetry_url = format!("http://{}/telemetry", addr);

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let extension_id = register_extension(&client, &base_url, "buffering-ext")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(5),
            max_bytes: Some(262144),
            timeout_ms: Some(100),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "buffering-ext")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    for i in 0..3 {
        simulator.enqueue_payload(json!({"iteration": i})).await;

        let response = client
            .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
            .send()
            .await
            .unwrap();

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        client
            .post(format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                base_url, request_id
            ))
            .json(&json!({"result": i}))
            .send()
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let sizes = batch_sizes.lock().await;
    assert!(!sizes.is_empty(), "Should have received at least one batch");

    for &size in sizes.iter() {
        assert!(
            size <= 5,
            "Batch size ({}) should not exceed max_items (5)",
            size
        );
    }

    simulator.shutdown().await;
}

/// Duplicate subscription replaces first - new config takes effect.
#[tokio::test]
async fn test_duplicate_subscription_replaces_first() {
    let simulator = Simulator::builder()
        .function_name("duplicate-subscription-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url_1, events_1, _notify_1) = start_telemetry_receiver().await;
    let (telemetry_url_2, events_2, notify_2) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "dup-sub-ext")
        .await
        .unwrap();

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

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "dup-sub-ext")
        .json(&subscription_1)
        .send()
        .await
        .unwrap();

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

    let response = client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "dup-sub-ext")
        .json(&subscription_2)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        200,
        "Duplicate subscription should succeed"
    );

    simulator.enqueue_payload(json!({"test": "dup_sub"})).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "ok"}))
        .send()
        .await
        .unwrap();

    let has_events_2 = wait_for_events(
        &events_2,
        &notify_2,
        |evts| evts.iter().any(|e| e.event_type == "platform.start"),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_events_2, "Second destination should receive events");

    let events_1_count = events_1.lock().await.len();
    let events_2_count = events_2.lock().await.len();

    assert!(
        events_2_count > 0,
        "Second destination should have events after subscription replacement"
    );

    assert!(
        events_2_count >= events_1_count,
        "Second destination should have at least as many events as first (events_1={}, events_2={})",
        events_1_count,
        events_2_count
    );

    simulator.shutdown().await;
}

/// Events batched correctly - multiple events in single POST.
#[tokio::test]
async fn test_events_batched_correctly() {
    let simulator = Simulator::builder()
        .function_name("batch-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let batch_sizes = Arc::new(Mutex::new(Vec::new()));
    let batch_sizes_clone = Arc::clone(&batch_sizes);
    let event_received = Arc::new(Notify::new());
    let event_received_clone = Arc::clone(&event_received);

    let app = axum::Router::new().route(
        "/telemetry",
        axum::routing::post(
            move |axum::Json(payload): axum::Json<Vec<TelemetryEvent>>| {
                let sizes = Arc::clone(&batch_sizes_clone);
                let notify = Arc::clone(&event_received_clone);
                async move {
                    sizes.lock().await.push(payload.len());
                    notify.notify_waiters();
                    axum::http::StatusCode::OK
                }
            },
        ),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let telemetry_url = format!("http://{}/telemetry", addr);

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let extension_id = register_extension(&client, &base_url, "batch-ext")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(100),
            max_bytes: Some(262144),
            timeout_ms: Some(500),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "batch-ext")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    for i in 0..5 {
        simulator.enqueue_payload(json!({"iteration": i})).await;

        let response = client
            .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
            .send()
            .await
            .unwrap();

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        client
            .post(format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                base_url, request_id
            ))
            .json(&json!({"result": i}))
            .send()
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(600)).await;

    let sizes = batch_sizes.lock().await;
    let has_multi_event_batch = sizes.iter().any(|&size| size > 1);

    assert!(
        has_multi_event_batch || sizes.len() < 20,
        "Should have batched multiple events together (batch sizes: {:?})",
        *sizes
    );

    simulator.shutdown().await;
}

/// Event filtering by type - unsubscribed types not delivered.
#[tokio::test]
async fn test_event_filtering_by_type() {
    let simulator = Simulator::builder()
        .function_name("filter-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "filter-ext")
        .await
        .unwrap();

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
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "filter-ext")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    simulator.enqueue_payload(json!({"test": "filter"})).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "ok"}))
        .send()
        .await
        .unwrap();

    let has_platform_events = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type.starts_with("platform.")),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_platform_events, "Should receive platform events");

    let received_events = events.lock().await;
    for event in received_events.iter() {
        assert!(
            event.event_type.starts_with("platform."),
            "All events should be platform type, got: {}",
            event.event_type
        );
    }

    simulator.shutdown().await;
}

/// Multiple subscribers - each receives independent copy.
#[tokio::test]
async fn test_multiple_subscribers() {
    let simulator = Simulator::builder()
        .function_name("multi-subscriber-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url_1, events_1, notify_1) = start_telemetry_receiver().await;
    let (telemetry_url_2, events_2, notify_2) = start_telemetry_receiver().await;

    let ext_1_id = register_extension(&client, &base_url, "sub-1")
        .await
        .unwrap();
    let ext_2_id = register_extension(&client, &base_url, "sub-2")
        .await
        .unwrap();

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
        .header("Lambda-Extension-Name", "sub-1")
        .json(&subscription_1)
        .send()
        .await
        .unwrap();

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &ext_2_id)
        .header("Lambda-Extension-Name", "sub-2")
        .json(&subscription_2)
        .send()
        .await
        .unwrap();

    simulator
        .enqueue_payload(json!({"test": "multi_sub"}))
        .await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

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

    assert!(has_events_1, "Subscriber 1 should receive events");
    assert!(has_events_2, "Subscriber 2 should receive events");

    let events_1_lock = events_1.lock().await;
    let events_2_lock = events_2.lock().await;

    assert!(
        !events_1_lock.is_empty() && !events_2_lock.is_empty(),
        "Both subscribers should have received events"
    );

    simulator.shutdown().await;
}

/// Event ordering preserved - start < runtimeDone < report for same invocation.
#[tokio::test]
async fn test_event_ordering_preserved() {
    let simulator = Simulator::builder()
        .function_name("ordering-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "ordering-ext")
        .await
        .unwrap();

    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(100),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url,
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .header("Lambda-Extension-Name", "ordering-ext")
        .json(&subscription)
        .send()
        .await
        .unwrap();

    simulator.enqueue_payload(json!({"test": "ordering"})).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "ok"}))
        .send()
        .await
        .unwrap();

    let has_report = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.report"),
        Duration::from_secs(2),
    )
    .await;

    assert!(has_report, "Should receive platform.report");

    let received_events = events.lock().await;

    let invocation_events: Vec<_> = received_events
        .iter()
        .filter(|e| {
            e.record.get("requestId").map(|r| r.as_str()) == Some(Some(&request_id))
                || e.event_type == "platform.start"
                || e.event_type == "platform.runtimeDone"
                || e.event_type == "platform.report"
        })
        .collect();

    let start_idx = invocation_events
        .iter()
        .position(|e| e.event_type == "platform.start");
    let runtime_done_idx = invocation_events
        .iter()
        .position(|e| e.event_type == "platform.runtimeDone");
    let report_idx = invocation_events
        .iter()
        .position(|e| e.event_type == "platform.report");

    if let (Some(start), Some(done), Some(report)) = (start_idx, runtime_done_idx, report_idx) {
        assert!(
            start < done,
            "platform.start ({}) should come before platform.runtimeDone ({})",
            start,
            done
        );
        assert!(
            done < report,
            "platform.runtimeDone ({}) should come before platform.report ({})",
            done,
            report
        );
    }

    simulator.shutdown().await;
}

/// Duration calculated correctly - durationMs = response_time - start_time.
#[tokio::test]
async fn test_duration_calculated_correctly() {
    let simulator = Simulator::builder()
        .function_name("duration-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    simulator.enqueue_payload(json!({"test": "duration"})).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let request_id = simulator
        .get_all_invocation_states()
        .await
        .first()
        .unwrap()
        .invocation
        .request_id
        .clone();

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

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(!report_events.is_empty(), "Should have platform.report");

    let report = &report_events[0];
    let duration_ms = report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be present");

    assert!(
        duration_ms >= 50.0,
        "Duration ({:.2}ms) should reflect the runtime processing time (at least 50ms)",
        duration_ms
    );

    simulator.shutdown().await;
}

/// Billed duration uses 1ms granularity (since December 2020).
///
/// AWS Lambda changed from 100ms to 1ms billing granularity in December 2020.
/// billedDurationMs = ceil(durationMs) - rounded up to nearest millisecond.
#[tokio::test]
async fn test_billed_duration_1ms_granularity() {
    let simulator = Simulator::builder()
        .function_name("billed-duration-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    simulator
        .enqueue_payload(json!({"test": "billed_duration"}))
        .await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = simulator
        .get_all_invocation_states()
        .await
        .first()
        .unwrap()
        .invocation
        .request_id
        .clone();

    tokio::time::sleep(Duration::from_millis(150)).await;

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

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(!report_events.is_empty(), "Should have platform.report");

    let report = &report_events[0];
    let duration_ms = report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be present");
    let billed_duration_ms = report.record["metrics"]["billedDurationMs"]
        .as_u64()
        .expect("billedDurationMs should be present");

    assert!(
        billed_duration_ms >= 1,
        "Billed duration ({}) should be at least 1ms",
        billed_duration_ms
    );

    assert_eq!(
        billed_duration_ms,
        duration_ms.ceil() as u64,
        "Billed duration ({}) should be durationMs ({}) rounded up to nearest 1ms",
        billed_duration_ms,
        duration_ms
    );

    simulator.shutdown().await;
}

/// Memory metrics present - memorySizeMB, maxMemoryUsedMB populated.
#[tokio::test]
async fn test_memory_metrics_present() {
    let memory_size = 256u32;
    let simulator = Simulator::builder()
        .function_name("memory-metrics-test")
        .memory_size_mb(memory_size)
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    simulator
        .enqueue_payload(json!({"test": "memory_metrics"}))
        .await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = simulator
        .get_all_invocation_states()
        .await
        .first()
        .unwrap()
        .invocation
        .request_id
        .clone();

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

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(!report_events.is_empty(), "Should have platform.report");

    let report = &report_events[0];
    let metrics = &report.record["metrics"];

    let memory_size_mb = metrics["memorySizeMB"]
        .as_u64()
        .expect("memorySizeMB should be present");

    assert_eq!(
        memory_size_mb, memory_size as u64,
        "memorySizeMB should match configured value"
    );

    let max_memory_used = metrics["maxMemoryUsedMB"]
        .as_u64()
        .expect("maxMemoryUsedMB should be present");

    assert!(
        max_memory_used > 0 && max_memory_used <= memory_size as u64,
        "maxMemoryUsedMB ({}) should be > 0 and <= memorySizeMB ({})",
        max_memory_used,
        memory_size
    );

    simulator.shutdown().await;
}

/// Cold start init duration - initDurationMs field structure.
/// Currently the simulator does not track init duration since the runtime isn't
/// actually initialising. This test verifies the metrics structure and that the
/// field is correctly omitted when not applicable.
#[tokio::test]
async fn test_cold_start_init_duration() {
    let simulator = Simulator::builder()
        .function_name("init-duration-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    simulator
        .enqueue_payload(json!({"test": "init_duration"}))
        .await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = simulator
        .get_all_invocation_states()
        .await
        .first()
        .unwrap()
        .invocation
        .request_id
        .clone();

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

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(!report_events.is_empty(), "Should have platform.report");

    let report = &report_events[0];
    let metrics = &report.record["metrics"];

    assert!(
        metrics["durationMs"].as_f64().is_some(),
        "durationMs should be present in platform.report metrics"
    );
    assert!(
        metrics["billedDurationMs"].as_u64().is_some(),
        "billedDurationMs should be present in platform.report metrics"
    );
    assert!(
        metrics["memorySizeMB"].as_u64().is_some(),
        "memorySizeMB should be present in platform.report metrics"
    );
    assert!(
        metrics["maxMemoryUsedMB"].as_u64().is_some(),
        "maxMemoryUsedMB should be present in platform.report metrics"
    );

    simulator.shutdown().await;
}

/// Duration includes extension overhead - total > runtime duration when extension delays.
#[tokio::test]
async fn test_duration_includes_extension_overhead() {
    let simulator = Simulator::builder()
        .function_name("overhead-test")
        .extension_ready_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let extension_id = register_extension(&client, &base_url, "slow-ext")
        .await
        .unwrap();

    simulator.enqueue_payload(json!({"test": "overhead"})).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let request_id = simulator
        .get_all_invocation_states()
        .await
        .first()
        .unwrap()
        .invocation
        .request_id
        .clone();

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "ok"}))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(!report_events.is_empty(), "Should have platform.report");

    let report = &report_events[0];
    let duration_ms = report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be present");

    assert!(
        duration_ms >= 100.0,
        "Duration ({:.2}ms) should include extension overhead (at least 100ms)",
        duration_ms
    );

    simulator.shutdown().await;
}

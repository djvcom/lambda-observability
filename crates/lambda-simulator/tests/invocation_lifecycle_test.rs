//! Tests for invocation lifecycle events and telemetry (INV-* test scenarios).

use lambda_simulator::{
    EventType, InvocationBuilder, Simulator, TelemetryEvent, TelemetryEventType,
    TelemetrySubscription,
};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

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

async fn subscribe_to_telemetry(
    client: &Client,
    base_url: &str,
    extension_id: &str,
    telemetry_url: &str,
) {
    let subscription = TelemetrySubscription {
        types: vec![TelemetryEventType::Platform],
        buffering: Some(lambda_simulator::telemetry::BufferingConfig {
            max_items: Some(100),
            max_bytes: Some(262144),
            timeout_ms: Some(25),
        }),
        destination: lambda_simulator::telemetry::Destination {
            protocol: "HTTP".to_string(),
            uri: telemetry_url.to_string(),
        },
    };

    client
        .put(format!("{}/2022-07-01/telemetry", base_url))
        .header("Lambda-Extension-Identifier", extension_id)
        .header("Lambda-Extension-Name", "test-extension")
        .json(&subscription)
        .send()
        .await
        .unwrap();
}

/// platform.start emitted when runtime receives invocation from /next.
#[tokio::test]
async fn test_platform_start_emitted_when_runtime_calls_next() {
    let simulator = Simulator::builder()
        .function_name("platform-start-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "telemetry-ext")
        .await
        .unwrap();

    subscribe_to_telemetry(&client, &base_url, &extension_id, &telemetry_url).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "platform_start"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    // Simulate runtime calling /next - this triggers platform.start
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

    assert!(
        has_platform_start,
        "platform.start should be emitted when runtime calls /next"
    );

    let received_events = events.lock().await;
    let platform_start = received_events
        .iter()
        .find(|e| e.event_type == "platform.start")
        .expect("Should have platform.start event");

    assert_eq!(
        platform_start.record["requestId"],
        json!(request_id),
        "platform.start should have correct request_id"
    );

    assert!(
        platform_start.time.timestamp() > 0,
        "platform.start should have valid timestamp"
    );

    drop(received_events);
    let _ = next_handle.await;

    simulator.shutdown().await;
}

/// platform.runtimeDone emitted when runtime submits response.
#[tokio::test]
async fn test_platform_runtime_done_emitted_on_response() {
    let simulator = Simulator::builder()
        .function_name("runtime-done-test")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "telemetry-ext")
        .await
        .unwrap();

    subscribe_to_telemetry(&client, &base_url, &extension_id, &telemetry_url).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "runtime_done"}))
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

    let has_runtime_done = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.runtimeDone"),
        Duration::from_secs(2),
    )
    .await;

    assert!(
        has_runtime_done,
        "platform.runtimeDone should be emitted on response"
    );

    let received_events = events.lock().await;
    let runtime_done = received_events
        .iter()
        .find(|e| e.event_type == "platform.runtimeDone")
        .expect("Should have platform.runtimeDone event");

    assert_eq!(
        runtime_done.record["requestId"],
        json!(request_id),
        "platform.runtimeDone should have correct request_id"
    );

    assert_eq!(
        runtime_done.record["status"],
        json!("success"),
        "platform.runtimeDone should have success status"
    );

    let metrics = &runtime_done.record["metrics"];
    assert!(
        metrics.get("durationMs").is_some(),
        "platform.runtimeDone should have durationMs metric"
    );

    simulator.shutdown().await;
}

/// platform.report emitted after all extensions signal readiness.
#[tokio::test]
async fn test_platform_report_emitted_after_extensions_ready() {
    let simulator = Simulator::builder()
        .function_name("report-test")
        .extension_ready_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "telemetry-ext")
        .await
        .unwrap();

    subscribe_to_telemetry(&client, &base_url, &extension_id, &telemetry_url).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "report"}))
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

    tokio::spawn({
        let client = Client::new();
        let base_url = base_url.clone();
        let extension_id = extension_id.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            client
                .get(format!("{}/2020-01-01/extension/event/next", base_url))
                .header("Lambda-Extension-Identifier", &extension_id)
                .send()
                .await
                .unwrap();
        }
    });

    let has_report = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.report"),
        Duration::from_secs(2),
    )
    .await;

    assert!(
        has_report,
        "platform.report should be emitted after extensions ready"
    );

    let received_events = events.lock().await;
    let report = received_events
        .iter()
        .find(|e| e.event_type == "platform.report")
        .expect("Should have platform.report event");

    assert_eq!(
        report.record["requestId"],
        json!(request_id),
        "platform.report should have correct request_id"
    );

    let metrics = &report.record["metrics"];
    assert!(
        metrics.get("durationMs").is_some(),
        "platform.report should have durationMs"
    );
    assert!(
        metrics.get("billedDurationMs").is_some(),
        "platform.report should have billedDurationMs"
    );
    assert!(
        metrics.get("memorySizeMB").is_some(),
        "platform.report should have memorySizeMB"
    );
    assert!(
        metrics.get("maxMemoryUsedMB").is_some(),
        "platform.report should have maxMemoryUsedMB"
    );

    simulator.shutdown().await;
}

/// Zero-byte invocation payload is handled correctly.
#[tokio::test]
async fn test_zero_byte_payload_handled_correctly() {
    let simulator = Simulator::builder()
        .function_name("zero-payload-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!(null))
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

    let returned_request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .unwrap()
        .to_str()
        .unwrap();

    assert_eq!(returned_request_id, request_id);

    let payload: serde_json::Value = response.json().await.unwrap();
    assert!(payload.is_null(), "Null payload should be returned as null");

    let response_result = client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "handled_null"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response_result.status(), 202);

    simulator.shutdown().await;
}

/// Large invocation payload (approaching 6MB limit) is handled.
#[tokio::test]
async fn test_large_payload_handled_correctly() {
    let simulator = Simulator::builder()
        .function_name("large-payload-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let large_string = "x".repeat(1024 * 1024);
    let large_payload = json!({
        "data": large_string,
        "metadata": {
            "size": "1MB",
            "type": "test"
        }
    });

    let invocation = InvocationBuilder::new()
        .payload(large_payload.clone())
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

    let returned_payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        returned_payload["data"].as_str().unwrap().len(),
        1024 * 1024,
        "Large payload should be returned intact"
    );

    let response_result = client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "handled_large"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response_result.status(), 202);

    simulator.shutdown().await;
}

/// platform.runtimeDone has error status when runtime reports error.
#[tokio::test]
async fn test_runtime_done_emitted_with_error_status() {
    let simulator = Simulator::builder()
        .function_name("error-status-test")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "telemetry-ext")
        .await
        .unwrap();

    subscribe_to_telemetry(&client, &base_url, &extension_id, &telemetry_url).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "error"}))
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
            "{}/2018-06-01/runtime/invocation/{}/error",
            base_url, request_id
        ))
        .json(&json!({
            "errorType": "TestError",
            "errorMessage": "Intentional test error"
        }))
        .send()
        .await
        .unwrap();

    let has_runtime_done = wait_for_events(
        &events,
        &notify,
        |evts| evts.iter().any(|e| e.event_type == "platform.runtimeDone"),
        Duration::from_secs(2),
    )
    .await;

    assert!(
        has_runtime_done,
        "platform.runtimeDone should be emitted on error"
    );

    let received_events = events.lock().await;
    let runtime_done = received_events
        .iter()
        .find(|e| e.event_type == "platform.runtimeDone")
        .expect("Should have platform.runtimeDone event");

    assert_eq!(
        runtime_done.record["status"],
        json!("error"),
        "platform.runtimeDone should have error status"
    );

    assert_eq!(
        runtime_done.record["requestId"],
        json!(request_id),
        "platform.runtimeDone should have correct request_id"
    );

    simulator.shutdown().await;
}

/// platform.report is still emitted after an error response.
#[tokio::test]
async fn test_platform_report_emitted_after_error() {
    let simulator = Simulator::builder()
        .function_name("report-after-error-test")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let (telemetry_url, events, notify) = start_telemetry_receiver().await;

    let extension_id = register_extension(&client, &base_url, "telemetry-ext")
        .await
        .unwrap();

    subscribe_to_telemetry(&client, &base_url, &extension_id, &telemetry_url).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "error_report"}))
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
            "{}/2018-06-01/runtime/invocation/{}/error",
            base_url, request_id
        ))
        .json(&json!({
            "errorType": "TestError",
            "errorMessage": "Error for report test"
        }))
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

    assert!(
        has_report,
        "platform.report should be emitted even after error"
    );

    let received_events = events.lock().await;
    let report = received_events
        .iter()
        .find(|e| e.event_type == "platform.report")
        .expect("Should have platform.report event");

    assert_eq!(
        report.record["requestId"],
        json!(request_id),
        "platform.report should have correct request_id"
    );

    assert_eq!(
        report.record["status"],
        json!("error"),
        "platform.report should have error status"
    );

    simulator.shutdown().await;
}

/// Trace ID is propagated to runtime via header.
#[tokio::test]
async fn test_trace_id_propagated_to_runtime() {
    let simulator = Simulator::builder()
        .function_name("trace-propagation-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enqueue_payload(json!({"test": "trace"})).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let trace_id = response
        .headers()
        .get("Lambda-Runtime-Trace-Id")
        .expect("Lambda-Runtime-Trace-Id header should be present")
        .to_str()
        .unwrap();

    assert!(
        trace_id.starts_with("Root="),
        "Trace ID should start with 'Root='"
    );

    simulator.shutdown().await;
}

/// Trace context is included in platform.start telemetry event.
#[tokio::test]
async fn test_trace_context_included_in_platform_start() {
    let simulator = Simulator::builder()
        .function_name("trace-in-start-test")
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "trace_context"}))
        .build()
        .unwrap();

    let expected_trace_id = invocation.trace_id.clone();
    simulator.enqueue(invocation).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let platform_start_events = simulator
        .get_telemetry_events_by_type("platform.start")
        .await;

    assert!(
        !platform_start_events.is_empty(),
        "Should have platform.start events"
    );

    let start_event = &platform_start_events[platform_start_events.len() - 1];

    let tracing = &start_event.record["tracing"];
    assert!(
        tracing.is_object(),
        "platform.start should have tracing object"
    );

    assert_eq!(
        tracing["type"],
        json!("X-Amzn-Trace-Id"),
        "Tracing type should be X-Amzn-Trace-Id"
    );

    assert_eq!(
        tracing["value"],
        json!(expected_trace_id),
        "Tracing value should match the invocation trace_id"
    );

    simulator.shutdown().await;
}

/// Auto-generated trace ID has valid format.
#[tokio::test]
async fn test_trace_id_format_validated() {
    let simulator = Simulator::builder()
        .function_name("trace-format-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enqueue_payload(json!({"test": "format"})).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let trace_id = response
        .headers()
        .get("Lambda-Runtime-Trace-Id")
        .unwrap()
        .to_str()
        .unwrap();

    let trace_regex = regex::Regex::new(r"^Root=1-[0-9a-f]{8}-[0-9a-f]{24}$").unwrap();
    assert!(
        trace_regex.is_match(trace_id),
        "Trace ID '{}' should match format Root=1-{{8-hex}}-{{24-hex}}",
        trace_id
    );

    simulator.shutdown().await;
}

/// Trace context is included in INVOKE extension event.
#[tokio::test]
async fn test_trace_context_included_in_invoke_event() {
    let simulator = Simulator::builder()
        .function_name("trace-in-invoke-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(&client, &base_url, "trace-ext")
        .await
        .unwrap();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "trace_invoke"}))
        .build()
        .unwrap();

    let expected_trace_id = invocation.trace_id.clone();
    simulator.enqueue(invocation).await;

    let invoke_response = client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .send()
        .await
        .unwrap();

    assert_eq!(invoke_response.status(), 200);

    let invoke_event: serde_json::Value = invoke_response.json().await.unwrap();

    assert_eq!(
        invoke_event["eventType"],
        json!("INVOKE"),
        "Should receive INVOKE event"
    );

    let tracing = &invoke_event["tracing"];
    assert!(
        tracing.is_object(),
        "INVOKE event should have tracing object"
    );

    assert_eq!(
        tracing["type"],
        json!("X-Amzn-Trace-Id"),
        "Tracing type should be X-Amzn-Trace-Id"
    );

    assert_eq!(
        tracing["value"],
        json!(expected_trace_id),
        "Tracing value should match the invocation trace_id"
    );

    simulator.shutdown().await;
}

/// Trace context is preserved through to platform.report.
#[tokio::test]
async fn test_trace_context_included_in_platform_report() {
    let simulator = Simulator::builder()
        .function_name("trace-in-report-test")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();

    simulator.enable_telemetry_capture().await;

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "trace_report"}))
        .build()
        .unwrap();

    let expected_trace_id = invocation.trace_id.clone();
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

    let _ = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(2))
        .await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert!(
        !report_events.is_empty(),
        "Should have platform.report events"
    );

    let report = report_events
        .iter()
        .find(|e| e.record["requestId"] == json!(request_id))
        .expect("Should have platform.report for this request");

    let tracing = &report.record["tracing"];
    assert!(
        tracing.is_object(),
        "platform.report should have tracing object"
    );

    assert_eq!(
        tracing["type"],
        json!("X-Amzn-Trace-Id"),
        "Tracing type should be X-Amzn-Trace-Id"
    );

    assert_eq!(
        tracing["value"],
        json!(expected_trace_id),
        "Tracing value should be preserved through to platform.report"
    );

    simulator.shutdown().await;
}

/// When trace ID is not provided, a valid one is auto-generated.
#[tokio::test]
async fn test_auto_generated_trace_id_is_valid() {
    let simulator = Simulator::builder()
        .function_name("auto-trace-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    for _ in 0..3 {
        simulator
            .enqueue_payload(json!({"test": "auto_trace"}))
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

        let trace_id = response
            .headers()
            .get("Lambda-Runtime-Trace-Id")
            .expect("Lambda-Runtime-Trace-Id should be present")
            .to_str()
            .unwrap();

        assert!(
            !trace_id.is_empty(),
            "Auto-generated trace ID should not be empty"
        );
        assert!(
            trace_id.starts_with("Root=1-"),
            "Auto-generated trace ID should start with 'Root=1-'"
        );

        client
            .post(format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                base_url, request_id
            ))
            .json(&json!({"result": "ok"}))
            .send()
            .await
            .unwrap();
    }

    simulator.shutdown().await;
}

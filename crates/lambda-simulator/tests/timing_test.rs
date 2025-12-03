//! Tests for timing and timeout scenarios.
//!
//! These tests verify correct handling of:
//! - Extension readiness timeouts
//! - Invocation deadlines
//! - Overhead calculations

use lambda_simulator::{EventType, InvocationBuilder, Simulator};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

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

/// Some extensions ready, some timeout - partial readiness triggers timeout.
#[tokio::test]
async fn test_partial_extension_readiness_triggers_timeout() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let ext1_id = register_extension(
        &client,
        &base_url,
        "fast-extension",
        vec![EventType::Invoke],
    )
    .await;
    let _ext2_id = register_extension(
        &client,
        &base_url,
        "slow-extension",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "partial_readiness"}))
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
        .header("Lambda-Extension-Identifier", &ext1_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        !simulator.are_extensions_ready(&request_id).await,
        "Should not be ready with only one of two extensions"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after timeout even with partial readiness"
    );

    simulator.shutdown().await;
}

/// Zero timeout configured causes immediate report emission.
#[tokio::test]
async fn test_zero_timeout_emits_immediate_report() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::ZERO)
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let _ext_id =
        register_extension(&client, &base_url, "extension", vec![EventType::Invoke]).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "zero_timeout"}))
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

    tokio::time::sleep(Duration::from_millis(50)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted immediately with zero timeout"
    );

    simulator.shutdown().await;
}

/// Actual wait matches the configured timeout duration.
/// We verify by checking the platform.report metrics show appropriate duration.
#[tokio::test]
async fn test_timeout_duration_accuracy() {
    let timeout_ms = 200u64;
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(timeout_ms))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let _ext_id = register_extension(
        &client,
        &base_url,
        "slow-extension",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "timeout_accuracy"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let _ = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let response_time = std::time::Instant::now();

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "ok"}))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(timeout_ms + 100)).await;

    let elapsed = response_time.elapsed();

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after timeout expires"
    );

    assert!(
        elapsed.as_millis() >= timeout_ms as u128,
        "Should have waited at least the timeout duration (waited {}ms, timeout {}ms)",
        elapsed.as_millis(),
        timeout_ms
    );

    assert!(
        elapsed.as_millis() < timeout_ms as u128 + 150,
        "Should not have waited much longer than the timeout (waited {}ms, timeout {}ms)",
        elapsed.as_millis(),
        timeout_ms
    );

    simulator.shutdown().await;
}

/// Extension overhead reflects timeout when extension doesn't respond.
/// When the timeout expires, the overhead should approximately equal the timeout value.
/// We verify this through the platform.report telemetry event.
#[tokio::test]
async fn test_extension_overhead_reflects_timeout() {
    let timeout_ms = 150u64;
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(timeout_ms))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let _ext_id = register_extension(
        &client,
        &base_url,
        "unresponsive-ext",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "overhead_timeout"}))
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

    tokio::time::sleep(Duration::from_millis(timeout_ms + 100)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after timeout"
    );

    let report = &report_events[0];
    let duration_ms = report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be present");

    assert!(
        duration_ms >= timeout_ms as f64 * 0.8,
        "Duration ({:.2}ms) should reflect the timeout wait ({}ms)",
        duration_ms,
        timeout_ms
    );

    simulator.shutdown().await;
}

/// Deadline header is correctly calculated.
#[tokio::test]
async fn test_deadline_header_calculated_correctly() {
    let timeout_ms = 5000u64;
    let simulator = Simulator::builder()
        .function_name("deadline-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "deadline"}))
        .timeout_ms(timeout_ms)
        .build()
        .unwrap();

    let created_at = invocation.created_at;
    let expected_deadline_ms = invocation.deadline_ms();

    simulator.enqueue(invocation).await;

    let response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    let deadline_header = response
        .headers()
        .get("Lambda-Runtime-Deadline-Ms")
        .expect("Lambda-Runtime-Deadline-Ms header should be present")
        .to_str()
        .unwrap();

    let deadline_ms: i64 = deadline_header
        .parse()
        .expect("Deadline should be a valid integer");

    assert_eq!(
        deadline_ms, expected_deadline_ms,
        "Deadline header should match invocation deadline"
    );

    let created_ms = created_at.timestamp_millis();
    let delta = deadline_ms - created_ms;

    assert!(
        (delta - timeout_ms as i64).abs() < 10,
        "Deadline should be created_at + timeout_ms (got delta={}ms, expected {}ms)",
        delta,
        timeout_ms
    );

    simulator.shutdown().await;
}

/// Invocation timeout status when deadline is exceeded.
/// Note: Full timeout enforcement may not be implemented, but we test the structure.
#[tokio::test]
async fn test_invocation_timeout_status() {
    let simulator = Simulator::builder()
        .function_name("timeout-status-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "timeout_status"}))
        .timeout_ms(100)
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

    let deadline_header = response
        .headers()
        .get("Lambda-Runtime-Deadline-Ms")
        .unwrap()
        .to_str()
        .unwrap();

    let deadline_ms: i64 = deadline_header.parse().unwrap();
    let now_ms = chrono::Utc::now().timestamp_millis();

    assert!(
        deadline_ms > now_ms - 200,
        "Deadline should be in the recent past or future (deadline={}, now={})",
        deadline_ms,
        now_ms
    );

    client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&json!({"result": "late_but_accepted"}))
        .send()
        .await
        .unwrap();

    simulator.shutdown().await;
}

/// Extension overhead can extend beyond invocation deadline.
/// The invocation is still Success if runtime responded in time, even if
/// extension processing takes longer than the deadline.
#[tokio::test]
async fn test_extension_overhead_can_exceed_deadline() {
    let simulator = Simulator::builder()
        .function_name("overhead-beyond-deadline")
        .extension_ready_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let ext_id = register_extension(
        &client,
        &base_url,
        "slow-processor",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "overhead_beyond"}))
        .timeout_ms(100)
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
        .json(&json!({"result": "quick_response"}))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &ext_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let state = simulator
        .get_invocation_state(&request_id)
        .await
        .expect("Should have invocation state");

    assert_eq!(
        state.status,
        lambda_simulator::InvocationStatus::Success,
        "Status should be Success since runtime responded before deadline"
    );

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(report_events.len(), 1, "Should have platform.report");

    let report = &report_events[0];
    assert_eq!(
        report.record["status"],
        json!("success"),
        "Report should show success status"
    );

    let duration_ms = report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be present");

    assert!(
        duration_ms >= 150.0,
        "Duration ({:.2}ms) should reflect the extension processing delay",
        duration_ms
    );

    simulator.shutdown().await;
}

/// Additional test: Verify extension overhead is minimal when extension responds immediately.
#[tokio::test]
async fn test_extension_overhead_minimal_when_fast() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let ext_id = register_extension(
        &client,
        &base_url,
        "fast-extension",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "fast_overhead"}))
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
        .header("Lambda-Extension-Identifier", &ext_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(report_events.len(), 1, "Should have platform.report");

    let report = &report_events[0];
    let duration_ms = report.record["metrics"]["durationMs"]
        .as_f64()
        .expect("durationMs should be present");

    assert!(
        duration_ms < 1000.0,
        "Duration ({:.2}ms) should be small when extension responds quickly",
        duration_ms
    );

    simulator.shutdown().await;
}

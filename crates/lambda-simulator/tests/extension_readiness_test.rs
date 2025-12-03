//! Integration tests for extension readiness tracking.
//!
//! These tests verify that the Lambda lifecycle is correctly simulated:
//! 1. Extensions receive INVOKE events
//! 2. Runtime processes and responds
//! 3. Extensions do post-invocation work
//! 4. Extensions poll /next to signal readiness
//! 5. platform.report is emitted after all extensions are ready
//! 6. Process freezes after platform.report

use lambda_simulator::{EventType, InvocationBuilder, Simulator};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

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
async fn test_extension_readiness_with_single_extension() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let extension_id = register_extension(
        &client,
        &base_url,
        "test-extension",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "data"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();

    simulator.enqueue(invocation).await;

    assert!(
        !simulator.are_extensions_ready(&request_id).await,
        "Extensions should not be ready before runtime responds"
    );

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

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        !simulator.are_extensions_ready(&request_id).await,
        "Extensions should not be ready before polling /next"
    );

    let report_events_before = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert!(
        report_events_before.is_empty(),
        "platform.report should not be emitted before extensions are ready"
    );

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        simulator.are_extensions_ready(&request_id).await,
        "Extensions should be ready after polling /next"
    );

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after extensions are ready"
    );

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_extension_readiness_with_multiple_extensions() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let ext1_id =
        register_extension(&client, &base_url, "extension-1", vec![EventType::Invoke]).await;
    let ext2_id =
        register_extension(&client, &base_url, "extension-2", vec![EventType::Invoke]).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "multi"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();

    simulator.enqueue(invocation).await;

    client
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

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert!(
        report_events.is_empty(),
        "platform.report should not be emitted with only partial readiness"
    );

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &ext2_id)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        simulator.are_extensions_ready(&request_id).await,
        "Should be ready after all extensions poll /next"
    );

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after all extensions are ready"
    );

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_extension_readiness_timeout() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    register_extension(
        &client,
        &base_url,
        "slow-extension",
        vec![EventType::Invoke],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "timeout"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();

    simulator.enqueue(invocation).await;

    client
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

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after timeout even without extension readiness"
    );

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_no_invoke_extensions_immediate_report() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    register_extension(
        &client,
        &base_url,
        "shutdown-only",
        vec![EventType::Shutdown],
    )
    .await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "no-invoke"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();

    simulator.enqueue(invocation).await;

    assert!(
        simulator.are_extensions_ready(&request_id).await,
        "Should be immediately ready when no extensions subscribe to INVOKE"
    );

    client
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

    tokio::time::sleep(Duration::from_millis(100)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted immediately with no INVOKE subscribers"
    );

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_wait_for_extensions_ready_helper() {
    let simulator = Simulator::builder()
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let extension_id =
        register_extension(&client, &base_url, "wait-test", vec![EventType::Invoke]).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "wait"}))
        .build()
        .unwrap();
    let request_id = invocation.request_id.clone();

    simulator.enqueue(invocation).await;

    client
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

    let wait_result = tokio::time::timeout(
        Duration::from_millis(100),
        simulator.wait_for_extensions_ready(&request_id, Duration::from_secs(5)),
    )
    .await;

    assert!(
        wait_result.is_err(),
        "wait_for_extensions_ready should not complete before extension polls"
    );

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .send()
        .await
        .unwrap();

    simulator
        .wait_for_extensions_ready(&request_id, Duration::from_secs(5))
        .await
        .expect("wait_for_extensions_ready should complete after extension polls");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;
    assert_eq!(report_events.len(), 1, "platform.report should be emitted");

    simulator.shutdown().await;
}

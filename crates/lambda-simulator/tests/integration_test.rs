//! Integration tests for the Lambda runtime simulator.

use lambda_simulator::{InvocationBuilder, InvocationStatus, Simulator};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_basic_invocation_flow() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"message": "Hello, Lambda!"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let response = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    assert_eq!(response.status(), 200);

    let aws_request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .expect("Missing request ID header")
        .to_str()
        .unwrap();

    assert_eq!(aws_request_id, request_id);

    let payload: serde_json::Value = response.json().await.expect("Failed to parse payload");
    assert_eq!(payload["message"], "Hello, Lambda!");

    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    let response = client
        .post(&response_url)
        .json(&json!({"result": "success"}))
        .send()
        .await
        .expect("Failed to send response");

    assert_eq!(response.status(), 202);

    let state = simulator
        .get_invocation_state(&request_id)
        .await
        .expect("Invocation state not found");

    assert_eq!(state.status, InvocationStatus::Success);
    assert!(state.response.is_some());

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_invocation_error() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enqueue_payload(json!({"trigger": "error"})).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let response = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let request_id = response
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .expect("Missing request ID header")
        .to_str()
        .unwrap()
        .to_string();

    let error_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/error",
        runtime_api_url, request_id
    );

    let response = client
        .post(&error_url)
        .json(&json!({
            "errorType": "RuntimeError",
            "errorMessage": "Something went wrong",
            "stackTrace": ["line 1", "line 2"]
        }))
        .send()
        .await
        .expect("Failed to send error");

    assert_eq!(response.status(), 202);

    let state = simulator
        .get_invocation_state(&request_id)
        .await
        .expect("Invocation state not found");

    assert_eq!(state.status, InvocationStatus::Error);
    assert!(state.error.is_some());

    let error = state.error.unwrap();
    assert_eq!(error.error_type, "RuntimeError");
    assert_eq!(error.error_message, "Something went wrong");

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_init_error() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let init_error_url = format!("{}/2018-06-01/runtime/init/error", runtime_api_url);

    let response = client
        .post(&init_error_url)
        .json(&json!({
            "errorType": "InitError",
            "errorMessage": "Failed to initialize"
        }))
        .send()
        .await
        .expect("Failed to send init error");

    assert_eq!(response.status(), 200);

    let init_error = simulator.get_init_error().await;
    assert!(init_error.is_some());
    let error_msg = init_error.unwrap();
    assert!(error_msg.contains("InitError"));
    assert!(error_msg.contains("Failed to initialize"));

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_multiple_invocations() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    for i in 0..3 {
        simulator.enqueue_payload(json!({"iteration": i})).await;
    }

    for i in 0..3 {
        let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
        let response = client
            .get(&next_url)
            .send()
            .await
            .expect("Failed to get invocation");

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .expect("Missing request ID header")
            .to_str()
            .unwrap()
            .to_string();

        let payload: serde_json::Value = response.json().await.expect("Failed to parse payload");
        assert_eq!(payload["iteration"], i);

        let response_url = format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            runtime_api_url, request_id
        );
        client
            .post(&response_url)
            .json(&json!({"result": i}))
            .send()
            .await
            .expect("Failed to send response");
    }

    let states = simulator.get_all_invocation_states().await;
    assert_eq!(states.len(), 3);

    for state in states {
        assert_eq!(state.status, InvocationStatus::Success);
    }

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_long_poll_behavior() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);

    let request_future = async {
        client
            .get(&next_url)
            .send()
            .await
            .expect("Failed to get invocation")
    };

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        simulator.enqueue_payload(json!({"delayed": true})).await;
    });

    let response = timeout(Duration::from_secs(5), request_future)
        .await
        .expect("Request timed out");

    assert_eq!(response.status(), 200);

    let payload: serde_json::Value = response.json().await.expect("Failed to parse payload");
    assert_eq!(payload["delayed"], true);
}

#[tokio::test]
async fn test_headers_on_next_invocation() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enqueue_payload(json!({"test": "headers"})).await;

    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let response = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    let headers = response.headers();
    assert!(headers.contains_key("Lambda-Runtime-Aws-Request-Id"));
    assert!(headers.contains_key("Lambda-Runtime-Deadline-Ms"));
    assert!(headers.contains_key("Lambda-Runtime-Invoked-Function-Arn"));
    assert!(headers.contains_key("Lambda-Runtime-Trace-Id"));

    let deadline_str = headers
        .get("Lambda-Runtime-Deadline-Ms")
        .unwrap()
        .to_str()
        .unwrap();
    let deadline: i64 = deadline_str.parse().expect("Invalid deadline format");
    assert!(deadline > 0);

    let trace_id = headers
        .get("Lambda-Runtime-Trace-Id")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(trace_id.starts_with("Root="));

    simulator.shutdown().await;
}

/// Duplicate response handling - first response wins
#[tokio::test]
async fn test_duplicate_response_first_wins() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "duplicate"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    // Get the invocation
    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    // Submit first response
    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    let first_response = client
        .post(&response_url)
        .json(&json!({"result": "first"}))
        .send()
        .await
        .expect("Failed to send first response");

    assert_eq!(
        first_response.status(),
        202,
        "First response should be accepted"
    );

    // Submit duplicate response
    let second_response = client
        .post(&response_url)
        .json(&json!({"result": "second"}))
        .send()
        .await
        .expect("Failed to send second response");

    // Duplicate response should be rejected
    assert_eq!(
        second_response.status(),
        400,
        "Duplicate response should be rejected with 400 Bad Request"
    );

    // Verify the first response was recorded
    let state = simulator
        .get_invocation_state(&request_id)
        .await
        .expect("Invocation state not found");

    assert_eq!(state.status, InvocationStatus::Success);
    let response = state.response.expect("Response should be recorded");
    // First response should win
    assert_eq!(
        response.payload["result"], "first",
        "First response should be preserved"
    );

    simulator.shutdown().await;
}

/// Response after error should be rejected
#[tokio::test]
async fn test_response_after_error_rejected() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "error_then_response"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    // Get the invocation
    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get invocation");

    // Submit error first
    let error_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/error",
        runtime_api_url, request_id
    );
    let error_response = client
        .post(&error_url)
        .json(&json!({
            "errorType": "TestError",
            "errorMessage": "Intentional error"
        }))
        .send()
        .await
        .expect("Failed to send error");

    assert_eq!(error_response.status(), 202);

    // Now try to submit a success response
    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id
    );
    let late_response = client
        .post(&response_url)
        .json(&json!({"result": "late"}))
        .send()
        .await
        .expect("Failed to send late response");

    // Response after error should be rejected
    assert_eq!(
        late_response.status(),
        400,
        "Response after error should be rejected with 400 Bad Request"
    );

    // Status should remain Error
    let state = simulator
        .get_invocation_state(&request_id)
        .await
        .expect("Invocation state not found");

    assert_eq!(
        state.status,
        InvocationStatus::Error,
        "Status should remain Error after late response"
    );
    assert!(state.error.is_some(), "Error details should be preserved");

    simulator.shutdown().await;
}

/// Invalid request ID returns 404
#[tokio::test]
async fn test_invalid_request_id_returns_404() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    // Try to submit response for non-existent request
    let fake_request_id = "non-existent-request-id";
    let response_url = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, fake_request_id
    );

    let response = client
        .post(&response_url)
        .json(&json!({"result": "success"}))
        .send()
        .await
        .expect("Failed to send response");

    assert_eq!(
        response.status(),
        404,
        "Response for unknown request ID should return 404"
    );

    simulator.shutdown().await;
}

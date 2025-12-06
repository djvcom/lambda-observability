//! Integration tests for the Lambda Extensions API.

use lambda_simulator::{EventType, InvocationBuilder, LifecycleEvent, Simulator};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

/// Helper to register an extension and return the extension ID.
async fn register_extension(
    client: &Client,
    base_url: &str,
    name: &str,
    events: Vec<EventType>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .header("Lambda-Extension-Name", name)
        .json(&json!({ "events": events }))
        .send()
        .await?;

    assert_eq!(response.status(), 200);

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .ok_or("Missing Lambda-Extension-Identifier header")?
        .to_str()?
        .to_string();

    // Verify function name and version headers are present
    assert!(
        response
            .headers()
            .get("Lambda-Extension-Function-Name")
            .is_some()
    );
    assert!(
        response
            .headers()
            .get("Lambda-Extension-Function-Version")
            .is_some()
    );

    Ok(extension_id)
}

/// Helper to poll for the next event with a timeout.
async fn next_event_with_timeout(
    client: &Client,
    base_url: &str,
    extension_id: &str,
    timeout_duration: Duration,
) -> Result<LifecycleEvent, Box<dyn std::error::Error + Send + Sync>> {
    let request = client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", extension_id)
        .send();

    let response = timeout(timeout_duration, request).await??;
    assert_eq!(response.status(), 200);

    let event: LifecycleEvent = response.json().await?;
    Ok(event)
}

#[tokio::test]
async fn test_extension_registration() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Register an extension
    let extension_id = register_extension(
        &client,
        &base_url,
        "test-extension",
        vec![EventType::Invoke],
    )
    .await
    .unwrap();

    assert!(!extension_id.is_empty());

    // Verify the extension is registered
    let extensions = simulator.get_registered_extensions().await;
    assert_eq!(extensions.len(), 1);
    assert_eq!(extensions[0].name, "test-extension");
    assert_eq!(extensions[0].events, vec![EventType::Invoke]);
}

#[tokio::test]
async fn test_multiple_extensions() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Register multiple extensions
    let ext1_id = register_extension(&client, &base_url, "extension-1", vec![EventType::Invoke])
        .await
        .unwrap();

    let ext2_id = register_extension(
        &client,
        &base_url,
        "extension-2",
        vec![EventType::Invoke, EventType::Shutdown],
    )
    .await
    .unwrap();

    assert_ne!(ext1_id, ext2_id);
    assert_eq!(simulator.extension_count().await, 2);

    let extensions = simulator.get_registered_extensions().await;
    assert_eq!(extensions.len(), 2);
}

#[tokio::test]
async fn test_extension_receives_invoke_event() {
    let simulator = Simulator::builder()
        .function_name("test-fn")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Register extension subscribing to INVOKE events
    let extension_id = register_extension(
        &client,
        &base_url,
        "invoke-subscriber",
        vec![EventType::Invoke],
    )
    .await
    .unwrap();

    // Start polling for events in background
    let poll_client = client.clone();
    let poll_url = base_url.clone();
    let poll_ext_id = extension_id.clone();
    let poll_handle = tokio::spawn(async move {
        next_event_with_timeout(
            &poll_client,
            &poll_url,
            &poll_ext_id,
            Duration::from_secs(5),
        )
        .await
    });

    // Give the poll request time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Enqueue an invocation
    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "data"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    // Wait for the event
    let event = poll_handle.await.unwrap().unwrap();

    // Verify it's an INVOKE event with correct data
    match event {
        LifecycleEvent::Invoke {
            request_id: event_request_id,
            deadline_ms,
            invoked_function_arn,
            tracing,
        } => {
            assert_eq!(event_request_id, request_id);
            assert!(deadline_ms > 0);
            assert!(invoked_function_arn.starts_with("arn:aws:lambda:"));
            assert_eq!(tracing.trace_type, "X-Amzn-Trace-Id");
            assert!(tracing.value.starts_with("Root=1-"));
        }
        _ => panic!("Expected INVOKE event"),
    }
}

#[tokio::test]
async fn test_extension_not_subscribed_to_invoke() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Register extension NOT subscribing to INVOKE events
    let extension_id = register_extension(
        &client,
        &base_url,
        "shutdown-only",
        vec![EventType::Shutdown],
    )
    .await
    .unwrap();

    // Enqueue an invocation
    simulator.enqueue_payload(json!({"test": "data"})).await;

    // Try to get next event with short timeout - should timeout since no INVOKE sent
    let result = next_event_with_timeout(
        &client,
        &base_url,
        &extension_id,
        Duration::from_millis(500),
    )
    .await;

    assert!(
        result.is_err(),
        "Should timeout - no INVOKE event for this extension"
    );
}

#[tokio::test]
async fn test_combined_runtime_and_extension() {
    let simulator = Simulator::builder()
        .function_name("combined-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Register extension
    let extension_id = register_extension(
        &client,
        &base_url,
        "monitor-extension",
        vec![EventType::Invoke],
    )
    .await
    .unwrap();

    // Start extension polling
    let ext_client = client.clone();
    let ext_url = base_url.clone();
    let ext_id = extension_id.clone();
    let extension_handle = tokio::spawn(async move {
        next_event_with_timeout(&ext_client, &ext_url, &ext_id, Duration::from_secs(5)).await
    });

    // Start runtime polling
    let runtime_client = client.clone();
    let runtime_url = base_url.clone();
    let runtime_handle = tokio::spawn(async move {
        let response = runtime_client
            .get(format!(
                "{}/2018-06-01/runtime/invocation/next",
                runtime_url
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        response.text().await.unwrap()
    });

    // Give polls time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Enqueue invocation
    let invocation = InvocationBuilder::new()
        .payload(json!({"message": "hello"}))
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    // Both should receive their respective data
    let ext_event = extension_handle.await.unwrap().unwrap();
    let runtime_payload = runtime_handle.await.unwrap();

    // Verify extension got INVOKE event
    match ext_event {
        LifecycleEvent::Invoke {
            request_id: ext_req_id,
            ..
        } => {
            assert_eq!(ext_req_id, request_id);
        }
        _ => panic!("Expected INVOKE event"),
    }

    // Verify runtime got the payload
    let payload: serde_json::Value = serde_json::from_str(&runtime_payload).unwrap();
    assert_eq!(payload["message"], "hello");
}

#[tokio::test]
async fn test_multiple_invocations_to_extension() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id =
        register_extension(&client, &base_url, "multi-invoke", vec![EventType::Invoke])
            .await
            .unwrap();

    // Enqueue multiple invocations
    simulator.enqueue_payload(json!({"num": 1})).await;
    simulator.enqueue_payload(json!({"num": 2})).await;
    simulator.enqueue_payload(json!({"num": 3})).await;

    // Extension should receive all three INVOKE events
    for _ in 0..3 {
        let event =
            next_event_with_timeout(&client, &base_url, &extension_id, Duration::from_secs(2))
                .await
                .unwrap();

        assert!(matches!(event, LifecycleEvent::Invoke { .. }));
    }
}

#[tokio::test]
async fn test_extension_registration_requires_name_header() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Try to register without Lambda-Extension-Name header
    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Lambda-Extension-Name")
    );
}

#[tokio::test]
async fn test_next_event_requires_identifier_header() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Try to get next event without Lambda-Extension-Identifier header
    let response = client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
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
async fn test_next_event_with_invalid_extension_id() {
    let simulator = Simulator::builder().build().await.unwrap();
    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    // Try to get next event with non-existent extension ID
    let response = client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", "invalid-id-12345")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 403);
    assert!(response.text().await.unwrap().contains("not registered"));
}

/// Function metadata in response headers.
/// Registration response includes Lambda-Extension-Function-Name and Lambda-Extension-Function-Version.
#[tokio::test]
async fn test_registration_response_includes_function_metadata_headers() {
    let simulator = Simulator::builder()
        .function_name("metadata-test-function")
        .function_version("$LATEST")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .header("Lambda-Extension-Name", "metadata-ext")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let function_name = response
        .headers()
        .get("Lambda-Extension-Function-Name")
        .expect("Lambda-Extension-Function-Name header should be present")
        .to_str()
        .unwrap();

    assert_eq!(
        function_name, "metadata-test-function",
        "Function name should match configured value"
    );

    let function_version = response
        .headers()
        .get("Lambda-Extension-Function-Version")
        .expect("Lambda-Extension-Function-Version header should be present")
        .to_str()
        .unwrap();

    assert_eq!(
        function_version, "$LATEST",
        "Function version should match configured value"
    );

    simulator.shutdown().await;
}

/// Empty events array - registration succeeds but no events delivered.
#[tokio::test]
async fn test_registration_with_empty_events_array_succeeds() {
    let simulator = Simulator::builder()
        .function_name("empty-events-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .header("Lambda-Extension-Name", "no-events-ext")
        .json(&json!({ "events": [] }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        200,
        "Registration with empty events array should succeed"
    );

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .expect("Should have extension ID")
        .to_str()
        .unwrap()
        .to_string();

    assert!(!extension_id.is_empty());

    let extensions = simulator.get_registered_extensions().await;
    assert_eq!(extensions.len(), 1);
    assert!(
        extensions[0].events.is_empty(),
        "Extension should have no subscribed events"
    );

    simulator.enqueue_payload(json!({"test": "data"})).await;

    let result = timeout(
        Duration::from_millis(300),
        client
            .get(format!("{}/2020-01-01/extension/event/next", base_url))
            .header("Lambda-Extension-Identifier", &extension_id)
            .send(),
    )
    .await;

    assert!(
        result.is_err(),
        "Extension with empty events should not receive any events (should timeout)"
    );

    simulator.shutdown().await;
}

/// Registration after initialisation phase - rejected with 403.
///
/// Per the Lambda Extensions API specification, extensions can only register
/// during the initialization phase. Once the runtime calls `/next` for the
/// first time, the initialization phase ends and no new extensions can register.
#[tokio::test]
async fn test_registration_after_init_phase_rejected() {
    let simulator = Simulator::builder()
        .function_name("late-registration-test")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator
        .enqueue_payload(json!({"test": "first_invocation"}))
        .await;

    let runtime_response = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(runtime_response.status(), 200);

    let response = client
        .post(format!("{}/2020-01-01/extension/register", base_url))
        .header("Lambda-Extension-Name", "late-extension")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        403,
        "Registration after init phase should be rejected with 403 Forbidden"
    );

    let body = response.text().await.unwrap();
    assert!(
        body.contains("initialization phase"),
        "Error message should mention initialization phase"
    );

    simulator.shutdown().await;
}

/// Registration during shutdown - returns error or connection refused.
/// When the simulator is shutting down, new extension registrations should be rejected.
#[tokio::test]
async fn test_registration_during_shutdown_rejected() {
    let simulator = Simulator::builder()
        .function_name("shutdown-registration-test")
        .shutdown_timeout(Duration::from_millis(100))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let register_url = format!("{}/2020-01-01/extension/register", base_url);

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;

    let response = client
        .post(&register_url)
        .header("Lambda-Extension-Name", "shutdown-ext")
        .json(&json!({ "events": [EventType::Invoke] }))
        .send()
        .await;

    match response {
        Ok(resp) => {
            assert!(
                resp.status() == 403 || resp.status() == 500 || resp.status() == 503,
                "Registration after shutdown should fail (got {})",
                resp.status()
            );
        }
        Err(_) => {
            // Connection error is expected - server has shut down
        }
    }
}

/// Deadline calculation - deadlineMs = created_at + timeout.
#[tokio::test]
async fn test_invoke_event_deadline_equals_created_at_plus_timeout() {
    let timeout_ms = 30000u64;
    let simulator = Simulator::builder()
        .function_name("deadline-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id =
        register_extension(&client, &base_url, "deadline-ext", vec![EventType::Invoke])
            .await
            .unwrap();

    let poll_client = client.clone();
    let poll_url = base_url.clone();
    let poll_ext_id = extension_id.clone();
    let poll_handle = tokio::spawn(async move {
        next_event_with_timeout(
            &poll_client,
            &poll_url,
            &poll_ext_id,
            Duration::from_secs(5),
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let invocation = InvocationBuilder::new()
        .payload(json!({"test": "deadline"}))
        .timeout_ms(timeout_ms)
        .build()
        .unwrap();

    let created_at_ms = invocation.created_at.timestamp_millis();
    simulator.enqueue(invocation).await;

    let event = poll_handle.await.unwrap().unwrap();

    match event {
        LifecycleEvent::Invoke { deadline_ms, .. } => {
            let expected_deadline = created_at_ms + timeout_ms as i64;
            let delta = (deadline_ms - expected_deadline).abs();

            assert!(
                delta < 100,
                "Deadline should be created_at + timeout (got {}, expected {}, delta {}ms)",
                deadline_ms,
                expected_deadline,
                delta
            );
        }
        _ => panic!("Expected INVOKE event"),
    }

    simulator.shutdown().await;
}

/// ARN format correct - matches Lambda ARN pattern.
#[tokio::test]
async fn test_invoke_event_arn_has_correct_format() {
    let simulator = Simulator::builder()
        .function_name("arn-format-test")
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_id = register_extension(&client, &base_url, "arn-ext", vec![EventType::Invoke])
        .await
        .unwrap();

    let poll_client = client.clone();
    let poll_url = base_url.clone();
    let poll_ext_id = extension_id.clone();
    let poll_handle = tokio::spawn(async move {
        next_event_with_timeout(
            &poll_client,
            &poll_url,
            &poll_ext_id,
            Duration::from_secs(5),
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    simulator
        .enqueue_payload(json!({"test": "arn_format"}))
        .await;

    let event = poll_handle.await.unwrap().unwrap();

    match event {
        LifecycleEvent::Invoke {
            invoked_function_arn,
            ..
        } => {
            assert!(
                invoked_function_arn.starts_with("arn:aws:lambda:"),
                "ARN should start with arn:aws:lambda: (got {})",
                invoked_function_arn
            );

            let parts: Vec<&str> = invoked_function_arn.split(':').collect();
            assert_eq!(parts.len(), 7, "ARN should have 7 colon-separated parts");
            assert_eq!(parts[0], "arn");
            assert_eq!(parts[1], "aws");
            assert_eq!(parts[2], "lambda");
            assert!(!parts[3].is_empty(), "Region should not be empty");
            assert!(!parts[4].is_empty(), "Account ID should not be empty");
            assert_eq!(parts[5], "function");
            assert!(
                !parts[6].is_empty(),
                "Function name should be present in ARN"
            );
        }
        _ => panic!("Expected INVOKE event"),
    }

    simulator.shutdown().await;
}

/// Extension never calls /next - readiness timeout applies.
#[tokio::test]
async fn test_readiness_timeout_when_extension_never_calls_next() {
    let simulator = Simulator::builder()
        .function_name("unresponsive-ext-test")
        .extension_ready_timeout(Duration::from_millis(200))
        .build()
        .await
        .unwrap();

    let base_url = simulator.runtime_api_url();
    let client = Client::new();

    simulator.enable_telemetry_capture().await;

    let _extension_id = register_extension(
        &client,
        &base_url,
        "unresponsive-ext",
        vec![EventType::Invoke],
    )
    .await
    .unwrap();

    simulator
        .enqueue_payload(json!({"test": "unresponsive"}))
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

    simulator
        .wait_for(
            || async { !simulator.get_telemetry_events_by_type("platform.report").await.is_empty() },
            Duration::from_secs(5),
        )
        .await
        .expect("Should receive platform.report event");

    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    assert_eq!(
        report_events.len(),
        1,
        "platform.report should be emitted after timeout even if extension never calls /next"
    );

    simulator.shutdown().await;
}

/// Extension overhead calculated - overhead_ms = ready_at - runtime_done_at.
/// This is verified through the platform.report duration which includes extension overhead.
#[tokio::test]
async fn test_extension_overhead_included_in_duration() {
    let simulator = Simulator::builder()
        .function_name("overhead-calc-test")
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
        "slow-overhead-ext",
        vec![EventType::Invoke],
    )
    .await
    .unwrap();

    simulator
        .enqueue_payload(json!({"test": "overhead_calc"}))
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

    tokio::time::sleep(Duration::from_millis(150)).await;

    client
        .get(format!("{}/2020-01-01/extension/event/next", base_url))
        .header("Lambda-Extension-Identifier", &extension_id)
        .send()
        .await
        .unwrap();

    simulator
        .wait_for(
            || async { !simulator.get_telemetry_events_by_type("platform.report").await.is_empty() },
            Duration::from_secs(5),
        )
        .await
        .expect("Should receive platform.report event");

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
        "Duration ({:.2}ms) should include extension overhead (delay before extension signaled ready)",
        duration_ms
    );

    simulator.shutdown().await;
}

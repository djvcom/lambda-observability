//! Integration test harness that runs all three components together:
//! - Lambda Runtime Simulator
//! - Lambda Function (with OTel instrumentation)
//! - OTel Extension (when implemented)
//!
//! This provides a realistic local test environment matching AWS Lambda behavior.

use lambda_runtime::service_fn;
use lambda_simulator::{EventType, InvocationBuilder, InvocationStatus, LifecycleEvent, Simulator};
use opentelemetry_lambda_example::http_handler;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use temp_env::async_with_vars;
use tokio::time::timeout;

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

    Ok(extension_id)
}

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

/// Creates an HTTP API Gateway v2 event payload
fn create_http_event(message: &str) -> serde_json::Value {
    json!({
        "version": "2.0",
        "routeKey": "POST /test",
        "rawPath": "/test",
        "headers": {
            "content-type": "application/json"
        },
        "body": json!({
            "message": message
        }).to_string(),
        "requestContext": {
            "http": {
                "method": "POST",
                "path": "/test",
                "protocol": "HTTP/1.1",
                "sourceIp": "127.0.0.1",
                "userAgent": "test-agent"
            },
            "requestId": "test-request",
            "routeKey": "POST /test",
            "stage": "$default"
        },
        "isBase64Encoded": false
    })
}

#[tokio::test]
async fn test_function_with_simulator() {
    let simulator = Simulator::builder()
        .function_name("otel-test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("otel-test-function")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let invocation = InvocationBuilder::new()
                .payload(create_http_event("Hello from test!"))
                .build()
                .unwrap();

            let request_id = invocation.request_id.clone();
            simulator.enqueue(invocation).await;

            let runtime_task = tokio::spawn(async move {
                let _ = lambda_runtime::run(service_fn(http_handler)).await;
            });

            timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(state) = simulator.get_invocation_state(&request_id).await
                        && state.status == InvocationStatus::Success
                    {
                        if let Some(response) = &state.response {
                            // HTTP response has statusCode and body
                            let status_code =
                                response.payload.get("statusCode").and_then(|v| v.as_u64());
                            assert_eq!(status_code, Some(200));

                            let body_str = response.payload.get("body").and_then(|v| v.as_str());
                            if let Some(body) = body_str {
                                let body_json: serde_json::Value =
                                    serde_json::from_str(body).unwrap();
                                let message = body_json.get("message").and_then(|v| v.as_str());
                                assert_eq!(message, Some("Processed: Hello from test!"));
                            }
                        }
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Test timed out");

            runtime_task.abort();
            let _ = runtime_task.await;
        },
    )
    .await;

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_function_with_extension_and_simulator() {
    let simulator = Simulator::builder()
        .function_name("otel-combined-test")
        .build()
        .await
        .expect("Failed to start simulator");

    let base_url = simulator.runtime_api_url();
    let runtime_api_base = base_url.replace("http://", "");
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "otel-extension",
        vec![EventType::Invoke, EventType::Shutdown],
    )
    .await
    .expect("Failed to register extension");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("otel-combined-test")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let ext_client = client.clone();
            let ext_url = base_url.clone();
            let ext_id = extension_id.clone();
            let extension_handle = tokio::spawn(async move {
                next_event_with_timeout(&ext_client, &ext_url, &ext_id, Duration::from_secs(5))
                    .await
            });

            tokio::time::sleep(Duration::from_millis(50)).await;

            let invocation = InvocationBuilder::new()
                .payload(create_http_event("Combined test"))
                .build()
                .unwrap();

            let request_id = invocation.request_id.clone();
            simulator.enqueue(invocation).await;

            let runtime_task = tokio::spawn(async move {
                let _ = lambda_runtime::run(service_fn(http_handler)).await;
            });

            let ext_event = extension_handle
                .await
                .expect("Extension task panicked")
                .expect("Extension failed to receive event");

            match ext_event {
                LifecycleEvent::Invoke {
                    request_id: ext_req_id,
                    ..
                } => {
                    assert_eq!(ext_req_id, request_id);
                }
                _ => panic!("Expected INVOKE event, got {:?}", ext_event),
            }

            timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(state) = simulator.get_invocation_state(&request_id).await
                        && state.status == InvocationStatus::Success
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Test timed out waiting for invocation");

            runtime_task.abort();
            let _ = runtime_task.await;
        },
    )
    .await;

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_multiple_invocations_with_extension() {
    let simulator = Simulator::builder()
        .function_name("multi-invoke-test")
        .build()
        .await
        .expect("Failed to start simulator");

    let base_url = simulator.runtime_api_url();
    let runtime_api_base = base_url.replace("http://", "");
    let client = Client::new();

    let extension_id = register_extension(
        &client,
        &base_url,
        "multi-otel-extension",
        vec![EventType::Invoke],
    )
    .await
    .expect("Failed to register extension");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("multi-invoke-test")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let messages = vec!["First", "Second", "Third"];
            let mut request_ids = Vec::new();

            for msg in &messages {
                let invocation = InvocationBuilder::new()
                    .payload(create_http_event(msg))
                    .build()
                    .unwrap();
                request_ids.push(invocation.request_id.clone());
                simulator.enqueue(invocation).await;
            }

            let ext_client = client.clone();
            let ext_url = base_url.clone();
            let ext_id = extension_id.clone();
            let extension_handle = tokio::spawn(async move {
                let mut events = Vec::new();
                for _ in 0..3 {
                    if let Ok(event) = next_event_with_timeout(
                        &ext_client,
                        &ext_url,
                        &ext_id,
                        Duration::from_secs(5),
                    )
                    .await
                    {
                        events.push(event);
                    }
                }
                events
            });

            let runtime_task = tokio::spawn(async move {
                let _ = lambda_runtime::run(service_fn(http_handler)).await;
            });

            timeout(Duration::from_secs(10), async {
                let mut completed = 0;
                while completed < messages.len() {
                    let states = simulator.get_all_invocation_states().await;
                    completed = states
                        .iter()
                        .filter(|s| s.status == InvocationStatus::Success)
                        .count();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Test timed out");

            let ext_events = extension_handle.await.expect("Extension task panicked");
            assert_eq!(ext_events.len(), 3);

            for event in ext_events {
                assert!(matches!(event, LifecycleEvent::Invoke { .. }));
            }

            runtime_task.abort();
            let _ = runtime_task.await;
        },
    )
    .await;

    simulator.shutdown().await;
}

//! Integration tests with a real Lambda runtime.

use lambda_runtime::{Error as LambdaError, LambdaEvent, run, service_fn};
use lambda_simulator::{InvocationBuilder, InvocationStatus, Simulator};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use temp_env::async_with_vars;
use tokio::time::timeout;

#[derive(Deserialize, Debug)]
struct Request {
    name: String,
    #[serde(default)]
    age: Option<u32>,
}

#[derive(Serialize, Debug)]
struct Response {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
}

async fn simple_handler(event: LambdaEvent<Request>) -> Result<Response, LambdaError> {
    let (payload, _context) = event.into_parts();

    let message = if let Some(age) = payload.age {
        format!("Hello, {}! You are {} years old.", payload.name, age)
    } else {
        format!("Hello, {}!", payload.name)
    };

    Ok(Response {
        message,
        details: None,
    })
}

async fn error_handler(_event: LambdaEvent<Request>) -> Result<Response, LambdaError> {
    Err("Intentional test error".into())
}

#[tokio::test]
async fn test_real_lambda_runtime_with_simulator() {
    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("test-function")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let invocation = InvocationBuilder::new()
                .payload(json!({
                    "name": "Alice",
                    "age": 30
                }))
                .build()
                .unwrap();

            let request_id = invocation.request_id.clone();
            simulator.enqueue(invocation).await;

            let runtime_task = tokio::spawn(async move {
                let func = service_fn(simple_handler);
                let _ = run(func).await;
            });

            timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(state) = simulator.get_invocation_state(&request_id).await
                        && state.status == InvocationStatus::Success
                    {
                        if let Some(response) = &state.response {
                            let message = response.payload.get("message").and_then(|v| v.as_str());
                            assert_eq!(message, Some("Hello, Alice! You are 30 years old."));
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
async fn test_lambda_runtime_error_handling() {
    let simulator = Simulator::builder()
        .function_name("error-test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("error-test-function")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let invocation = InvocationBuilder::new()
                .payload(json!({
                    "name": "Bob"
                }))
                .build()
                .unwrap();

            let request_id = invocation.request_id.clone();
            simulator.enqueue(invocation).await;

            let runtime_task = tokio::spawn(async move {
                let func = service_fn(error_handler);
                let _ = run(func).await;
            });

            timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(state) = simulator.get_invocation_state(&request_id).await
                        && state.status == InvocationStatus::Error
                    {
                        if let Some(error) = &state.error {
                            assert!(error.error_message.contains("Intentional test error"));
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
async fn test_multiple_invocations_with_real_runtime() {
    let simulator = Simulator::builder()
        .function_name("multi-test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("multi-test-function")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let names = vec!["Alice", "Bob", "Charlie"];
            let mut request_ids = Vec::new();

            for name in &names {
                let invocation = InvocationBuilder::new()
                    .payload(json!({"name": name}))
                    .build()
                    .unwrap();
                request_ids.push(invocation.request_id.clone());
                simulator.enqueue(invocation).await;
            }

            let runtime_task = tokio::spawn(async move {
                let func = service_fn(simple_handler);
                let _ = run(func).await;
            });

            timeout(Duration::from_secs(10), async {
                let mut completed = 0;
                while completed < names.len() {
                    let states = simulator.get_all_invocation_states().await;
                    completed = states
                        .iter()
                        .filter(|s| s.status == InvocationStatus::Success)
                        .count();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }

                for (i, request_id) in request_ids.iter().enumerate() {
                    let state = simulator
                        .get_invocation_state(request_id)
                        .await
                        .expect("State not found");
                    assert_eq!(state.status, InvocationStatus::Success);

                    if let Some(response) = &state.response
                        && let Some(message) =
                            response.payload.get("message").and_then(|v| v.as_str())
                    {
                        assert!(message.contains(names[i]));
                    }
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
async fn test_json_passthrough() {
    let simulator = Simulator::builder()
        .function_name("json-test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("json-test-function")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
        ],
        async {
            let complex_payload = json!({
                "name": "Test User",
                "age": 25,
                "metadata": {
                    "created_at": "2024-01-01",
                    "tags": ["test", "integration"],
                    "nested": {
                        "level": 2,
                        "value": 42
                    }
                }
            });

            let invocation = InvocationBuilder::new()
                .payload(complex_payload.clone())
                .build()
                .unwrap();

            let request_id = invocation.request_id.clone();
            simulator.enqueue(invocation).await;

            let runtime_task = tokio::spawn(async move {
                let func = service_fn(simple_handler);
                let _ = run(func).await;
            });

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
            .expect("Test timed out");

            runtime_task.abort();
            let _ = runtime_task.await;
        },
    )
    .await;

    simulator.shutdown().await;
}

//! Demo test showcasing the Lambda simulator lifecycle with visual output.
//!
//! This test demonstrates the full Lambda lifecycle including:
//! - Extension registration during init
//! - Runtime initialisation
//! - Invocation processing
//! - Process freeze/thaw
//! - Graceful shutdown
//!
//! Run with: cargo test -p lambda-simulator --test demo_test -- --nocapture

use lambda_simulator::{InvocationBuilder, ShutdownReason, Simulator};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("lambda_lifecycle=info")),
        )
        .with_target(false)
        .with_level(false)
        .without_time()
        .try_init();
}

#[tokio::test]
async fn demo_full_lambda_lifecycle() {
    init_tracing();

    let simulator = Simulator::builder()
        .function_name("demo-function")
        .memory_size_mb(256)
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let extension_name = "otel-extension";
    let register_url = format!("{}/2020-01-01/extension/register", runtime_api_url);
    let register_response = client
        .post(&register_url)
        .header("Lambda-Extension-Name", extension_name)
        .json(&json!({
            "events": ["INVOKE", "SHUTDOWN"]
        }))
        .send()
        .await
        .expect("Failed to register extension");

    assert_eq!(register_response.status(), 200);
    let extension_id = register_response
        .headers()
        .get("Lambda-Extension-Identifier")
        .expect("Missing extension identifier")
        .to_str()
        .unwrap()
        .to_string();

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension should register");

    let ext_next_url = format!("{}/2020-01-01/extension/event/next", runtime_api_url);
    let runtime_next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);

    let ext_client = client.clone();
    let ext_id = extension_id.clone();
    let ext_url = ext_next_url.clone();
    let extension_task = tokio::spawn(async move {
        loop {
            let response = ext_client
                .get(&ext_url)
                .header("Lambda-Extension-Identifier", &ext_id)
                .send()
                .await
                .expect("Extension /next failed");

            let event: serde_json::Value = response.json().await.unwrap();
            if event.get("eventType").and_then(|v| v.as_str()) == Some("SHUTDOWN") {
                break;
            }
            // Simulate post-invocation telemetry work (flushing spans, etc.)
            // This delay contributes to "extension overhead" in platform.report
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let runtime_client = client.clone();
    let runtime_url = runtime_next_url.clone();
    let api_url = runtime_api_url.clone();
    let runtime_task = tokio::spawn(async move {
        for _ in 0..2 {
            let response = runtime_client
                .get(&runtime_url)
                .send()
                .await
                .expect("Runtime /next failed");

            let request_id = response
                .headers()
                .get("Lambda-Runtime-Aws-Request-Id")
                .expect("Missing request ID")
                .to_str()
                .unwrap()
                .to_string();

            let response_url = format!(
                "{}/2018-06-01/runtime/invocation/{}/response",
                api_url, request_id
            );
            runtime_client
                .post(&response_url)
                .json(&json!({"result": "success"}))
                .send()
                .await
                .expect("Failed to send response");
        }
    });

    let request_id_1 = simulator
        .enqueue(
            InvocationBuilder::new()
                .payload(json!({"message": "Hello, Lambda!"}))
                .build()
                .unwrap(),
        )
        .await;

    simulator
        .wait_for_invocation_complete(&request_id_1, Duration::from_secs(5))
        .await
        .expect("First invocation should complete");

    let request_id_2 = simulator
        .enqueue(
            InvocationBuilder::new()
                .payload(json!({"message": "Second invocation"}))
                .build()
                .unwrap(),
        )
        .await;

    simulator
        .wait_for_invocation_complete(&request_id_2, Duration::from_secs(5))
        .await
        .expect("Second invocation should complete");

    let _ = tokio::time::timeout(Duration::from_secs(2), runtime_task).await;

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = tokio::time::timeout(Duration::from_secs(1), extension_task).await;

    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!("  Demo complete!");
    println!("═══════════════════════════════════════════════════════════");
}

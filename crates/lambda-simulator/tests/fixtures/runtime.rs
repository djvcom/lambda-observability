//! Minimal Lambda runtime for testing the simulator's process spawning.
//!
//! This binary implements a simple Lambda runtime that:
//! - Polls the Runtime API for invocations
//! - Responds with a simple success message
//! - Runs until shutdown or no more invocations
//!
//! It reads `AWS_LAMBDA_RUNTIME_API` from the environment to connect to
//! the simulator.

use std::env;
use std::time::Duration;

fn main() {
    let runtime_api = env::var("AWS_LAMBDA_RUNTIME_API").expect("AWS_LAMBDA_RUNTIME_API not set");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    let next_url = format!("http://{}/2018-06-01/runtime/invocation/next", runtime_api);

    loop {
        let response = match client.get(&next_url).send() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Runtime: Failed to get next invocation: {}", e);
                break;
            }
        };

        if !response.status().is_success() {
            eprintln!("Runtime: Got non-success status: {}", response.status());
            break;
        }

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let _body: serde_json::Value = response.json().unwrap_or_default();

        std::thread::sleep(Duration::from_millis(10));

        let response_url = format!(
            "http://{}/2018-06-01/runtime/invocation/{}/response",
            runtime_api, request_id
        );

        let result = client
            .post(&response_url)
            .json(&serde_json::json!({"status": "ok", "handler": "test_runtime"}))
            .send();

        if let Err(e) = result {
            eprintln!("Runtime: Failed to send response: {}", e);
            break;
        }
    }
}

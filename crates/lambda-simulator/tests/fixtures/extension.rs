//! Minimal Lambda extension for testing the simulator's process spawning.
//!
//! This binary implements a simple Lambda extension that:
//! - Registers with the Extensions API
//! - Polls for lifecycle events (INVOKE, SHUTDOWN)
//! - Simulates post-invocation work before signalling readiness
//! - Exits on SHUTDOWN event
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

    let register_url = format!("http://{}/2020-01-01/extension/register", runtime_api);

    let register_response = client
        .post(&register_url)
        .header("Lambda-Extension-Name", "test_extension")
        .json(&serde_json::json!({
            "events": ["INVOKE", "SHUTDOWN"]
        }))
        .send()
        .expect("Failed to register extension");

    if !register_response.status().is_success() {
        eprintln!(
            "Extension: Registration failed with status: {}",
            register_response.status()
        );
        return;
    }

    let extension_id = register_response
        .headers()
        .get("Lambda-Extension-Identifier")
        .and_then(|v| v.to_str().ok())
        .expect("Missing extension identifier")
        .to_string();

    let next_url = format!("http://{}/2020-01-01/extension/event/next", runtime_api);

    loop {
        let response = match client
            .get(&next_url)
            .header("Lambda-Extension-Identifier", &extension_id)
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Extension: Failed to get next event: {}", e);
                break;
            }
        };

        if !response.status().is_success() {
            eprintln!("Extension: Got non-success status: {}", response.status());
            break;
        }

        let event: serde_json::Value = response.json().unwrap_or_default();
        let event_type = event
            .get("eventType")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match event_type {
            "SHUTDOWN" => {
                break;
            }
            "INVOKE" => {
                std::thread::sleep(Duration::from_millis(30));
            }
            _ => {}
        }
    }
}

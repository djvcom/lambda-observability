//! Test fixture: Minimal Lambda extension for simulator tests.
//!
//! **This is not a user-facing example.** It exists solely to test the
//! `lambda-simulator` crate's extension lifecycle and process freezing
//! functionality (SIGSTOP/SIGCONT).
//!
//! This binary is intentionally minimal and uses blocking I/O because:
//! - It needs to be a separate process that the simulator can freeze
//! - Async runtimes complicate signal handling in ways that obscure test results
//! - We only need basic registration and event polling to verify the simulator works
//!
//! For a real Lambda extension implementation, see the `opentelemetry-lambda-extension`
//! crate.
//!
//! ## What it does
//!
//! 1. Prints its PID at startup (used by tests to verify process state)
//! 2. Registers with the Extensions API for INVOKE and SHUTDOWN events
//! 3. Polls for lifecycle events and logs them
//!
//! ## Usage (for simulator tests)
//!
//! ```bash
//! AWS_LAMBDA_RUNTIME_API=127.0.0.1:9001 cargo run -p lambda-simulator --example test_extension
//! ```

use reqwest::blocking::Client;
use serde_json::json;
use std::env;
use std::process;

fn main() {
    let pid = process::id();
    eprintln!("test_extension started with PID: {}", pid);

    let runtime_api =
        env::var("AWS_LAMBDA_RUNTIME_API").expect("AWS_LAMBDA_RUNTIME_API must be set");

    let client = Client::new();

    let register_url = format!("http://{}/2020-01-01/extension/register", runtime_api);

    eprintln!("[{}] Registering extension...", pid);

    let response = client
        .post(&register_url)
        .header("Lambda-Extension-Name", "test-extension")
        .json(&json!({
            "events": ["INVOKE", "SHUTDOWN"]
        }))
        .send()
        .expect("Failed to register extension");

    let extension_id = response
        .headers()
        .get("Lambda-Extension-Identifier")
        .and_then(|h| h.to_str().ok())
        .expect("Missing extension identifier")
        .to_string();

    eprintln!("[{}] Registered with ID: {}", pid, extension_id);

    let next_url = format!("http://{}/2020-01-01/extension/event/next", runtime_api);

    loop {
        eprintln!("[{}] Waiting for next event...", pid);

        let response = match client
            .get(&next_url)
            .header("Lambda-Extension-Identifier", &extension_id)
            .send()
        {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("[{}] Error waiting for event: {}", pid, e);
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };

        let body: serde_json::Value = match response.json() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[{}] Error parsing event: {}", pid, e);
                continue;
            }
        };

        let event_type = body
            .get("eventType")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        eprintln!("[{}] Received event: {} - {:?}", pid, event_type, body);

        if event_type == "SHUTDOWN" {
            eprintln!("[{}] Shutting down", pid);
            break;
        }
    }
}

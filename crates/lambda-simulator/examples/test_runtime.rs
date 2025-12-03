//! Test fixture: Minimal Lambda runtime for simulator tests.
//!
//! **This is not a user-facing example.** It exists solely to test the
//! `lambda-simulator` crate's process freezing functionality (SIGSTOP/SIGCONT).
//!
//! This binary is intentionally minimal and uses blocking I/O because:
//! - It needs to be a separate process that the simulator can freeze
//! - Async runtimes complicate signal handling in ways that obscure test results
//! - We only need basic request/response to verify the simulator works
//!
//! For real Lambda function examples with proper async handlers and OpenTelemetry
//! instrumentation, see the `opentelemetry-lambda-example` crate.
//!
//! ## What it does
//!
//! 1. Prints its PID at startup (used by tests to verify process state)
//! 2. Polls the Runtime API for invocations
//! 3. Echoes back the payload with its PID
//!
//! ## Usage (for simulator tests)
//!
//! ```bash
//! AWS_LAMBDA_RUNTIME_API=127.0.0.1:9001 cargo run -p lambda-simulator --example test_runtime
//! ```

use reqwest::blocking::Client;
use serde_json::Value;
use std::env;
use std::process;

fn main() {
    let pid = process::id();
    eprintln!("test_runtime started with PID: {}", pid);

    let runtime_api =
        env::var("AWS_LAMBDA_RUNTIME_API").expect("AWS_LAMBDA_RUNTIME_API must be set");

    let client = Client::new();
    let next_url = format!("http://{}/2018-06-01/runtime/invocation/next", runtime_api);

    loop {
        eprintln!("[{}] Polling for next invocation...", pid);

        let response = match client.get(&next_url).send() {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("[{}] Error polling: {}", pid, e);
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let body: Value = match response.json() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[{}] Error parsing body: {}", pid, e);
                continue;
            }
        };

        eprintln!("[{}] Received invocation {}: {:?}", pid, request_id, body);

        let response_url = format!(
            "http://{}/2018-06-01/runtime/invocation/{}/response",
            runtime_api, request_id
        );

        let response_body = serde_json::json!({
            "echo": body,
            "pid": pid,
        });

        match client.post(&response_url).json(&response_body).send() {
            Ok(_) => eprintln!("[{}] Sent response for {}", pid, request_id),
            Err(e) => eprintln!("[{}] Error sending response: {}", pid, e),
        }
    }
}

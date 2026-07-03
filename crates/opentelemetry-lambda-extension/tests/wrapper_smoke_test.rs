//! Smoke tests for the Node.js and Python handler wrappers in `wrappers/`.
//!
//! Each test runs the real wrapper module under its interpreter against a
//! stub handler, with a local listener standing in for the extension's
//! receiver, and asserts that the wrapper invokes the original handler and
//! posts `/invocation/complete` with the request ID. Tests skip gracefully
//! when the interpreter is not installed.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;

const REQUEST_ID: &str = "req-smoke-test";

fn wrappers_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../wrappers")
        .canonicalize()
        .expect("wrappers directory should exist")
}

fn interpreter_available(interpreter: &str) -> bool {
    Command::new(interpreter).arg("--version").output().is_ok()
}

/// Starts a stub receiver that accepts one HTTP request, responds `202`,
/// and sends the raw request head back on the returned channel.
fn start_stub_receiver() -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind stub receiver");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buffer = [0u8; 4096];
            let read = socket.read(&mut buffer).unwrap_or(0);
            let _ = socket.write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n");
            let _ = tx.send(String::from_utf8_lossy(&buffer[..read]).to_string());
        }
    });

    (port, rx)
}

fn assert_completion_signal(rx: &mpsc::Receiver<String>) {
    let request = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("Wrapper should signal invocation completion");

    assert!(
        request.starts_with("POST /invocation/complete"),
        "Expected POST /invocation/complete, got: {request}"
    );
    assert!(
        request
            .to_lowercase()
            .contains(&format!("lambda-request-id: {}", REQUEST_ID.to_lowercase())),
        "Completion signal should carry the request ID, got: {request}"
    );
}

#[test]
#[ignore = "requires node or python3 on PATH"]
fn nodejs_wrapper_invokes_handler_and_signals_completion() {
    if !interpreter_available("node") {
        eprintln!("Skipping: node is not installed");
        return;
    }

    let (port, rx) = start_stub_receiver();
    let task_root = tempfile::tempdir().expect("Failed to create task root");
    std::fs::write(
        task_root.path().join("handler_module.js"),
        "exports.handler = async (event) => ({ echoed: event.value });\n",
    )
    .expect("Failed to write stub handler");

    let wrapper = wrappers_dir().join("nodejs/otel-wrapper.js");
    let script = format!(
        r#"
        const wrapper = require({wrapper:?});
        wrapper
            .handler({{ value: 42 }}, {{ awsRequestId: "{REQUEST_ID}" }})
            .then((result) => {{
                if (result.echoed !== 42) {{
                    console.error("unexpected result", result);
                    process.exit(3);
                }}
            }})
            .catch((error) => {{
                console.error(error);
                process.exit(2);
            }});
        "#,
        wrapper = wrapper.to_str().unwrap(),
    );

    let output = Command::new("node")
        .arg("-e")
        .arg(script)
        .env("ORIG_HANDLER", "handler_module.handler")
        .env("LAMBDA_TASK_ROOT", task_root.path())
        .env("LAMBDA_OTEL_RECEIVER_HTTP_PORT", port.to_string())
        .output()
        .expect("Failed to run node");

    assert!(
        output.status.success(),
        "Node wrapper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_completion_signal(&rx);
}

#[test]
#[ignore = "requires node or python3 on PATH"]
fn python_wrapper_invokes_handler_and_signals_completion() {
    if !interpreter_available("python3") {
        eprintln!("Skipping: python3 is not installed");
        return;
    }

    let (port, rx) = start_stub_receiver();
    let task_root = tempfile::tempdir().expect("Failed to create task root");
    std::fs::write(
        task_root.path().join("handler_module.py"),
        "def handler(event, context):\n    return {\"echoed\": event[\"value\"]}\n",
    )
    .expect("Failed to write stub handler");

    let wrapper_dir = wrappers_dir().join("python");
    let script = format!(
        r#"
import sys
import types

sys.path.insert(0, {wrapper_dir:?})
sys.path.insert(0, {task_root:?})

import otel_wrapper

context = types.SimpleNamespace(aws_request_id="{REQUEST_ID}")
result = otel_wrapper.handler({{"value": 42}}, context)
assert result == {{"echoed": 42}}, result
"#,
        wrapper_dir = wrapper_dir.to_str().unwrap(),
        task_root = task_root.path().to_str().unwrap(),
    );

    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .env("ORIG_HANDLER", "handler_module.handler")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_PORT", port.to_string())
        .output()
        .expect("Failed to run python3");

    assert!(
        output.status.success(),
        "Python wrapper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_completion_signal(&rx);
}

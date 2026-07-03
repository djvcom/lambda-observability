//! Lifecycle end-to-end tests for freeze-safe export behaviour.
//!
//! These tests verify the extension's core lifecycle guarantees against a
//! real spawned extension process with `FreezeMode::Process` (SIGSTOP/SIGCONT),
//! mirroring how Lambda freezes the execution environment once the runtime
//! has responded and every extension is parked on `/next`:
//!
//! 1. Buffered telemetry is exported *before* the environment freezes, so no
//!    HTTP export is ever left dangling across a freeze.
//! 2. `platform.runtimeDone` acts as a backup completion signal when no
//!    handler wrapper calls `/invocation/complete`.
//! 3. Late or missing `runtimeDone` delivery falls back to a bounded
//!    deadline-based hold that never exceeds the invocation deadline.
//! 4. The SHUTDOWN flush completes within the deadline carried by the
//!    SHUTDOWN event, even when the collector is unresponsive.
//!
//! Run with pre-built binaries:
//! ```sh
//! cargo build --workspace
//! cargo test -p opentelemetry-lambda-extension --test lifecycle_test -- --ignored
//! ```

mod common;

use common::wait_for_http_ready;
use lambda_simulator::process::{ManagedProcess, ProcessConfig, ProcessRole};
use lambda_simulator::{DeliveryPolicy, FreezeMode, InvocationBuilder, Simulator};
use mock_collector::{MockServer, Protocol as MockProtocol};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::metrics::v1::{Metric, ResourceMetrics, ScopeMetrics};
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;
use serial_test::serial;
use std::time::{Duration, Instant};

/// Non-default port so tests do not clash with a locally running collector.
const RECEIVER_PORT: u16 = 24418;

fn find_binary(name: &str) -> String {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());

    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let candidates = [
        workspace_root.join("target/debug").join(name),
        workspace_root.join("target/release").join(name),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    panic!(
        "Binary '{}' not found. Run `cargo build --workspace` first.",
        name
    );
}

/// Spawns the extension binary configured to export to the given endpoint
/// and waits until it has registered and its receiver is healthy.
async fn spawn_extension(simulator: &Simulator, collector_endpoint: &str) -> ManagedProcess {
    let runtime_api_base = simulator.runtime_api_url().replace("http://", "");
    let extension_binary = find_binary("opentelemetry-lambda-extension");

    let config = ProcessConfig::new(&extension_binary, ProcessRole::Extension)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("LAMBDA_OTEL_EXPORTER_ENDPOINT", collector_endpoint)
        .env("LAMBDA_OTEL_EXPORTER_PROTOCOL", "http")
        .env("LAMBDA_OTEL_EXPORTER_COMPRESSION", "none")
        .env("LAMBDA_OTEL_FLUSH_STRATEGY", "end")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_PORT", RECEIVER_PORT.to_string())
        .env("RUST_LOG", "info")
        .inherit_stdio(true);

    let extension = simulator
        .spawn_process_with_config(config)
        .expect("Failed to spawn extension");

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(10),
        )
        .await
        .expect("Extension did not register");

    wait_for_http_ready(RECEIVER_PORT, Duration::from_secs(5))
        .await
        .expect("Extension OTLP receiver not ready");

    extension
}

fn make_trace_request(span_name: &str) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    name: span_name.to_string(),
                    trace_id: vec![1; 16],
                    span_id: vec![2; 8],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn make_metrics_request() -> ExportMetricsServiceRequest {
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: "test.metric".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn make_logs_request() -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord::default()],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

async fn post_protobuf(client: &reqwest::Client, path: &str, body: Vec<u8>) {
    let response = client
        .post(format!("http://127.0.0.1:{}{}", RECEIVER_PORT, path))
        .header("Content-Type", "application/x-protobuf")
        .body(body)
        .send()
        .await
        .expect("Failed to reach extension receiver");
    assert!(
        response.status().is_success(),
        "Receiver rejected {} with {}",
        path,
        response.status()
    );
}

/// Signals invocation completion the way a handler wrapper would.
async fn post_invocation_complete(client: &reqwest::Client, request_id: &str) {
    let _ = client
        .post(format!(
            "http://127.0.0.1:{}/invocation/complete",
            RECEIVER_PORT
        ))
        .header("Lambda-Request-Id", request_id)
        .send()
        .await;
}

/// Acts as the Lambda runtime for one invocation: polls `/next`, runs
/// `during_invocation`, then posts the response.
async fn run_invocation<F, Fut>(
    simulator: &Simulator,
    client: &reqwest::Client,
    during_invocation: F,
) -> String
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let invocation = InvocationBuilder::new()
        .payload(serde_json::json!({"test": "lifecycle"}))
        .build()
        .expect("Failed to build invocation");
    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;

    let base_url = simulator.runtime_api_url();
    let next = client
        .get(format!("{}/2018-06-01/runtime/invocation/next", base_url))
        .send()
        .await
        .expect("Runtime /next failed");
    assert!(next.status().is_success());
    let delivered_id = next
        .headers()
        .get("Lambda-Runtime-Aws-Request-Id")
        .expect("Missing request id header")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(delivered_id, request_id);

    during_invocation(request_id.clone()).await;

    let response = client
        .post(format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            base_url, request_id
        ))
        .json(&serde_json::json!({"statusCode": 200}))
        .send()
        .await
        .expect("Runtime /response failed");
    assert!(response.status().is_success());

    request_id
}

async fn span_count(collector: &mock_collector::ServerHandle) -> usize {
    collector.with_collector(|c| c.span_count()).await
}

/// The core freeze-safety guarantee: with a handler wrapper signalling
/// completion, all buffered telemetry must reach the collector before the
/// environment freezes, and the freeze must still occur (the hold on `/next`
/// must release).
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn flush_completes_before_freeze_with_completion_signal() {
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .freeze_mode(FreezeMode::Process)
        .extension_ready_timeout(Duration::from_secs(10))
        .build()
        .await
        .expect("Failed to start simulator");

    let _extension = spawn_extension(&simulator, &collector_endpoint).await;
    let client = reqwest::Client::new();

    run_invocation(&simulator, &client, |request_id| async move {
        let client = reqwest::Client::new();
        post_protobuf(
            &client,
            "/v1/traces",
            make_trace_request("handler-span").encode_to_vec(),
        )
        .await;
        post_invocation_complete(&client, &request_id).await;
    })
    .await;

    simulator
        .wait_for_frozen(Duration::from_secs(10))
        .await
        .expect("Environment should freeze after the extension re-polls /next");

    assert!(
        span_count(&collector).await >= 1,
        "Buffered spans must be exported before the environment freezes; \
         found none at the collector at freeze time"
    );

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;
}

/// Without a wrapper, `platform.runtimeDone` must act as the backup
/// completion signal: telemetry still reaches the collector before freeze.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn runtime_done_backup_flushes_before_freeze() {
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .freeze_mode(FreezeMode::Process)
        .extension_ready_timeout(Duration::from_secs(10))
        .build()
        .await
        .expect("Failed to start simulator");

    let _extension = spawn_extension(&simulator, &collector_endpoint).await;
    let client = reqwest::Client::new();

    run_invocation(&simulator, &client, |_request_id| async move {
        let client = reqwest::Client::new();
        post_protobuf(
            &client,
            "/v1/traces",
            make_trace_request("handler-span").encode_to_vec(),
        )
        .await;
        // No /invocation/complete: the extension must rely on runtimeDone.
    })
    .await;

    simulator
        .wait_for_frozen(Duration::from_secs(10))
        .await
        .expect("Environment should freeze after the extension re-polls /next");

    assert!(
        span_count(&collector).await >= 1,
        "Spans must be exported before freeze using runtimeDone as the \
         completion signal; found none at the collector at freeze time"
    );

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;
}

/// When `runtimeDone` is delivered too late, the deadline fallback must
/// release the hold, flush what is buffered, and let the environment freeze
/// with the data already exported.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn late_runtime_done_releases_hold_and_recovers() {
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .freeze_mode(FreezeMode::Process)
        .extension_ready_timeout(Duration::from_secs(15))
        .invocation_timeout(Duration::from_secs(3))
        .build()
        .await
        .expect("Failed to start simulator");

    // runtimeDone arrives well after the invocation deadline.
    simulator
        .set_telemetry_delivery_policy(
            "platform.runtimeDone",
            DeliveryPolicy::Delay(Duration::from_secs(5)),
        )
        .await;

    let _extension = spawn_extension(&simulator, &collector_endpoint).await;
    let client = reqwest::Client::new();

    run_invocation(&simulator, &client, |_request_id| async move {
        let client = reqwest::Client::new();
        post_protobuf(
            &client,
            "/v1/traces",
            make_trace_request("handler-span").encode_to_vec(),
        )
        .await;
    })
    .await;

    // The extension must not hold /next past the invocation deadline: the
    // freeze must occur within the deadline plus a small margin.
    simulator
        .wait_for_frozen(Duration::from_secs(6))
        .await
        .expect("Deadline fallback should release the hold and allow freeze");

    assert!(
        span_count(&collector).await >= 1,
        "Spans must be exported via the deadline fallback before freeze"
    );

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;
}

/// When completion signals never arrive, the first invocation pays a bounded
/// deadline hold, after which holding is disabled: the second invocation's
/// extension overhead must be small.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn suppressed_runtime_done_disables_holding_after_first_timeout() {
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .freeze_mode(FreezeMode::Process)
        .extension_ready_timeout(Duration::from_secs(15))
        .invocation_timeout(Duration::from_secs(3))
        .build()
        .await
        .expect("Failed to start simulator");

    simulator
        .set_telemetry_delivery_policy("platform.runtimeDone", DeliveryPolicy::Suppress)
        .await;

    let _extension = spawn_extension(&simulator, &collector_endpoint).await;
    let client = reqwest::Client::new();

    let request_id_1 = run_invocation(&simulator, &client, |_| async {}).await;

    simulator
        .wait_for_extensions_ready(&request_id_1, Duration::from_secs(6))
        .await
        .expect("Extension should release /next via the deadline fallback");

    let overhead_1 = simulator
        .get_extension_overhead_ms(&request_id_1)
        .await
        .expect("First invocation should have extension overhead recorded");
    assert!(
        overhead_1 >= 1_000.0,
        "First invocation should pay a bounded deadline hold while waiting \
         for completion signals that never arrive (observed {}ms)",
        overhead_1
    );

    let request_id_2 = run_invocation(&simulator, &client, |_| async {}).await;

    simulator
        .wait_for_extensions_ready(&request_id_2, Duration::from_secs(6))
        .await
        .expect("Extension should be ready promptly on the second invocation");

    let overhead_2 = simulator
        .get_extension_overhead_ms(&request_id_2)
        .await
        .expect("Second invocation should have extension overhead recorded");
    assert!(
        overhead_2 < 500.0,
        "After the first deadline timeout, holding must be disabled so \
         subsequent invocations pay no hold cost (observed {}ms)",
        overhead_2
    );

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;
}

/// The SHUTDOWN flush must respect the deadline in the SHUTDOWN event: with
/// an unresponsive collector and a backlog across all three signal types, the
/// extension must give up exporting and exit within the deadline rather than
/// retrying past SIGKILL.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn shutdown_flush_completes_within_deadline() {
    // A collector that accepts TCP connections but never responds, so every
    // export attempt runs to its full request timeout.
    let tarpit = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind tarpit listener");
    let tarpit_addr = tarpit.local_addr().unwrap();
    tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = tarpit.accept().await {
            sockets.push(socket);
        }
    });

    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .shutdown_timeout(SHUTDOWN_TIMEOUT)
        .build()
        .await
        .expect("Failed to start simulator");

    let extension = spawn_extension(&simulator, &format!("http://{}", tarpit_addr)).await;
    let client = reqwest::Client::new();

    // Backlog across all three signal types so the final flush has several
    // batches to attempt against the unresponsive collector.
    post_protobuf(
        &client,
        "/v1/traces",
        make_trace_request("stuck-span").encode_to_vec(),
    )
    .await;
    post_protobuf(
        &client,
        "/v1/metrics",
        make_metrics_request().encode_to_vec(),
    )
    .await;
    post_protobuf(&client, "/v1/logs", make_logs_request().encode_to_vec()).await;

    let shutdown_started = Instant::now();
    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;

    // The extension must exit on its own within the SHUTDOWN deadline (plus
    // a scheduling margin). In real Lambda, exceeding it means SIGKILL and
    // silently lost telemetry.
    let exit = tokio::time::timeout(
        SHUTDOWN_TIMEOUT + Duration::from_millis(500),
        tokio::task::spawn_blocking(move || {
            let mut extension = extension;
            extension.wait()
        }),
    )
    .await;

    let elapsed = shutdown_started.elapsed();
    assert!(
        exit.is_ok(),
        "Extension must exit within the SHUTDOWN deadline ({}ms observed, \
         {}ms allowed); in real Lambda it would have been SIGKILLed mid-export",
        elapsed.as_millis(),
        (SHUTDOWN_TIMEOUT + Duration::from_millis(500)).as_millis()
    );
}

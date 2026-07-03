//! Shared harness for tests that run the real extension binary against the
//! Lambda simulator and a mock OTLP collector.

use super::wait_for_http_ready;
use lambda_simulator::process::{ManagedProcess, ProcessConfig, ProcessRole};
use lambda_simulator::{InvocationBuilder, Simulator};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::metrics::v1::{Metric, ResourceMetrics, ScopeMetrics};
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use std::time::Duration;

/// Non-default port so tests do not clash with a locally running collector.
pub const RECEIVER_PORT: u16 = 24418;

/// Locates a workspace binary built by `cargo build --workspace`.
pub fn find_binary(name: &str) -> String {
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
pub async fn spawn_extension(simulator: &Simulator, collector_endpoint: &str) -> ManagedProcess {
    spawn_extension_with_env(simulator, collector_endpoint, &[]).await
}

/// Spawns the extension like [`spawn_extension`], with additional
/// environment variables that take precedence over the defaults.
pub async fn spawn_extension_with_env(
    simulator: &Simulator,
    collector_endpoint: &str,
    extra_env: &[(&str, String)],
) -> ManagedProcess {
    let runtime_api_base = simulator.runtime_api_url().replace("http://", "");
    let extension_binary = find_binary("opentelemetry-lambda-extension");

    let mut config = ProcessConfig::new(&extension_binary, ProcessRole::Extension)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("LAMBDA_OTEL_EXPORTER_ENDPOINT", collector_endpoint)
        .env("LAMBDA_OTEL_EXPORTER_PROTOCOL", "http")
        .env("LAMBDA_OTEL_EXPORTER_COMPRESSION", "none")
        .env("LAMBDA_OTEL_FLUSH_STRATEGY", "end")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_PORT", RECEIVER_PORT.to_string())
        .env(
            "RUST_LOG",
            std::env::var("LIFECYCLE_TEST_LOG").unwrap_or_else(|_| "info".to_string()),
        )
        .inherit_stdio(true);

    for (key, value) in extra_env {
        config = config.env(*key, value.clone());
    }

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

/// Builds a single-span OTLP trace request.
pub fn make_trace_request(span_name: &str) -> ExportTraceServiceRequest {
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

/// Builds a single-metric OTLP metrics request.
pub fn make_metrics_request() -> ExportMetricsServiceRequest {
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

/// Builds a single-record OTLP logs request.
pub fn make_logs_request() -> ExportLogsServiceRequest {
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

/// Posts an encoded protobuf payload to the extension's receiver.
pub async fn post_protobuf(client: &reqwest::Client, path: &str, body: Vec<u8>) {
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

/// Starts a collector that accepts TCP connections but never responds, so
/// every export attempt runs to its full request timeout.
pub async fn start_tarpit_collector() -> std::net::SocketAddr {
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
    tarpit_addr
}

/// Signals invocation completion the way a handler wrapper would.
pub async fn post_invocation_complete(client: &reqwest::Client, request_id: &str) {
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
pub async fn run_invocation<F, Fut>(
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

/// Waits for the `platform.report` event of the given invocation and
/// returns its `durationMs` metric. With an instant test handler this is
/// dominated by how long the extension held `/next` after the response.
pub async fn report_duration_ms(simulator: &Simulator, request_id: &str) -> f64 {
    simulator
        .wait_for(
            || async {
                simulator
                    .get_telemetry_events_by_type("platform.report")
                    .await
                    .iter()
                    .any(|e| e.record["requestId"].as_str() == Some(request_id))
            },
            Duration::from_secs(20),
        )
        .await
        .expect("platform.report not emitted");

    simulator
        .get_telemetry_events_by_type("platform.report")
        .await
        .iter()
        .find(|e| e.record["requestId"].as_str() == Some(request_id))
        .and_then(|e| e.record["metrics"]["durationMs"].as_f64())
        .expect("report should carry durationMs")
}

/// Returns the number of spans received by the mock collector.
pub async fn span_count(collector: &mock_collector::ServerHandle) -> usize {
    collector.with_collector(|c| c.span_count()).await
}

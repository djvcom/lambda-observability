//! End-to-end integration test with real process spawning.
//!
//! This test validates the complete Lambda telemetry pipeline using actual
//! processes rather than in-process tokio tasks:
//!
//! 1. Start mock collector to receive OTLP exports
//! 2. Start the OpenTelemetry Lambda extension (real binary)
//! 3. Start an instrumented runtime (real binary)
//! 4. Run invocations through the simulator
//! 5. Verify telemetry arrives at the collector
//!
//! This test requires the following binaries to be built:
//! - `opentelemetry-lambda-extension` from the extension crate
//! - `http_runtime` from the example crate

use lambda_simulator::process::{ProcessConfig, ProcessRole};
use lambda_simulator::{
    FreezeMode, InvocationBuilder, InvocationStatus, ShutdownReason, Simulator,
};
use mock_collector::{MockServer, Protocol as MockProtocol};
use serial_test::serial;
use std::time::Duration;

mod common;
use common::wait_for_http_ready;

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
        std::path::PathBuf::from("target/debug").join(name),
        std::path::PathBuf::from("target/release").join(name),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    panic!(
        "Binary '{}' not found. Searched: {:?}. Run `cargo build --workspace` first.",
        name,
        candidates.iter().map(|p| p.display()).collect::<Vec<_>>()
    );
}

fn extension_binary_path() -> String {
    find_binary("opentelemetry-lambda-extension")
}

fn runtime_binary_path() -> String {
    find_binary("http_runtime")
}

#[tokio::test]
#[serial]
async fn test_e2e_with_real_processes() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("lambda_simulator=debug,lambda_extension=debug")
        .with_test_writer()
        .try_init();

    // =========================================================================
    // PHASE 1: Start Mock Collector
    // =========================================================================
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let collector_endpoint = format!("http://{}", collector.addr());
    println!("Mock collector started at: {}", collector_endpoint);

    // =========================================================================
    // PHASE 2: Start Simulator
    // =========================================================================
    let simulator = Simulator::builder()
        .function_name("e2e-process-test")
        .freeze_mode(FreezeMode::None)
        .extension_ready_timeout(Duration::from_secs(10))
        .build()
        .await
        .expect("Failed to start simulator");

    simulator.enable_telemetry_capture().await;

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");
    println!("Simulator started at: {}", runtime_api);

    // =========================================================================
    // PHASE 3: Start Extension Process
    // =========================================================================
    let extension_binary = extension_binary_path();
    println!("Starting extension from: {}", extension_binary);

    let extension_config = ProcessConfig::new(&extension_binary, ProcessRole::Extension)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("LAMBDA_OTEL_EXPORTER_ENDPOINT", &collector_endpoint)
        .env("LAMBDA_OTEL_EXPORTER_PROTOCOL", "http")
        .env("LAMBDA_OTEL_EXPORTER_COMPRESSION", "none")
        .env("LAMBDA_OTEL_FLUSH_STRATEGY", "end")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_ENABLED", "true")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_PORT", "4318")
        .env("LAMBDA_OTEL_TELEMETRY_API_ENABLED", "true")
        .env("LAMBDA_OTEL_TELEMETRY_API_LISTENER_PORT", "9001")
        .env(
            "RUST_LOG",
            "lambda_extension=debug,opentelemetry_lambda_extension=debug",
        )
        .inherit_stdio(true);

    let mut extension = simulator
        .spawn_process_with_config(extension_config)
        .expect("Failed to spawn extension");

    println!("Extension spawned with PID: {}", extension.pid());

    // Wait for extension to register
    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(10),
        )
        .await
        .expect("Extension did not register in time");

    let extensions = simulator.get_registered_extensions().await;
    println!(
        "Extension registered: {:?}",
        extensions.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // Wait for extension's OTLP receiver to be ready
    wait_for_http_ready(4318, Duration::from_secs(5))
        .await
        .expect("Extension OTLP receiver did not become ready");
    println!("Extension OTLP receiver is ready");

    // =========================================================================
    // PHASE 4: Start Runtime Process
    // =========================================================================
    let runtime_binary = runtime_binary_path();
    println!("Starting runtime from: {}", runtime_binary);

    let runtime_config = ProcessConfig::new(&runtime_binary, ProcessRole::Runtime)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("AWS_LAMBDA_FUNCTION_NAME", "e2e-process-test")
        .env("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST")
        .env("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "128")
        .env("AWS_REGION", "us-east-1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("OTEL_TRACES_EXPORTER", "otlp")
        .env(
            "RUST_LOG",
            "opentelemetry=debug,opentelemetry_lambda_example=debug,opentelemetry_otlp=debug",
        )
        .inherit_stdio(true);

    let runtime = simulator
        .spawn_process_with_config(runtime_config)
        .expect("Failed to spawn runtime");

    println!("Runtime spawned with PID: {}", runtime.pid());

    // Allow runtime to initialise and connect to the API
    tokio::time::sleep(Duration::from_millis(500)).await;

    // =========================================================================
    // PHASE 5: Enqueue and Execute Invocations
    // =========================================================================
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let parent_span_id = "00f067aa0ba902b7";
    let traceparent = format!("00-{}-{}-01", trace_id, parent_span_id);

    let http_event = serde_json::json!({
        "version": "2.0",
        "routeKey": "POST /test",
        "rawPath": "/test",
        "headers": {
            "traceparent": traceparent,
            "content-type": "application/json"
        },
        "body": serde_json::json!({
            "message": "E2E process spawn test",
            "delay_ms": 50
        }).to_string(),
        "requestContext": {
            "http": {
                "method": "POST",
                "path": "/test"
            }
        }
    });

    let invocation = InvocationBuilder::new()
        .payload(http_event)
        .build()
        .unwrap();

    let request_id = invocation.request_id.clone();
    simulator.enqueue(invocation).await;
    println!("Enqueued invocation: {}", request_id);

    // Wait for invocation to complete
    let state = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(15))
        .await
        .expect("Invocation did not complete");

    assert_eq!(
        state.status,
        InvocationStatus::Success,
        "Invocation failed: {:?}",
        state.error
    );
    println!("Invocation completed successfully");

    // Wait for extension readiness after invocation
    println!("Waiting for extension readiness...");
    match simulator
        .wait_for_extensions_ready(&request_id, Duration::from_secs(5))
        .await
    {
        Ok(()) => println!("Extensions signaled ready"),
        Err(_) => println!("Extension readiness wait timed out"),
    }

    // Allow time for the runtime's OTLP exporter to flush to the extension's receiver
    tokio::time::sleep(Duration::from_millis(500)).await;

    // =========================================================================
    // PHASE 6: Validate Platform Telemetry Events
    // =========================================================================
    let all_events = simulator.get_telemetry_events().await;
    println!("Total telemetry events captured: {}", all_events.len());
    for event in &all_events {
        println!("  - {} at {}", event.event_type, event.time);
    }

    let start_events = simulator
        .get_telemetry_events_by_type("platform.start")
        .await;
    let runtime_done_events = simulator
        .get_telemetry_events_by_type("platform.runtimeDone")
        .await;
    let report_events = simulator
        .get_telemetry_events_by_type("platform.report")
        .await;

    println!(
        "Platform events: {} start, {} runtimeDone, {} report",
        start_events.len(),
        runtime_done_events.len(),
        report_events.len()
    );

    assert!(
        !start_events.is_empty(),
        "Expected at least one platform.start event"
    );
    assert!(
        !runtime_done_events.is_empty(),
        "Expected at least one platform.runtimeDone event"
    );

    // =========================================================================
    // PHASE 7: Graceful Shutdown
    // =========================================================================
    println!("Triggering graceful shutdown...");
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    println!("Waiting for extension process to exit...");
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if extension.try_wait().ok().flatten().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    println!("Extension process exited");

    // Wait for spans to arrive at collector. We expect 3 spans from the runtime
    // (POST /test, process_message, simulate_work). Waiting for all of them ensures
    // the extension has had time to export metrics including the shutdown_count.
    println!("Waiting for spans at collector...");
    match collector.wait_for_spans(3, Duration::from_secs(10)).await {
        Ok(()) => println!("Received span(s) at collector"),
        Err(e) => println!("Timed out waiting for spans: {}", e),
    }

    // =========================================================================
    // PHASE 8: Validate Function Spans at Collector (best-effort)
    // =========================================================================
    // Note: Due to async nature of the OTel batch exporter and process lifecycle,
    // spans may not always arrive before the extension shuts down. This is verified
    // more reliably in the in-process e2e tests. The primary focus here is validating
    // the process spawning mechanics and platform metrics.
    collector
        .with_collector(|c| {
            println!("Collector has {} span(s)", c.span_count());

            if c.span_count() == 0 {
                println!("INFO: No spans received at collector (timing dependent)");
                return;
            }

            println!("Available spans:");
            for span in c.spans() {
                println!(
                    "  - {} (trace_id: {})",
                    span.span().name,
                    hex::encode(&span.span().trace_id)
                );
            }

            // Find the http.request span and verify trace context propagation
            let span_assertion = c.expect_span_with_name("http.request");
            let matching_spans = span_assertion.get_all();

            if matching_spans.is_empty() {
                println!("No http.request span found");
                return;
            }

            let span = matching_spans[0];
            let span_trace_id = hex::encode(&span.span().trace_id);
            let span_parent_id = hex::encode(&span.span().parent_span_id);

            println!("http.request span:");
            println!("  trace_id: {} (expected: {})", span_trace_id, trace_id);
            println!(
                "  parent_span_id: {} (expected: {})",
                span_parent_id, parent_span_id
            );

            // Verify trace context propagation if we do receive spans
            assert_eq!(
                span_trace_id, trace_id,
                "Trace context propagation failed - wrong trace_id"
            );
            assert_eq!(
                span_parent_id, parent_span_id,
                "Trace context propagation failed - wrong parent_span_id"
            );

            println!("Trace context propagation verified!");
        })
        .await;

    // =========================================================================
    // PHASE 9: Validate Platform Metrics at Collector
    // =========================================================================
    collector
        .with_collector(|c| {
            println!("Collector has {} metric(s)", c.metric_count());

            let metric_names: Vec<String> = c
                .metrics()
                .iter()
                .map(|m| m.metric().name.clone())
                .collect();
            println!("Metrics received:");
            for name in &metric_names {
                println!("  - {}", name);
            }

            // These assertions are the key value of this test - they verify that
            // platform metrics derived from Telemetry API events are being exported
            assert!(
                metric_names.contains(&"faas.invocation.duration".to_string()),
                "Missing faas.invocation.duration metric. Got: {:?}",
                metric_names
            );
            assert!(
                metric_names.contains(&"aws.lambda.billed_duration".to_string()),
                "Missing aws.lambda.billed_duration metric. Got: {:?}",
                metric_names
            );
            assert!(
                metric_names.contains(&"aws.lambda.max_memory_used".to_string()),
                "Missing aws.lambda.max_memory_used metric. Got: {:?}",
                metric_names
            );
            assert!(
                metric_names.contains(&"extension.shutdown_count".to_string()),
                "Missing extension.shutdown_count metric. Got: {:?}",
                metric_names
            );

            println!("All platform metrics verified!");
        })
        .await;

    // =========================================================================
    // PHASE 10: Cleanup
    // =========================================================================
    drop(runtime);
    drop(extension);

    collector
        .shutdown()
        .await
        .expect("Collector shutdown failed");

    println!("E2E process spawn test completed!");
}

#[tokio::test]
#[serial]
async fn test_e2e_with_freeze_mode() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("lambda_simulator=debug")
        .with_test_writer()
        .try_init();

    // Start mock collector
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let collector_endpoint = format!("http://{}", collector.addr());

    // Start simulator with freeze mode enabled
    let simulator = Simulator::builder()
        .function_name("e2e-freeze-test")
        .freeze_mode(FreezeMode::Process)
        .extension_ready_timeout(Duration::from_secs(10))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");
    println!("Simulator with freeze mode started at: {}", runtime_api);

    // Start extension (using default port 4318 - the RECEIVER_HTTP_PORT env var
    // doesn't work due to figment's underscore splitting)
    let extension_binary = extension_binary_path();
    let extension_config = ProcessConfig::new(&extension_binary, ProcessRole::Extension)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("LAMBDA_OTEL_EXPORTER_ENDPOINT", &collector_endpoint)
        .env("LAMBDA_OTEL_EXPORTER_PROTOCOL", "http")
        .env("LAMBDA_OTEL_EXPORTER_COMPRESSION", "none")
        .env("LAMBDA_OTEL_FLUSH_STRATEGY", "end")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_ENABLED", "true")
        .env("LAMBDA_OTEL_TELEMETRY_API_ENABLED", "false")
        .env(
            "RUST_LOG",
            "lambda_extension=debug,opentelemetry_lambda_extension=debug",
        )
        .inherit_stdio(true);

    let extension = simulator
        .spawn_process_with_config(extension_config)
        .expect("Failed to spawn extension");

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(10),
        )
        .await
        .expect("Extension did not register");

    wait_for_http_ready(4318, Duration::from_secs(5))
        .await
        .expect("Extension OTLP receiver did not become ready");

    // Start runtime
    let runtime_binary = runtime_binary_path();
    let runtime_config = ProcessConfig::new(&runtime_binary, ProcessRole::Runtime)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("AWS_LAMBDA_FUNCTION_NAME", "e2e-freeze-test")
        .env("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST")
        .env("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "128")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318")
        .inherit_stdio(true);

    let runtime = simulator
        .spawn_process_with_config(runtime_config)
        .expect("Failed to spawn runtime");

    let runtime_pid = runtime.pid();
    let extension_pid = extension.pid();
    println!(
        "Spawned runtime (PID: {}) and extension (PID: {})",
        runtime_pid, extension_pid
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Execute first invocation
    let request_id_1 = simulator
        .enqueue_payload(serde_json::json!({
            "version": "2.0",
            "routeKey": "POST /test",
            "rawPath": "/test",
            "headers": {"content-type": "application/json"},
            "body": "{\"message\":\"first\"}",
            "requestContext": {"http": {"method": "POST", "path": "/test"}}
        }))
        .await;

    let state_1 = simulator
        .wait_for_invocation_complete(&request_id_1, Duration::from_secs(15))
        .await
        .expect("First invocation did not complete");

    assert_eq!(state_1.status, InvocationStatus::Success);
    println!("First invocation completed");

    // Wait for extension readiness (triggers freeze)
    let _ = simulator
        .wait_for_extensions_ready(&request_id_1, Duration::from_secs(5))
        .await;

    // Give some time for freeze to occur
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Execute second invocation (should thaw processes)
    let request_id_2 = simulator
        .enqueue_payload(serde_json::json!({
            "version": "2.0",
            "routeKey": "POST /test",
            "rawPath": "/test",
            "headers": {"content-type": "application/json"},
            "body": "{\"message\":\"second\"}",
            "requestContext": {"http": {"method": "POST", "path": "/test"}}
        }))
        .await;

    let state_2 = simulator
        .wait_for_invocation_complete(&request_id_2, Duration::from_secs(15))
        .await
        .expect("Second invocation did not complete");

    assert_eq!(state_2.status, InvocationStatus::Success);
    println!("Second invocation completed after thaw");

    // Shutdown
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    let _ = collector.wait_for_spans(1, Duration::from_secs(5)).await;

    collector
        .with_collector(|c| {
            println!(
                "Freeze test: {} spans, {} metrics collected",
                c.span_count(),
                c.metric_count()
            );
        })
        .await;

    drop(runtime);
    drop(extension);

    collector
        .shutdown()
        .await
        .expect("Collector shutdown failed");

    println!("E2E freeze mode test completed!");
}

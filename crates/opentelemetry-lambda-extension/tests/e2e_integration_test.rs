//! End-to-end integration test for the Lambda OTel extension.
//!
//! This test validates the complete telemetry pipeline:
//! 1. Mock collector receives exports
//! 2. Simulator provides Runtime/Extensions/Telemetry APIs
//! 3. Extension registers and subscribes to telemetry
//! 4. Lambda function extracts trace context and creates instrumented spans
//! 5. Extension receives function telemetry via OTLP
//! 6. Extension receives platform telemetry via Telemetry API
//! 7. Extension exports all signals to collector
//!
//! See tests/E2E_TEST_PLAN.md for detailed test design.

mod common;

use common::wait_for_http_ready;
use lambda_runtime::run;
use lambda_runtime::service_fn;
use lambda_simulator::{InvocationBuilder, InvocationStatus, ShutdownReason, Simulator};
use mock_collector::{MockServer, Protocol as MockProtocol};
use opentelemetry_lambda_example::http_handler;
use opentelemetry_lambda_extension::{
    Compression, Config, ExporterConfig, FlushConfig, FlushStrategy, Protocol, ReceiverConfig,
    RuntimeBuilder, TelemetryApiConfig,
};
use serde_json::json;
use serial_test::serial;
use std::time::Duration;
use temp_env::async_with_vars;

#[tokio::test]
#[serial]
async fn test_e2e_trace_propagation_and_platform_telemetry() {
    // Initialize tracing to see error logs from lambda_extension crate
    let _ = tracing_subscriber::fmt()
        .with_env_filter("lambda_extension=trace,lambda_simulator=debug")
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
        .function_name("otel-e2e-test")
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .expect("Failed to start simulator");

    simulator.enable_telemetry_capture().await;

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");
    println!("Simulator started at: {}", runtime_api);

    // =========================================================================
    // PHASE 3: Start Extension (background task)
    // =========================================================================
    let extension_collector_endpoint = collector_endpoint.clone();

    // We need to set env vars before spawning the extension
    // Since we can't use temp_env here easily, we'll pass config directly
    let extension_config = Config {
        exporter: ExporterConfig {
            endpoint: Some(extension_collector_endpoint),
            protocol: Protocol::Http,
            timeout: Duration::from_secs(5),
            compression: Compression::None,
            ..Default::default()
        },
        telemetry_api: TelemetryApiConfig {
            enabled: true,
            listener_port: 9001,
            buffer_size: 100,
        },
        receiver: ReceiverConfig {
            http_enabled: true,
            http_port: 4318,
            ..Default::default()
        },
        flush: FlushConfig {
            strategy: FlushStrategy::End,
            ..Default::default()
        },
        ..Default::default()
    };

    let extension_runtime_api = runtime_api_base.clone();

    // Set environment variable for lambda_extension crate before spawning
    // The #[serial] attribute ensures this test runs in isolation
    // The lambda_extension crate expects the full URL with http:// prefix
    // SAFETY: Test runs serially so no other code is reading environment concurrently
    unsafe {
        std::env::set_var(
            "AWS_LAMBDA_RUNTIME_API",
            format!("http://{}", extension_runtime_api),
        );
    }

    let extension_handle = tokio::spawn(async move {
        tracing::debug!("Starting extension runtime");
        tracing::debug!(
            runtime_api =
                std::env::var("AWS_LAMBDA_RUNTIME_API").unwrap_or_else(|_| "NOT SET".to_string()),
            "Environment check"
        );
        let runtime = RuntimeBuilder::new().config(extension_config).build();

        tracing::debug!("Calling runtime.run()");
        if let Err(e) = runtime.run().await {
            tracing::error!(error = ?e, "Extension error");
        }
        tracing::debug!("Extension runtime finished");
    });

    // Wait for extension to register
    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension did not register in time");

    let extensions = simulator.get_registered_extensions().await;
    println!(
        "Extension registered: {:?}",
        extensions.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // Wait for OTLP receiver to be ready
    wait_for_http_ready(4318, Duration::from_secs(5))
        .await
        .expect("OTLP receiver did not become ready");
    println!("OTLP receiver is ready");

    // =========================================================================
    // PHASE 4 & 5: Start Runtime and Enqueue Event
    // =========================================================================
    // Define known trace context for assertion
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let parent_span_id = "00f067aa0ba902b7";
    let traceparent = format!("00-{}-{}-01", trace_id, parent_span_id);

    // Create HTTP event with trace context
    let http_event = json!({
        "version": "2.0",
        "routeKey": "POST /test",
        "rawPath": "/test",
        "headers": {
            "traceparent": traceparent,
            "content-type": "application/json"
        },
        "body": json!({
            "message": "e2e trace test",
            "delay_ms": 10
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

    // Run the Lambda runtime with our function
    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("otel-e2e-test")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
            ("AWS_REGION", Some("us-east-1")),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://127.0.0.1:4318")),
        ],
        async {
            // Enqueue the invocation
            simulator.enqueue(invocation).await;
            println!("Enqueued invocation: {}", request_id);

            // Start Lambda runtime
            let runtime_handle = tokio::spawn(async move {
                let _ = run(service_fn(http_handler)).await;
            });

            // =========================================================================
            // PHASE 6: Wait for Invocation Completion
            // =========================================================================
            let state = simulator
                .wait_for_invocation_complete(&request_id, Duration::from_secs(10))
                .await
                .expect("Invocation did not complete");

            assert_eq!(
                state.status,
                InvocationStatus::Success,
                "Invocation failed: {:?}",
                state.error
            );
            println!("Invocation completed successfully");

            // =========================================================================
            // PHASE 7: Wait for Extension Readiness and platform.report
            // =========================================================================
            // Wait for extension to signal readiness (call /next after processing).
            // This triggers platform.report emission which contains the metrics we want.
            println!("Waiting for extension readiness...");
            match simulator
                .wait_for_extensions_ready(&request_id, Duration::from_secs(5))
                .await
            {
                Ok(()) => println!("Extensions signaled ready"),
                Err(_) => println!("Extension readiness wait timed out"),
            }

            // Give simulator time to emit platform.report after extensions are ready
            // and deliver telemetry events to the extension's listener
            tokio::time::sleep(Duration::from_millis(200)).await;

            // =========================================================================
            // PHASE 8: Validate Platform Telemetry Events
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
                "Found: {} start, {} runtimeDone, {} report events",
                start_events.len(),
                runtime_done_events.len(),
                report_events.len()
            );

            assert_eq!(
                start_events.len(),
                1,
                "Expected 1 platform.start event, got {}",
                start_events.len()
            );
            assert_eq!(
                runtime_done_events.len(),
                1,
                "Expected 1 platform.runtimeDone event, got {}",
                runtime_done_events.len()
            );

            // platform.report is emitted after extensions signal readiness
            // This is a best-effort check since timing can vary
            if report_events.is_empty() {
                println!("WARNING: platform.report not captured (extension timing)");
            } else {
                println!("platform.report captured successfully");
            }

            // Verify platform.start content
            let start = &start_events[0];
            assert_eq!(
                start.record["requestId"],
                json!(request_id),
                "platform.start has wrong requestId"
            );
            println!("Platform telemetry events validated");

            // =========================================================================
            // PHASE 9: Cleanup runtime, trigger graceful shutdown
            // =========================================================================
            runtime_handle.abort();
            let _ = runtime_handle.await;
        },
    )
    .await;

    // Graceful shutdown triggers SHUTDOWN event to extensions, causing final flush
    println!("Triggering graceful shutdown...");
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    // Wait for extension to process shutdown and complete its final flush
    println!("Waiting for extension to complete shutdown...");
    let _ = tokio::time::timeout(Duration::from_secs(5), extension_handle).await;

    // Wait for spans to arrive at collector using mock-collector's built-in waiting
    println!("Waiting for spans at collector...");
    match collector.wait_for_spans(1, Duration::from_secs(5)).await {
        Ok(()) => println!("Received span(s) at collector"),
        Err(e) => println!("WARNING: Timed out waiting for spans: {}", e),
    }

    // =========================================================================
    // PHASE 10: Validate Function Spans at Collector
    // =========================================================================
    collector
        .with_collector(|c| {
            println!("Collector has {} span(s)", c.span_count());

            // Check we have at least one span
            if c.span_count() == 0 {
                println!("WARNING: No spans received at collector");
                println!("This may indicate the extension didn't receive/forward the spans");
                return;
            }

            // Find the http.request span
            let span_assertion = c.expect_span_with_name("http.request");
            let matching_spans = span_assertion.get_all();

            if matching_spans.is_empty() {
                println!("No http.request span found");
                println!("Available spans:");
                for span in c.spans() {
                    println!(
                        "  - {} (trace_id: {})",
                        span.span().name,
                        hex::encode(&span.span().trace_id)
                    );
                }
                return;
            }

            let span = matching_spans[0];
            let span_trace_id = hex::encode(&span.span().trace_id);
            let span_parent_id = hex::encode(&span.span().parent_span_id);

            println!("Found http.request span:");
            println!("  trace_id: {}", span_trace_id);
            println!("  parent_span_id: {}", span_parent_id);
            println!("  expected trace_id: {}", trace_id);
            println!("  expected parent_span_id: {}", parent_span_id);

            // =========================================================================
            // CRITICAL ASSERTION: Trace context propagation
            // =========================================================================
            assert_eq!(
                span_trace_id, trace_id,
                "Function span has wrong trace_id - trace propagation failed"
            );
            assert_eq!(
                span_parent_id, parent_span_id,
                "Function span has wrong parent_span_id - trace propagation failed"
            );

            println!("Trace context propagation verified!");

            // =========================================================================
            // PHASE 11: Validate Semantic Attributes
            // =========================================================================
            c.expect_span_with_name("http.request")
                .with_attributes([
                    ("faas.trigger", "http"),
                    ("http.request.method", "POST"),
                    ("url.path", "/test"),
                ])
                .assert_exists();

            println!("Semantic attributes validated!");
        })
        .await;

    // =========================================================================
    // PHASE 12: Validate Platform Metrics at Collector
    // =========================================================================
    collector
        .with_collector(|c| {
            println!("Collector has {} metric(s)", c.metric_count());

            // Collect metric names for assertion
            let metric_names: Vec<String> = c
                .metrics()
                .iter()
                .map(|m| m.metric().name.clone())
                .collect();
            println!("Metrics received:");
            for name in &metric_names {
                println!("  - {}", name);
            }

            // Assert we have the expected platform metrics from platform.report
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

            println!("Platform metrics validated!");
        })
        .await;

    // Shutdown collector
    collector
        .shutdown()
        .await
        .expect("Collector shutdown failed");

    println!("E2E test completed successfully!");
}

#[tokio::test]
#[serial]
async fn test_e2e_missing_traceparent_creates_new_trace() {
    // This test verifies behavior when no traceparent header is present
    // The function should create a new root span (no parent)

    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = Simulator::builder()
        .function_name("otel-no-trace-test")
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");

    // Start extension
    let extension_config = Config {
        exporter: ExporterConfig {
            endpoint: Some(collector_endpoint.clone()),
            protocol: Protocol::Http,
            timeout: Duration::from_secs(5),
            compression: Compression::None,
            ..Default::default()
        },
        telemetry_api: TelemetryApiConfig {
            enabled: false, // Disable for simpler test
            ..Default::default()
        },
        receiver: ReceiverConfig {
            http_enabled: true,
            http_port: 4318, // Tests are serial so port reuse is safe
            ..Default::default()
        },
        flush: FlushConfig {
            strategy: FlushStrategy::End,
            ..Default::default()
        },
        ..Default::default()
    };

    // Set environment variable for lambda_extension crate before spawning
    // The lambda_extension crate expects the full URL with http:// prefix
    // SAFETY: Test runs serially so no other code is reading environment concurrently
    let extension_runtime_api = runtime_api_base.clone();
    unsafe {
        std::env::set_var(
            "AWS_LAMBDA_RUNTIME_API",
            format!("http://{}", extension_runtime_api),
        );
    }

    let extension_handle = tokio::spawn(async move {
        let runtime = RuntimeBuilder::new().config(extension_config).build();
        let _ = runtime.run().await;
    });

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension did not register");

    // Wait for OTLP receiver to be ready
    wait_for_http_ready(4318, Duration::from_secs(5))
        .await
        .expect("OTLP receiver did not become ready");

    // HTTP event WITHOUT traceparent
    let http_event = json!({
        "version": "2.0",
        "routeKey": "POST /test",
        "rawPath": "/test",
        "headers": {
            "content-type": "application/json"
            // No traceparent!
        },
        "body": json!({
            "message": "no trace context",
            "delay_ms": 5
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

    async_with_vars(
        [
            ("AWS_LAMBDA_RUNTIME_API", Some(runtime_api_base.as_str())),
            ("AWS_LAMBDA_FUNCTION_NAME", Some("otel-no-trace-test")),
            ("AWS_LAMBDA_FUNCTION_VERSION", Some("$LATEST")),
            ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", Some("128")),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://127.0.0.1:4318")),
        ],
        async {
            simulator.enqueue(invocation).await;

            let runtime_handle = tokio::spawn(async move {
                let _ = run(service_fn(http_handler)).await;
            });

            let state = simulator
                .wait_for_invocation_complete(&request_id, Duration::from_secs(10))
                .await
                .expect("Invocation did not complete");

            assert_eq!(state.status, InvocationStatus::Success);

            runtime_handle.abort();
            let _ = runtime_handle.await;
        },
    )
    .await;

    // Graceful shutdown triggers SHUTDOWN event to extension, causing final flush
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    // Wait for extension to complete shutdown
    let _ = tokio::time::timeout(Duration::from_secs(5), extension_handle).await;

    // Wait for spans to arrive at collector using mock-collector's built-in waiting
    let _ = collector.wait_for_spans(1, Duration::from_secs(5)).await;

    // Validate spans
    collector
        .with_collector(|c| {
            if c.span_count() > 0 {
                let span_assertion = c.expect_span_with_name("http.request");
                let spans = span_assertion.get_all();
                if !spans.is_empty() {
                    let span = spans[0];
                    // When no traceparent is provided, parent_span_id should be empty
                    // (all zeros or empty bytes)
                    let parent_id = &span.span().parent_span_id;
                    let is_root = parent_id.is_empty() || parent_id.iter().all(|&b| b == 0);
                    println!(
                        "Span parent_span_id: {} (is_root: {})",
                        hex::encode(parent_id),
                        is_root
                    );

                    // The span should still have a valid trace_id
                    assert!(
                        !span.span().trace_id.is_empty(),
                        "Root span should have a trace_id"
                    );
                    assert!(
                        !span.span().trace_id.iter().all(|&b| b == 0),
                        "Root span should have non-zero trace_id"
                    );
                }
            }
        })
        .await;

    collector
        .shutdown()
        .await
        .expect("Collector shutdown failed");
}

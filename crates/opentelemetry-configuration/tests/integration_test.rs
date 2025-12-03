//! Integration tests verifying the OpenTelemetry configuration wires everything together correctly.
//!
//! Note: Since the tracing subscriber can only be initialised once per process,
//! we use a single comprehensive test that exercises all the key functionality.

use mock_collector::{MockServer, Protocol as MockProtocol};
use opentelemetry_configuration::{OtelSdkBuilder, Protocol, SdkError};
use std::time::Duration;
use tracing::instrument;

#[tokio::test(flavor = "multi_thread")]
async fn test_tracing_spans_exported_to_otlp_collector() -> Result<(), SdkError> {
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let endpoint = format!("http://{}", collector.addr());

    let guard = OtelSdkBuilder::new()
        .service_name("integration-test-service")
        .endpoint(&endpoint)
        .protocol(Protocol::HttpBinary)
        .traces(true)
        .metrics(true)
        .logs(true)
        .build()?;

    // Test 1: Simple instrumented function creates a span
    do_traced_work();

    // Test 2: Nested spans maintain parent-child relationship
    outer_work();

    // Test 3: Span attributes are captured
    do_work_with_attributes("test-user", 42);

    // Flush to ensure spans are exported immediately.
    // Use block_in_place to allow blocking without starving the tokio runtime.
    tokio::task::block_in_place(|| guard.flush());

    // Wait for spans to arrive at collector
    // We expect: do_traced_work, outer_work, inner_work, do_work_with_attributes = 4 spans
    collector
        .wait_for_spans(4, Duration::from_secs(5))
        .await
        .expect("Should receive all spans");

    // Verify spans were created with correct names
    collector
        .with_collector(|c| {
            c.expect_span_with_name("do_traced_work").assert_exists();
            c.expect_span_with_name("outer_work").assert_exists();
            c.expect_span_with_name("inner_work").assert_exists();
            c.expect_span_with_name("do_work_with_attributes")
                .assert_exists();
        })
        .await;

    // Explicit shutdown (also needs block_in_place)
    tokio::task::block_in_place(|| guard.shutdown())?;

    Ok(())
}

#[instrument]
fn do_traced_work() {
    tracing::info!("Doing some traced work");
}

#[instrument]
fn outer_work() {
    tracing::info!("Starting outer work");
    inner_work();
    tracing::info!("Finished outer work");
}

#[instrument]
fn inner_work() {
    tracing::info!("Doing inner work");
}

#[instrument(fields(user_id = %user_id, count = %count))]
fn do_work_with_attributes(user_id: &str, count: i32) {
    tracing::info!("Processing for user {} with count {}", user_id, count);
}

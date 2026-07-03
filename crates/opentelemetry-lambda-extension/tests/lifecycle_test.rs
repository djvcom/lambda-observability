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

use common::harness::{
    make_logs_request, make_metrics_request, make_trace_request, post_invocation_complete,
    post_protobuf, report_duration_ms, run_invocation, span_count, spawn_extension,
    start_tarpit_collector,
};
use lambda_simulator::{DeliveryPolicy, FreezeMode, Simulator};
use mock_collector::{MockServer, Protocol as MockProtocol};
use prost::Message;
use serial_test::serial;
use std::time::{Duration, Instant};

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

    simulator
        .wait_for_frozen(Duration::from_secs(6))
        .await
        .expect("Deadline fallback should release the hold within the invocation deadline plus a small margin");

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
    simulator.enable_telemetry_capture().await;

    let _extension = spawn_extension(&simulator, &collector_endpoint).await;
    let client = reqwest::Client::new();

    let request_id_1 = run_invocation(&simulator, &client, |_| async {}).await;
    let duration_1 = report_duration_ms(&simulator, &request_id_1).await;
    assert!(
        duration_1 >= 1_000.0,
        "First invocation should pay a bounded deadline hold while waiting \
         for completion signals that never arrive (observed {}ms)",
        duration_1
    );

    let request_id_2 = run_invocation(&simulator, &client, |_| async {}).await;
    let duration_2 = report_duration_ms(&simulator, &request_id_2).await;
    assert!(
        duration_2 < 500.0,
        "After the first deadline timeout, holding must be disabled so \
         subsequent invocations pay no hold cost (observed {}ms)",
        duration_2
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
    let tarpit_addr = start_tarpit_collector().await;

    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

    let simulator = Simulator::builder()
        .function_name("lifecycle-test")
        .shutdown_timeout(SHUTDOWN_TIMEOUT)
        .build()
        .await
        .expect("Failed to start simulator");

    let extension = spawn_extension(&simulator, &format!("http://{}", tarpit_addr)).await;
    let client = reqwest::Client::new();

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

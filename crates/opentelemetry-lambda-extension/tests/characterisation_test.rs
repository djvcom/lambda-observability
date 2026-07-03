//! Characterisation tests measuring the extension's behaviour under load
//! and over long-running invocations.
//!
//! Unlike the lifecycle tests, these are less pass/fail properties and more
//! measurements with generous regression bounds: they print the observed
//! figures (per-invocation overhead percentiles, mid-invocation export
//! progress, resident memory under flood) so behavioural drift is visible
//! in test output before it becomes a failure.
//!
//! Run with pre-built binaries:
//! ```sh
//! cargo build --workspace
//! cargo test -p opentelemetry-lambda-extension --test characterisation_test -- --ignored --nocapture
//! ```

mod common;

use common::harness::{
    RECEIVER_PORT, make_trace_request, post_invocation_complete, post_protobuf, report_duration_ms,
    run_invocation, span_count, spawn_extension, spawn_extension_with_env,
};
use mock_collector::{MockServer, Protocol as MockProtocol};
use prost::Message;
use serial_test::serial;
use std::time::Duration;

/// With a wrapper signalling completion and a near-instant handler, the
/// `platform.report` duration is dominated by the extension's
/// post-invocation work, so its percentiles over a sustained run measure
/// the per-invocation overhead the extension adds.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn sustained_invocation_overhead() {
    const INVOCATIONS: usize = 200;

    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = lambda_simulator::Simulator::builder()
        .function_name("characterisation-test")
        .extension_ready_timeout(Duration::from_secs(15))
        .invocation_timeout(Duration::from_secs(3))
        .build()
        .await
        .expect("Failed to start simulator");
    simulator.enable_telemetry_capture().await;

    let _extension = spawn_extension(&simulator, &collector_endpoint).await;
    let client = reqwest::Client::new();

    let mut durations_ms = Vec::with_capacity(INVOCATIONS);
    for i in 0..INVOCATIONS {
        let request_id = run_invocation(&simulator, &client, |request_id| async move {
            let client = reqwest::Client::new();
            post_protobuf(
                &client,
                "/v1/traces",
                make_trace_request(&format!("span-{i}")).encode_to_vec(),
            )
            .await;
            post_invocation_complete(&client, &request_id).await;
        })
        .await;

        durations_ms.push(report_duration_ms(&simulator, &request_id).await);
    }

    durations_ms.sort_by(|a, b| a.total_cmp(b));
    let percentile = |p: f64| durations_ms[((durations_ms.len() - 1) as f64 * p) as usize];
    let p50 = percentile(0.50);
    let p99 = percentile(0.99);
    println!(
        "invocation duration (instant handler) over {} invocations: \
         p50={p50:.2}ms p99={p99:.2}ms max={:.2}ms",
        durations_ms.len(),
        durations_ms.last().unwrap()
    );

    assert!(
        p99 < 500.0,
        "p99 invocation duration ({p99:.2}ms) should stay well under the hold deadline"
    );
}

/// With a periodic flush strategy, telemetry from a long-running invocation
/// must reach the collector while the handler is still executing, not only
/// at invocation end.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn long_invocation_periodic_export() {
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = lambda_simulator::Simulator::builder()
        .function_name("characterisation-test")
        .extension_ready_timeout(Duration::from_secs(15))
        .invocation_timeout(Duration::from_secs(30))
        .build()
        .await
        .expect("Failed to start simulator");

    let _extension = spawn_extension_with_env(
        &simulator,
        &collector_endpoint,
        &[
            ("LAMBDA_OTEL_FLUSH_STRATEGY", "periodic".to_string()),
            ("LAMBDA_OTEL_FLUSH_INTERVAL", "1000".to_string()),
        ],
    )
    .await;
    let client = reqwest::Client::new();

    let collector_ref = &collector;
    run_invocation(&simulator, &client, |_request_id| async move {
        let client = reqwest::Client::new();
        post_protobuf(
            &client,
            "/v1/traces",
            make_trace_request("mid-invocation-span").encode_to_vec(),
        )
        .await;

        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while span_count(collector_ref).await == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(
            span_count(collector_ref).await >= 1,
            "Periodic flush should export spans while the handler is still running"
        );
        println!("mid-invocation export observed before the handler responded");
    })
    .await;

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;
}

/// Reads the extension process's resident set size from `/proc`.
fn resident_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Under a sustained flood of signals with an unreachable collector, the
/// shared queue budget must hold the extension's resident memory to a
/// bounded ceiling instead of growing with the volume of telemetry offered.
#[tokio::test]
#[serial]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn rss_ceiling_under_flood() {
    const QUEUE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;
    const RSS_CEILING_BYTES: u64 = 150 * 1024 * 1024;
    const BATCHES: usize = 2_000;
    const SPANS_PER_BATCH: usize = 512;

    let simulator = lambda_simulator::Simulator::builder()
        .function_name("characterisation-test")
        .extension_ready_timeout(Duration::from_secs(15))
        .build()
        .await
        .expect("Failed to start simulator");

    let extension = spawn_extension_with_env(
        &simulator,
        "http://127.0.0.1:9",
        &[(
            "LAMBDA_OTEL_FLUSH_MAX_QUEUE_BYTES",
            QUEUE_BUDGET_BYTES.to_string(),
        )],
    )
    .await;
    let pid = extension.pid();
    let client = reqwest::Client::new();

    let mut batch = make_trace_request("flood-span");
    let span = batch.resource_spans[0].scope_spans[0].spans[0].clone();
    for i in 1..SPANS_PER_BATCH {
        let mut extra = span.clone();
        extra.name = format!("flood-span-{i}-with-some-padding-to-carry-real-weight");
        batch.resource_spans[0].scope_spans[0].spans.push(extra);
    }
    let body = batch.encode_to_vec();
    println!(
        "flooding {} batches of {} bytes ({} MiB total encoded) against an {} MiB budget",
        BATCHES,
        body.len(),
        BATCHES * body.len() / (1024 * 1024),
        QUEUE_BUDGET_BYTES / (1024 * 1024)
    );

    let mut max_rss = 0u64;
    let mut rejected = 0usize;
    for i in 0..BATCHES {
        let response = client
            .post(format!("http://127.0.0.1:{}/v1/traces", RECEIVER_PORT))
            .header("Content-Type", "application/x-protobuf")
            .body(body.clone())
            .send()
            .await
            .expect("Failed to reach extension receiver");
        if !response.status().is_success() {
            rejected += 1;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        if i % 50 == 0
            && let Some(rss) = resident_bytes(pid)
        {
            max_rss = max_rss.max(rss);
        }
    }
    if let Some(rss) = resident_bytes(pid) {
        max_rss = max_rss.max(rss);
    }

    println!(
        "max RSS {} MiB over {} batches ({} rejected with backpressure)",
        max_rss / (1024 * 1024),
        BATCHES,
        rejected
    );

    assert!(max_rss > 0, "Expected to sample RSS from /proc");
    assert!(
        max_rss < RSS_CEILING_BYTES,
        "Resident memory ({} MiB) should stay under {} MiB with an {} MiB queue budget",
        max_rss / (1024 * 1024),
        RSS_CEILING_BYTES / (1024 * 1024),
        QUEUE_BUDGET_BYTES / (1024 * 1024)
    );

    simulator
        .graceful_shutdown(lambda_simulator::ShutdownReason::Spindown)
        .await;
}

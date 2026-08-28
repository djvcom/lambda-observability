//! Start-up timing measurement for the extension binary.
//!
//! Runs the real extension binary against the Lambda simulator and
//! measures the two figures that govern the extension's cold-start
//! contribution:
//!
//! - **spawn to register**: process spawn until `POST /extension/register`
//!   arrives. This is serial with the whole cold start, because Lambda only
//!   begins runtime init once every extension has registered.
//! - **spawn to next**: process spawn until the first
//!   `GET /extension/event/next` poll. This is the point at which the
//!   extension has finished its own init.
//!
//! The test is informational: it prints the per-run figures and the
//! summary statistics without asserting any thresholds, so it cannot flake
//! in CI. A regression gate can be layered on later by setting the
//! `STARTUP_TIMING_MAX_REGISTER_MS` and `STARTUP_TIMING_MAX_NEXT_MS`
//! environment variables, which turn the summary medians into assertions.
//!
//! Run with:
//!
//! ```sh
//! cargo build --workspace
//! cargo test -p opentelemetry-lambda-extension --test startup_timing_test -- --ignored --nocapture
//! ```

mod common;

use common::harness::find_binary;
use lambda_simulator::Simulator;
use lambda_simulator::process::{ProcessConfig, ProcessRole};
use std::time::Duration;

const RUNS: usize = 7;
const RECEIVER_PORT: u16 = 24519;

struct RunTiming {
    register_ms: f64,
    next_ms: f64,
}

async fn measure_one_run() -> RunTiming {
    let simulator = Simulator::builder()
        .function_name("startup-timing")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_base = simulator.runtime_api_url().replace("http://", "");
    let extension_binary = find_binary("opentelemetry-lambda-extension");

    let config = ProcessConfig::new(&extension_binary, ProcessRole::Extension)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("LAMBDA_OTEL_RECEIVER_HTTP_PORT", RECEIVER_PORT.to_string())
        .env("RUST_LOG", "error");

    let spawned_at = chrono::Utc::now();
    let _extension = simulator
        .spawn_process_with_config(config)
        .expect("Failed to spawn extension");

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(10),
        )
        .await
        .expect("Extension did not register");

    let registered = simulator
        .get_registered_extensions()
        .await
        .into_iter()
        .next()
        .expect("Extension registration should be recorded");

    let mut first_next = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        first_next = simulator.first_next_poll_at(&registered.id).await;
        if first_next.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_micros(200)).await;
    }
    let first_next = first_next.expect("Extension did not poll /next");

    simulator.shutdown().await;

    let register_ms = (registered.registered_at - spawned_at)
        .num_microseconds()
        .expect("Register delta should fit in microseconds") as f64
        / 1000.0;
    let next_ms = (first_next - spawned_at)
        .num_microseconds()
        .expect("Next delta should fit in microseconds") as f64
        / 1000.0;

    RunTiming {
        register_ms,
        next_ms,
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

fn summarise(label: &str, values: &mut [f64]) -> f64 {
    let median = median(values);
    let min = values.first().copied().unwrap_or_default();
    let max = values.last().copied().unwrap_or_default();
    println!(
        "{label}: min {min:.2} ms, median {median:.2} ms, max {max:.2} ms over {} runs",
        values.len()
    );
    median
}

fn optional_gate(median_ms: f64, env_var: &str) {
    if let Ok(limit) = std::env::var(env_var) {
        let limit: f64 = limit
            .parse()
            .unwrap_or_else(|_| panic!("{env_var} should hold a number of milliseconds"));
        assert!(
            median_ms <= limit,
            "median {median_ms:.2} ms exceeded the {limit:.2} ms limit set by {env_var}"
        );
    }
}

#[tokio::test]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn measure_spawn_to_register_and_next() {
    let mut register = Vec::with_capacity(RUNS);
    let mut next = Vec::with_capacity(RUNS);

    for run in 1..=RUNS {
        let timing = measure_one_run().await;
        println!(
            "run {run}: spawn to register {:.2} ms, spawn to next {:.2} ms",
            timing.register_ms, timing.next_ms
        );
        register.push(timing.register_ms);
        next.push(timing.next_ms);
    }

    let register_median = summarise("spawn to register", &mut register);
    let next_median = summarise("spawn to next", &mut next);

    optional_gate(register_median, "STARTUP_TIMING_MAX_REGISTER_MS");
    optional_gate(next_median, "STARTUP_TIMING_MAX_NEXT_MS");
}

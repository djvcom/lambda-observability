//! Demo test showcasing Lambda process freeze/thaw with real processes.
//!
//! This test demonstrates the full Lambda lifecycle with actual SIGSTOP/SIGCONT
//! process freezing, showing:
//! - Real process spawning (extension and runtime binaries)
//! - Cold start initialisation
//! - Invocation processing with trace context
//! - Process freezing between invocations (like real Lambda)
//! - Process thawing for subsequent invocations
//! - Warm start behaviour
//! - Graceful shutdown
//! - Telemetry metrics collection
//!
//! ## Platform Support
//!
//! This test requires Unix-like systems with SIGSTOP/SIGCONT signal support.
//! It will not work on Windows.
//!
//! ## Prerequisites
//!
//! Build workspace binaries before running:
//! ```sh
//! cargo build --workspace
//! ```
//!
//! ## Running the Demo
//!
//! ```sh
//! cargo test -p lambda-simulator --test freeze_demo_test -- --nocapture --ignored
//! ```
//!
//! ## Interactive Mode (for VHS recordings)
//!
//! The test can wait for stdin input between phases, allowing VHS to control pacing:
//! ```sh
//! DEMO_INTERACTIVE=1 cargo test -p lambda-simulator --test freeze_demo_test -- --nocapture --ignored
//! ```

use lambda_simulator::process::{ProcessConfig, ProcessRole};
use lambda_simulator::{
    FreezeMode, InvocationBuilder, InvocationStatus, ShutdownReason, Simulator,
};
use mock_collector::{MockServer, Protocol as MockProtocol};
use std::io::{self, BufRead};
use std::time::Duration;

mod common;
use common::{is_process_stopped, truncate_id, wait_for_http_ready};

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

const EXPECTED_MIN_SPANS: usize = 9;
const EXPECTED_MIN_LOGS: usize = 1;

fn is_interactive() -> bool {
    std::env::var("DEMO_INTERACTIVE").is_ok()
}

fn wait_for_input() {
    if is_interactive() {
        let stdin = io::stdin();
        let _ = stdin.lock().lines().next();
    }
}

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
        "Binary '{}' not found. Run `cargo build --workspace` first.",
        name
    );
}

fn print_header(text: &str) {
    let bar = "═".repeat(60);
    println!();
    println!("{BOLD}{CYAN}{bar}{RESET}");
    println!("{BOLD}{CYAN}  {text}{RESET}");
    println!("{BOLD}{CYAN}{bar}{RESET}");
}

fn print_step(icon: &str, text: &str) {
    println!("{BOLD}{GREEN}{icon}{RESET} {text}");
}

fn print_info(text: &str) {
    println!("{DIM}   {text}{RESET}");
}

fn print_event(phase: &str, text: &str) {
    println!("{YELLOW}[{phase}]{RESET} {text}");
}

fn print_metric(name: &str, value: &str) {
    println!("   {BLUE}•{RESET} {name}: {MAGENTA}{value}{RESET}");
}

#[tokio::test]
#[ignore = "requires pre-built binaries: cargo build --workspace"]
async fn demo_freeze_thaw_with_real_processes() {
    print_header("Lambda Process Freeze/Thaw Demo");
    println!();
    println!("{DIM}This demo shows real Lambda behaviour with SIGSTOP/SIGCONT{RESET}");
    println!("{DIM}process freezing between invocations.{RESET}");
    println!();

    // =========================================================================
    // Start Mock Collector
    // =========================================================================
    print_step("📡", "Starting OTLP collector...");
    let collector = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let collector_endpoint = format!("http://{}", collector.addr());
    print_info(&format!("Collector listening at {}", collector_endpoint));

    // =========================================================================
    // Start Simulator with Freeze Mode
    // =========================================================================
    print_step(
        "🚀",
        "Starting Lambda simulator with FreezeMode::Process...",
    );
    let simulator = Simulator::builder()
        .function_name("freeze-demo")
        .memory_size_mb(256)
        .freeze_mode(FreezeMode::Process)
        .extension_ready_timeout(Duration::from_secs(10))
        .build()
        .await
        .expect("Failed to start simulator");

    simulator.enable_telemetry_capture().await;
    let runtime_api = simulator.runtime_api_url();
    let runtime_api_base = runtime_api.replace("http://", "");
    print_info(&format!("Runtime API at {}", runtime_api));

    // =========================================================================
    // Spawn Extension Process
    // =========================================================================
    print_step("🔌", "Spawning OpenTelemetry extension process...");
    let extension_binary = find_binary("opentelemetry-lambda-extension");
    let extension_config = ProcessConfig::new(&extension_binary, ProcessRole::Extension)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("LAMBDA_OTEL_EXPORTER_ENDPOINT", &collector_endpoint)
        .env("LAMBDA_OTEL_EXPORTER_PROTOCOL", "http")
        .env("LAMBDA_OTEL_EXPORTER_COMPRESSION", "none")
        .env("LAMBDA_OTEL_FLUSH_STRATEGY", "end")
        .env("LAMBDA_OTEL_RECEIVER_HTTP_ENABLED", "true")
        .env("LAMBDA_OTEL_TELEMETRY_API_ENABLED", "true")
        .env("RUST_LOG", "info,opentelemetry_lambda_extension=info")
        .inherit_stdio(true);

    let mut extension = simulator
        .spawn_process_with_config(extension_config)
        .expect("Failed to spawn extension");

    print_info(&format!("Extension PID: {}", extension.pid()));

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(10),
        )
        .await
        .expect("Extension did not register");

    wait_for_http_ready(4318, Duration::from_secs(5))
        .await
        .expect("Extension OTLP receiver not ready");

    print_event("INIT", "Extension registered and ready");

    // =========================================================================
    // Spawn Runtime Process
    // =========================================================================
    print_step("⚡", "Spawning instrumented runtime process...");
    let runtime_binary = find_binary("http_runtime");
    let runtime_config = ProcessConfig::new(&runtime_binary, ProcessRole::Runtime)
        .env("AWS_LAMBDA_RUNTIME_API", &runtime_api_base)
        .env("AWS_LAMBDA_FUNCTION_NAME", "freeze-demo")
        .env("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST")
        .env("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "256")
        .env("AWS_REGION", "us-east-1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("RUST_LOG", "info,opentelemetry=debug")
        .inherit_stdio(true);

    let runtime = simulator
        .spawn_process_with_config(runtime_config)
        .expect("Failed to spawn runtime");

    print_info(&format!("Runtime PID: {}", runtime.pid()));
    print_event("INIT", "Runtime initialised (cold start)");

    tokio::time::sleep(Duration::from_millis(300)).await;
    wait_for_input();

    // =========================================================================
    // First Invocation (Cold Start)
    // =========================================================================
    println!();
    print_step("1️⃣", "First invocation (cold start)...");

    let invocation_1 = InvocationBuilder::new()
        .payload(serde_json::json!({
            "version": "2.0",
            "routeKey": "POST /hello",
            "rawPath": "/hello",
            "headers": {"content-type": "application/json"},
            "body": "{\"message\":\"Hello from first invocation\",\"delay_ms\":10}",
            "requestContext": {"http": {"method": "POST", "path": "/hello"}}
        }))
        .build()
        .expect("Failed to build first invocation");

    let request_id_1 = invocation_1.request_id.clone();
    simulator.enqueue(invocation_1).await;

    let state_1 = simulator
        .wait_for_invocation_complete(&request_id_1, Duration::from_secs(15))
        .await
        .expect("First invocation timed out");

    assert_eq!(state_1.status, InvocationStatus::Success);
    print_event(
        "INVOKE",
        &format!("Request {} completed", truncate_id(&request_id_1, 8)),
    );

    simulator
        .wait_for_extensions_ready(&request_id_1, Duration::from_secs(5))
        .await
        .expect("Extensions should be ready after first invocation");

    wait_for_input();

    // =========================================================================
    // Freeze Phase
    // =========================================================================
    println!();
    print_step("❄️", "Freezing processes (SIGSTOP)...");
    print_info("Runtime and extension processes suspended");
    print_info("This simulates Lambda's behaviour between invocations");

    let runtime_pid = runtime.pid();
    let extension_pid = extension.pid();

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        is_process_stopped(runtime_pid),
        "Runtime process should be frozen (SIGSTOP)"
    );
    assert!(
        is_process_stopped(extension_pid),
        "Extension process should be frozen (SIGSTOP)"
    );
    print_event("FREEZE", "Verified: Both processes are stopped");

    tokio::time::sleep(Duration::from_millis(400)).await;
    wait_for_input();

    // =========================================================================
    // Second Invocation (Warm Start - Thaw)
    // =========================================================================
    // The fact that this invocation completes successfully proves thaw worked.
    // We don't check process state here because by the time we could check,
    // the process may already be frozen again after completing.
    println!();
    print_step("2️⃣", "Second invocation (warm start, thawing processes)...");
    print_info("Processes receiving SIGCONT to resume");

    let invocation_2 = InvocationBuilder::new()
        .payload(serde_json::json!({
            "version": "2.0",
            "routeKey": "POST /hello",
            "rawPath": "/hello",
            "headers": {"content-type": "application/json"},
            "body": "{\"message\":\"Second invocation after thaw\",\"delay_ms\":5}",
            "requestContext": {"http": {"method": "POST", "path": "/hello"}}
        }))
        .build()
        .expect("Failed to build second invocation");

    let request_id_2 = invocation_2.request_id.clone();
    simulator.enqueue(invocation_2).await;

    let state_2 = simulator
        .wait_for_invocation_complete(&request_id_2, Duration::from_secs(15))
        .await
        .expect("Second invocation timed out");

    assert_eq!(state_2.status, InvocationStatus::Success);
    print_event(
        "INVOKE",
        &format!("Request {} completed (warm)", truncate_id(&request_id_2, 8)),
    );

    simulator
        .wait_for_extensions_ready(&request_id_2, Duration::from_secs(5))
        .await
        .expect("Extensions should be ready after second invocation");

    wait_for_input();

    // =========================================================================
    // Third Invocation (Warm Start)
    // =========================================================================
    println!();
    print_step("3️⃣", "Third invocation (warm start)...");

    let invocation_3 = InvocationBuilder::new()
        .payload(serde_json::json!({
            "version": "2.0",
            "routeKey": "GET /status",
            "rawPath": "/status",
            "headers": {},
            "body": "{\"message\":\"Status check\",\"delay_ms\":3}",
            "requestContext": {"http": {"method": "GET", "path": "/status"}}
        }))
        .build()
        .expect("Failed to build third invocation");

    let request_id_3 = invocation_3.request_id.clone();
    simulator.enqueue(invocation_3).await;

    let state_3 = simulator
        .wait_for_invocation_complete(&request_id_3, Duration::from_secs(15))
        .await
        .expect("Third invocation timed out");

    assert_eq!(state_3.status, InvocationStatus::Success);
    print_event(
        "INVOKE",
        &format!("Request {} completed (warm)", truncate_id(&request_id_3, 8)),
    );

    simulator
        .wait_for_extensions_ready(&request_id_3, Duration::from_secs(5))
        .await
        .expect("Extensions should be ready after third invocation");

    // =========================================================================
    // Capture telemetry before shutdown
    // =========================================================================
    let telemetry_events = simulator.get_telemetry_events().await;

    wait_for_input();

    // =========================================================================
    // Shutdown
    // =========================================================================
    println!();
    print_step("🛑", "Initiating graceful shutdown...");

    // In real Lambda, the platform terminates the runtime before the extension exits.
    // We need to drop the runtime first so its OTel providers flush to the extension,
    // then let the extension export to the collector before it exits.
    print_info("Terminating runtime process (like Lambda platform does)");
    drop(runtime);

    // Give the extension time to receive any final telemetry from the runtime
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Now trigger the graceful shutdown for the extension
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;
    print_event("SHUTDOWN", "Extension received shutdown signal");

    // Wait for extension process to exit
    match tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if extension.try_wait().ok().flatten().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    {
        Ok(()) => print_event("SHUTDOWN", "Extension exited cleanly"),
        Err(_) => print_event("SHUTDOWN", "Extension exit timed out (will be killed)"),
    }

    // Wait for telemetry to arrive at collector
    collector
        .wait_for_spans(EXPECTED_MIN_SPANS, Duration::from_secs(5))
        .await
        .expect("Telemetry spans should arrive at collector");
    collector
        .wait_for_logs(EXPECTED_MIN_LOGS, Duration::from_secs(2))
        .await
        .expect("Telemetry logs should arrive at collector");

    // =========================================================================
    // Telemetry Summary
    // =========================================================================
    println!();
    print_header("Telemetry Summary");
    print_info(&format!(
        "{} platform events captured",
        telemetry_events.len()
    ));

    collector
        .with_collector(|c| {
            println!();
            print_info(&format!("{} spans exported", c.span_count()));
            print_info(&format!("{} metrics exported", c.metric_count()));
            print_info(&format!("{} logs exported", c.log_count()));

            if c.span_count() > 0 {
                println!();
                println!("{BOLD}   Spans:{RESET}");
                for span in c.spans() {
                    let span_name = &span.span().name;
                    print_metric(span_name, "✓");
                }
            }

            if c.metric_count() > 0 {
                println!();
                println!("{BOLD}   Metrics:{RESET}");
                for metric in c.metrics() {
                    let name = &metric.metric().name;
                    print_metric(name, "✓");
                }
            }

            if c.log_count() > 0 {
                println!();
                println!("{BOLD}   Logs:{RESET}");

                c.expect_log_with_body("Hello from first invocation")
                    .assert_exists();
                print_metric("Cold start log received", "✓");

                c.expect_log_with_body("Second invocation after thaw")
                    .assert_exists();
                print_metric("Warm start (post-thaw) log received", "✓");

                c.expect_log_with_body("Status check").assert_exists();
                print_metric("Third invocation log received", "✓");

                c.expect_log_with_body("Request completed")
                    .assert_at_least(3);
                print_metric("Handler completion logs (3+)", "✓");
            }
        })
        .await;

    // =========================================================================
    // Cleanup
    // =========================================================================
    drop(extension);
    collector
        .shutdown()
        .await
        .expect("Collector shutdown failed");

    println!();
    print_header("Demo Complete!");
    println!();
    println!("{DIM}The simulator accurately reproduces Lambda's freeze/thaw{RESET}");
    println!("{DIM}behaviour using real SIGSTOP/SIGCONT signals.{RESET}");
    println!();
}

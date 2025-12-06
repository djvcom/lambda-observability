//! Integration tests for process spawning with the simulator.
//!
//! These tests verify that the simulator can spawn and manage runtime and
//! extension binaries, automatically injecting Lambda environment variables
//! and registering PIDs for freeze/thaw.

use lambda_simulator::process::{ProcessConfig, ProcessRole};
use lambda_simulator::{FreezeMode, InvocationStatus, SimulatorPhase, Simulator};
use serde_json::json;
use std::process::Command;
use std::time::Duration;

fn get_process_state(pid: u32) -> Option<char> {
    let output = Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .ok()?;

    let state_str = String::from_utf8_lossy(&output.stdout);
    state_str.trim().chars().next()
}

fn is_process_stopped(pid: u32) -> bool {
    get_process_state(pid).map(|s| s == 'T').unwrap_or(false)
}

fn is_process_running(pid: u32) -> bool {
    get_process_state(pid).is_some()
}

#[tokio::test]
async fn test_spawn_runtime_process() {
    let simulator = Simulator::builder()
        .function_name("test-spawn-runtime")
        .build()
        .await
        .expect("Failed to create simulator");

    let runtime_binary = env!("CARGO_BIN_EXE_test_runtime");

    let runtime = simulator
        .spawn_process(runtime_binary, ProcessRole::Runtime)
        .expect("Failed to spawn runtime");

    assert!(runtime.pid() > 0, "Should have valid PID");
    assert_eq!(runtime.role(), ProcessRole::Runtime);
    assert!(
        is_process_running(runtime.pid()),
        "Process should be running"
    );

    simulator
        .wait_for_phase(SimulatorPhase::Ready, Duration::from_secs(5))
        .await
        .expect("Runtime should become ready");

    let request_id = simulator.enqueue_payload(json!({"test": "spawn"})).await;

    let state = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(5))
        .await
        .expect("Invocation should complete");

    assert_eq!(state.status, InvocationStatus::Success);

    drop(runtime);

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_spawn_extension_process() {
    let simulator = Simulator::builder()
        .function_name("test-spawn-extension")
        .build()
        .await
        .expect("Failed to create simulator");

    let extension_binary = env!("CARGO_BIN_EXE_test_extension");

    let extension = simulator
        .spawn_process(extension_binary, ProcessRole::Extension)
        .expect("Failed to spawn extension");

    assert!(extension.pid() > 0, "Should have valid PID");
    assert_eq!(extension.role(), ProcessRole::Extension);
    assert!(
        is_process_running(extension.pid()),
        "Process should be running"
    );

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension should register");

    assert_eq!(simulator.extension_count().await, 1);

    drop(extension);

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_spawn_runtime_and_extension_together() {
    let simulator = Simulator::builder()
        .function_name("test-spawn-both")
        .build()
        .await
        .expect("Failed to create simulator");

    let runtime_binary = env!("CARGO_BIN_EXE_test_runtime");
    let extension_binary = env!("CARGO_BIN_EXE_test_extension");

    let extension = simulator
        .spawn_process(extension_binary, ProcessRole::Extension)
        .expect("Failed to spawn extension");

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension should register");

    let runtime = simulator
        .spawn_process(runtime_binary, ProcessRole::Runtime)
        .expect("Failed to spawn runtime");

    simulator
        .wait_for_phase(SimulatorPhase::Ready, Duration::from_secs(5))
        .await
        .expect("Runtime should become ready");

    let request_id_1 = simulator.enqueue_payload(json!({"invocation": 1})).await;

    let state_1 = simulator
        .wait_for_invocation_complete(&request_id_1, Duration::from_secs(5))
        .await
        .expect("First invocation should complete");

    assert_eq!(state_1.status, InvocationStatus::Success);

    let request_id_2 = simulator.enqueue_payload(json!({"invocation": 2})).await;

    let state_2 = simulator
        .wait_for_invocation_complete(&request_id_2, Duration::from_secs(5))
        .await
        .expect("Second invocation should complete");

    assert_eq!(state_2.status, InvocationStatus::Success);

    drop(runtime);
    drop(extension);

    simulator.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_spawn_with_freeze_mode_registers_pid() {
    let simulator = Simulator::builder()
        .function_name("test-spawn-freeze")
        .freeze_mode(FreezeMode::Process)
        .build()
        .await
        .expect("Failed to create simulator");

    let runtime_binary = env!("CARGO_BIN_EXE_test_runtime");

    let runtime = simulator
        .spawn_process(runtime_binary, ProcessRole::Runtime)
        .expect("Failed to spawn runtime");

    let pid = runtime.pid();
    assert!(is_process_running(pid), "Process should be running");

    simulator
        .wait_for_phase(SimulatorPhase::Ready, Duration::from_secs(5))
        .await
        .expect("Runtime should become ready");

    assert!(
        !is_process_stopped(pid),
        "Process should not be stopped initially"
    );

    drop(runtime);

    simulator.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_freeze_thaw_with_spawned_processes() {
    let simulator = Simulator::builder()
        .function_name("test-freeze-thaw-spawn")
        .freeze_mode(FreezeMode::Process)
        .build()
        .await
        .expect("Failed to create simulator");

    let runtime_binary = env!("CARGO_BIN_EXE_test_runtime");
    let extension_binary = env!("CARGO_BIN_EXE_test_extension");

    let extension = simulator
        .spawn_process(extension_binary, ProcessRole::Extension)
        .expect("Failed to spawn extension");

    let extension_pid = extension.pid();

    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension should register");

    let runtime = simulator
        .spawn_process(runtime_binary, ProcessRole::Runtime)
        .expect("Failed to spawn runtime");

    let runtime_pid = runtime.pid();

    simulator
        .wait_for_phase(SimulatorPhase::Ready, Duration::from_secs(5))
        .await
        .expect("Runtime should become ready");

    assert!(
        !is_process_stopped(runtime_pid),
        "Runtime should be running"
    );
    assert!(
        !is_process_stopped(extension_pid),
        "Extension should be running"
    );

    let request_id_1 = simulator.enqueue_payload(json!({"invocation": 1})).await;

    let state_1 = simulator
        .wait_for_invocation_complete(&request_id_1, Duration::from_secs(5))
        .await
        .expect("First invocation should complete");

    assert_eq!(state_1.status, InvocationStatus::Success);

    let request_id_2 = simulator.enqueue_payload(json!({"invocation": 2})).await;

    let state_2 = simulator
        .wait_for_invocation_complete(&request_id_2, Duration::from_secs(5))
        .await
        .expect("Second invocation should complete");

    assert_eq!(state_2.status, InvocationStatus::Success);

    drop(runtime);
    drop(extension);

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_process_config_with_additional_env() {
    let simulator = Simulator::builder()
        .function_name("test-spawn-config")
        .build()
        .await
        .expect("Failed to create simulator");

    let runtime_binary = env!("CARGO_BIN_EXE_test_runtime");

    let config = ProcessConfig::new(runtime_binary, ProcessRole::Runtime)
        .env("CUSTOM_TEST_VAR", "test_value")
        .inherit_stdio(false);

    let runtime = simulator
        .spawn_process_with_config(config)
        .expect("Failed to spawn runtime");

    assert!(runtime.pid() > 0);
    assert!(is_process_running(runtime.pid()));

    simulator
        .wait_for_phase(SimulatorPhase::Ready, Duration::from_secs(5))
        .await
        .expect("Runtime should become ready");

    let request_id = simulator.enqueue_payload(json!({"test": "config"})).await;

    let state = simulator
        .wait_for_invocation_complete(&request_id, Duration::from_secs(5))
        .await
        .expect("Invocation should complete");

    assert_eq!(state.status, InvocationStatus::Success);

    drop(runtime);

    simulator.shutdown().await;
}

#[tokio::test]
async fn test_managed_process_cleanup_on_drop() {
    let simulator = Simulator::builder()
        .function_name("test-spawn-cleanup")
        .build()
        .await
        .expect("Failed to create simulator");

    let runtime_binary = env!("CARGO_BIN_EXE_test_runtime");

    let pid = {
        let runtime = simulator
            .spawn_process(runtime_binary, ProcessRole::Runtime)
            .expect("Failed to spawn runtime");
        let pid = runtime.pid();
        assert!(is_process_running(pid), "Process should be running");
        pid
    };

    simulator
        .wait_for(
            || async move { !is_process_running(pid) },
            Duration::from_secs(5),
        )
        .await
        .expect("Process should be terminated after ManagedProcess is dropped");

    simulator.shutdown().await;
}

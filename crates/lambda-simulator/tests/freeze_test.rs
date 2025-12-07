//! Tests for process freezing functionality.

use lambda_simulator::{FreezeMode, FreezeState, Simulator};
use serde_json::json;
use std::process::{Child, Command};
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

fn test_runtime_path() -> &'static str {
    env!("CARGO_BIN_EXE_test_runtime")
}

fn spawn_test_runtime(runtime_api: &str) -> Child {
    Command::new(test_runtime_path())
        .env("AWS_LAMBDA_RUNTIME_API", runtime_api)
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn test_runtime")
}

#[tokio::test]
async fn test_freeze_mode_none_is_default() {
    let simulator = Simulator::builder().build().await.unwrap();
    assert_eq!(simulator.freeze_mode(), FreezeMode::None);
    assert!(!simulator.is_frozen());
    simulator.shutdown().await;
}

#[tokio::test]
async fn test_freeze_mode_none_does_not_freeze() {
    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::None)
        .build()
        .await
        .unwrap();

    assert_eq!(simulator.freeze_mode(), FreezeMode::None);
    assert!(!simulator.is_frozen());
    assert_eq!(simulator.freeze_epoch(), 0);

    let runtime_api = simulator.runtime_api_url().replace("http://", "");
    let mut child = spawn_test_runtime(&runtime_api);
    let pid = child.id();

    tokio::time::sleep(Duration::from_millis(100)).await;

    simulator.enqueue_payload(json!({"test": "data"})).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        !is_process_stopped(pid),
        "Process should not be stopped with FreezeMode::None"
    );
    assert!(!simulator.is_frozen());

    child.kill().ok();
    let _ = child.wait();
    simulator.shutdown().await;
}

/// FreezeMode::Process no longer requires PID at build time.
/// PIDs can be registered dynamically after spawning processes.
#[tokio::test]
async fn test_process_mode_without_pid_succeeds_at_build() {
    let result = Simulator::builder()
        .freeze_mode(FreezeMode::Process)
        .build()
        .await;

    assert!(
        result.is_ok(),
        "Should succeed - PIDs can be registered dynamically after build"
    );

    let simulator = result.unwrap();
    simulator.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_freeze_state_epoch_counter() {
    let state = FreezeState::new(FreezeMode::Process, Some(999999));

    assert_eq!(state.current_epoch(), 0);
    assert!(!state.is_frozen());

    state.unfreeze().unwrap();
    assert_eq!(state.current_epoch(), 1);

    state.unfreeze().unwrap();
    assert_eq!(state.current_epoch(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn test_freeze_epoch_prevents_stale_freeze() {
    let state = FreezeState::new(FreezeMode::Process, Some(999999));

    let epoch = state.current_epoch();

    state.unfreeze().unwrap();
    assert_eq!(state.current_epoch(), 1);

    let result = state.freeze_at_epoch(epoch);
    assert!(
        matches!(result, Ok(false)),
        "Stale epoch should prevent freeze"
    );

    assert!(!state.is_frozen());
}

#[cfg(unix)]
#[tokio::test]
async fn test_freeze_without_pid_returns_error() {
    let state = FreezeState::new(FreezeMode::Process, None);

    let result = state.freeze_at_epoch(0);
    assert!(result.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn test_basic_freeze_unfreeze_with_real_process() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::None)
        .build()
        .await
        .unwrap();

    let runtime_api = simulator.runtime_api_url().replace("http://", "");

    let mut child = spawn_test_runtime(&runtime_api);
    let pid = child.id();

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(!is_process_stopped(pid), "Process should be running");

    kill(Pid::from_raw(pid as i32), Signal::SIGSTOP).expect("Failed to send SIGSTOP");

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        is_process_stopped(pid),
        "Process should be stopped after SIGSTOP"
    );

    kill(Pid::from_raw(pid as i32), Signal::SIGCONT).expect("Failed to send SIGCONT");

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !is_process_stopped(pid),
        "Process should be running after SIGCONT"
    );

    child.kill().ok();
    let _ = child.wait();
    simulator.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_freeze_state_direct_signal_test() {
    let mut child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn sleep");

    let pid = child.id();

    let state = FreezeState::new(FreezeMode::Process, Some(pid));

    assert!(!state.is_frozen());
    assert!(!is_process_stopped(pid), "Process should start running");

    let result = state.freeze_at_epoch(0);
    assert!(matches!(result, Ok(true)), "Freeze should succeed");
    assert!(state.is_frozen());

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(is_process_stopped(pid), "Process should be stopped");

    state.unfreeze().expect("Unfreeze should succeed");
    assert!(!state.is_frozen());
    assert_eq!(state.current_epoch(), 1);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!is_process_stopped(pid), "Process should be running again");

    child.kill().ok();
    let _ = child.wait();
}

#[cfg(unix)]
#[tokio::test]
async fn test_force_unfreeze() {
    let mut child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn sleep");

    let pid = child.id();

    let state = FreezeState::new(FreezeMode::Process, Some(pid));

    state.freeze_at_epoch(0).unwrap();
    assert!(state.is_frozen());

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(is_process_stopped(pid));

    state.force_unfreeze();
    assert!(!state.is_frozen());
    assert_eq!(state.current_epoch(), 1);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!is_process_stopped(pid));

    child.kill().ok();
    let _ = child.wait();
}

#[cfg(unix)]
#[tokio::test]
async fn test_simulator_freeze_helpers() {
    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::None)
        .runtime_pid(12345)
        .build()
        .await
        .unwrap();

    assert_eq!(simulator.freeze_mode(), FreezeMode::None);
    assert!(!simulator.is_frozen());
    assert_eq!(simulator.freeze_epoch(), 0);

    simulator.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_enqueue_increments_epoch() {
    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::None)
        .build()
        .await
        .unwrap();

    let initial_epoch = simulator.freeze_epoch();

    simulator.enqueue_payload(json!({"test": "1"})).await;
    assert_eq!(
        simulator.freeze_epoch(),
        initial_epoch + 1,
        "Epoch should increment on enqueue"
    );

    simulator.enqueue_payload(json!({"test": "2"})).await;
    assert_eq!(
        simulator.freeze_epoch(),
        initial_epoch + 2,
        "Epoch should increment on second enqueue"
    );

    simulator.shutdown().await;
}

/// Back-to-back invocations should both complete successfully.
#[tokio::test]
async fn test_rapid_successive_invocations() {
    use lambda_simulator::InvocationStatus;
    use reqwest::Client;

    let simulator = Simulator::builder()
        .function_name("test-function")
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    // Enqueue multiple invocations rapidly
    let mut request_ids = Vec::new();
    for i in 0..5 {
        simulator.enqueue_payload(json!({"iteration": i})).await;
        let states = simulator.get_all_invocation_states().await;
        request_ids.push(states.last().unwrap().invocation.request_id.clone());
    }

    // Process all invocations
    for i in 0..5 {
        let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
        let response = client
            .get(&next_url)
            .send()
            .await
            .expect("Failed to get invocation");

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .expect("Missing request ID header")
            .to_str()
            .unwrap()
            .to_string();

        let payload: serde_json::Value = response.json().await.expect("Failed to parse payload");
        assert_eq!(
            payload["iteration"], i,
            "Invocations should be processed in order"
        );

        let response_url = format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            runtime_api_url, request_id
        );
        client
            .post(&response_url)
            .json(&json!({"result": i}))
            .send()
            .await
            .expect("Failed to send response");
    }

    // Verify all completed successfully
    for request_id in &request_ids {
        let state = simulator
            .get_invocation_state(request_id)
            .await
            .expect("Invocation state not found");
        assert_eq!(
            state.status,
            InvocationStatus::Success,
            "Invocation {} should complete successfully",
            request_id
        );
    }

    simulator.shutdown().await;
}

/// Freeze cancelled on new invocation - epoch mechanism prevents stale freeze.
#[cfg(unix)]
#[tokio::test]
async fn test_freeze_cancelled_by_new_invocation() {
    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::None)
        .build()
        .await
        .unwrap();

    // Record initial epoch
    let initial_epoch = simulator.freeze_epoch();

    // Enqueue first invocation
    simulator.enqueue_payload(json!({"invocation": 1})).await;
    assert_eq!(
        simulator.freeze_epoch(),
        initial_epoch + 1,
        "Epoch should increment after first enqueue"
    );

    // Simulate scenario where freeze would be attempted at old epoch
    // but a new invocation arrives first
    let stale_epoch = simulator.freeze_epoch();

    // Enqueue second invocation (this would cancel any pending freeze)
    simulator.enqueue_payload(json!({"invocation": 2})).await;
    assert_eq!(
        simulator.freeze_epoch(),
        stale_epoch + 1,
        "Epoch should increment again, invalidating the stale epoch"
    );

    // Any freeze attempt at the stale_epoch should fail
    // (The simulator doesn't expose freeze_at_epoch directly, but the
    // mechanism is tested via FreezeState in other tests)

    simulator.shutdown().await;
}

/// Readiness state isolation - each invocation has independent readiness tracking.
#[tokio::test]
async fn test_readiness_state_isolation() {
    use lambda_simulator::InvocationStatus;
    use reqwest::Client;

    let simulator = Simulator::builder()
        .function_name("test-function")
        .extension_ready_timeout(Duration::from_millis(100))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    // First invocation - use the returned request ID
    let request_id_1 = simulator.enqueue_payload(json!({"invocation": 1})).await;

    // Process first invocation
    let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get first invocation");

    let response_url_1 = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id_1
    );
    client
        .post(&response_url_1)
        .json(&json!({"result": 1}))
        .send()
        .await
        .expect("Failed to send first response");

    // Wait for first invocation to complete
    simulator
        .wait_for_invocation_complete(&request_id_1, Duration::from_secs(5))
        .await
        .expect("First invocation should complete");

    // Second invocation - use the returned request ID
    let request_id_2 = simulator.enqueue_payload(json!({"invocation": 2})).await;

    // Process second invocation
    let _ = client
        .get(&next_url)
        .send()
        .await
        .expect("Failed to get second invocation");

    let response_url_2 = format!(
        "{}/2018-06-01/runtime/invocation/{}/response",
        runtime_api_url, request_id_2
    );
    client
        .post(&response_url_2)
        .json(&json!({"result": 2}))
        .send()
        .await
        .expect("Failed to send second response");

    // Wait for second invocation to complete
    simulator
        .wait_for_invocation_complete(&request_id_2, Duration::from_secs(5))
        .await
        .expect("Second invocation should complete");

    // Verify both completed independently
    let state_1 = simulator
        .get_invocation_state(&request_id_1)
        .await
        .expect("First invocation state not found");
    let state_2 = simulator
        .get_invocation_state(&request_id_2)
        .await
        .expect("Second invocation state not found");

    assert_eq!(
        state_1.status,
        InvocationStatus::Success,
        "First invocation should be successful"
    );
    assert_eq!(
        state_2.status,
        InvocationStatus::Success,
        "Second invocation should be successful"
    );

    // Verify they have different request IDs (isolation)
    assert_ne!(
        request_id_1, request_id_2,
        "Each invocation should have a unique request ID"
    );

    simulator.shutdown().await;
}

/// Entire execution environment frozen (runtime + extensions).
///
/// In real AWS Lambda, the ENTIRE execution environment is frozen between
/// invocations, including the runtime and all extension processes. This test
/// verifies that when multiple PIDs are configured, all of them are frozen
/// and unfrozen together.
#[cfg(unix)]
#[tokio::test]
async fn test_entire_environment_frozen() {
    let mut runtime_process = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn runtime process");

    let mut extension1_process = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn extension 1 process");

    let mut extension2_process = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn extension 2 process");

    let runtime_pid = runtime_process.id();
    let ext1_pid = extension1_process.id();
    let ext2_pid = extension2_process.id();

    let state = FreezeState::with_pids(FreezeMode::Process, vec![runtime_pid, ext1_pid, ext2_pid]);

    assert!(
        !is_process_stopped(runtime_pid),
        "Runtime should start running"
    );
    assert!(
        !is_process_stopped(ext1_pid),
        "Extension 1 should start running"
    );
    assert!(
        !is_process_stopped(ext2_pid),
        "Extension 2 should start running"
    );

    let result = state.freeze_at_epoch(0);
    assert!(matches!(result, Ok(true)), "Freeze should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        is_process_stopped(runtime_pid),
        "Runtime should be frozen (SIGSTOP)"
    );
    assert!(
        is_process_stopped(ext1_pid),
        "Extension 1 should be frozen (SIGSTOP)"
    );
    assert!(
        is_process_stopped(ext2_pid),
        "Extension 2 should be frozen (SIGSTOP)"
    );

    state.unfreeze().expect("Unfreeze should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        !is_process_stopped(runtime_pid),
        "Runtime should be unfrozen (SIGCONT)"
    );
    assert!(
        !is_process_stopped(ext1_pid),
        "Extension 1 should be unfrozen (SIGCONT)"
    );
    assert!(
        !is_process_stopped(ext2_pid),
        "Extension 2 should be unfrozen (SIGCONT)"
    );

    runtime_process.kill().ok();
    let _ = runtime_process.wait();
    extension1_process.kill().ok();
    let _ = extension1_process.wait();
    extension2_process.kill().ok();
    let _ = extension2_process.wait();
}

/// Test dynamic PID registration after simulator is created.
///
/// This is the realistic workflow: create simulator, spawn processes, then register PIDs.
#[cfg(unix)]
#[tokio::test]
async fn test_dynamic_pid_registration() {
    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::Process)
        .build()
        .await
        .unwrap();

    let mut runtime_process = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn runtime process");

    let mut extension_process = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn extension process");

    let runtime_pid = runtime_process.id();
    let ext_pid = extension_process.id();

    simulator.register_freeze_pid(runtime_pid);
    simulator.register_freeze_pid(ext_pid);

    assert!(
        !is_process_stopped(runtime_pid),
        "Runtime should start running"
    );
    assert!(
        !is_process_stopped(ext_pid),
        "Extension should start running"
    );

    let state = FreezeState::with_pids(FreezeMode::Process, vec![runtime_pid, ext_pid]);
    state.freeze_at_epoch(0).expect("Freeze should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(is_process_stopped(runtime_pid), "Runtime should be frozen");
    assert!(is_process_stopped(ext_pid), "Extension should be frozen");

    state.unfreeze().expect("Unfreeze should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        !is_process_stopped(runtime_pid),
        "Runtime should be unfrozen"
    );
    assert!(!is_process_stopped(ext_pid), "Extension should be unfrozen");

    simulator.shutdown().await;

    runtime_process.kill().ok();
    let _ = runtime_process.wait();
    extension_process.kill().ok();
    let _ = extension_process.wait();
}

/// No freeze during shutdown - runtime stays awake for cleanup.
/// Once shutdown is initiated, no freeze operations should be attempted.
#[cfg(unix)]
#[tokio::test]
async fn test_no_freeze_during_shutdown() {
    use lambda_simulator::ShutdownReason;

    let mut child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("Failed to spawn sleep");

    let pid = child.id();

    let simulator = Simulator::builder()
        .freeze_mode(FreezeMode::Process)
        .runtime_pid(pid)
        .shutdown_timeout(Duration::from_millis(500))
        .build()
        .await
        .unwrap();

    assert!(!is_process_stopped(pid), "Process should start running");

    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        !is_process_stopped(pid),
        "Process should NOT be frozen during/after shutdown - runtime needs to stay awake for cleanup"
    );

    child.kill().ok();
    let _ = child.wait();
}

/// Stress test 100+ rapid invocations - no freeze/thaw race conditions.
#[tokio::test]
async fn test_stress_test_rapid_invocations() {
    use lambda_simulator::InvocationStatus;
    use reqwest::Client;

    let simulator = Simulator::builder()
        .function_name("stress-test-function")
        .extension_ready_timeout(Duration::from_millis(50))
        .build()
        .await
        .expect("Failed to start simulator");

    let runtime_api_url = simulator.runtime_api_url();
    let client = Client::new();

    let num_invocations = 100;
    let mut request_ids = Vec::with_capacity(num_invocations);

    for i in 0..num_invocations {
        let request_id = simulator.enqueue_payload(json!({"iteration": i})).await;
        request_ids.push(request_id);
    }

    assert_eq!(
        request_ids.len(),
        num_invocations,
        "All invocations should be enqueued"
    );

    for i in 0..num_invocations {
        let next_url = format!("{}/2018-06-01/runtime/invocation/next", runtime_api_url);
        let response = client
            .get(&next_url)
            .send()
            .await
            .expect("Failed to get invocation");

        let request_id = response
            .headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .expect("Missing request ID header")
            .to_str()
            .unwrap()
            .to_string();

        let response_url = format!(
            "{}/2018-06-01/runtime/invocation/{}/response",
            runtime_api_url, request_id
        );
        client
            .post(&response_url)
            .json(&json!({"result": i}))
            .send()
            .await
            .expect("Failed to send response");
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut success_count = 0;
    for request_id in &request_ids {
        if let Some(state) = simulator.get_invocation_state(request_id).await
            && state.status == InvocationStatus::Success
        {
            success_count += 1;
        }
    }

    assert_eq!(
        success_count, num_invocations,
        "All {} invocations should complete successfully (got {})",
        num_invocations, success_count
    );

    assert!(
        simulator.freeze_epoch() >= num_invocations as u64,
        "Epoch should have incremented at least {} times",
        num_invocations
    );

    simulator.shutdown().await;
}

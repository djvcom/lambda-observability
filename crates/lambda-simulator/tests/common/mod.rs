//! Shared test utilities for lambda-simulator integration tests.

use std::time::{Duration, Instant};

/// Reads the process state character from /proc on Linux or uses ps on macOS.
///
/// Returns the single-character state code:
/// - `R` = Running
/// - `S` = Sleeping
/// - `T` = Stopped (traced or SIGSTOP)
/// - `Z` = Zombie
/// - `D` = Uninterruptible sleep
#[cfg(target_os = "linux")]
pub fn get_process_state(pid: u32) -> Option<char> {
    std::fs::read_to_string(format!("/proc/{}/stat", pid))
        .ok()
        .and_then(|stat| {
            stat.split_whitespace()
                .nth(2)
                .and_then(|s| s.chars().next())
        })
}

#[cfg(target_os = "macos")]
pub fn get_process_state(pid: u32) -> Option<char> {
    std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .chars()
                .next()
        })
}

/// Returns true if the process is in the stopped state (SIGSTOP).
pub fn is_process_stopped(pid: u32) -> bool {
    get_process_state(pid).map(|s| s == 'T').unwrap_or(false)
}

/// Returns true if the process exists.
#[allow(dead_code)]
pub fn process_exists(pid: u32) -> bool {
    get_process_state(pid).is_some()
}

/// Safely truncates a request ID for display purposes.
pub fn truncate_id(id: &str, max_len: usize) -> &str {
    id.get(..max_len).unwrap_or(id)
}

pub async fn wait_for_http_ready(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let url = format!("http://127.0.0.1:{}/health", port);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    while Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return Ok(());
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    Err(format!(
        "HTTP server health check timed out after {:?} on port {}",
        timeout, port
    ))
}

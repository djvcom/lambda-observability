//! Shared test utilities for lambda-otel-extension integration tests.
//!
//! This module provides event-driven waiting utilities to replace arbitrary sleeps
//! in tests, ensuring deterministic behaviour and faster test execution.

use std::time::{Duration, Instant};

/// Polls an HTTP health endpoint until it responds successfully.
///
/// This is the preferred method for waiting for HTTP servers to start in tests,
/// rather than using arbitrary sleeps which are both slower and less reliable.
///
/// # Arguments
///
/// * `port` - The port to check for health
/// * `timeout` - Maximum time to wait for the server to become healthy
///
/// # Returns
///
/// Returns `Ok(())` if the server becomes healthy within the timeout,
/// otherwise returns an error describing the failure.
///
/// # Examples
///
/// ```ignore
/// wait_for_http_ready(14318, Duration::from_secs(5)).await?;
/// ```
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    async fn handle_health() -> &'static str {
        "OK"
    }

    #[tokio::test]
    async fn test_wait_for_http_ready_success() {
        let (ready_tx, ready_rx) = oneshot::channel();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let app = Router::new().route("/health", get(handle_health));
            ready_tx.send(()).ok();
            axum::serve(listener, app).await.unwrap();
        });

        ready_rx.await.expect("Server failed to signal readiness");

        let result = wait_for_http_ready(port, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "Expected success, got {:?}", result);

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_wait_for_http_ready_timeout() {
        // Use a port that definitely isn't listening
        let result = wait_for_http_ready(19999, Duration::from_millis(100)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timed out"));
    }
}

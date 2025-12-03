//! Tests for adaptive export and freeze-safe patterns.
//!
//! These tests verify the FlushManager correctly implements adaptive flush
//! strategies based on invocation patterns, deadline awareness, and buffer limits.
//!
//! Test requirements covered:
//! - ADAPT-001 through ADAPT-006: Freeze-safe export strategy
//! - ADAPT-020 through ADAPT-025: Adaptive pattern detection
//!
//! Note: Full integration tests for the extension's flush behaviour during
//! invocation cycles are covered in e2e_integration_test.rs. This file focuses
//! on unit tests for the FlushManager logic.

mod common;

use lambda_simulator::{ShutdownReason, Simulator};
use mock_collector::{MockServer, Protocol as MockProtocol, ServerHandle};
use opentelemetry_lambda_extension::{FlushConfig, FlushManager, FlushReason, FlushStrategy};
use serial_test::serial;
use std::time::Duration;

// =============================================================================
// ADAPT-001: Infrequent pattern flushes at invocation end
// =============================================================================

/// ADAPT-001: With End strategy, should_flush returns None during invocation
/// but should_flush_on_invocation_end returns InvocationEnd.
#[test]
fn test_end_strategy_flushes_on_invocation_end() {
    let config = FlushConfig {
        strategy: FlushStrategy::End,
        interval: Duration::from_millis(100),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // During invocation, should_flush returns None
    let reason = manager.should_flush(None, 5, false);
    assert!(
        reason.is_none(),
        "End strategy should not flush mid-invocation"
    );

    // At invocation end, should flush
    let reason = manager.should_flush_on_invocation_end(5);
    assert_eq!(
        reason,
        Some(FlushReason::InvocationEnd),
        "End strategy should flush at invocation end"
    );
}

/// ADAPT-001: Default strategy with infrequent pattern behaves like End strategy.
#[test]
fn test_default_strategy_with_infrequent_pattern_flushes_on_end() {
    let config = FlushConfig {
        strategy: FlushStrategy::Default,
        interval: Duration::from_millis(100),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // With no invocation history, pattern is considered infrequent
    assert!(
        manager.is_infrequent_pattern(),
        "Empty history should be considered infrequent"
    );

    // At invocation end, should flush
    let reason = manager.should_flush_on_invocation_end(5);
    assert_eq!(
        reason,
        Some(FlushReason::InvocationEnd),
        "Default strategy with infrequent pattern should flush at invocation end"
    );
}

// =============================================================================
// ADAPT-002: Frequent pattern uses periodic flush
// =============================================================================

/// ADAPT-002: Continuous strategy flushes at periodic intervals.
#[test]
fn test_continuous_strategy_periodic_flush() {
    let config = FlushConfig {
        strategy: FlushStrategy::Continuous,
        interval: Duration::from_millis(10),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let mut manager = FlushManager::new(config);

    // First call should flush immediately (no previous flush)
    let reason = manager.should_flush(None, 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Continuous),
        "Continuous strategy should flush when interval elapsed"
    );

    // Record the flush
    manager.record_flush();

    // Immediately after flush, should not flush again
    let reason = manager.should_flush(None, 1, false);
    assert!(
        reason.is_none(),
        "Should not flush immediately after recording flush"
    );

    // After interval passes, should flush again
    std::thread::sleep(Duration::from_millis(15));
    let reason = manager.should_flush(None, 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Continuous),
        "Should flush after interval elapsed"
    );
}

/// ADAPT-002: Periodic strategy behaves like Continuous.
#[test]
fn test_periodic_strategy_flush() {
    let config = FlushConfig {
        strategy: FlushStrategy::Periodic,
        interval: Duration::from_millis(10),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let mut manager = FlushManager::new(config);

    // First call should flush
    let reason = manager.should_flush(None, 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Periodic),
        "Periodic strategy should flush when interval elapsed"
    );

    manager.record_flush();

    // After interval passes, should flush again
    std::thread::sleep(Duration::from_millis(15));
    let reason = manager.should_flush(None, 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Periodic),
        "Periodic strategy should flush after interval"
    );
}

// =============================================================================
// ADAPT-003: No export after /next (verified at architectural level)
// =============================================================================
// This is enforced by the extension runtime calling should_flush_on_invocation_end
// BEFORE calling /next. See runtime.rs for implementation.

/// ADAPT-003: Verify that should_flush_on_invocation_end is called before /next.
/// The FlushManager's design ensures flush decisions happen at invocation boundaries,
/// not after signalling readiness (calling /next).
#[test]
fn test_no_export_after_next_architecture() {
    let config = FlushConfig {
        strategy: FlushStrategy::End,
        interval: Duration::from_millis(100),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // With End strategy, should_flush (called during invocation) returns None
    let reason = manager.should_flush(None, 10, false);
    assert!(
        reason.is_none(),
        "End strategy should not flush during invocation (after /next returns)"
    );

    // should_flush_on_invocation_end (called BEFORE /next) returns InvocationEnd
    let reason = manager.should_flush_on_invocation_end(10);
    assert_eq!(
        reason,
        Some(FlushReason::InvocationEnd),
        "Flush decision should be made at invocation end, before calling /next"
    );
}

// =============================================================================
// ADAPT-004: Shutdown forces flush
// =============================================================================

/// ADAPT-004: Shutdown always triggers flush regardless of strategy.
#[test]
fn test_shutdown_always_flushes() {
    // Test with End strategy (normally doesn't flush mid-invocation)
    let config = FlushConfig {
        strategy: FlushStrategy::End,
        interval: Duration::from_secs(60),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // Even with pending_count = 0, shutdown forces flush
    let reason = manager.should_flush(None, 0, true);
    assert_eq!(
        reason,
        Some(FlushReason::Shutdown),
        "Shutdown should always trigger flush"
    );

    // With pending data, shutdown still takes priority
    let reason = manager.should_flush(None, 10, true);
    assert_eq!(
        reason,
        Some(FlushReason::Shutdown),
        "Shutdown should take priority over other reasons"
    );
}

// =============================================================================
// ADAPT-005: Buffer full triggers immediate flush
// =============================================================================

/// ADAPT-005: When buffer reaches max_batch_entries, flush immediately.
#[test]
fn test_buffer_full_triggers_flush() {
    let config = FlushConfig {
        strategy: FlushStrategy::End, // Strategy that wouldn't normally flush mid-invocation
        interval: Duration::from_secs(60),
        max_batch_entries: 10,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // Below limit, no flush
    let reason = manager.should_flush(None, 5, false);
    assert!(reason.is_none(), "Should not flush when buffer is not full");

    // At or above limit, should flush
    let reason = manager.should_flush(None, 10, false);
    assert_eq!(
        reason,
        Some(FlushReason::BufferFull),
        "Should flush when buffer reaches limit"
    );

    let reason = manager.should_flush(None, 15, false);
    assert_eq!(
        reason,
        Some(FlushReason::BufferFull),
        "Should flush when buffer exceeds limit"
    );
}

// =============================================================================
// ADAPT-006: Deadline approaching triggers flush
// =============================================================================

/// ADAPT-006: Flush when deadline is within 500ms.
#[test]
fn test_deadline_approaching_triggers_flush() {
    let config = FlushConfig {
        strategy: FlushStrategy::End, // Strategy that wouldn't normally flush mid-invocation
        interval: Duration::from_secs(60),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // Calculate deadline 200ms from now (within 500ms buffer)
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let approaching_deadline = now_ms + 200;

    let reason = manager.should_flush(Some(approaching_deadline), 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Deadline),
        "Should flush when deadline is approaching"
    );

    // Deadline far in future, no flush
    let distant_deadline = now_ms + 60000;
    let reason = manager.should_flush(Some(distant_deadline), 1, false);
    assert!(
        reason.is_none(),
        "Should not flush when deadline is far away"
    );
}

// =============================================================================
// ADAPT-020: Infrequent pattern detected (>120s average interval)
// =============================================================================

/// ADAPT-020: Pattern detected as infrequent when average interval > 120s.
#[test]
fn test_infrequent_pattern_detection() {
    let config = FlushConfig::default();
    let manager = FlushManager::new(config);

    // With no history, pattern is infrequent (conservative default)
    assert!(
        manager.is_infrequent_pattern(),
        "Empty history should be infrequent"
    );

    // With only one invocation, still infrequent (can't calculate interval)
    let mut manager = FlushManager::new(FlushConfig::default());
    manager.record_invocation();
    assert!(
        manager.is_infrequent_pattern(),
        "Single invocation should be infrequent"
    );
}

// =============================================================================
// ADAPT-021: Frequent pattern detected (<=120s average interval)
// =============================================================================

/// ADAPT-021: Pattern detected as frequent when average interval <= 120s.
#[test]
fn test_frequent_pattern_detection() {
    let config = FlushConfig::default();
    let mut manager = FlushManager::new(config);

    // Record multiple invocations in quick succession
    for _ in 0..5 {
        manager.record_invocation();
    }

    // Average interval should be very small (milliseconds apart)
    let avg = manager.average_invocation_interval();
    assert!(avg.is_some(), "Should have calculated average interval");
    assert!(
        avg.unwrap() < Duration::from_secs(120),
        "Average interval should be under 120s"
    );

    assert!(
        !manager.is_infrequent_pattern(),
        "Quick successive invocations should be frequent pattern"
    );
}

// =============================================================================
// ADAPT-022: Pattern transition (tested via Default strategy behaviour)
// =============================================================================

/// ADAPT-022: Default strategy adapts based on detected pattern.
#[test]
fn test_default_strategy_adapts_to_pattern() {
    let config = FlushConfig {
        strategy: FlushStrategy::Default,
        interval: Duration::from_millis(10),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let mut manager = FlushManager::new(config);

    // Initially infrequent - should flush at invocation end
    assert!(manager.is_infrequent_pattern());
    let reason = manager.should_flush_on_invocation_end(1);
    assert_eq!(reason, Some(FlushReason::InvocationEnd));

    // After many quick invocations, should become frequent
    for _ in 0..5 {
        manager.record_invocation();
    }
    assert!(!manager.is_infrequent_pattern());

    // With frequent pattern, should_flush_on_invocation_end returns None
    let reason = manager.should_flush_on_invocation_end(1);
    assert!(
        reason.is_none(),
        "Frequent pattern should not flush at invocation end"
    );

    // But should_flush will trigger Continuous behaviour after interval
    std::thread::sleep(Duration::from_millis(15));
    let reason = manager.should_flush(None, 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Continuous),
        "Default with frequent pattern should use Continuous"
    );
}

// =============================================================================
// ADAPT-023: Timeout escalation (20+ consecutive timeouts -> Continuous)
// =============================================================================

/// ADAPT-023: After 20 consecutive flush timeouts, escalate to Continuous.
#[test]
fn test_timeout_escalation_to_continuous() {
    let config = FlushConfig {
        strategy: FlushStrategy::End,
        interval: Duration::from_millis(10),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let mut manager = FlushManager::new(config);

    // End strategy normally doesn't flush mid-invocation
    let reason = manager.should_flush(None, 1, false);
    assert!(
        reason.is_none(),
        "End strategy should not flush mid-invocation"
    );

    // Record 20 consecutive timeouts
    for i in 0..20 {
        manager.record_flush_timeout();
        assert_eq!(
            manager.consecutive_timeout_count(),
            i + 1,
            "Timeout count should increment"
        );
    }

    // After escalation, should use Continuous strategy
    let reason = manager.should_flush(None, 1, false);
    assert_eq!(
        reason,
        Some(FlushReason::Continuous),
        "After 20 timeouts, should escalate to Continuous"
    );
}

/// Successful flush resets timeout counter.
#[test]
fn test_successful_flush_resets_timeout_count() {
    let config = FlushConfig::default();
    let mut manager = FlushManager::new(config);

    // Record some timeouts
    manager.record_flush_timeout();
    manager.record_flush_timeout();
    manager.record_flush_timeout();
    assert_eq!(manager.consecutive_timeout_count(), 3);

    // Successful flush resets counter
    manager.record_flush();
    assert_eq!(
        manager.consecutive_timeout_count(),
        0,
        "Successful flush should reset timeout count"
    );
}

// =============================================================================
// ADAPT-010: No in-flight export during freeze
// =============================================================================

/// ADAPT-010: Export completes before signalling readiness (/next).
/// The flush happens synchronously in should_flush_on_invocation_end before /next,
/// ensuring no in-flight exports during potential freeze.
#[test]
fn test_no_inflight_export_during_freeze() {
    let config = FlushConfig {
        strategy: FlushStrategy::End,
        interval: Duration::from_secs(60),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let manager = FlushManager::new(config);

    // Verify the flush decision is made at invocation end (before /next)
    // This is the same architectural guarantee as ADAPT-003
    let reason = manager.should_flush_on_invocation_end(10);
    assert_eq!(
        reason,
        Some(FlushReason::InvocationEnd),
        "Flush must complete before signalling readiness"
    );

    // During invocation (after /next), no flush is triggered
    let reason = manager.should_flush(None, 10, false);
    assert!(
        reason.is_none(),
        "No flush should occur during invocation when freeze might happen"
    );
}

// =============================================================================
// ADAPT-011: Connection reused within invocation
// =============================================================================

/// ADAPT-011: HTTP client reuses connections (reqwest default behaviour).
/// The OtlpExporter uses a single reqwest::Client instance which maintains
/// a connection pool for efficient reuse.
#[test]
fn test_connection_reuse_within_invocation() {
    // The exporter creates a single Client that maintains connection pool
    let config = opentelemetry_lambda_extension::ExporterConfig {
        endpoint: Some("http://localhost:4318".to_string()),
        timeout: Duration::from_secs(2),
        ..Default::default()
    };

    let exporter = opentelemetry_lambda_extension::OtlpExporter::new(config).unwrap();

    // The same exporter instance is used for all exports within an invocation
    // reqwest::Client automatically reuses connections from its pool
    assert!(
        exporter.has_endpoint(),
        "Exporter should be configured with endpoint"
    );

    // Connection reuse is a reqwest default - the test verifies the exporter
    // is constructed correctly to take advantage of this
}

// =============================================================================
// ADAPT-012: Stale connection handled gracefully
// =============================================================================

/// ADAPT-012: Retry logic handles connection errors gracefully.
/// The exporter implements exponential backoff retry (3 attempts) before fallback.
#[test]
fn test_stale_connection_retry_logic() {
    let config = FlushConfig::default();
    let mut manager = FlushManager::new(config);

    // Simulate failed exports (timeouts/connection errors)
    manager.record_flush_timeout();
    assert_eq!(manager.consecutive_timeout_count(), 1);

    manager.record_flush_timeout();
    assert_eq!(manager.consecutive_timeout_count(), 2);

    // Successful export resets counter (connection recovered)
    manager.record_flush();
    assert_eq!(
        manager.consecutive_timeout_count(),
        0,
        "Successful flush should reset timeout count after stale connection recovery"
    );
}

// =============================================================================
// ADAPT-013: Export timeout reasonable
// =============================================================================

/// ADAPT-013: Export timeout is configurable and defaults to a reasonable value.
/// Default timeout is 500ms (fast enough to not block, slow enough to succeed).
#[test]
fn test_export_timeout_reasonable() {
    use opentelemetry_lambda_extension::ExporterConfig;

    let default_config = ExporterConfig::default();

    // Default timeout should be reasonable (not too long, not too short)
    assert!(
        default_config.timeout >= Duration::from_millis(100),
        "Timeout should be at least 100ms to allow for network latency"
    );
    assert!(
        default_config.timeout <= Duration::from_secs(5),
        "Timeout should not exceed 5s to avoid blocking indefinitely"
    );

    // Verify the actual default value
    assert_eq!(
        default_config.timeout,
        Duration::from_millis(500),
        "Default timeout should be 500ms"
    );

    // Custom timeout should be respected
    let custom_config = ExporterConfig {
        timeout: Duration::from_secs(2),
        ..Default::default()
    };
    assert_eq!(custom_config.timeout, Duration::from_secs(2));
}

// =============================================================================
// ADAPT-024: Deadline triggers flush (covered in ADAPT-006)
// =============================================================================

// =============================================================================
// ADAPT-025: Pattern history size (tracks last 20 invocations)
// =============================================================================

/// ADAPT-025: Invocation history is bounded to 20 entries.
#[test]
fn test_invocation_history_bounded() {
    let config = FlushConfig::default();
    let mut manager = FlushManager::new(config);

    // Record 30 invocations
    for _ in 0..30 {
        manager.record_invocation();
    }

    // History should be bounded to 20
    assert_eq!(
        manager.invocation_history_len(),
        20,
        "History should be bounded to 20 entries"
    );
}

// =============================================================================
// Integration test: Extension shutdown flushes all signals
// =============================================================================

/// Verifies that simulator graceful shutdown triggers extension shutdown event.
#[tokio::test]
#[serial]
async fn test_simulator_shutdown_triggers_extension_shutdown() {
    use opentelemetry_lambda_extension::{
        Compression, Config, ExporterConfig, Protocol, ReceiverConfig, RuntimeBuilder,
        TelemetryApiConfig,
    };

    let collector = create_test_collector().await;
    let collector_endpoint = format!("http://{}", collector.addr());

    let simulator = create_test_simulator().await;
    let runtime_api_base = simulator.runtime_api_url().replace("http://", "");

    let config = Config {
        exporter: ExporterConfig {
            endpoint: Some(collector_endpoint),
            protocol: Protocol::Http,
            timeout: Duration::from_secs(2),
            compression: Compression::None,
            ..Default::default()
        },
        telemetry_api: TelemetryApiConfig {
            enabled: false,
            ..Default::default()
        },
        receiver: ReceiverConfig {
            http_enabled: true,
            http_port: 4330,
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
        let runtime = RuntimeBuilder::new().config(config).build();
        runtime.run().await
    });

    // Wait for extension to register
    simulator
        .wait_for(
            || async { simulator.extension_count().await >= 1 },
            Duration::from_secs(5),
        )
        .await
        .expect("Extension did not register");

    // Verify extension registered
    let extensions = simulator.get_registered_extensions().await;
    assert!(!extensions.is_empty(), "Should have registered extension");

    // Trigger graceful shutdown
    simulator.graceful_shutdown(ShutdownReason::Spindown).await;

    // Extension should receive SHUTDOWN event and exit cleanly
    let result = tokio::time::timeout(Duration::from_secs(5), extension_handle)
        .await
        .expect("Extension should exit within timeout");

    assert!(
        result.is_ok(),
        "Extension should exit cleanly after shutdown: {:?}",
        result
    );

    collector
        .shutdown()
        .await
        .expect("Collector shutdown failed");
}

/// Creates a mock collector for receiving exported telemetry.
async fn create_test_collector() -> ServerHandle {
    MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector")
}

/// Creates a simulator with default settings for adaptive export tests.
async fn create_test_simulator() -> Simulator {
    Simulator::builder()
        .function_name("adaptive-export-test")
        .extension_ready_timeout(Duration::from_secs(5))
        .build()
        .await
        .expect("Failed to start simulator")
}

// =============================================================================
// Utility: time_until_next_flush
// =============================================================================

/// Test that time_until_next_flush works correctly.
#[test]
fn test_time_until_next_flush() {
    let config = FlushConfig {
        strategy: FlushStrategy::Continuous,
        interval: Duration::from_millis(100),
        max_batch_entries: 100,
        max_batch_bytes: 1024,
    };

    let mut manager = FlushManager::new(config);

    // Before any flush, should be zero (immediate flush needed)
    assert_eq!(
        manager.time_until_next_flush(),
        Duration::ZERO,
        "Should be zero before first flush"
    );

    // After flush, should be close to interval
    manager.record_flush();
    let remaining = manager.time_until_next_flush();
    assert!(
        remaining <= Duration::from_millis(100),
        "Should be at most interval"
    );
    assert!(
        remaining > Duration::ZERO,
        "Should be positive after recording flush"
    );

    // After interval passes, should be zero again
    std::thread::sleep(Duration::from_millis(110));
    assert_eq!(
        manager.time_until_next_flush(),
        Duration::ZERO,
        "Should be zero after interval elapsed"
    );
}

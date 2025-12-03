//! Cold start detection for Lambda functions.
//!
//! Tracks whether the current invocation is a cold start (first invocation
//! in this container) following the pattern from the OpenTelemetry Node.js
//! Lambda instrumentation.

use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag tracking whether we've seen an invocation yet.
/// Starts as `true` and is set to `false` after the first invocation.
static IS_COLD_START: AtomicBool = AtomicBool::new(true);

/// Checks if this is a cold start and clears the flag.
///
/// Returns `true` on the first call (cold start), and `false` on all
/// subsequent calls (warm starts).
///
/// This function also checks the `AWS_LAMBDA_INITIALIZATION_TYPE` environment
/// variable. If set to `"provisioned-concurrency"`, the function returns
/// `false` since provisioned concurrency pre-warms the Lambda container.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads. The atomic swap
/// ensures that exactly one invocation will see `true`.
///
/// # Example
///
/// ```
/// use opentelemetry_lambda_tower::check_cold_start;
///
/// // First call returns true (cold start)
/// // Note: In tests, this may return false if other tests ran first
/// let _ = check_cold_start();
///
/// // Subsequent calls return false (warm start)
/// assert!(!check_cold_start());
/// ```
pub fn check_cold_start() -> bool {
    // Check for provisioned concurrency - never a cold start
    if std::env::var("AWS_LAMBDA_INITIALIZATION_TYPE")
        .map(|v| v == "provisioned-concurrency")
        .unwrap_or(false)
    {
        // Still clear the flag but return false
        IS_COLD_START.store(false, Ordering::SeqCst);
        return false;
    }

    // Atomically swap to false and return the previous value
    // First call: swaps true -> false, returns true
    // Subsequent calls: swaps false -> false, returns false
    IS_COLD_START.swap(false, Ordering::SeqCst)
}

/// Resets the cold start flag for testing purposes.
///
/// # Safety
///
/// This function should only be used in tests. Using it in production
/// code will cause incorrect cold start reporting.
#[cfg(test)]
pub(crate) fn reset_cold_start_for_testing() {
    IS_COLD_START.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_cold_start_first_invocation() {
        reset_cold_start_for_testing();

        // First call should return true
        assert!(check_cold_start());

        // Second call should return false
        assert!(!check_cold_start());

        // Third call should also return false
        assert!(!check_cold_start());
    }

    #[test]
    #[serial]
    fn test_provisioned_concurrency_not_cold_start() {
        reset_cold_start_for_testing();

        // Set provisioned concurrency environment variable
        temp_env::with_var(
            "AWS_LAMBDA_INITIALIZATION_TYPE",
            Some("provisioned-concurrency"),
            || {
                // Should return false even on first call
                assert!(!check_cold_start());
            },
        );
    }

    #[test]
    #[serial]
    fn test_other_initialization_type_is_cold_start() {
        reset_cold_start_for_testing();

        // Set a different initialization type
        temp_env::with_var("AWS_LAMBDA_INITIALIZATION_TYPE", Some("on-demand"), || {
            // Should still be a cold start
            assert!(check_cold_start());
        });
    }
}

//! Adaptive flush management.
//!
//! This module provides intelligent flush timing based on invocation patterns.
//! It tracks invocation frequency and adapts the flush strategy accordingly.

use crate::config::{FlushConfig, FlushStrategy};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const HISTORY_SIZE: usize = 20;
const FREQUENT_THRESHOLD: Duration = Duration::from_secs(120);
const DEADLINE_BUFFER: Duration = Duration::from_millis(500);

/// Reason for triggering a flush.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReason {
    /// Flush triggered by shutdown signal.
    Shutdown,
    /// Flush triggered by approaching deadline.
    Deadline,
    /// Flush triggered by buffer size limits.
    BufferFull,
    /// Flush triggered by periodic timer.
    Periodic,
    /// Flush triggered at end of invocation (infrequent pattern).
    InvocationEnd,
    /// Flush triggered by continuous strategy.
    Continuous,
}

/// Manages adaptive flush timing based on invocation patterns.
pub struct FlushManager {
    config: FlushConfig,
    invocation_timestamps: VecDeque<Instant>,
    last_flush: Option<Instant>,
    consecutive_timeout_count: usize,
}

impl FlushManager {
    /// Creates a new flush manager with the given configuration.
    pub fn new(config: FlushConfig) -> Self {
        Self {
            config,
            invocation_timestamps: VecDeque::with_capacity(HISTORY_SIZE),
            last_flush: None,
            consecutive_timeout_count: 0,
        }
    }

    /// Creates a new flush manager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(FlushConfig::default())
    }

    /// Records that an invocation has started.
    pub fn record_invocation(&mut self) {
        let now = Instant::now();

        if self.invocation_timestamps.len() >= HISTORY_SIZE {
            self.invocation_timestamps.pop_front();
        }
        self.invocation_timestamps.push_back(now);
    }

    /// Records that a flush completed successfully.
    pub fn record_flush(&mut self) {
        self.last_flush = Some(Instant::now());
        self.consecutive_timeout_count = 0;
    }

    /// Records that a flush timed out or failed.
    pub fn record_flush_timeout(&mut self) {
        self.consecutive_timeout_count += 1;
    }

    /// Determines if a flush should be triggered now.
    ///
    /// # Arguments
    ///
    /// * `deadline_ms` - The invocation deadline in milliseconds since epoch
    /// * `pending_count` - Number of signals waiting to be flushed
    /// * `is_shutdown` - Whether a shutdown is in progress
    pub fn should_flush(
        &self,
        deadline_ms: Option<i64>,
        pending_count: usize,
        is_shutdown: bool,
    ) -> Option<FlushReason> {
        if is_shutdown {
            return Some(FlushReason::Shutdown);
        }

        if let Some(deadline) = deadline_ms
            && self.is_deadline_approaching(deadline)
        {
            return Some(FlushReason::Deadline);
        }

        if pending_count > 0 && self.is_buffer_full(pending_count) {
            return Some(FlushReason::BufferFull);
        }

        if pending_count == 0 {
            return None;
        }

        match self.effective_strategy() {
            FlushStrategy::Continuous => {
                if self.should_flush_continuous() {
                    return Some(FlushReason::Continuous);
                }
            }
            FlushStrategy::Periodic => {
                if self.should_flush_periodic() {
                    return Some(FlushReason::Periodic);
                }
            }
            FlushStrategy::End => {
                return None;
            }
            FlushStrategy::Default => {
                if self.is_infrequent_pattern() {
                    return None;
                } else if self.should_flush_continuous() {
                    return Some(FlushReason::Continuous);
                }
            }
        }

        None
    }

    /// Should be called at the end of an invocation to check for end-of-invocation flush.
    pub fn should_flush_on_invocation_end(&self, pending_count: usize) -> Option<FlushReason> {
        if pending_count == 0 {
            return None;
        }

        match self.effective_strategy() {
            FlushStrategy::End => Some(FlushReason::InvocationEnd),
            FlushStrategy::Default if self.is_infrequent_pattern() => {
                Some(FlushReason::InvocationEnd)
            }
            _ => None,
        }
    }

    /// Returns the effective flush strategy, considering escalation.
    fn effective_strategy(&self) -> FlushStrategy {
        if self.consecutive_timeout_count >= HISTORY_SIZE {
            return FlushStrategy::Continuous;
        }

        self.config.strategy
    }

    /// Returns the average time between invocations, if available.
    pub fn average_invocation_interval(&self) -> Option<Duration> {
        if self.invocation_timestamps.len() < 2 {
            return None;
        }

        let first = self.invocation_timestamps.front()?;
        let last = self.invocation_timestamps.back()?;

        let total_duration = last.duration_since(*first);
        let intervals = self.invocation_timestamps.len() - 1;

        Some(total_duration / intervals as u32)
    }

    /// Returns whether invocations are infrequent (>120s average interval).
    pub fn is_infrequent_pattern(&self) -> bool {
        match self.average_invocation_interval() {
            Some(avg) => avg > FREQUENT_THRESHOLD,
            None => true,
        }
    }

    fn is_deadline_approaching(&self, deadline_ms: i64) -> bool {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let remaining = deadline_ms - now_ms;
        remaining < DEADLINE_BUFFER.as_millis() as i64
    }

    fn is_buffer_full(&self, pending_count: usize) -> bool {
        pending_count >= self.config.max_batch_entries
    }

    fn should_flush_continuous(&self) -> bool {
        match self.last_flush {
            Some(last) => last.elapsed() >= self.config.interval,
            None => true,
        }
    }

    fn should_flush_periodic(&self) -> bool {
        self.should_flush_continuous()
    }

    /// Returns the configured flush interval.
    pub fn interval(&self) -> Duration {
        self.config.interval
    }

    /// Returns the time until the next periodic flush should occur.
    pub fn time_until_next_flush(&self) -> Duration {
        match self.last_flush {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed >= self.config.interval {
                    Duration::ZERO
                } else {
                    self.config.interval - elapsed
                }
            }
            None => Duration::ZERO,
        }
    }

    /// Returns the number of consecutive flush timeouts.
    pub fn consecutive_timeout_count(&self) -> usize {
        self.consecutive_timeout_count
    }

    /// Returns the number of invocations in the history.
    pub fn invocation_history_len(&self) -> usize {
        self.invocation_timestamps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> FlushConfig {
        FlushConfig {
            strategy: FlushStrategy::Default,
            interval: Duration::from_millis(100),
            max_batch_bytes: 1024,
            max_batch_entries: 10,
        }
    }

    #[test]
    fn test_shutdown_always_flushes() {
        let manager = FlushManager::new(test_config());

        let reason = manager.should_flush(None, 1, true);
        assert_eq!(reason, Some(FlushReason::Shutdown));

        let reason = manager.should_flush(None, 0, true);
        assert_eq!(reason, Some(FlushReason::Shutdown));
    }

    #[test]
    fn test_buffer_full_triggers_flush() {
        let mut config = test_config();
        config.max_batch_entries = 5;

        let manager = FlushManager::new(config);

        let reason = manager.should_flush(None, 5, false);
        assert_eq!(reason, Some(FlushReason::BufferFull));

        let reason = manager.should_flush(None, 3, false);
        assert_ne!(reason, Some(FlushReason::BufferFull));
    }

    #[test]
    fn test_empty_buffer_no_flush() {
        let manager = FlushManager::new(test_config());

        let reason = manager.should_flush(None, 0, false);
        assert!(reason.is_none());
    }

    #[test]
    fn test_continuous_strategy() {
        let mut config = test_config();
        config.strategy = FlushStrategy::Continuous;
        config.interval = Duration::from_millis(10);

        let mut manager = FlushManager::new(config);

        let reason = manager.should_flush(None, 1, false);
        assert_eq!(reason, Some(FlushReason::Continuous));

        manager.record_flush();

        let reason = manager.should_flush(None, 1, false);
        assert!(reason.is_none());

        std::thread::sleep(Duration::from_millis(15));

        let reason = manager.should_flush(None, 1, false);
        assert_eq!(reason, Some(FlushReason::Continuous));
    }

    #[test]
    fn test_end_strategy() {
        let mut config = test_config();
        config.strategy = FlushStrategy::End;

        let manager = FlushManager::new(config);

        let reason = manager.should_flush(None, 1, false);
        assert!(reason.is_none());

        let reason = manager.should_flush_on_invocation_end(1);
        assert_eq!(reason, Some(FlushReason::InvocationEnd));
    }

    #[test]
    fn test_infrequent_pattern_detection() {
        let mut manager = FlushManager::new(test_config());

        assert!(manager.is_infrequent_pattern());

        manager.record_invocation();
        assert!(manager.is_infrequent_pattern());
    }

    #[test]
    fn test_frequent_pattern_detection() {
        let mut manager = FlushManager::new(test_config());

        for _ in 0..5 {
            manager.record_invocation();
        }

        let avg = manager.average_invocation_interval();
        assert!(avg.is_some());
        assert!(!manager.is_infrequent_pattern());
    }

    #[test]
    fn test_timeout_escalation() {
        let mut config = test_config();
        config.strategy = FlushStrategy::End;

        let mut manager = FlushManager::new(config);

        for _ in 0..HISTORY_SIZE {
            manager.record_flush_timeout();
        }

        let reason = manager.should_flush(None, 1, false);
        assert_eq!(reason, Some(FlushReason::Continuous));
    }

    #[test]
    fn test_record_flush_resets_timeout_count() {
        let mut manager = FlushManager::new(test_config());

        manager.record_flush_timeout();
        manager.record_flush_timeout();
        assert_eq!(manager.consecutive_timeout_count(), 2);

        manager.record_flush();
        assert_eq!(manager.consecutive_timeout_count(), 0);
    }

    #[test]
    fn test_time_until_next_flush() {
        let mut config = test_config();
        config.interval = Duration::from_millis(100);

        let mut manager = FlushManager::new(config);

        assert_eq!(manager.time_until_next_flush(), Duration::ZERO);

        manager.record_flush();

        let remaining = manager.time_until_next_flush();
        assert!(remaining <= Duration::from_millis(100));
        assert!(remaining > Duration::ZERO);
    }

    #[test]
    fn test_invocation_history_limit() {
        let mut manager = FlushManager::new(test_config());

        for _ in 0..30 {
            manager.record_invocation();
        }

        assert_eq!(manager.invocation_history_len(), HISTORY_SIZE);
    }
}

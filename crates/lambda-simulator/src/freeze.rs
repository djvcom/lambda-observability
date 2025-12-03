//! Process freezing simulation for Lambda's freeze/thaw behaviour.
//!
//! This module provides the ability to simulate AWS Lambda's process freezing
//! behaviour, where the entire execution environment (runtime and all extensions)
//! is stopped (SIGSTOP) when waiting for work and resumed (SIGCONT) when an
//! invocation arrives.
//!
//! In real AWS Lambda, the entire sandbox is frozen between invocations. This
//! includes the runtime process and all extension processes. This module supports
//! freezing multiple PIDs to accurately simulate this behaviour.

use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Notify;

#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;

/// Controls whether process freezing is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreezeMode {
    /// No process freezing (default). Fast mode for most tests.
    #[default]
    None,
    /// Send SIGSTOP/SIGCONT to simulate Lambda freeze/thaw.
    /// Only available on Unix platforms.
    Process,
}

/// Errors that can occur during freeze operations.
#[derive(Debug, Clone)]
pub enum FreezeError {
    /// No PIDs configured for process freezing.
    NoPidConfigured,
    /// The target process was not found.
    ProcessNotFound(i32),
    /// Permission denied to send signal.
    PermissionDenied(i32),
    /// Platform does not support process freezing.
    UnsupportedPlatform,
    /// Signal sending failed for another reason.
    SignalFailed(String),
    /// Multiple errors occurred when freezing/unfreezing multiple PIDs.
    Multiple(Vec<FreezeError>),
}

impl std::fmt::Display for FreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezeError::NoPidConfigured => write!(f, "No PIDs configured for process freezing"),
            FreezeError::ProcessNotFound(pid) => write!(f, "Process {} not found", pid),
            FreezeError::PermissionDenied(pid) => {
                write!(f, "Permission denied to send signal to process {}", pid)
            }
            FreezeError::UnsupportedPlatform => {
                write!(f, "Process freezing not supported on this platform")
            }
            FreezeError::SignalFailed(msg) => write!(f, "Signal failed: {}", msg),
            FreezeError::Multiple(errors) => {
                write!(f, "Multiple freeze errors: ")?;
                for (i, err) in errors.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", err)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for FreezeError {}

/// State for managing process freezing.
///
/// Uses an epoch counter to prevent race conditions where a freeze is scheduled
/// but work arrives before it executes. The epoch is incremented on unfreeze,
/// and freeze operations check the epoch matches before proceeding.
///
/// Supports freezing multiple PIDs (runtime + extensions) to accurately simulate
/// Lambda's behaviour where the entire execution environment is frozen. PIDs can
/// be registered dynamically after creation using `register_pid()`.
pub struct FreezeState {
    /// The freeze mode (None or Process).
    mode: FreezeMode,

    /// The PIDs to freeze (runtime and extension processes).
    /// Uses RwLock for interior mutability to allow registering PIDs after creation.
    pids: RwLock<Vec<i32>>,

    /// Whether the processes are currently frozen.
    is_frozen: AtomicBool,

    /// Epoch counter for race condition prevention.
    /// Incremented on each unfreeze to invalidate pending freeze operations.
    freeze_epoch: AtomicU64,

    /// Notifier for freeze state changes.
    state_changed: Notify,
}

impl std::fmt::Debug for FreezeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FreezeState")
            .field("mode", &self.mode)
            .field("pids", &*self.pids.read())
            .field("is_frozen", &self.is_frozen.load(Ordering::SeqCst))
            .field("freeze_epoch", &self.freeze_epoch.load(Ordering::SeqCst))
            .finish()
    }
}

impl FreezeState {
    /// Creates a new freeze state with a single PID (for backwards compatibility).
    pub fn new(mode: FreezeMode, pid: Option<u32>) -> Self {
        Self {
            mode,
            pids: RwLock::new(pid.map(|p| vec![p as i32]).unwrap_or_default()),
            is_frozen: AtomicBool::new(false),
            freeze_epoch: AtomicU64::new(0),
            state_changed: Notify::new(),
        }
    }

    /// Creates a new freeze state with multiple PIDs.
    ///
    /// Use this to freeze the runtime and all extension processes together,
    /// which accurately simulates Lambda's freeze behaviour.
    pub fn with_pids(mode: FreezeMode, pids: Vec<u32>) -> Self {
        Self {
            mode,
            pids: RwLock::new(pids.into_iter().map(|p| p as i32).collect()),
            is_frozen: AtomicBool::new(false),
            freeze_epoch: AtomicU64::new(0),
            state_changed: Notify::new(),
        }
    }

    /// Creates a new freeze state wrapped in an `Arc`.
    ///
    /// This is a convenience method for cases where shared ownership is needed.
    pub fn new_shared(mode: FreezeMode, pid: Option<u32>) -> Arc<Self> {
        Arc::new(Self::new(mode, pid))
    }

    /// Creates a new freeze state with multiple PIDs wrapped in an `Arc`.
    pub fn with_pids_shared(mode: FreezeMode, pids: Vec<u32>) -> Arc<Self> {
        Arc::new(Self::with_pids(mode, pids))
    }

    /// Returns the current freeze mode.
    pub fn mode(&self) -> FreezeMode {
        self.mode
    }

    /// Returns the number of registered PIDs.
    pub fn pid_count(&self) -> usize {
        self.pids.read().len()
    }

    /// Returns the first configured PID, if any (for backwards compatibility).
    pub fn pid(&self) -> Option<i32> {
        self.pids.read().first().copied()
    }

    /// Registers a PID to be frozen/unfrozen with the execution environment.
    ///
    /// Call this after spawning runtime or extension processes to include them
    /// in freeze/thaw operations.
    pub fn register_pid(&self, pid: u32) {
        self.pids.write().push(pid as i32);
    }

    /// Returns whether the processes are currently frozen.
    pub fn is_frozen(&self) -> bool {
        self.is_frozen.load(Ordering::SeqCst)
    }

    /// Returns the current freeze epoch.
    pub fn current_epoch(&self) -> u64 {
        self.freeze_epoch.load(Ordering::SeqCst)
    }

    /// Waits for the freeze state to change.
    pub async fn wait_for_state_change(&self) {
        self.state_changed.notified().await;
    }

    /// Attempts to freeze all configured processes at the given epoch.
    ///
    /// The freeze will only proceed if the current epoch matches the provided epoch.
    /// This prevents race conditions where work arrives between scheduling and execution.
    ///
    /// All configured PIDs (runtime and extensions) are frozen together to simulate
    /// Lambda's behaviour where the entire execution environment is frozen.
    ///
    /// Returns Ok(true) if frozen, Ok(false) if epoch mismatch (work arrived), Err on failure.
    #[cfg(unix)]
    pub fn freeze_at_epoch(&self, epoch: u64) -> Result<bool, FreezeError> {
        if self.mode != FreezeMode::Process {
            return Ok(false);
        }

        let current_epoch = self.freeze_epoch.load(Ordering::SeqCst);
        if current_epoch != epoch {
            return Ok(false);
        }

        let pids = self.pids.read();
        if pids.is_empty() {
            return Err(FreezeError::NoPidConfigured);
        }

        if self
            .is_frozen
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(true);
        }

        let mut errors = Vec::new();
        let mut any_success = false;

        for &pid in pids.iter() {
            match kill(Pid::from_raw(pid), Signal::SIGSTOP) {
                Ok(()) => {
                    any_success = true;
                }
                Err(nix::errno::Errno::ESRCH) => {
                    errors.push(FreezeError::ProcessNotFound(pid));
                }
                Err(nix::errno::Errno::EPERM) => {
                    errors.push(FreezeError::PermissionDenied(pid));
                }
                Err(e) => {
                    errors.push(FreezeError::SignalFailed(format!("PID {}: {}", pid, e)));
                }
            }
        }

        if !errors.is_empty() && !any_success {
            self.is_frozen.store(false, Ordering::SeqCst);
            if errors.len() == 1 {
                return Err(errors.remove(0));
            }
            return Err(FreezeError::Multiple(errors));
        }

        self.state_changed.notify_waiters();
        Ok(true)
    }

    #[cfg(not(unix))]
    pub fn freeze_at_epoch(&self, _epoch: u64) -> Result<bool, FreezeError> {
        if self.mode == FreezeMode::Process {
            Err(FreezeError::UnsupportedPlatform)
        } else {
            Ok(false)
        }
    }

    /// Unfreezes all configured processes and increments the epoch.
    ///
    /// Incrementing the epoch cancels any pending freeze operations that were
    /// scheduled before this unfreeze.
    #[cfg(unix)]
    pub fn unfreeze(&self) -> Result<(), FreezeError> {
        self.freeze_epoch.fetch_add(1, Ordering::SeqCst);

        if !self.is_frozen.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        let pids = self.pids.read();
        if pids.is_empty() {
            return Ok(());
        }

        let mut errors = Vec::new();

        for &pid in pids.iter() {
            match kill(Pid::from_raw(pid), Signal::SIGCONT) {
                Ok(()) => {}
                Err(nix::errno::Errno::ESRCH) => {
                    errors.push(FreezeError::ProcessNotFound(pid));
                }
                Err(nix::errno::Errno::EPERM) => {
                    errors.push(FreezeError::PermissionDenied(pid));
                }
                Err(e) => {
                    errors.push(FreezeError::SignalFailed(format!("PID {}: {}", pid, e)));
                }
            }
        }

        self.state_changed.notify_waiters();

        if errors.is_empty() {
            Ok(())
        } else if errors.len() == 1 {
            Err(errors.remove(0))
        } else {
            Err(FreezeError::Multiple(errors))
        }
    }

    #[cfg(not(unix))]
    pub fn unfreeze(&self) -> Result<(), FreezeError> {
        self.freeze_epoch.fetch_add(1, Ordering::SeqCst);
        self.is_frozen.store(false, Ordering::SeqCst);
        self.state_changed.notify_waiters();
        Ok(())
    }

    /// Unconditionally unfreezes all configured processes without checking state.
    ///
    /// Used during shutdown to ensure processes aren't left frozen.
    #[cfg(unix)]
    pub fn force_unfreeze(&self) {
        self.freeze_epoch.fetch_add(1, Ordering::SeqCst);
        self.is_frozen.store(false, Ordering::SeqCst);

        let pids = self.pids.read();
        for &pid in pids.iter() {
            let _ = kill(Pid::from_raw(pid), Signal::SIGCONT);
        }
        self.state_changed.notify_waiters();
    }

    #[cfg(not(unix))]
    pub fn force_unfreeze(&self) {
        self.freeze_epoch.fetch_add(1, Ordering::SeqCst);
        self.is_frozen.store(false, Ordering::SeqCst);
        self.state_changed.notify_waiters();
    }
}

impl Default for FreezeState {
    fn default() -> Self {
        Self {
            mode: FreezeMode::None,
            pids: RwLock::new(Vec::new()),
            is_frozen: AtomicBool::new(false),
            freeze_epoch: AtomicU64::new(0),
            state_changed: Notify::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freeze_mode_default_is_none() {
        assert_eq!(FreezeMode::default(), FreezeMode::None);
    }

    #[test]
    fn test_freeze_state_default() {
        let state = FreezeState::default();
        assert_eq!(state.mode(), FreezeMode::None);
        assert_eq!(state.pid(), None);
        assert_eq!(state.pid_count(), 0);
        assert!(!state.is_frozen());
        assert_eq!(state.current_epoch(), 0);
    }

    #[test]
    fn test_epoch_increments_on_unfreeze() {
        let state = FreezeState::new(FreezeMode::None, None);
        assert_eq!(state.current_epoch(), 0);

        state.unfreeze().unwrap();
        assert_eq!(state.current_epoch(), 1);

        state.unfreeze().unwrap();
        assert_eq!(state.current_epoch(), 2);
    }

    #[test]
    fn test_freeze_without_process_mode_returns_false() {
        let state = FreezeState::new(FreezeMode::None, Some(12345));
        let result = state.freeze_at_epoch(0).unwrap();
        assert!(!result);
        assert!(!state.is_frozen());
    }

    #[cfg(unix)]
    #[test]
    fn test_freeze_without_pid_returns_error() {
        let state = FreezeState::new(FreezeMode::Process, None);
        let result = state.freeze_at_epoch(0);
        assert!(matches!(result, Err(FreezeError::NoPidConfigured)));
    }

    #[test]
    fn test_epoch_mismatch_prevents_freeze() {
        let state = FreezeState::new(FreezeMode::Process, Some(12345));

        state.unfreeze().unwrap();
        assert_eq!(state.current_epoch(), 1);

        let result = state.freeze_at_epoch(0);
        assert!(matches!(result, Ok(false)));
        assert!(!state.is_frozen());
    }

    #[test]
    fn test_with_pids_creates_multi_pid_state() {
        let state = FreezeState::with_pids(FreezeMode::Process, vec![111, 222, 333]);
        assert_eq!(state.pid_count(), 3);
        assert_eq!(state.pid(), Some(111));
    }

    #[test]
    fn test_register_pid() {
        let state = FreezeState::new(FreezeMode::Process, Some(111));
        assert_eq!(state.pid_count(), 1);

        state.register_pid(222);
        assert_eq!(state.pid_count(), 2);

        state.register_pid(333);
        assert_eq!(state.pid_count(), 3);
    }
}

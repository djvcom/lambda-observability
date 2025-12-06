//! Process spawning and lifecycle management for Lambda simulation.
//!
//! This module provides utilities for spawning and managing runtime and extension
//! processes within the Lambda simulator. It handles:
//!
//! - Automatic injection of Lambda environment variables
//! - PID registration for freeze/thaw simulation
//! - RAII-based cleanup on drop
//! - Stdio inheritance for demos and debugging
//!
//! # Example
//!
//! ```no_run
//! use lambda_simulator::{Simulator, FreezeMode};
//! use lambda_simulator::process::ProcessRole;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let simulator = Simulator::builder()
//!     .freeze_mode(FreezeMode::Process)
//!     .build()
//!     .await?;
//!
//! // Spawn a runtime - PID automatically registered for freeze/thaw
//! // In tests, use env!("CARGO_BIN_EXE_<name>") for binary path resolution
//! let runtime = simulator.spawn_process(
//!     "/path/to/my_runtime",
//!     ProcessRole::Runtime,
//! )?;
//!
//! // Spawn an extension
//! let extension = simulator.spawn_process(
//!     "/path/to/my_extension",
//!     ProcessRole::Extension,
//! )?;
//!
//! // Processes are automatically cleaned up when dropped
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

/// The role of a spawned process in the Lambda environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRole {
    /// The Lambda runtime process that handles invocations.
    Runtime,
    /// A Lambda extension process that receives lifecycle events.
    Extension,
}

impl std::fmt::Display for ProcessRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessRole::Runtime => write!(f, "runtime"),
            ProcessRole::Extension => write!(f, "extension"),
        }
    }
}

/// Configuration for spawning a managed process.
#[derive(Debug, Clone)]
pub struct ProcessConfig {
    binary_path: PathBuf,
    additional_env: HashMap<String, String>,
    args: Vec<String>,
    inherit_stdio: bool,
    role: ProcessRole,
}

impl ProcessConfig {
    /// Creates a new process configuration.
    ///
    /// # Arguments
    ///
    /// * `binary_path` - Path to the executable binary
    /// * `role` - Whether this is a runtime or extension process
    pub fn new(binary_path: impl Into<PathBuf>, role: ProcessRole) -> Self {
        Self {
            binary_path: binary_path.into(),
            additional_env: HashMap::new(),
            args: Vec::new(),
            inherit_stdio: true,
            role,
        }
    }

    /// Adds an environment variable to the process.
    ///
    /// These are merged with the Lambda environment variables provided by the
    /// simulator, with these taking precedence.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.additional_env.insert(key.into(), value.into());
        self
    }

    /// Adds a command-line argument to the process.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Sets whether to inherit stdio from the parent process.
    ///
    /// When true (the default), stdout and stderr from the spawned process
    /// will be visible. This is useful for demos and debugging.
    #[must_use]
    pub fn inherit_stdio(mut self, inherit: bool) -> Self {
        self.inherit_stdio = inherit;
        self
    }

    /// Returns the role of this process.
    pub fn role(&self) -> ProcessRole {
        self.role
    }

    /// Returns the binary path.
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }
}

/// A managed child process with automatic cleanup on drop.
///
/// When a `ManagedProcess` is dropped, it will:
/// 1. Send SIGTERM (Unix) or terminate (Windows) to the process
/// 2. Wait for the process to exit
///
/// This ensures no orphaned processes are left behind.
pub struct ManagedProcess {
    child: Child,
    pid: u32,
    role: ProcessRole,
    binary_name: String,
}

impl ManagedProcess {
    /// Returns the PID of the managed process.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the role of this process.
    pub fn role(&self) -> ProcessRole {
        self.role
    }

    /// Returns the binary name (for logging).
    pub fn binary_name(&self) -> &str {
        &self.binary_name
    }

    /// Returns a mutable reference to the underlying child process.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Waits for the process to exit and returns the exit status.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Attempts to kill the process.
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    /// Checks if the process has exited.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl std::fmt::Debug for ManagedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedProcess")
            .field("pid", &self.pid)
            .field("role", &self.role)
            .field("binary_name", &self.binary_name)
            .finish()
    }
}

/// Error type for process spawning operations.
#[derive(Debug)]
pub enum ProcessError {
    /// The specified binary was not found.
    BinaryNotFound(PathBuf),
    /// Failed to spawn the process.
    SpawnFailed(io::Error),
    /// The process terminated unexpectedly.
    Terminated {
        pid: u32,
        status: Option<ExitStatus>,
    },
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::BinaryNotFound(path) => {
                write!(f, "Binary not found: {}", path.display())
            }
            ProcessError::SpawnFailed(e) => {
                write!(f, "Failed to spawn process: {}", e)
            }
            ProcessError::Terminated { pid, status } => {
                write!(f, "Process {} terminated unexpectedly", pid)?;
                if let Some(s) = status {
                    write!(f, " with status {:?}", s)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProcessError::SpawnFailed(e) => Some(e),
            _ => None,
        }
    }
}

/// Spawns a process with the given configuration and Lambda environment variables.
///
/// This is an internal function used by `Simulator::spawn_process`.
pub(crate) fn spawn_process(
    config: ProcessConfig,
    lambda_env: HashMap<String, String>,
) -> Result<ManagedProcess, ProcessError> {
    if !config.binary_path.exists() {
        return Err(ProcessError::BinaryNotFound(config.binary_path));
    }

    let binary_name = config
        .binary_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut cmd = Command::new(&config.binary_path);

    for (key, value) in lambda_env {
        cmd.env(key, value);
    }

    for (key, value) in config.additional_env {
        cmd.env(key, value);
    }

    for arg in &config.args {
        cmd.arg(arg);
    }

    if config.inherit_stdio {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    cmd.stdin(Stdio::null());

    let child = cmd.spawn().map_err(ProcessError::SpawnFailed)?;
    let pid = child.id();

    tracing::debug!(
        "Spawned {} process: {} (PID: {})",
        config.role,
        binary_name,
        pid
    );

    Ok(ManagedProcess {
        child,
        pid,
        role: config.role,
        binary_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_config_builder() {
        let config = ProcessConfig::new("/usr/bin/echo", ProcessRole::Runtime)
            .env("FOO", "bar")
            .arg("--help")
            .inherit_stdio(false);

        assert_eq!(config.role(), ProcessRole::Runtime);
        assert_eq!(config.binary_path(), Path::new("/usr/bin/echo"));
        assert!(!config.inherit_stdio);
        assert_eq!(config.additional_env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(config.args, vec!["--help"]);
    }

    #[test]
    fn test_process_role_display() {
        assert_eq!(ProcessRole::Runtime.to_string(), "runtime");
        assert_eq!(ProcessRole::Extension.to_string(), "extension");
    }
}

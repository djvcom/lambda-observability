//! Error types for the Lambda runtime simulator.

use thiserror::Error;

/// Errors that can occur during simulation operations.
#[derive(Error, Debug)]
pub enum SimulatorError {
    /// Error starting the HTTP server.
    #[error("Failed to start server: {0}")]
    ServerStart(String),

    /// Error binding to the specified address.
    #[error("Failed to bind to address: {0}")]
    BindError(String),

    /// Invalid configuration provided.
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Timeout occurred during operation.
    #[error("Timeout occurred: {0}")]
    Timeout(String),

    /// Invocation not found.
    #[error("Invocation not found: {0}")]
    InvocationNotFound(String),
}

/// Errors that can occur during runtime operations.
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Request ID not found.
    #[error("Request ID not found: {0}")]
    RequestIdNotFound(String),

    /// Invalid request ID format.
    #[error("Invalid request ID format: {0}")]
    InvalidRequestId(String),

    /// No invocation available.
    #[error("No invocation available")]
    NoInvocation,

    /// Runtime not initialized.
    #[error("Runtime not initialized")]
    NotInitialized,

    /// Runtime already initialized.
    #[error("Runtime already initialized")]
    AlreadyInitialized,

    /// Invalid payload provided.
    #[error("Invalid payload: {0}")]
    InvalidPayload(String),
}

/// Errors that can occur when building invocations.
#[derive(Error, Debug)]
pub enum BuilderError {
    /// Required field is missing.
    #[error("Missing required field: {0}")]
    MissingField(String),

    /// Invalid freeze configuration.
    #[error("Invalid freeze configuration: {0}")]
    InvalidFreezeConfig(String),
}

/// Result type for simulator operations.
pub type SimulatorResult<T> = Result<T, SimulatorError>;

/// Result type for runtime operations.
pub type RuntimeResult<T> = Result<T, RuntimeError>;

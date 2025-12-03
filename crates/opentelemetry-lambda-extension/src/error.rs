//! Error types for the Lambda OTel extension.

use crate::exporter::ExportError;
use crate::runtime::RuntimeError;
use thiserror::Error;

/// A specialised Result type for extension operations.
pub type Result<T> = std::result::Result<T, ExtensionError>;

/// Errors that can occur in the extension.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ExtensionError {
    /// Configuration error.
    #[error("configuration error")]
    Config(#[source] Box<figment::Error>),

    /// Export error.
    #[error(transparent)]
    Export(#[from] ExportError),

    /// Runtime error.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    /// Tracing subscriber initialisation failed.
    #[error("failed to initialise tracing")]
    Tracing(#[from] tracing_subscriber::util::TryInitError),
}

impl From<figment::Error> for ExtensionError {
    fn from(err: figment::Error) -> Self {
        ExtensionError::Config(Box::new(err))
    }
}

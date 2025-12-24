//! Opinionated OpenTelemetry SDK configuration and lifecycle management.
//!
//! This crate wires together the OpenTelemetry SDK, OTLP exporters, and the
//! `tracing` crate ecosystem into a cohesive configuration system. It handles
//! initialisation, flushing, and shutdown of all signal providers (traces,
//! metrics, logs).
//!
//! # Features
//!
//! - **Layered configuration** - Combine defaults, config files, environment
//!   variables, and programmatic overrides using [figment](https://docs.rs/figment)
//! - **Sensible defaults** - Protocol-specific endpoints (localhost:4318 for HTTP,
//!   localhost:4317 for gRPC)
//! - **Export fallback** (planned) - API for preserving telemetry data when exports
//!   fail. Note: Currently the fallback types are defined but not yet wired into the
//!   export pipeline. This will be implemented in a future release.
//! - **Drop-based lifecycle** - Automatic flush and shutdown when guard goes out
//!   of scope
//! - **Tracing integration** - Automatic setup of `tracing-opentelemetry` and
//!   `opentelemetry-appender-tracing` layers
//!
//! # Example
//!
//! ```no_run
//! use opentelemetry_configuration::{OtelSdkBuilder, Protocol, ExportFallback, SdkError};
//!
//! fn main() -> Result<(), SdkError> {
//!     // Simple case - uses defaults
//!     let _guard = OtelSdkBuilder::new()
//!         .service_name("my-service")
//!         .build()?;
//!
//!     // Full configuration
//!     let _guard = OtelSdkBuilder::new()
//!         .with_file("/etc/otel-config.toml")        // Layer config file
//!         .with_standard_env()                       // Standard OTEL_* env vars
//!         .endpoint("http://collector:4318")         // Override endpoint
//!         .protocol(Protocol::HttpBinary)
//!         .service_name("my-service")
//!         .fallback(ExportFallback::Stdout)          // Write failures to stdout
//!         .build()?;
//!
//!     tracing::info!("Application running");
//!
//!     // On drop, all providers are flushed and shut down
//!     Ok(())
//! }
//! ```
//!
//! # Custom Fallback Handlers (Planned)
//!
//! The fallback API is designed to preserve telemetry data when exports fail.
//! While the types and builder methods are available, the fallback is not yet
//! wired into the export pipeline. This section documents the intended usage
//! for when the feature is fully implemented:
//!
//! ```no_run
//! use opentelemetry_configuration::{OtelSdkBuilder, SdkError};
//!
//! let _guard = OtelSdkBuilder::new()
//!     .service_name("my-service")
//!     .with_fallback(|failure| {
//!         // Access the original OTLP protobuf payload
//!         let bytes = failure.request.to_protobuf();
//!         eprintln!(
//!             "Failed to export {} ({} items, {} bytes): {}",
//!             failure.request.signal_type(),
//!             failure.request.item_count(),
//!             bytes.len(),
//!             failure.error
//!         );
//!         // Write to S3, queue, backup collector, etc.
//!         Ok(())
//!     })
//!     .build()?;
//! # Ok::<(), SdkError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod builder;
mod config;
mod error;
mod fallback;
mod guard;
mod rust_detector;

pub use builder::{OtelSdkBuilder, ResourceConfigBuilder};
pub use config::{
    BatchConfig, ComputeEnvironment, EndpointConfig, OtelSdkConfig, Protocol, ResourceConfig,
    SignalConfig,
};
pub use error::SdkError;
pub use fallback::{ExportFailure, ExportFallback, FailedRequest, FallbackHandler};
pub use guard::OtelGuard;
pub use rust_detector::{RustBuildInfo, RustResourceDetector, emit_rustc_env};

// Re-export figment for power users who want to construct their own configuration
pub use figment;

/// Captures Rust build-time information as resource attributes.
///
/// This macro reads environment variables set by [`emit_rustc_env`] in build.rs.
/// Use it with [`OtelSdkBuilder::with_rust_build_info`] to add rustc version
/// and channel information to your telemetry resource attributes.
///
/// # Example
///
/// ```ignore
/// // In build.rs:
/// fn main() {
///     opentelemetry_configuration::emit_rustc_env();
/// }
///
/// // In main.rs:
/// use opentelemetry_configuration::{OtelSdkBuilder, capture_rust_build_info};
///
/// let _guard = OtelSdkBuilder::new()
///     .service_name("my-service")
///     .with_rust_build_info(capture_rust_build_info!())
///     .build()?;
/// ```
///
/// # Attributes Added
///
/// When combined with [`emit_rustc_env`] in build.rs, this adds:
/// - `process.runtime.version` - rustc version (e.g., "1.84.0")
/// - `process.runtime.description` - full version string
/// - `rust.channel` - release channel ("stable", "beta", or "nightly")
#[macro_export]
macro_rules! capture_rust_build_info {
    () => {
        $crate::RustBuildInfo {
            rustc_version: option_env!("RUSTC_VERSION"),
            rust_channel: option_env!("RUST_CHANNEL"),
            rustc_version_full: option_env!("RUSTC_VERSION_FULL"),
        }
    };
}

//! Event-specific trace context extractors.
//!
//! This module provides extractors for common Lambda event types.
//! Each extractor is feature-gated:
//!
//! - `http` - API Gateway HTTP API (v2) and REST API (v1)
//! - `sqs` - SQS message events
//! - `sns` - SNS notification events
//!
//! Enable features via Cargo.toml:
//!
//! ```toml
//! [dependencies]
//! opentelemetry-lambda-tower = { version = "0.1", features = ["http", "sqs"] }
//! ```

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "sqs")]
pub mod sqs;

#[cfg(feature = "sns")]
pub mod sns;

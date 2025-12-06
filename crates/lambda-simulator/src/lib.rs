//! # Lambda Runtime Simulator
//!
//! A high-fidelity AWS Lambda Runtime API simulator for testing Lambda runtimes
//! and extensions in a local development environment.
//!
//! ## Overview
//!
//! This crate provides a simulator that implements the AWS Lambda Runtime API,
//! allowing you to test Lambda runtimes and extensions without deploying to AWS.
//! It's particularly useful for testing Lambda extensions like OpenTelemetry
//! collectors, custom extensions, and runtime implementations.
//!
//! ## Features
//!
//! - **Runtime API**: Full implementation of the Lambda Runtime Interface
//! - **High Fidelity**: Accurately simulates Lambda behavior including timeouts
//! - **Test-Friendly**: Easy to use in test harnesses with state inspection
//! - **Async**: Built on tokio for efficient async execution
//!
//! ## Quick Start
//!
//! ```no_run
//! use lambda_simulator::{Simulator, InvocationBuilder};
//! use serde_json::json;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a simulator
//!     let simulator = Simulator::builder()
//!         .function_name("my-test-function")
//!         .build()
//!         .await?;
//!
//!     // Get the Runtime API URL
//!     let runtime_api = simulator.runtime_api_url();
//!     println!("Runtime API available at: {}", runtime_api);
//!
//!     // Enqueue an invocation
//!     simulator.enqueue_payload(json!({
//!         "message": "Hello, Lambda!"
//!     })).await;
//!
//!     // Your runtime would connect to runtime_api and process the invocation
//!     // ...
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Testing Lambda Runtimes
//!
//! The simulator provides efficient, event-driven wait helpers that eliminate
//! flaky sleep-based synchronisation:
//!
//! ```no_run
//! use lambda_simulator::{Simulator, InvocationStatus};
//! use serde_json::json;
//! use std::time::Duration;
//!
//! #[tokio::test]
//! async fn test_my_runtime() {
//!     let simulator = Simulator::builder()
//!         .function_name("test-function")
//!         .build()
//!         .await
//!         .unwrap();
//!
//!     // Set the runtime API URL for your runtime
//!     std::env::set_var("AWS_LAMBDA_RUNTIME_API", simulator.runtime_api_url());
//!
//!     // Enqueue returns the request ID for tracking
//!     let request_id = simulator.enqueue_payload(json!({"key": "value"})).await;
//!
//!     // Start your runtime in a background task
//!     // tokio::spawn(async { my_runtime::run().await });
//!
//!     // Wait for completion with timeout - no polling loops needed!
//!     let state = simulator
//!         .wait_for_invocation_complete(&request_id, Duration::from_secs(5))
//!         .await
//!         .expect("Invocation should complete");
//!
//!     assert_eq!(state.status, InvocationStatus::Success);
//! }
//! ```
//!
//! ## Testing with Telemetry Capture
//!
//! Capture telemetry events in memory without setting up HTTP servers:
//!
//! ```no_run
//! use lambda_simulator::Simulator;
//! use serde_json::json;
//!
//! #[tokio::test]
//! async fn test_telemetry_events() {
//!     let simulator = Simulator::builder().build().await.unwrap();
//!
//!     // Enable in-memory capture
//!     simulator.enable_telemetry_capture().await;
//!
//!     // Enqueue and process invocations...
//!     simulator.enqueue_payload(json!({"test": "data"})).await;
//!
//!     // Assert on captured events
//!     let start_events = simulator.get_telemetry_events_by_type("platform.start").await;
//!     assert_eq!(start_events.len(), 1);
//! }
//! ```
//!
//! ## Predicate-Based Waiting
//!
//! Wait for arbitrary conditions with `wait_for`:
//!
//! ```no_run
//! use lambda_simulator::Simulator;
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let simulator = Simulator::builder().build().await?;
//!
//! // Wait for extensions to register
//! simulator.wait_for(
//!     || async { simulator.extension_count().await >= 2 },
//!     Duration::from_secs(5)
//! ).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Architecture
//!
//! The simulator uses an HTTP server built with `axum` and `tokio` to implement
//! the Lambda Runtime API endpoints. Invocations are queued and delivered to
//! runtimes via long-polling on the `/runtime/invocation/next` endpoint.
//!
//! ## AWS Lambda Runtime API
//!
//! The simulator implements these endpoints:
//!
//! - `GET /2018-06-01/runtime/invocation/next` - Get next invocation (long-poll)
//! - `POST /2018-06-01/runtime/invocation/{requestId}/response` - Submit response
//! - `POST /2018-06-01/runtime/invocation/{requestId}/error` - Report error
//! - `POST /2018-06-01/runtime/init/error` - Report initialization error
//!
//! For more details on the Lambda Runtime API, see:
//! <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-api.html>

pub mod error;
pub mod extension;
pub(crate) mod extension_readiness;
pub mod extensions_api;
pub mod freeze;
pub mod invocation;
pub mod process;
pub(crate) mod response_body;
pub mod runtime_api;
pub mod simulator;
pub mod state;
pub mod telemetry;
pub(crate) mod telemetry_api;
pub(crate) mod telemetry_state;

pub use error::{BuilderError, RuntimeError, SimulatorError, SimulatorResult};
pub use extension::{EventType, ExtensionId, LifecycleEvent, RegisteredExtension, ShutdownReason};
pub use freeze::{FreezeError, FreezeMode, FreezeState};
pub use invocation::{
    Invocation, InvocationBuilder, InvocationError, InvocationResponse, InvocationStatus,
};
pub use simulator::{Simulator, SimulatorBuilder, SimulatorConfig, SimulatorPhase};
pub use state::InvocationState;
pub use telemetry::{TelemetryEvent, TelemetryEventType, TelemetrySubscription};

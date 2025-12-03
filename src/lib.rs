//! Lambda OTel Workspace - Integration tests for Lambda runtime simulation with OpenTelemetry.
//!
//! This is a virtual package that provides workspace-level integration tests.
//! The actual functionality is provided by the workspace member crates:
//!
//! - `lambda-runtime-simulator`: Simulates AWS Lambda Runtime, Extensions, and Telemetry APIs
//! - `lambda-otel-extension`: OpenTelemetry extension for Lambda
//! - `lambda-otel-function`: Example instrumented Lambda function

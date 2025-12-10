//! HTTP Lambda example with OpenTelemetry tracing.
//!
//! This example mirrors the AWS Lambda Rust Runtime's `http-basic-lambda` example
//! but adds automatic OpenTelemetry instrumentation via `OtelTracingLayer`.
//!
//! The key difference from using `lambda_http::run` directly is that we use
//! `lambda_runtime::run` with our tracing layer, which gives us:
//! - Automatic trace context extraction from incoming requests
//! - Properly attributed spans following OpenTelemetry semantic conventions
//! - Automatic span flushing before Lambda freezes
//!
//! # Running
//!
//! ```bash
//! cargo build --example http_handler --release
//! ```

use lambda_http::{Body, Error, Request, RequestExt, Response, http::StatusCode};
use opentelemetry_lambda_tower::{LambdaHttpExtractor, OtelTracingLayer};
use tower::ServiceBuilder;

async fn function_handler(event: Request) -> Result<Response<Body>, Error> {
    let who = event
        .query_string_parameters_ref()
        .and_then(|params| params.first("name"))
        .unwrap_or("world");

    tracing::info!(name = %who, "Processing greeting request");

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html")
        .body(format!("Hello, {who}!").into())
        .map_err(Box::new)?;

    Ok(resp)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    let tracing_layer = OtelTracingLayer::new(LambdaHttpExtractor::new());

    let service = ServiceBuilder::new()
        .layer(tracing_layer)
        .layer_fn(lambda_http::Adapter::from)
        .service_fn(function_handler);

    lambda_runtime::run(service).await
}

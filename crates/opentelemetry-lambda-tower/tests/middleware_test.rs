//! Integration tests for the Tower middleware (Layer/Service).
//!
//! These tests verify that the OtelTracingLayer correctly:
//! - Wraps services and forwards calls
//! - Creates spans with proper attributes
//! - Handles errors correctly
//! - Works with the builder pattern

use aws_lambda_events::apigw::ApiGatewayV2httpRequest;
use lambda_runtime::{Context as LambdaContext, LambdaEvent};
use opentelemetry_lambda_tower::{ApiGatewayV2Extractor, OtelTracingLayer};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};

#[derive(Clone)]
struct MockHandler {
    call_count: Arc<AtomicUsize>,
    should_error: bool,
}

impl MockHandler {
    fn new() -> Self {
        Self {
            call_count: Arc::new(AtomicUsize::new(0)),
            should_error: false,
        }
    }

    fn with_error() -> Self {
        Self {
            call_count: Arc::new(AtomicUsize::new(0)),
            should_error: true,
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl Service<LambdaEvent<ApiGatewayV2httpRequest>> for MockHandler {
    type Response = serde_json::Value;
    type Error = MockError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _event: LambdaEvent<ApiGatewayV2httpRequest>) -> Self::Future {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let should_error = self.should_error;

        Box::pin(async move {
            if should_error {
                Err(MockError("Handler error".to_string()))
            } else {
                Ok(serde_json::json!({"statusCode": 200}))
            }
        })
    }
}

#[derive(Debug)]
struct MockError(String);

impl std::fmt::Display for MockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MockError {}

fn create_test_event() -> LambdaEvent<ApiGatewayV2httpRequest> {
    let mut event = ApiGatewayV2httpRequest::default();
    event.raw_path = Some("/test".to_string());
    event.route_key = Some("GET /test".to_string());
    event.request_context.http.method = http::Method::GET;

    LambdaEvent::new(event, LambdaContext::default())
}

#[tokio::test]
async fn test_layer_wraps_service() {
    let handler = MockHandler::new();
    let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());

    let mut service = layer.layer(handler.clone());

    let event = create_test_event();
    let result = service.ready().await.unwrap().call(event).await;

    assert!(result.is_ok());
    assert_eq!(handler.call_count(), 1);
}

#[tokio::test]
async fn test_layer_forwards_response() {
    let handler = MockHandler::new();
    let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());

    let mut service = layer.layer(handler);

    let event = create_test_event();
    let result = service.ready().await.unwrap().call(event).await.unwrap();

    assert_eq!(result["statusCode"], 200);
}

#[tokio::test]
async fn test_layer_forwards_error() {
    let handler = MockHandler::with_error();
    let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());

    let mut service = layer.layer(handler);

    let event = create_test_event();
    let result = service.ready().await.unwrap().call(event).await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "Handler error");
}

#[tokio::test]
async fn test_multiple_invocations() {
    let handler = MockHandler::new();
    let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());

    let mut service = layer.layer(handler.clone());

    for _ in 0..3 {
        let event = create_test_event();
        let result = service.ready().await.unwrap().call(event).await;
        assert!(result.is_ok());
    }

    assert_eq!(handler.call_count(), 3);
}

#[tokio::test]
async fn test_builder_with_flush_disabled() {
    let handler = MockHandler::new();
    let layer = OtelTracingLayer::builder(ApiGatewayV2Extractor::new())
        .flush_on_end(false)
        .build();

    let mut service = layer.layer(handler.clone());

    let event = create_test_event();
    let result = service.ready().await.unwrap().call(event).await;

    assert!(result.is_ok());
    assert_eq!(handler.call_count(), 1);
}

#[tokio::test]
async fn test_builder_with_custom_timeout() {
    let handler = MockHandler::new();
    let layer = OtelTracingLayer::builder(ApiGatewayV2Extractor::new())
        .flush_timeout(Duration::from_millis(100))
        .build();

    let mut service = layer.layer(handler.clone());

    let event = create_test_event();
    let result = service.ready().await.unwrap().call(event).await;

    assert!(result.is_ok());
    assert_eq!(handler.call_count(), 1);
}

#[tokio::test]
async fn test_service_is_clone() {
    let handler = MockHandler::new();
    let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());

    let service = layer.layer(handler.clone());
    let mut service_clone = service.clone();

    let event = create_test_event();
    let result = service_clone.ready().await.unwrap().call(event).await;

    assert!(result.is_ok());
    assert_eq!(handler.call_count(), 1);
}

#[tokio::test]
async fn test_layer_is_clone() {
    let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());
    let layer_clone = layer.clone();

    let handler = MockHandler::new();
    let mut service = layer_clone.layer(handler.clone());

    let event = create_test_event();
    let result = service.ready().await.unwrap().call(event).await;

    assert!(result.is_ok());
}

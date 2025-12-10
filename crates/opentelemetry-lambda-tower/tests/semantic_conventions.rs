//! Integration tests validating OpenTelemetry semantic conventions.
//!
//! These tests invoke instrumented Lambda handlers with real JSON payloads
//! and assert that the produced spans conform to semantic conventions.

use aws_lambda_events::apigw::ApiGatewayV2httpRequest;
use aws_lambda_events::sqs::SqsEvent;
use lambda_runtime::{Context as LambdaContext, LambdaEvent};
use mock_collector::MockServer;
use opentelemetry_configuration::{OtelSdkBuilder, Protocol};
use opentelemetry_lambda_tower::{ApiGatewayV2Extractor, OtelTracingLayer, SqsEventExtractor};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use serde_json::json;
use std::time::Duration;
use tower::{Service, ServiceBuilder, ServiceExt};

const API_GATEWAY_V2_EVENT: &str = r#"{
  "version": "2.0",
  "routeKey": "GET /users/{id}",
  "rawPath": "/users/123",
  "rawQueryString": "foo=bar",
  "headers": {
    "content-type": "application/json",
    "user-agent": "Mozilla/5.0",
    "host": "api.example.com",
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
  },
  "requestContext": {
    "accountId": "123456789012",
    "apiId": "api-id",
    "domainName": "api.example.com",
    "domainPrefix": "api",
    "http": {
      "method": "GET",
      "path": "/users/123",
      "protocol": "HTTP/1.1",
      "sourceIp": "192.168.1.1",
      "userAgent": "Mozilla/5.0"
    },
    "requestId": "request-id",
    "routeKey": "GET /users/{id}",
    "stage": "$default",
    "time": "01/Jan/2024:00:00:00 +0000",
    "timeEpoch": 1704067200000
  },
  "isBase64Encoded": false
}"#;

const SQS_EVENT: &str = r#"{
  "Records": [
    {
      "messageId": "msg-001",
      "receiptHandle": "receipt-001",
      "body": "{\"action\": \"process\"}",
      "attributes": {
        "ApproximateReceiveCount": "1",
        "SentTimestamp": "1704067200000",
        "SenderId": "sender-id",
        "ApproximateFirstReceiveTimestamp": "1704067200001",
        "AWSTraceHeader": "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
      },
      "messageAttributes": {},
      "md5OfBody": "md5",
      "eventSource": "aws:sqs",
      "eventSourceARN": "arn:aws:sqs:us-east-1:123456789012:my-queue",
      "awsRegion": "us-east-1"
    }
  ]
}"#;

#[tokio::test]
async fn test_semantic_conventions() {
    let server = MockServer::builder().start().await.unwrap();

    let _guard = OtelSdkBuilder::new()
        .endpoint(format!("http://{}", server.addr()))
        .protocol(Protocol::Grpc)
        .service_name("test-lambda")
        .metrics(false)
        .logs(false)
        .build()
        .unwrap();

    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    // Test 1: HTTP handler span name
    {
        let mut service = ServiceBuilder::new()
            .layer(OtelTracingLayer::new(ApiGatewayV2Extractor::new()))
            .service_fn(|_event: LambdaEvent<ApiGatewayV2httpRequest>| async {
                Ok::<_, std::convert::Infallible>(json!({"statusCode": 200}))
            });

        let payload: ApiGatewayV2httpRequest =
            serde_json::from_str(API_GATEWAY_V2_EVENT).expect("valid JSON");
        let event = LambdaEvent::new(payload, LambdaContext::default());

        let _ = service.ready().await.unwrap().call(event).await.unwrap();
    }

    // Test 2: SQS handler span name
    {
        let mut service = ServiceBuilder::new()
            .layer(OtelTracingLayer::new(SqsEventExtractor::new()))
            .service_fn(|_event: LambdaEvent<SqsEvent>| async {
                Ok::<_, std::convert::Infallible>(())
            });

        let payload: SqsEvent = serde_json::from_str(SQS_EVENT).expect("valid JSON");
        let event = LambdaEvent::new(payload, LambdaContext::default());

        let _ = service.ready().await.unwrap().call(event).await.unwrap();
    }

    // Wait for spans to be exported
    server
        .wait_for_spans(2, Duration::from_secs(10))
        .await
        .unwrap();

    // Assert HTTP semantic conventions
    server
        .with_collector(|collector| {
            collector
                .expect_span_with_name("GET /users/{id}")
                .with_attribute("http.request.method", "GET")
                .with_attribute("url.path", "/users/123")
                .with_attribute("http.route", "/users/{id}")
                .with_attribute("url.scheme", "https")
                .with_attribute("faas.trigger", "http")
                .assert_exists();
        })
        .await;

    // Assert SQS semantic conventions
    server
        .with_collector(|collector| {
            collector
                .expect_span_with_name("my-queue process")
                .with_attribute("messaging.system", "aws_sqs")
                .with_attribute("messaging.operation.type", "process")
                .with_attribute("messaging.destination.name", "my-queue")
                .with_attribute("faas.trigger", "pubsub")
                .assert_exists();
        })
        .await;

    server.shutdown().await.unwrap();
}

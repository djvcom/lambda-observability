//! Integration tests using real-world Lambda sample events.
//!
//! These tests verify that the extractors correctly handle events matching
//! the format AWS Lambda sends, based on sample events from
//! https://github.com/tschoffelen/lambda-sample-events

use aws_lambda_events::apigw::ApiGatewayV2httpRequest;
use aws_lambda_events::sqs::SqsEvent;
use lambda_runtime::Context as LambdaContext;
use opentelemetry_lambda_tower::{ApiGatewayV2Extractor, SqsEventExtractor, TraceContextExtractor};

/// API Gateway HTTP API v2 sample event (from lambda-sample-events repo)
const API_GATEWAY_HTTP_API_EVENT: &str = r#"{
  "version": "2.0",
  "routeKey": "$default",
  "rawPath": "/path/to/resource",
  "rawQueryString": "parameter1=value1&parameter1=value2&parameter2=value",
  "cookies": [
    "cookie1",
    "cookie2"
  ],
  "headers": {
    "Header1": "value1",
    "Header2": "value1,value2"
  },
  "queryStringParameters": {
    "parameter1": "value1,value2",
    "parameter2": "value"
  },
  "requestContext": {
    "accountId": "123456789012",
    "apiId": "api-id",
    "domainName": "id.execute-api.us-east-1.amazonaws.com",
    "domainPrefix": "id",
    "http": {
      "method": "POST",
      "path": "/path/to/resource",
      "protocol": "HTTP/1.1",
      "sourceIp": "192.168.0.1",
      "userAgent": "agent"
    },
    "requestId": "id",
    "routeKey": "$default",
    "stage": "$default",
    "time": "12/Mar/2020:19:03:58 +0000",
    "timeEpoch": 1583348638390
  },
  "body": "eyJ0ZXN0IjoiYm9keSJ9",
  "pathParameters": {
    "parameter1": "value1"
  },
  "isBase64Encoded": true,
  "stageVariables": {
    "stageVariable1": "value1",
    "stageVariable2": "value2"
  }
}"#;

/// API Gateway HTTP API v2 event with traceparent header
const API_GATEWAY_HTTP_API_WITH_TRACE: &str = r#"{
  "version": "2.0",
  "routeKey": "GET /users/{id}",
  "rawPath": "/users/123",
  "rawQueryString": "",
  "headers": {
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    "content-type": "application/json",
    "user-agent": "Mozilla/5.0"
  },
  "requestContext": {
    "accountId": "123456789012",
    "apiId": "api-id",
    "domainName": "api.example.com",
    "http": {
      "method": "GET",
      "path": "/users/123",
      "protocol": "HTTP/1.1",
      "sourceIp": "203.0.113.42",
      "userAgent": "Mozilla/5.0"
    },
    "requestId": "req-123",
    "routeKey": "GET /users/{id}",
    "stage": "prod"
  },
  "body": null,
  "isBase64Encoded": false
}"#;

/// SQS sample event (from lambda-sample-events repo)
const SQS_EVENT: &str = r#"{
  "Records": [
    {
      "messageId": "19dd0b57-b21e-4ac1-bd88-01bbb068cb78",
      "receiptHandle": "MessageReceiptHandle",
      "body": "Hello from SQS!",
      "attributes": {
        "ApproximateReceiveCount": "1",
        "SentTimestamp": "1523232000000",
        "SenderId": "123456789012",
        "ApproximateFirstReceiveTimestamp": "1523232000001"
      },
      "messageAttributes": {},
      "md5OfBody": "098f6bcd4621d373cade4e832627b4f6",
      "eventSource": "aws:sqs",
      "eventSourceARN": "arn:aws:sqs:us-east-1:123456789012:MyQueue",
      "awsRegion": "us-east-1"
    }
  ]
}"#;

/// SQS event with X-Ray trace header
const SQS_EVENT_WITH_TRACE: &str = r#"{
  "Records": [
    {
      "messageId": "msg-001",
      "receiptHandle": "handle-001",
      "body": "{\"orderId\": 12345}",
      "attributes": {
        "ApproximateReceiveCount": "1",
        "SentTimestamp": "1700000000000",
        "SenderId": "123456789012",
        "ApproximateFirstReceiveTimestamp": "1700000000001",
        "AWSTraceHeader": "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
      },
      "messageAttributes": {},
      "eventSource": "aws:sqs",
      "eventSourceARN": "arn:aws:sqs:eu-west-1:123456789012:order-processing-queue",
      "awsRegion": "eu-west-1"
    },
    {
      "messageId": "msg-002",
      "receiptHandle": "handle-002",
      "body": "{\"orderId\": 12346}",
      "attributes": {
        "ApproximateReceiveCount": "1",
        "SentTimestamp": "1700000000100",
        "SenderId": "123456789012",
        "ApproximateFirstReceiveTimestamp": "1700000000101",
        "AWSTraceHeader": "Root=1-67890abc-def0123456789abcdef01234;Parent=1234567890abcdef;Sampled=0"
      },
      "messageAttributes": {},
      "eventSource": "aws:sqs",
      "eventSourceARN": "arn:aws:sqs:eu-west-1:123456789012:order-processing-queue",
      "awsRegion": "eu-west-1"
    }
  ]
}"#;

fn create_lambda_context() -> LambdaContext {
    LambdaContext::default()
}

mod api_gateway_v2_tests {
    use super::*;
    use opentelemetry::trace::TraceContextExt;

    #[test]
    fn test_deserialize_sample_event() {
        let event: ApiGatewayV2httpRequest =
            serde_json::from_str(API_GATEWAY_HTTP_API_EVENT).expect("Failed to deserialize");

        assert_eq!(event.raw_path, Some("/path/to/resource".to_string()));
        assert_eq!(event.request_context.http.method.as_str(), "POST");
    }

    #[test]
    fn test_span_name_from_sample_event() {
        let event: ApiGatewayV2httpRequest =
            serde_json::from_str(API_GATEWAY_HTTP_API_EVENT).expect("Failed to deserialize");
        let ctx = create_lambda_context();
        let extractor = ApiGatewayV2Extractor::new();

        let span_name = extractor.span_name(&event, &ctx);

        // The route_key is "$default" so it should use the raw path
        assert_eq!(span_name, "POST /path/to/resource");
    }

    #[test]
    fn test_trigger_type() {
        let extractor = ApiGatewayV2Extractor::new();
        assert_eq!(extractor.trigger_type(), "http");
    }

    #[test]
    fn test_extract_context_no_traceparent() {
        let event: ApiGatewayV2httpRequest =
            serde_json::from_str(API_GATEWAY_HTTP_API_EVENT).expect("Failed to deserialize");
        let extractor = ApiGatewayV2Extractor::new();

        let ctx = extractor.extract_context(&event);

        // Without traceparent, span context should not be valid
        assert!(!ctx.span().span_context().is_valid());
    }

    #[test]
    fn test_extract_context_with_traceparent() {
        let event: ApiGatewayV2httpRequest =
            serde_json::from_str(API_GATEWAY_HTTP_API_WITH_TRACE).expect("Failed to deserialize");
        let extractor = ApiGatewayV2Extractor::new();

        let ctx = extractor.extract_context(&event);

        // With traceparent, span context should be valid
        assert!(ctx.span().span_context().is_valid());
        assert_eq!(
            ctx.span().span_context().trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(
            ctx.span().span_context().span_id().to_string(),
            "00f067aa0ba902b7"
        );
        assert!(ctx.span().span_context().is_sampled());
    }

    #[test]
    fn test_span_name_with_route_pattern() {
        let event: ApiGatewayV2httpRequest =
            serde_json::from_str(API_GATEWAY_HTTP_API_WITH_TRACE).expect("Failed to deserialize");
        let ctx = create_lambda_context();
        let extractor = ApiGatewayV2Extractor::new();

        let span_name = extractor.span_name(&event, &ctx);

        // Should use the route pattern from route_key
        assert_eq!(span_name, "GET /users/{id}");
    }
}

mod sqs_tests {
    use super::*;

    #[test]
    fn test_deserialize_sample_event() {
        let event: SqsEvent = serde_json::from_str(SQS_EVENT).expect("Failed to deserialize");

        assert_eq!(event.records.len(), 1);
        assert_eq!(
            event.records[0].message_id,
            Some("19dd0b57-b21e-4ac1-bd88-01bbb068cb78".to_string())
        );
        assert_eq!(event.records[0].body, Some("Hello from SQS!".to_string()));
    }

    #[test]
    fn test_span_name_from_sample_event() {
        let event: SqsEvent = serde_json::from_str(SQS_EVENT).expect("Failed to deserialize");
        let ctx = create_lambda_context();
        let extractor = SqsEventExtractor::new();

        let span_name = extractor.span_name(&event, &ctx);

        // Should use queue name from ARN
        assert_eq!(span_name, "MyQueue process");
    }

    #[test]
    fn test_trigger_type() {
        let extractor = SqsEventExtractor::new();
        assert_eq!(extractor.trigger_type(), "pubsub");
    }

    #[test]
    fn test_extract_links_no_trace_header() {
        let event: SqsEvent = serde_json::from_str(SQS_EVENT).expect("Failed to deserialize");
        let extractor = SqsEventExtractor::new();

        let links = extractor.extract_links(&event);

        // No AWSTraceHeader means no links
        assert!(links.is_empty());
    }

    #[test]
    fn test_extract_links_with_trace_headers() {
        let event: SqsEvent =
            serde_json::from_str(SQS_EVENT_WITH_TRACE).expect("Failed to deserialize");
        let extractor = SqsEventExtractor::new();

        let links = extractor.extract_links(&event);

        // Should have 2 links (one per message)
        assert_eq!(links.len(), 2);

        // First link
        let link1 = &links[0];
        assert!(link1.span_context.is_valid());
        assert_eq!(
            link1.span_context.trace_id().to_string(),
            "5759e988bd862e3fe1be46a994272793"
        );
        assert_eq!(link1.span_context.span_id().to_string(), "53995c3f42cd8ad8");
        assert!(link1.span_context.is_sampled());

        // Second link
        let link2 = &links[1];
        assert!(link2.span_context.is_valid());
        assert_eq!(
            link2.span_context.trace_id().to_string(),
            "67890abcdef0123456789abcdef01234"
        );
        assert_eq!(link2.span_context.span_id().to_string(), "1234567890abcdef");
        assert!(!link2.span_context.is_sampled());
    }

    #[test]
    fn test_span_name_with_fifo_queue() {
        let event: SqsEvent =
            serde_json::from_str(SQS_EVENT_WITH_TRACE).expect("Failed to deserialize");
        let ctx = create_lambda_context();
        let extractor = SqsEventExtractor::new();

        let span_name = extractor.span_name(&event, &ctx);

        // Uses queue name from the ARN
        assert_eq!(span_name, "order-processing-queue process");
    }
}

mod batch_tests {
    use super::*;

    #[test]
    fn test_sqs_batch_message_count() {
        let event: SqsEvent =
            serde_json::from_str(SQS_EVENT_WITH_TRACE).expect("Failed to deserialize");

        // Verify we can get message count for batch processing
        assert_eq!(event.records.len(), 2);
    }
}

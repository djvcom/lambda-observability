# opentelemetry-lambda-tower

Tower middleware for automatic OpenTelemetry instrumentation of AWS Lambda handlers.

This crate provides a Tower layer that automatically instruments Lambda handler invocations with OpenTelemetry tracing, following semantic conventions for FaaS and messaging systems.

## Features

- **Automatic span creation** - Creates spans for each Lambda invocation with proper naming
- **Trace context propagation** - Extracts W3C TraceContext from HTTP headers and X-Ray environment variables
- **Span links for messaging** - Creates span links for SQS/SNS batch messages
- **Cold start detection** - Automatically detects and records cold starts
- **Configurable flushing** - Ensures spans are exported before Lambda freezes

## Quick Start

```rust
use lambda_runtime::{service_fn, LambdaEvent};
use opentelemetry_lambda_tower::{OtelTracingLayer, ApiGatewayV2Extractor};
use tower::ServiceBuilder;

async fn handler(event: LambdaEvent<serde_json::Value>) -> Result<String, lambda_runtime::Error> {
    Ok("Hello!".to_string())
}

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    let service = ServiceBuilder::new()
        .layer(OtelTracingLayer::new(ApiGatewayV2Extractor::new()))
        .service(service_fn(handler));

    lambda_runtime::run(service).await
}
```

## Event Type Extractors

The crate provides extractors for common Lambda event types:

| Extractor | Event Type | Feature Flag |
|-----------|------------|--------------|
| `ApiGatewayV2Extractor` | API Gateway HTTP API (v2) | `http` (default) |
| `ApiGatewayV1Extractor` | API Gateway REST API (v1) | `http` (default) |
| `SqsEventExtractor` | SQS Messages | `sqs` (default) |
| `SnsEventExtractor` | SNS Notifications | `sns` |

### HTTP Events (API Gateway)

```rust
use opentelemetry_lambda_tower::{OtelTracingLayer, ApiGatewayV2Extractor};

let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());
```

Extracts trace context from:
- `traceparent` HTTP header (W3C TraceContext)
- `_X_AMZN_TRACE_ID` environment variable (X-Ray format, converted to W3C)

### SQS Events

```rust
use opentelemetry_lambda_tower::{OtelTracingLayer, SqsEventExtractor};

let layer = OtelTracingLayer::new(SqsEventExtractor::new());
```

For SQS batch processing, span links are created for each message's trace context rather than parent-child relationships, following OpenTelemetry messaging semantic conventions.

### SNS Events

```rust
use opentelemetry_lambda_tower::{OtelTracingLayer, SnsEventExtractor};

let layer = OtelTracingLayer::new(SnsEventExtractor::new());
```

## Configuration

### Builder Pattern

```rust
use opentelemetry_lambda_tower::{OtelTracingLayerBuilder, ApiGatewayV2Extractor};
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::sync::Arc;
use std::time::Duration;

let provider = Arc::new(SdkTracerProvider::builder().build());

let layer = OtelTracingLayerBuilder::new(ApiGatewayV2Extractor::new())
    .tracer_provider(provider)
    .flush_on_end(true)
    .flush_timeout(Duration::from_secs(5))
    .build();
```

### Options

| Method | Default | Description |
|--------|---------|-------------|
| `tracer_provider()` | None | Set the tracer provider for flushing |
| `flush_on_end()` | `true` | Flush spans after each invocation |
| `flush_timeout()` | 5s | Timeout for flush operations |

## Custom Extractors

Implement the `TraceContextExtractor` trait for custom event types:

```rust
use opentelemetry_lambda_tower::TraceContextExtractor;
use opentelemetry::Context;
use opentelemetry::trace::Link;
use lambda_runtime::Context as LambdaContext;
use tracing::Span;

struct MyEventExtractor;

impl TraceContextExtractor<MyEvent> for MyEventExtractor {
    fn extract_context(&self, event: &MyEvent) -> Context {
        // Extract parent trace context
        Context::current()
    }

    fn extract_links(&self, event: &MyEvent) -> Vec<Link> {
        // Return span links for batch processing
        vec![]
    }

    fn trigger_type(&self) -> &'static str {
        "other"
    }

    fn span_name(&self, event: &MyEvent, ctx: &LambdaContext) -> String {
        format!("{} invoke", ctx.env_config.function_name)
    }

    fn record_attributes(&self, event: &MyEvent, span: &Span) {
        // Record event-specific span attributes
    }
}
```

## Semantic Conventions

The middleware records attributes following OpenTelemetry semantic conventions:

### FaaS Attributes
- `faas.trigger` - Trigger type (http, pubsub, other)
- `faas.invocation_id` - Lambda request ID
- `faas.coldstart` - Whether this is a cold start
- `faas.name` - Function name
- `faas.version` - Function version

### HTTP Attributes (API Gateway)
- `http.request.method` - HTTP method
- `url.path` - Request path
- `http.route` - Route pattern
- `client.address` - Client IP

### Messaging Attributes (SQS/SNS)
- `messaging.system` - `aws_sqs` or `aws_sns`
- `messaging.operation.type` - `process`
- `messaging.destination.name` - Queue/topic name
- `messaging.batch.message_count` - Batch size

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `http` | Yes | API Gateway event extractors |
| `sqs` | Yes | SQS event extractor |
| `sns` | No | SNS event extractor |
| `full` | No | All extractors |

## Licence

MIT

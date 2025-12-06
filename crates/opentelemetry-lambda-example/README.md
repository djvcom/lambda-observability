# opentelemetry-lambda-example

Example Lambda functions demonstrating OpenTelemetry instrumentation with the workspace crates.

## Overview

This crate provides reference implementations showing the recommended approach for instrumenting AWS Lambda functions with OpenTelemetry. It demonstrates integration with:

- [`opentelemetry-configuration`](../opentelemetry-configuration) - SDK setup with layered configuration
- [`opentelemetry-lambda-tower`](../opentelemetry-lambda-tower) - Tower middleware for automatic instrumentation

## Supported Event Types

- **HTTP** - API Gateway HTTP API (v2) requests
- **SQS** - Simple Queue Service batch messages

## Quick Start

### HTTP Handler

```rust
use opentelemetry_lambda_example::{init_telemetry, create_http_service};
use lambda_runtime::run;

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    let guard = init_telemetry()?;
    let service = create_http_service(&guard);
    run(service).await
}
```

### SQS Handler

```rust
use opentelemetry_lambda_example::{init_telemetry, create_sqs_service};
use lambda_runtime::run;

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    let guard = init_telemetry()?;
    let service = create_sqs_service(&guard);
    run(service).await
}
```

## Configuration

The SDK is configured using a layered approach:

1. **Sensible defaults** - localhost:4318 for HTTP OTLP
2. **Config file** - `/var/task/otel-config.toml` (optional)
3. **Environment variables** - Standard `OTEL_*` variables

### Environment Variables

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http
OTEL_SERVICE_NAME=my-lambda-function
```

### Config File

```toml
[exporter]
endpoint = "http://collector:4318"
protocol = "http"

[resource]
service_name = "my-lambda-function"
```

## Architecture

The example demonstrates:

1. **SDK initialisation** - Using `OtelSdkBuilder` for configuration
2. **Guard lifecycle** - Automatic flush and shutdown on drop
3. **Tower middleware** - `OtelTracingLayer` for automatic span creation
4. **Context propagation** - Extracting trace context from headers/attributes
5. **SQS batch processing** - Using span links for batch message traces

## Request Payload

All handlers accept a JSON payload with these optional fields:

| Field | Type | Description |
|-------|------|-------------|
| `message` | string | Input message to process |
| `simulate_error` | boolean | Returns an error response |
| `delay_ms` | number | Simulates processing delay |

## Response Formats

### HTTP Response

```json
{
  "statusCode": 200,
  "body": "{\"message\":\"Processed: hello\",\"requestId\":\"abc-123\"}"
}
```

### SQS Batch Response

```json
{
  "batchItemFailures": [
    {"itemIdentifier": "msg-456"}
  ]
}
```

## Licence

MIT

# opentelemetry-lambda-extension

AWS Lambda extension for collecting and exporting OpenTelemetry traces, metrics, and logs from Lambda functions.

## Overview

This extension receives telemetry data (traces, metrics, logs) from instrumented Lambda functions via a local OTLP receiver and exports them to your observability backend. It integrates with Lambda's Extensions API for proper lifecycle management and handles the unique constraints of Lambda's execution model.

## Features

- **Multi-signal support** - Traces, metrics, and logs via OTLP (HTTP and gRPC)
- **Automatic batching** - Intelligent signal aggregation with size and time limits
- **Adaptive flushing** - Flush before Lambda freezes to prevent data loss
- **Platform telemetry** - Captures Lambda platform metrics (duration, memory, cold starts)
- **Span correlation** - Links function spans with platform telemetry
- **Resource detection** - Automatically detects Lambda resource attributes
- **Configurable exports** - HTTP or gRPC, with compression and timeout options

## Installation

### Prerequisites

Install [cargo-lambda](https://www.cargo-lambda.info/):

```bash
# Using pip
pip3 install cargo-lambda

# Or using Homebrew (macOS)
brew tap cargo-lambda/cargo-lambda
brew install cargo-lambda
```

### As a Lambda Layer

Build and deploy the extension using cargo-lambda:

```bash
# Build optimised for Lambda (handles cross-compilation automatically)
cargo lambda build --release --extension

# The binary is ready at:
# target/lambda/extensions/opentelemetry-lambda-extension
```

Create and deploy the layer:

```bash
# Create layer structure
mkdir -p layer/extensions
cp target/lambda/extensions/opentelemetry-lambda-extension layer/extensions/

# Package the layer
cd layer && zip -r ../extension-layer.zip .

# Deploy to AWS
aws lambda publish-layer-version \
    --layer-name opentelemetry-lambda-extension \
    --zip-file fileb://extension-layer.zip \
    --compatible-runtimes provided.al2023 nodejs24.x python3.14 \
    --compatible-architectures x86_64
```

For ARM64 (Graviton2):

```bash
cargo lambda build --release --extension --arm64

# Then package and deploy with:
# --compatible-architectures arm64
```

### Configuration

Configure the extension via environment variables or a TOML config file.

#### Environment Variables

```bash
# OTLP endpoint
OTEL_EXPORTER_OTLP_ENDPOINT=https://your-collector.example.com

# Protocol (http or grpc)
OTEL_EXPORTER_OTLP_PROTOCOL=http

# Compression
OTEL_EXPORTER_OTLP_COMPRESSION=gzip

# Headers (for authentication)
OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer token"

# Extension-specific settings
OTEL_LAMBDA_FLUSH_TIMEOUT=5s
OTEL_LAMBDA_RECEIVER_PORT=9999
```

#### TOML Configuration

Place a `config.toml` in the Lambda function's deployment package:

```toml
[exporter]
endpoint = "https://your-collector.example.com"
protocol = "http"
compression = "gzip"
timeout = "30s"

[exporter.headers]
Authorization = "Bearer your-token"

[receiver]
port = 9999
host = "127.0.0.1"

[flush]
strategy = "invocation"
timeout = "5s"
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Lambda Execution Environment                 │
│                                                                      │
│  ┌─────────────┐          OTLP/HTTP or gRPC         ┌────────────┐  │
│  │   Lambda    │ ─────────────────────────────────▶ │  Extension │  │
│  │  Function   │    traces, metrics, logs           │  Receiver  │  │
│  │ (instrumented)│                                    │  :9999     │  │
│  └─────────────┘                                    └─────┬──────┘  │
│                                                           │         │
│                                                           ▼         │
│                                                    ┌────────────┐   │
│                                                    │ Aggregator │   │
│                                                    │  & Batcher │   │
│                                                    └─────┬──────┘   │
│                                                           │         │
│  ┌─────────────┐                                         │         │
│  │ Platform    │                                         ▼         │
│  │ Telemetry   │ ──────────────────────────────▶ ┌────────────┐   │
│  │ (Lambda API)│    platform metrics              │  Exporter  │   │
│  └─────────────┘                                  │  (OTLP)    │   │
│                                                    └─────┬──────┘   │
└──────────────────────────────────────────────────────────┼──────────┘
                                                           │
                                                           ▼
                                               ┌────────────────────┐
                                               │  Your Collector    │
                                               │  (Jaeger, Grafana, │
                                               │   Datadog, etc.)   │
                                               └────────────────────┘
```

## Lambda Lifecycle Integration

The extension integrates with Lambda's execution lifecycle:

1. **Init** - Extension registers, starts OTLP receiver, subscribes to platform telemetry
2. **Invoke** - Receives signals from function, aggregates, correlates with platform data
3. **Shutdown** - Flushes all pending signals before termination

### Freeze/Thaw Handling

Lambda may freeze the execution environment between invocations. The extension:

- Flushes signals before freeze (after each invocation)
- Detects thaw events and reconnects if needed
- Uses adaptive timeouts based on remaining execution time

## Instrumentation

Configure your Lambda function to send telemetry to the extension:

```bash
# Point OTLP exporters at the extension
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:9999
OTEL_EXPORTER_OTLP_PROTOCOL=http
```

### Example with the OpenTelemetry SDK

```rust
use opentelemetry_lambda_tower::{OtelTracingLayer, ApiGatewayV2Extractor};
use tower::ServiceBuilder;

let service = ServiceBuilder::new()
    .layer(OtelTracingLayer::new(ApiGatewayV2Extractor::new()))
    .service(service_fn(handler));
```

## Platform Metrics

The extension automatically captures Lambda platform metrics from the Telemetry API:

| Metric | Description |
|--------|-------------|
| `faas.duration` | Function execution duration |
| `faas.billed_duration` | Billed duration (rounded up) |
| `faas.max_memory` | Maximum memory used |
| `faas.init_duration` | Cold start initialisation time |
| `faas.coldstart` | Boolean indicating cold start |

## Resource Attributes

The extension detects and adds Lambda resource attributes:

| Attribute | Source |
|-----------|--------|
| `faas.name` | `AWS_LAMBDA_FUNCTION_NAME` |
| `faas.version` | `AWS_LAMBDA_FUNCTION_VERSION` |
| `faas.instance` | `AWS_LAMBDA_LOG_STREAM_NAME` |
| `faas.max_memory` | `AWS_LAMBDA_FUNCTION_MEMORY_SIZE` |
| `cloud.provider` | `aws` |
| `cloud.region` | `AWS_REGION` |
| `cloud.account.id` | Extracted from function ARN |

## Troubleshooting

### Extension not receiving data

1. Verify the function is sending to `http://localhost:9999`
2. Check extension logs in CloudWatch: `/aws/lambda/<function>/extension`
3. Ensure the layer is attached to the function

### Data not appearing in backend

1. Check `OTEL_EXPORTER_OTLP_ENDPOINT` is correct
2. Verify authentication headers are set
3. Review extension logs for export errors
4. Check network connectivity (VPC configuration)

### High latency

1. Consider using gRPC instead of HTTP
2. Enable compression: `OTEL_EXPORTER_OTLP_COMPRESSION=gzip`
3. Tune batch settings to reduce export frequency

## Licence

MIT

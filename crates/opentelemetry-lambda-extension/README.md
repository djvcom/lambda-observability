# opentelemetry-lambda-extension

AWS Lambda extension for collecting and exporting OpenTelemetry traces, metrics, and logs from Lambda functions.

## Overview

This extension receives telemetry data (traces, metrics, logs) from instrumented Lambda functions via a local OTLP receiver and exports them to your observability backend. It integrates with Lambda's Extensions API for proper lifecycle management and handles the unique constraints of Lambda's execution model.

## Features

- **Multi-signal support** - Traces, metrics, and logs via OTLP/HTTP
- **Freeze-safe flushing** - Exports finish in the post-invocation window, before the execution environment freezes, so no HTTP request is left dangling across a freeze
- **Invocation completion signalling** - Handler wrappers (see [`wrappers/`](../../wrappers)) tell the extension the moment the handler finishes, with `platform.runtimeDone` as backup and a bounded deadline fallback
- **Bounded memory** - All buffered telemetry lives under one shared byte/entry budget derived from the function's memory size; the oldest signals are dropped rather than growing without limit
- **Deadline-aware shutdown** - The final flush is budgeted against the SHUTDOWN deadline, abandoning deliberately rather than being SIGKILLed mid-export
- **Platform telemetry** - Captures Lambda platform metrics (duration, memory, cold starts)
- **Resource detection** - Automatically detects Lambda resource attributes

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

### Binary Size Optimisation

This extension is designed to be lightweight compared to the ~30 MB OpenTelemetry Collector Lambda distribution.

The workspace `Cargo.toml` includes optimised release profiles:

```toml
[profile.release]
lto = true           # Link-time optimisation for cross-crate inlining
codegen-units = 1    # Better optimisation at cost of compile time
strip = true         # Remove debug symbols
panic = "abort"      # Remove unwinding code

[profile.release-small]
inherits = "release"
opt-level = "z"      # Optimise for size over speed
```

| Profile | Size | Use Case |
|---------|------|----------|
| `--release` | ~9.4 MB | Recommended default |
| `--profile release-small` | ~7.0 MB | When size is critical |

For even smaller binaries, you can apply UPX compression to the Linux binary:

```bash
# After building with cargo-lambda
upx --best target/lambda/extensions/opentelemetry-lambda-extension
```

This typically achieves 50-70% additional compression.

#### Analysing Binary Size

To identify what's contributing to binary size:

```bash
# Install cargo-bloat
cargo install cargo-bloat

# Show size by crate
cargo bloat --release -p opentelemetry-lambda-extension --crates

# Show largest functions
cargo bloat --release -p opentelemetry-lambda-extension -n 20
```

### Configuration

Configuration is layered, in order of priority: compiled-in defaults, then
the optional TOML file `/var/task/otel-extension.toml`, then standard
`OTEL_*` environment variables, then extension-specific `LAMBDA_OTEL_*`
environment variables.

#### Standard OpenTelemetry Environment Variables

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://your-collector.example.com
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_COMPRESSION=gzip
OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer token"
```

#### Extension-Specific Environment Variables

Every config field maps to `LAMBDA_OTEL_<SECTION>_<FIELD>`. Durations are
in milliseconds.

| Variable | Default | Description |
|----------|---------|-------------|
| `LAMBDA_OTEL_EXPORTER_ENDPOINT` | unset | OTLP endpoint; without it, exports fall back to stdout |
| `LAMBDA_OTEL_EXPORTER_PROTOCOL` | `http` | Only `http` is supported |
| `LAMBDA_OTEL_EXPORTER_TIMEOUT` | `500` | Per-request export timeout |
| `LAMBDA_OTEL_EXPORTER_COMPRESSION` | `gzip` | `gzip` or `none` |
| `LAMBDA_OTEL_RECEIVER_HTTP_PORT` | `4318` | Local OTLP receiver port |
| `LAMBDA_OTEL_RECEIVER_HTTP_ENABLED` | `true` | Enable the local receiver |
| `LAMBDA_OTEL_FLUSH_STRATEGY` | `default` | `default`, `end`, `periodic` or `continuous` |
| `LAMBDA_OTEL_FLUSH_INTERVAL` | `20000` | Periodic flush interval |
| `LAMBDA_OTEL_FLUSH_COMPLETION_WAIT` | `auto` | `auto`, `off`, or a cap in milliseconds on how long `/next` is held awaiting completion |
| `LAMBDA_OTEL_FLUSH_INVOKE_BUDGET` | `3000` | Post-invocation flush budget, also reserved before the invocation deadline while holding `/next` (capped at half the remaining time). Raising it delays back-to-back warm invocations, never the response already sent; size it to cover a round trip to the endpoint |
| `LAMBDA_OTEL_FLUSH_MAX_BATCH_BYTES` | `4194304` | Encoded bytes per export batch |
| `LAMBDA_OTEL_FLUSH_MAX_BATCH_ENTRIES` | `1000` | Signals per export batch |
| `LAMBDA_OTEL_FLUSH_MAX_QUEUE_BYTES` | 10% of function memory, clamped to 4–32 MiB | Shared byte budget across all buffered telemetry |
| `LAMBDA_OTEL_FLUSH_MAX_QUEUE_ENTRIES` | `4096` | Shared entry budget across all buffered telemetry |
| `LAMBDA_OTEL_TELEMETRY_API_ENABLED` | `true` | Subscribe to the Lambda Telemetry API |
| `LAMBDA_OTEL_TELEMETRY_API_LISTENER_PORT` | `9999` | Port for receiving Telemetry API events |

#### TOML Configuration

Place `otel-extension.toml` in the Lambda function's deployment package
(`/var/task`):

```toml
[exporter]
endpoint = "https://your-collector.example.com"
protocol = "http"
compression = "gzip"
timeout = 500

[exporter.headers]
Authorization = "Bearer your-token"

[receiver]
http_port = 4318
http_enabled = true

[flush]
strategy = "default"
interval = 20000
completion_wait = "auto"
invoke_budget = 3000
max_queue_bytes = 16777216
max_queue_entries = 4096
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Lambda Execution Environment                 │
│                                                                      │
│  ┌─────────────┐            OTLP/HTTP                ┌────────────┐ │
│  │   Lambda    │ ──────────────────────────────────▶ │  Extension │ │
│  │  Function   │    traces, metrics, logs            │  Receiver  │ │
│  │(instrumented)│ ── POST /invocation/complete ────▶ │  :4318     │ │
│  └─────────────┘    (handler wrapper)                └─────┬──────┘ │
│                                                            │        │
│                                                            ▼        │
│                                                     ┌────────────┐  │
│                                                     │ Aggregator │  │
│                                                     │ (budgeted) │  │
│                                                     └─────┬──────┘  │
│                                                            │        │
│  ┌─────────────┐                                           │        │
│  │ Platform    │                                           ▼        │
│  │ Telemetry   │ ──────────────────────────────▶  ┌────────────┐   │
│  │ (Lambda API)│    platform metrics               │  Exporter  │   │
│  └─────────────┘                                   │ (OTLP/HTTP)│   │
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

1. **Init** - Extension registers, starts the OTLP receiver, subscribes to platform telemetry
2. **Invoke** - Receives signals from the function and holds its `/next` poll until the invocation completes, then flushes in the post-invocation window
3. **Shutdown** - Flushes all pending signals within the deadline carried by the SHUTDOWN event

### Freeze-Safe Flushing

Lambda freezes the execution environment only once the runtime has
responded *and* every extension has re-polled `/next`. The extension
exploits this: it delays its re-poll until the invocation is complete and
the flush has finished, so an export can never be interrupted by a freeze
and left dangling.

Completion is detected from three sources, in order of preference:

1. **Handler wrapper** (primary) - a `POST /invocation/complete` to the
   local receiver, sent by the wrappers in [`wrappers/`](../../wrappers)
   the moment the handler returns. This adds a few milliseconds of billed
   duration after the response has been sent; the client's response
   latency is unaffected.
2. **`platform.runtimeDone`** (backup) - the Telemetry API event, which
   can be delivered late or not at all in production.
3. **Deadline fallback** - the hold never extends past the invocation
   deadline (minus a safety margin). After a hold times out, holding is
   disabled until a timely completion signal is observed again, so a
   degraded environment pays the cost at most once.

The hold window is controlled by `flush.completion_wait`
(`auto`/`off`/cap in milliseconds).

## Instrumentation

Configure your Lambda function to send telemetry to the extension:

```bash
# Point OTLP exporters at the extension
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

For Node.js and Python functions, also set
`AWS_LAMBDA_EXEC_WRAPPER=/opt/otel-handler` with the wrappers from
[`wrappers/`](../../wrappers) so the extension learns the moment each
invocation completes.

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

1. Verify the function is sending to `http://127.0.0.1:4318`
2. Check extension logs in CloudWatch: `/aws/lambda/<function>/extension`
3. Ensure the layer is attached to the function

### Data not appearing in backend

1. Check `OTEL_EXPORTER_OTLP_ENDPOINT` is correct
2. Verify authentication headers are set
3. Review extension logs for export errors
4. Check network connectivity (VPC configuration)

### High latency

1. Enable compression: `OTEL_EXPORTER_OTLP_COMPRESSION=gzip`
2. Deploy a handler wrapper so exports run in the post-invocation window rather than alongside the next handler
3. Tune batch settings to reduce export frequency

## Licence

MIT

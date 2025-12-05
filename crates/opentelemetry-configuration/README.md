# opentelemetry-configuration

Opinionated OpenTelemetry SDK configuration for Rust applications.

This crate wires together the OpenTelemetry SDK, OTLP exporters, and the `tracing` crate ecosystem into a cohesive configuration system. It handles initialisation, flushing, and shutdown of all signal providers (traces, metrics, logs).

## Features

- **Layered configuration** - Combine defaults, config files, environment variables, and programmatic overrides using [figment](https://docs.rs/figment)
- **Sensible defaults** - Protocol-specific endpoints (localhost:4318 for HTTP, localhost:4317 for gRPC)
- **Drop-based lifecycle** - Automatic flush and shutdown when guard goes out of scope
- **Tracing integration** - Automatic setup of `tracing-opentelemetry` and `opentelemetry-appender-tracing` layers

## Quick Start

```rust
use opentelemetry_configuration::{OtelSdkBuilder, SdkError};

fn main() -> Result<(), SdkError> {
    let _guard = OtelSdkBuilder::new()
        .service_name("my-service")
        .build()?;

    tracing::info!("Application running");

    // On drop, all providers are flushed and shut down
    Ok(())
}
```

## Configuration

### Programmatic

```rust
use opentelemetry_configuration::{OtelSdkBuilder, Protocol, SdkError};

let _guard = OtelSdkBuilder::new()
    .endpoint("http://collector:4318")
    .protocol(Protocol::HttpBinary)
    .service_name("my-service")
    .service_version("1.0.0")
    .deployment_environment("production")
    .build()?;
```

### From Environment Variables

```rust
use opentelemetry_configuration::OtelSdkBuilder;

let _guard = OtelSdkBuilder::new()
    .with_standard_env()  // Reads OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_SERVICE_NAME, etc.
    .build()?;
```

### From Config File

```rust
use opentelemetry_configuration::OtelSdkBuilder;

let _guard = OtelSdkBuilder::new()
    .with_file("/etc/otel-config.toml")
    .build()?;
```

### TOML Configuration Format

```toml
[endpoint]
url = "http://collector:4318"
protocol = "httpbinary"  # or "grpc", "httpjson"
timeout = "10s"

[endpoint.headers]
Authorization = "Bearer token"

[resource]
service_name = "my-service"
service_version = "1.0.0"
deployment_environment = "production"

[traces]
enabled = true

[traces.batch]
max_queue_size = 2048
max_export_batch_size = 512
scheduled_delay = "5s"

[metrics]
enabled = true

[logs]
enabled = true
```

## Batch Configuration

The batch processor settings control how telemetry data is batched before export:

| Setting | Default | Description |
|---------|---------|-------------|
| `max_queue_size` | 2048 | Maximum spans/logs buffered before dropping |
| `max_export_batch_size` | 512 | Maximum items per export batch |
| `scheduled_delay` | 5s | Interval between export attempts |

## Protocol Support

| Protocol | Default Port | Content-Type |
|----------|--------------|--------------|
| `HttpBinary` (default) | 4318 | `application/x-protobuf` |
| `HttpJson` | 4318 | `application/json` |
| `Grpc` | 4317 | gRPC |

## Lifecycle Management

The `OtelGuard` returned by `build()` manages the lifecycle of all providers:

```rust
let guard = OtelSdkBuilder::new()
    .service_name("my-service")
    .build()?;

// Manual flush if needed
guard.flush();

// Explicit shutdown (consumes guard)
guard.shutdown()?;

// Or let drop handle it automatically
```

## Disabling Signals

```rust
let _guard = OtelSdkBuilder::new()
    .service_name("my-service")
    .disable_traces()
    .disable_metrics()
    .disable_logs()
    .build()?;
```

## Custom Resource Attributes

```rust
let _guard = OtelSdkBuilder::new()
    .service_name("my-service")
    .resource_attribute("deployment.region", "eu-west-1")
    .resource_attribute("team", "Australia II")
    .build()?;
```

## Licence

MIT

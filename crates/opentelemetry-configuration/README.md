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
    .traces(false)
    .metrics(false)
    .logs(false)
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

## Instrumentation Scope Name

By default, the instrumentation scope name (`otel.library.name`) is set to the service name. You can override it explicitly:

```rust
let _guard = OtelSdkBuilder::new()
    .service_name("my-api")
    .instrumentation_scope_name("my-api-tracing")
    .build()?;
```

## Compute Environment Detection

Resource attributes are automatically detected based on the compute environment. By default (`Auto`), generic detectors run and the environment is probed:

```rust
use opentelemetry_configuration::{OtelSdkBuilder, ComputeEnvironment};

// Explicit Lambda environment
let _guard = OtelSdkBuilder::new()
    .service_name("my-lambda")
    .compute_environment(ComputeEnvironment::Lambda)
    .build()?;

// Kubernetes environment
let _guard = OtelSdkBuilder::new()
    .service_name("my-k8s-service")
    .compute_environment(ComputeEnvironment::Kubernetes)
    .build()?;

// No automatic detection
let _guard = OtelSdkBuilder::new()
    .service_name("my-service")
    .compute_environment(ComputeEnvironment::None)
    .build()?;
```

Available environments:
- `Auto` (default): Runs host/OS/process/Rust detectors, probes for Lambda and K8s
- `Lambda`: Generic detectors + Rust detector + Lambda-specific attributes (faas.*, cloud.*)
- `Kubernetes`: Generic detectors + Rust detector + K8s detector
- `None`: No automatic detection

## Rust Build Information

### Runtime Detection (Automatic)

All compute environments (except `None`) automatically detect Rust-specific attributes:
- `process.runtime.name` = "rust"
- `rust.target_os`, `rust.target_arch`, `rust.target_family`
- `rust.debug` (true for debug builds)
- `process.executable.size` (binary size in bytes)

### Compile-Time Information (Optional)

To capture rustc version and channel, add to your `build.rs`:

```rust
fn main() {
    opentelemetry_configuration::emit_rustc_env();
}
```

Then in your application:

```rust
use opentelemetry_configuration::{OtelSdkBuilder, capture_rust_build_info};

let _guard = OtelSdkBuilder::new()
    .service_name("my-service")
    .with_rust_build_info(capture_rust_build_info!())
    .build()?;
```

This adds:
- `process.runtime.version` (e.g., "1.84.0")
- `process.runtime.description` (full rustc version string)
- `rust.channel` ("stable", "beta", or "nightly")

## Licence

MIT

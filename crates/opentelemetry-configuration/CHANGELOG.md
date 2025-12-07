# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/djvcom/lambda-observability/compare/opentelemetry-configuration-v0.1.0...opentelemetry-configuration-v0.1.1) - 2025-12-07

### Added

- *(opentelemetry-configuration)* complete high-priority improvements

### Other

- *(opentelemetry-configuration)* remove inline comments
- remove accidentally staged TODO.md
- *(opentelemetry-configuration)* use let-chains for clippy

## [0.1.0] - 2025-12-03

### Added

- Opinionated OpenTelemetry SDK setup with `OtelSdkBuilder`
- Layered configuration using figment
  - Sensible defaults (localhost:4318 for HTTP OTLP)
  - File-based configuration (TOML)
  - Environment variable overrides (`OTEL_*` prefix)
- Drop-based lifecycle management via `OtelGuard`
  - Automatic flush on drop
  - Graceful shutdown with configurable timeout
- Tracer, meter, and logger provider setup
- Integration with `tracing` via `tracing-opentelemetry`
- Log bridging from `log` and `tracing` to OpenTelemetry logs
- Support for HTTP and gRPC OTLP exporters

### Features

- `http` (default) - HTTP OTLP exporter
- `grpc` - gRPC OTLP exporter via tonic

### Known Limitations

- Fallback export feature defined but not yet wired into pipeline

[Unreleased]: https://github.com/australiaii/lambda-observability/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/australiaii/lambda-observability/releases/tag/v0.1.0

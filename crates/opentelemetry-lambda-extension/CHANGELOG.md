# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.3...opentelemetry-lambda-extension-v0.1.4) - 2025-12-23

### Other

- update Cargo.lock dependencies

## [0.1.3](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.2...opentelemetry-lambda-extension-v0.1.3) - 2025-12-10

### Other

- update Cargo.lock dependencies

## [0.1.2](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.1...opentelemetry-lambda-extension-v0.1.2) - 2025-12-08

### Other

- *(deps)* bump criterion in the rust-minor-patch group ([#22](https://github.com/djvcom/lambda-observability/pull/22))

## [0.1.1](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.0...opentelemetry-lambda-extension-v0.1.1) - 2025-12-07

### Fixed

- *(opentelemetry-lambda-extension)* correct doc example API usage
- *(opentelemetry-lambda-extension)* improve OTel compliance and docs

### Other

- *(opentelemetry-lambda-extension)* update backpressure test expectation

## [0.1.0] - 2025-12-03

### Added

- Deployable Lambda extension binary for OTLP telemetry collection
- Lambda Extensions API integration
  - Automatic registration
  - INVOKE and SHUTDOWN event handling
  - Graceful shutdown with telemetry flush
- OTLP receiver endpoints
  - `POST /v1/traces` - trace data
  - `POST /v1/metrics` - metric data
  - `POST /v1/logs` - log data
- Lambda Telemetry API subscription
  - Platform events (runtimeDone, initStart, etc.)
  - Function logs
  - Extension logs
- Adaptive export strategies
  - Per-invocation flush
  - Batch aggregation
  - Timeout-based flush
- Platform telemetry to OTLP conversion
  - Spans from lifecycle events
  - Metrics from duration and memory data
- Resource detection
  - `faas.name`, `faas.version`, `faas.max_memory`
  - `cloud.provider`, `cloud.region`, `cloud.account.id`
- Configuration via environment variables and TOML
- Compression support (gzip)

### Binary

- `lambda-otel-extension` - standalone Lambda extension binary

[Unreleased]: https://github.com/australiaii/lambda-observability/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/australiaii/lambda-observability/releases/tag/v0.1.0

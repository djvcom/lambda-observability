# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2025-12-03

### Added

- Reference Lambda handler implementations with OpenTelemetry instrumentation
- HTTP handler example (`create_http_service`)
  - API Gateway HTTP API (v2) request processing
  - Trace context extraction from `traceparent` header
  - Automatic span creation with semantic attributes
- SQS handler example (`create_sqs_service`)
  - Batch message processing
  - Span links for distributed tracing
  - Partial batch failure reporting
- `init_telemetry()` function demonstrating SDK setup
- Request body parsing with error simulation support
- Proper response serialisation for Lambda

[Unreleased]: https://github.com/australiaii/lambda-observability/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/australiaii/lambda-observability/releases/tag/v0.1.0

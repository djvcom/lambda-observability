# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2025-12-03

### Added

- Tower middleware for automatic Lambda handler instrumentation
- `OtelTracingLayer` with builder pattern configuration
- Trace context extraction via `ContextExtractor` trait
- Built-in extractors (feature-gated):
  - `ApiGatewayV2Extractor` - HTTP API (v2) with `traceparent` header
  - `ApiGatewayV1Extractor` - REST API (v1)
  - `SqsEventExtractor` - SQS batch processing with span links
  - `SnsEventExtractor` - SNS notifications
- Automatic span creation with FaaS semantic attributes
- `_X_AMZN_TRACE_ID` environment variable fallback
- Configurable flush-on-end behaviour
- Flush timeout configuration

### Features

- `http` - API Gateway extractors
- `sqs` - SQS event extractor
- `sns` - SNS event extractor

[Unreleased]: https://github.com/australiaii/lambda-observability/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/australiaii/lambda-observability/releases/tag/v0.1.0

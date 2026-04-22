# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-tower-v0.1.5...opentelemetry-lambda-tower-v0.1.6) - 2026-04-22

### Other

- *(deps)* bump the rust-minor-patch group with 5 updates ([#82](https://github.com/djvcom/lambda-observability/pull/82))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#80](https://github.com/djvcom/lambda-observability/pull/80))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#75](https://github.com/djvcom/lambda-observability/pull/75))
- *(deps)* bump the rust-minor-patch group with 4 updates ([#70](https://github.com/djvcom/lambda-observability/pull/70))
- *(deps)* bump the rust-minor-patch group with 4 updates ([#69](https://github.com/djvcom/lambda-observability/pull/69))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#67](https://github.com/djvcom/lambda-observability/pull/67))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#63](https://github.com/djvcom/lambda-observability/pull/63))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#61](https://github.com/djvcom/lambda-observability/pull/61))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#52](https://github.com/djvcom/lambda-observability/pull/52))

## [0.1.5](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-tower-v0.1.4...opentelemetry-lambda-tower-v0.1.5) - 2026-01-06

### Added

- feat!(workspace): remove opentelemetry-configuration crate ([#40](https://github.com/djvcom/lambda-observability/pull/40))

### Other

- *(deps)* bump the rust-minor-patch group with 4 updates ([#39](https://github.com/djvcom/lambda-observability/pull/39))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#37](https://github.com/djvcom/lambda-observability/pull/37))

## [0.1.4](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-tower-v0.1.3...opentelemetry-lambda-tower-v0.1.4) - 2025-12-24

### Other

- *(deps)* bump the rust-minor-patch group with 3 updates ([#32](https://github.com/djvcom/lambda-observability/pull/32))

## [0.1.3](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-tower-v0.1.2...opentelemetry-lambda-tower-v0.1.3) - 2025-12-23

### Other

- *(deps)* bump the rust-minor-patch group with 4 updates ([#29](https://github.com/djvcom/lambda-observability/pull/29))

## [0.1.2](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-tower-v0.1.1...opentelemetry-lambda-tower-v0.1.2) - 2025-12-10

### Added

- *(opentelemetry-lambda-tower)* add semantic convention integration tests

### Fixed

- *(opentelemetry-lambda-tower)* add required-features for examples

### Other

- Merge pull request #25 from djvcom/feat/tower-semantic-conventions-test

## [0.1.1](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-tower-v0.1.0...opentelemetry-lambda-tower-v0.1.1) - 2025-12-07

### Other

- *(opentelemetry-lambda-tower)* fix API naming in README

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

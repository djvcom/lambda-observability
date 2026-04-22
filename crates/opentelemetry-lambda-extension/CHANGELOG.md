# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.6...opentelemetry-lambda-extension-v0.1.7) - 2026-04-22

### Security

- Ship updated transitive TLS dependencies to address advisories against `aws-lc-sys` ([RUSTSEC-2026-0044](https://rustsec.org/advisories/RUSTSEC-2026-0044), [RUSTSEC-2026-0045](https://rustsec.org/advisories/RUSTSEC-2026-0045), [RUSTSEC-2026-0046](https://rustsec.org/advisories/RUSTSEC-2026-0046), [RUSTSEC-2026-0047](https://rustsec.org/advisories/RUSTSEC-2026-0047), [RUSTSEC-2026-0048](https://rustsec.org/advisories/RUSTSEC-2026-0048)) and `rustls-webpki` ([RUSTSEC-2026-0098](https://rustsec.org/advisories/RUSTSEC-2026-0098), [RUSTSEC-2026-0099](https://rustsec.org/advisories/RUSTSEC-2026-0099), [RUSTSEC-2026-0104](https://rustsec.org/advisories/RUSTSEC-2026-0104)) ([#83](https://github.com/djvcom/lambda-observability/pull/83)).

### Other

- *(opentelemetry-lambda-extension)* pass child cancellation token to receiver, by @ymgyt — first external contribution, thank you! ([#81](https://github.com/djvcom/lambda-observability/pull/81))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#80](https://github.com/djvcom/lambda-observability/pull/80))
- *(deps)* bump rand in the rust-minor-patch group ([#78](https://github.com/djvcom/lambda-observability/pull/78))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#75](https://github.com/djvcom/lambda-observability/pull/75))
- *(deps)* bump proptest in the rust-minor-patch group ([#72](https://github.com/djvcom/lambda-observability/pull/72))
- *(deps)* bump the rust-minor-patch group with 4 updates ([#70](https://github.com/djvcom/lambda-observability/pull/70))
- *(deps)* bump the rust-minor-patch group with 4 updates ([#69](https://github.com/djvcom/lambda-observability/pull/69))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#67](https://github.com/djvcom/lambda-observability/pull/67))
- *(deps)* bump the rust-minor-patch group with 4 updates ([#64](https://github.com/djvcom/lambda-observability/pull/64))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#63](https://github.com/djvcom/lambda-observability/pull/63))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#62](https://github.com/djvcom/lambda-observability/pull/62))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#61](https://github.com/djvcom/lambda-observability/pull/61))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#60](https://github.com/djvcom/lambda-observability/pull/60))
- *(deps)* bump rand from 0.9.2 to 0.10.0 ([#58](https://github.com/djvcom/lambda-observability/pull/58))
- *(deps)* bump tempfile in the rust-minor-patch group ([#59](https://github.com/djvcom/lambda-observability/pull/59))
- *(deps)* bump anyhow in the rust-minor-patch group ([#56](https://github.com/djvcom/lambda-observability/pull/56))
- *(deps)* bump proptest in the rust-minor-patch group ([#55](https://github.com/djvcom/lambda-observability/pull/55))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#54](https://github.com/djvcom/lambda-observability/pull/54))
- *(deps)* bump flate2 in the rust-minor-patch group ([#53](https://github.com/djvcom/lambda-observability/pull/53))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#52](https://github.com/djvcom/lambda-observability/pull/52))
- *(deps)* bump tonic in the rust-minor-patch group ([#51](https://github.com/djvcom/lambda-observability/pull/51))
- *(deps)* bump tower in the rust-minor-patch group ([#45](https://github.com/djvcom/lambda-observability/pull/45))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#44](https://github.com/djvcom/lambda-observability/pull/44))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#42](https://github.com/djvcom/lambda-observability/pull/42))

## [0.1.6](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.5...opentelemetry-lambda-extension-v0.1.6) - 2026-01-06

### Added

- feat!(workspace): remove opentelemetry-configuration crate ([#40](https://github.com/djvcom/lambda-observability/pull/40))

### Other

- *(deps)* bump the rust-minor-patch group with 4 updates ([#39](https://github.com/djvcom/lambda-observability/pull/39))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#37](https://github.com/djvcom/lambda-observability/pull/37))

## [0.1.5](https://github.com/djvcom/lambda-observability/compare/opentelemetry-lambda-extension-v0.1.4...opentelemetry-lambda-extension-v0.1.5) - 2025-12-24

### Other

- *(deps)* bump the rust-minor-patch group with 2 updates ([#33](https://github.com/djvcom/lambda-observability/pull/33))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#32](https://github.com/djvcom/lambda-observability/pull/32))

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

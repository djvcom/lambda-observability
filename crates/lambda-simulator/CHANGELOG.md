# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6](https://github.com/djvcom/lambda-observability/compare/lambda-simulator-v0.1.5...lambda-simulator-v0.1.6) - 2026-04-22

### Security

- Ship updated transitive TLS dependencies to address advisories against `aws-lc-sys` ([RUSTSEC-2026-0044](https://rustsec.org/advisories/RUSTSEC-2026-0044), [RUSTSEC-2026-0045](https://rustsec.org/advisories/RUSTSEC-2026-0045), [RUSTSEC-2026-0046](https://rustsec.org/advisories/RUSTSEC-2026-0046), [RUSTSEC-2026-0047](https://rustsec.org/advisories/RUSTSEC-2026-0047), [RUSTSEC-2026-0048](https://rustsec.org/advisories/RUSTSEC-2026-0048)) and `rustls-webpki` ([RUSTSEC-2026-0098](https://rustsec.org/advisories/RUSTSEC-2026-0098), [RUSTSEC-2026-0099](https://rustsec.org/advisories/RUSTSEC-2026-0099), [RUSTSEC-2026-0104](https://rustsec.org/advisories/RUSTSEC-2026-0104)) ([#83](https://github.com/djvcom/lambda-observability/pull/83)).

### Other

- *(deps)* bump the rust-minor-patch group with 2 updates ([#80](https://github.com/djvcom/lambda-observability/pull/80))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#75](https://github.com/djvcom/lambda-observability/pull/75))
- *(deps)* bump proptest in the rust-minor-patch group ([#72](https://github.com/djvcom/lambda-observability/pull/72))
- *(deps)* bump the rust-minor-patch group with 4 updates ([#69](https://github.com/djvcom/lambda-observability/pull/69))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#67](https://github.com/djvcom/lambda-observability/pull/67))
- *(deps)* bump the rust-minor-patch group with 2 updates ([#63](https://github.com/djvcom/lambda-observability/pull/63))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#61](https://github.com/djvcom/lambda-observability/pull/61))
- *(deps)* bump proptest in the rust-minor-patch group ([#55](https://github.com/djvcom/lambda-observability/pull/55))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#54](https://github.com/djvcom/lambda-observability/pull/54))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#52](https://github.com/djvcom/lambda-observability/pull/52))
- *(deps)* bump nix from 0.30.1 to 0.31.0 ([#48](https://github.com/djvcom/lambda-observability/pull/48))

## [0.1.5](https://github.com/djvcom/lambda-observability/compare/lambda-simulator-v0.1.4...lambda-simulator-v0.1.5) - 2026-01-06

### Other

- *(deps)* bump the rust-minor-patch group with 4 updates ([#39](https://github.com/djvcom/lambda-observability/pull/39))
- *(deps)* bump mock-collector in the rust-minor-patch group ([#37](https://github.com/djvcom/lambda-observability/pull/37))

## [0.1.4](https://github.com/djvcom/lambda-observability/compare/lambda-simulator-v0.1.3...lambda-simulator-v0.1.4) - 2025-12-24

### Added

- *(opentelemetry-configuration)* add compute environment detection and Rust resource detector ([#34](https://github.com/djvcom/lambda-observability/pull/34))

### Other

- *(deps)* bump the rust-minor-patch group with 3 updates ([#32](https://github.com/djvcom/lambda-observability/pull/32))

## [0.1.3](https://github.com/djvcom/lambda-observability/compare/lambda-simulator-v0.1.2...lambda-simulator-v0.1.3) - 2025-12-23

### Other

- update Cargo.lock dependencies

## [0.1.2](https://github.com/djvcom/lambda-observability/compare/lambda-simulator-v0.1.1...lambda-simulator-v0.1.2) - 2025-12-10

### Other

- update Cargo.lock dependencies

## [0.1.1](https://github.com/djvcom/lambda-observability/compare/lambda-simulator-v0.1.0...lambda-simulator-v0.1.1) - 2025-12-08

### Other

- *(deps)* bump criterion in the rust-minor-patch group ([#22](https://github.com/djvcom/lambda-observability/pull/22))

## [0.1.0] - 2025-12-03

### Added

- High-fidelity AWS Lambda Runtime API (`/2018-06-01/runtime/`)
  - `GET /invocation/next` with long-polling
  - `POST /invocation/{id}/response`
  - `POST /invocation/{id}/error`
  - `POST /init/error`
- Extensions API (`/2020-01-01/extension/`)
  - `POST /register` with identifier generation
  - `GET /event/next` for INVOKE and SHUTDOWN events
  - `POST /exit/{reason}` for graceful shutdown
- Telemetry API (`/2022-07-01/telemetry`)
  - Platform event subscription (platform, function, extension)
  - Buffering with configurable `max_items`, `max_bytes`, `timeout_ms`
  - All platform events: `runtimeDone`, `initStart`, `initRuntimeDone`, etc.
- Process freezing simulation
  - `FreezeMode::Process` using SIGSTOP/SIGCONT (Unix)
  - `FreezeMode::Notify` for cross-platform testing
  - Epoch-based race condition prevention
- Event-driven test helpers
  - `wait_for_runtime_ready()`
  - `wait_for_invocation_complete()`
  - `wait_for_extensions_ready()`
- Builder pattern configuration via `SimulatorBuilder`
- Environment configuration via `EnvConfig`

### Known Limitations

- Timeout enforcement not yet implemented (deadline calculated but not enforced)
- Environment variables not automatically injected
- Extension registration allowed after init phase

[Unreleased]: https://github.com/australiaii/lambda-observability/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/australiaii/lambda-observability/releases/tag/v0.1.0

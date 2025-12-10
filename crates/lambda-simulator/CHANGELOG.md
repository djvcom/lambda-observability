# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

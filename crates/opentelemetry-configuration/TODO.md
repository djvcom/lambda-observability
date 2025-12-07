# opentelemetry-configuration TODO

Consolidated findings from comprehensive code review. Items are prioritised for crates.io publication readiness.

## High Priority (Must Fix Before Publication)

### ~~H1: Add Environment Variable Tests~~ ✅ DONE
**Location**: `src/builder.rs` (tests module)

~~Despite having `with_env()` and `with_standard_env()`, there are no tests that actually set environment variables and verify they're picked up.~~

**Status**: Fixed. Added comprehensive tests using `temp_env` covering all standard OTEL environment variables, protocol mapping, signal disabling, and precedence verification.

### ~~H2: Add Endpoint URL Validation~~ ✅ DONE
**Location**: `src/builder.rs:490-495, 538-543`

~~Currently accepts any string as endpoint. Should validate URL is well-formed.~~

**Status**: Fixed. Added `SdkError::InvalidEndpoint` error variant and validation in both `extract_config()` and `build()` methods. URLs must start with `http://` or `https://`.

### ~~H3: Document Environment Variable Mapping~~ ✅ DONE
**Location**: `src/builder.rs:158-216`

~~The `with_standard_env()` implementation shows which env vars map to which config, but this isn't in the rustdoc.~~

**Status**: Fixed. Rustdoc at lines 158-167 now documents all supported environment variables and their configuration mappings.

### ~~H4: Add Examples Directory~~ ✅ DONE

~~For crates.io publication, include runnable examples in `examples/`.~~

**Status**: Fixed. Added:
- `examples/basic.rs` - Minimal setup demonstrating SDK initialisation
- `examples/configuration_layers.rs` - Shows layered configuration with precedence

---

## Medium Priority (Should Fix)

### M1: Add File Configuration Tests

The `with_file()` method isn't tested. Should verify:
- Valid TOML is parsed correctly
- Non-existent files are silently skipped
- Invalid TOML produces helpful error

### M2: Test Configuration Layering/Precedence

The core value proposition (layered config) isn't explicitly tested. Should verify:
- Programmatic overrides take precedence over env vars
- Env vars override file config
- File config overrides defaults

### M3: Support Additional Standard Env Vars
**Location**: `src/builder.rs:168-216`

Missing standard OTEL env vars:
- `OTEL_EXPORTER_OTLP_HEADERS`
- `OTEL_EXPORTER_OTLP_TIMEOUT`
- `OTEL_RESOURCE_ATTRIBUTES`
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (signal-specific)
- `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`
- `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`

### M4: Fix Header Parsing Errors
**Location**: `src/guard.rs:241-247`

Invalid headers are silently dropped. Should either return error or log warning:
```rust
for (key, value) in &config.endpoint.headers {
    match (key.parse::<MetadataKey<_>>(), value.parse::<MetadataValue<_>>()) {
        (Ok(k), Ok(v)) => { metadata.insert(k, v); }
        _ => { tracing::warn!(key = %key, "Failed to parse header"); }
    }
}
```

### M5: Improve Configuration Error Messages
**Location**: `src/builder.rs:477-481`

Figment error is boxed and wrapped, losing metadata about which field failed. Preserve or surface this information better.

### M6: Add Shutdown Timeout
**Location**: `src/guard.rs:158-175`

The `shutdown()` method doesn't have a timeout. In Lambda, blocked shutdown could prevent telemetry export.

**Recommendation**: Add configurable shutdown timeout, especially important for Lambda billing.

### M7: Document Flush vs Shutdown Difference
**Location**: `src/guard.rs`

`flush()` logs warnings via `tracing::warn!`, while `shutdown()` returns errors. This difference isn't clearly documented.

**Fix**: Add documentation explaining when to use each method.

### M8: Resource Clone API Inconsistency
**Location**: `src/guard.rs` (referenced from conversion.rs)

Some methods take ownership but then clone. Should either take ownership or take reference:
```rust
// Either:
pub fn set_resource(&mut self, resource: Resource) {
    self.resource = resource;  // Take ownership
}
// Or:
pub fn set_resource(&mut self, resource: &Resource) {
    self.resource = resource.clone();  // Clone from reference
}
```

---

## Low Priority (Nice to Have)

### L1: Support Custom Span Processors

Only supports `BatchSpanProcessor`. Some users might want `SimpleSpanProcessor` or custom processors.

**Recommendation**: Add `with_span_processor()` method.

### L2: Make Tracer Name Configurable
**Location**: `src/guard.rs:453, 459`

Hard-coded to `"lambda-otel-extension"`. Should be configurable or derived from service name.

### L3: Document Logger Provider Not Set Globally

Unlike tracer and meter providers, logger provider is not set globally. This is intentional but not documented.

### L4: Clean Up Verbose Fallback Documentation
**Location**: `src/lib.rs:50-77`

Fallback feature documented extensively despite not being implemented. Consider moving to separate module or feature-gating.

### L5: Remove Unused Dependencies

`serial_test` in dev-dependencies appears unused. Remove if not needed.

---

## Test Coverage

### Current State
- 31 unit tests covering config defaults, merging, builder, fallback strategies, protocol serialisation
- 1 integration test validating end-to-end flow with mock collector

### Missing Tests (HIGH Priority)

1. **Environment variable tests** - Core functionality untested
2. **File configuration tests** - TOML parsing untested
3. **Configuration layering tests** - Precedence order untested
4. **Invalid configuration tests** - Error handling untested

---

## Code Quality Strengths

### Excellent
- No unsafe code (`#![forbid(unsafe_code)]` enforced)
- Clean clippy run (no warnings)
- Proper use of let-chains (Rust 2024)

### Good
- Complete Cargo.toml metadata (description, repository, documentation, keywords, categories)
- CHANGELOG follows Keep a Changelog format
- Well-chosen dependencies (figment, opentelemetry, thiserror, humantime-serde)

---

## Architecture Strengths

### Figment Integration (Excellent)
- Clear precedence: defaults → files → env vars → programmatic overrides
- Protocol-aware endpoint defaults (4317 for gRPC, 4318 for HTTP)
- Good serde aliases for user convenience

### Builder Pattern (Excellent)
- Fluent API with sensible defaults
- `#[must_use]` attributes prevent silent no-ops
- Power-user escape hatch via `from_figment()`

### Drop-based Lifecycle (Excellent)
- Correct double-flush pattern (flush before shutdown)
- Uses `eprintln!` in Drop since logging may be unavailable
- `Option::take()` prevents double-shutdown

---

## Summary

| Priority | Count | Status |
|----------|-------|--------|
| High | 4 | ✅ All done |
| Medium | 8 | 0 done |
| Low | 5 | 0 done |

**Publication Status**: v0.1.0 published to crates.io. All high-priority items completed. Ready for v0.2.0 release.

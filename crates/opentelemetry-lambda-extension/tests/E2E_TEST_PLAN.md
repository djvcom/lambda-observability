# End-to-End Integration Test Plan: Lambda OTel Extension

## Overview

This document outlines the comprehensive test plan for validating the complete telemetry pipeline from Lambda function through the OTel extension to a backend collector.

## Architecture Under Test

```
                                    ┌─────────────────────┐
                                    │   Mock Collector    │
                                    │  (OTLP HTTP :4319)  │
                                    └─────────┬───────────┘
                                              │ receives
                                              │ exports
                    ┌─────────────────────────┴───────────────────────────┐
                    │                                                     │
                    │              Lambda OTel Extension                  │
                    │  ┌─────────────────────────────────────────────┐   │
                    │  │ OTLP Receiver (:4318) ◄── Function spans    │   │
                    │  │ Telemetry Listener (:9001) ◄── Platform     │   │
                    │  │ Extensions API Client ──► Simulator         │   │
                    │  │ Aggregator ──► Exporter ──► Collector       │   │
                    │  └─────────────────────────────────────────────┘   │
                    └────────────────────┬────────────────────────────────┘
                                         │
                    ┌────────────────────┼────────────────────┐
                    │                    │                    │
                    │     Lambda Runtime Simulator            │
                    │  ┌─────────────────┴──────────────────┐ │
                    │  │ Runtime API (/next, /response)     │ │
                    │  │ Extensions API (/register, /next)  │ │
                    │  │ Telemetry API (/subscribe)         │ │
                    │  │ Telemetry Push (to :9001)          │ │
                    │  └─────────────────┬──────────────────┘ │
                    └────────────────────┼────────────────────┘
                                         │
                    ┌────────────────────┴────────────────────┐
                    │         Lambda Function                 │
                    │  ┌─────────────────────────────────────┐│
                    │  │ HTTP Handler                        ││
                    │  │ - Extracts traceparent from headers ││
                    │  │ - Creates child span                ││
                    │  │ - Sets semantic attributes          ││
                    │  │ - Exports to Extension :4318        ││
                    │  └─────────────────────────────────────┘│
                    └─────────────────────────────────────────┘
```

## Test Phases and Assertions

### Phase 1: Start Mock Collector

**Objective**: OTLP collector is running and ready to receive exports

**Implementation**:
```rust
let collector = MockServer::builder()
    .protocol(MockProtocol::HttpBinary)
    .start()
    .await
    .expect("Failed to start mock collector");
let collector_endpoint = format!("http://{}", collector.addr());
```

**Assertions**:
- Collector starts successfully
- Port is bound and accessible

**What This Proves**: Backend is ready to receive telemetry

**False Positive Risk**: LOW - synchronous operation

---

### Phase 2: Start Simulator

**Objective**: Lambda runtime API simulator is running

**Implementation**:
```rust
let simulator = Simulator::builder()
    .function_name("otel-e2e-test")
    .extension_ready_timeout(Duration::from_secs(5))
    .build()
    .await
    .expect("Failed to start simulator");

simulator.enable_telemetry_capture().await;
let runtime_api_base = simulator.runtime_api_url().replace("http://", "");
```

**Assertions**:
- Simulator starts successfully
- Telemetry capture enabled for later assertions

**What This Proves**: Runtime/Extensions/Telemetry APIs are available

**False Positive Risk**: LOW

---

### Phase 3: Start Extension (Background Task)

**Objective**: Extension registers, subscribes to Telemetry API, starts receivers

**Implementation**:
```rust
let extension_config = Config {
    exporter: ExporterConfig {
        endpoint: Some(collector_endpoint.clone()),
        protocol: Protocol::Http,
        timeout: Duration::from_secs(5),
        ..Default::default()
    },
    telemetry_api: TelemetryApiConfig {
        enabled: true,
        listener_port: 9001,
        buffer_size: 100,
    },
    receiver: ReceiverConfig {
        http_enabled: true,
        http_port: 4318,
    },
    flush: FlushConfig {
        strategy: FlushStrategy::End,
        ..Default::default()
    },
    ..Default::default()
};

let runtime = RuntimeBuilder::new()
    .config(extension_config)
    .build();

let extension_handle = tokio::spawn(async move {
    runtime.run().await
});
```

**Synchronization**:
```rust
// Wait for extension to register
simulator.wait_for(
    || async { simulator.extension_count().await >= 1 },
    Duration::from_secs(5)
).await.expect("Extension did not register");

// Verify OTLP receiver is listening
let client = reqwest::Client::new();
let health = client.post("http://127.0.0.1:4318/v1/traces")
    .header("Content-Type", "application/x-protobuf")
    .body(vec![])
    .send()
    .await;
assert!(health.is_ok(), "OTLP receiver not responding");
```

**Assertions**:
1. Extension registered with simulator
2. OTLP receiver port 4318 is accepting connections
3. (Implicit) Telemetry API subscription succeeded

**What This Proves**:
- Extension lifecycle initialization completed
- Extension can receive function telemetry
- Extension can receive platform telemetry

**False Positive Risk**: MEDIUM - need to verify registration, not just task spawned

**Critical Race Condition**: Extension MUST register BEFORE runtime starts polling `/next`

---

### Phase 4: Start Lambda Runtime (Background Task)

**Objective**: Lambda runtime is polling `/next` and ready to process invocations

**Implementation**:
```rust
// The handler function must:
// 1. Extract traceparent from HTTP event headers
// 2. Create a span as child of that trace context
// 3. Set semantic attributes
// 4. Export to extension's OTLP receiver

let runtime_handle = tokio::spawn(async move {
    let func = service_fn(instrumented_http_handler);
    let _ = run(func).await;
});
```

**Synchronization**: Wait brief period for runtime to start polling

**Assertions**: None directly (verified by successful invocation)

**What This Proves**: Runtime is alive and will process invocations

**False Positive Risk**: HIGH - hard to verify without invocation

---

### Phase 5: Enqueue HTTP Event with Trace Context

**Objective**: Create invocation with W3C trace context in HTTP headers

**Implementation**:
```rust
// Known trace context for assertion
let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
let parent_span_id = "00f067aa0ba902b7";
let traceparent = format!("00-{}-{}-01", trace_id, parent_span_id);

let http_event = json!({
    "version": "2.0",
    "routeKey": "POST /test",
    "rawPath": "/test",
    "headers": {
        "traceparent": traceparent.clone(),
        "content-type": "application/json"
    },
    "body": "{\"message\": \"trace test\", \"delay_ms\": 10}",
    "requestContext": {
        "http": {
            "method": "POST",
            "path": "/test"
        }
    }
});

let invocation = InvocationBuilder::new()
    .payload(http_event)
    .build()
    .unwrap();
let request_id = invocation.request_id.clone();
simulator.enqueue(invocation).await;
```

**Assertions**:
- Invocation enqueued (implicit from no error)

**What This Proves**: Event with trace context is in the queue

**False Positive Risk**: LOW

---

### Phase 6: Wait for Invocation Completion

**Objective**: Runtime processes invocation successfully

**Implementation**:
```rust
let state = simulator
    .wait_for_invocation_complete(&request_id, Duration::from_secs(10))
    .await
    .expect("Invocation did not complete");

assert_eq!(state.status, InvocationStatus::Success);
```

**Assertions**:
1. Invocation completed (not pending/processing)
2. Status is Success (not Error/Timeout)

**What This Proves**:
- Runtime received event via `/next`
- Handler executed
- Runtime posted response via `/response`

**False Positive Risk**: LOW - explicit state check

---

### Phase 7: Wait for Extension Readiness and Flush

**Objective**: Extension has processed all signals and exported to collector

**Implementation**:
```rust
// Wait for extension to signal readiness (indicates it processed the invoke)
simulator.wait_for_extensions_ready(&request_id, Duration::from_secs(5))
    .await
    .expect("Extensions did not become ready");

// Give extension time to flush to collector
// (FlushStrategy::End flushes on each invoke completion)
tokio::time::sleep(Duration::from_millis(500)).await;
```

**Assertions**:
- Extension polled `/next` after invocation (readiness signal)

**What This Proves**: Extension received INVOKE event and completed processing

**False Positive Risk**: MEDIUM - need to verify collector actually received data

---

### Phase 8: Validate Platform Telemetry Events

**Objective**: Simulator sent correct platform events to extension

**Implementation**:
```rust
let start_events = simulator.get_telemetry_events_by_type("platform.start").await;
let runtime_done_events = simulator.get_telemetry_events_by_type("platform.runtimeDone").await;
let report_events = simulator.get_telemetry_events_by_type("platform.report").await;
```

**Assertions**:
```rust
// platform.start
assert_eq!(start_events.len(), 1);
let start = &start_events[0];
assert_eq!(start.record["requestId"], json!(request_id));

// platform.runtimeDone
assert_eq!(runtime_done_events.len(), 1);
let runtime_done = &runtime_done_events[0];
assert_eq!(runtime_done.record["requestId"], json!(request_id));
assert_eq!(runtime_done.record["status"], json!("success"));

// platform.report
assert_eq!(report_events.len(), 1);
let report = &report_events[0];
assert_eq!(report.record["requestId"], json!(request_id));
// Verify metrics exist
let metrics = &report.record["metrics"];
assert!(metrics["durationMs"].is_number());
assert!(metrics["billedDurationMs"].is_number());
assert!(metrics["memorySizeMB"].is_number());
assert!(metrics["maxMemoryUsedMB"].is_number());
```

**What This Proves**: Simulator's Telemetry API implementation is correct

**False Positive Risk**: LOW - explicit content verification

---

### Phase 9: Validate Function Spans at Collector

**Objective**: Function's spans arrived at collector with correct trace context

**Implementation**:
```rust
collector.with_collector(|c| {
    // Find the function's handler span
    let handler_span = c.expect_span_with_name("http.request")
        .assert_exists();

    // CRITICAL: Verify trace propagation
    // The span must have the same trace_id as the incoming traceparent
    // and parent_span_id matching the incoming span
    let span_trace_id = hex::encode(&handler_span.trace_id);
    let span_parent_id = hex::encode(&handler_span.parent_span_id);

    assert_eq!(span_trace_id, trace_id, "Function span has wrong trace_id");
    assert_eq!(span_parent_id, parent_span_id, "Function span has wrong parent_span_id");
}).await;
```

**Assertions**:
1. Span with expected name exists
2. trace_id matches incoming traceparent (propagation worked)
3. parent_span_id matches incoming parent (hierarchy correct)

**What This Proves**:
- Function extracted trace context from HTTP headers
- Function created child span correctly
- Extension received function's OTLP export
- Extension exported to collector

**False Positive Risk**: LOW with exact trace_id matching

---

### Phase 10: Validate Semantic Attributes on Function Spans

**Objective**: Function spans have correct OTel semantic conventions

**Implementation**:
```rust
collector.with_collector(|c| {
    let span = c.expect_span_with_name("http.request");
    let attrs = &span.attributes;

    // HTTP attributes
    assert_attribute_string(attrs, "http.request.method", "POST");
    assert_attribute_string(attrs, "url.path", "/test");

    // FaaS attributes
    assert_attribute_string(attrs, "faas.trigger", "http");
    assert_attribute_string(attrs, "faas.invocation_id", &request_id);

    // Cloud attributes (from resource)
    // These may be on the Resource, not span attributes

    // faas.parent_span marker (correlation point for platforms)
    assert_attribute_bool(attrs, "faas.parent_span", true);
}).await;
```

**Assertions**: Each semantic attribute has correct key and value

**What This Proves**: Function instrumentation follows OTel conventions

**False Positive Risk**: LOW - exact attribute matching

---

### Phase 11: Validate Platform Metrics at Collector

**Objective**: Extension converted platform.report to OTLP metrics

**Implementation**:
```rust
collector.with_collector(|c| {
    // Invoke duration metric
    c.expect_metric_with_name("faas.invoke_duration")
        .assert_exists();

    // Billed duration metric
    c.expect_metric_with_name("aws.lambda.billed_duration")
        .assert_exists();

    // Memory usage metric
    c.expect_metric_with_name("aws.lambda.max_memory_used")
        .assert_exists();
}).await;
```

**Assertions**:
1. Each expected metric exists
2. (If mock-collector supports) Metric values are reasonable

**What This Proves**:
- Extension received Telemetry API events
- TelemetryProcessor converted events to OTLP metrics
- Metrics exported to collector

**False Positive Risk**: LOW

---

### Phase 12: Cleanup

**Implementation**:
```rust
// Trigger graceful shutdown to ensure final flush
simulator.graceful_shutdown(ShutdownReason::Spindown).await;

// Wait for extension task
let _ = tokio::time::timeout(Duration::from_secs(2), extension_handle).await;

// Abort runtime (it would block forever on /next)
runtime_handle.abort();

// Shutdown collector
collector.shutdown().await.expect("Collector shutdown failed");
```

---

## Required Lambda Function Implementation

The test requires a Lambda function that:

1. **Accepts raw JSON** to access HTTP event headers
2. **Extracts traceparent** from `event.headers.traceparent`
3. **Parses W3C trace context** into trace_id and parent_span_id
4. **Creates instrumented span** with that parent context
5. **Sets semantic attributes** per OTel conventions
6. **Exports via OTLP** to `http://127.0.0.1:4318`

```rust
use opentelemetry::trace::{SpanKind, Tracer, TraceContextExt};
use opentelemetry_sdk::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;

pub async fn instrumented_http_handler(
    event: LambdaEvent<Value>,
) -> Result<Value, LambdaError> {
    let (payload, context) = event.into_parts();

    // Extract traceparent from HTTP event
    let traceparent = payload
        .get("headers")
        .and_then(|h| h.get("traceparent"))
        .and_then(|v| v.as_str());

    // Parse and create parent context
    let parent_ctx = traceparent
        .and_then(parse_traceparent)
        .unwrap_or_else(|| Context::current());

    // Get tracer and create span
    let tracer = opentelemetry::global::tracer("lambda-function");

    let mut span_builder = tracer
        .span_builder("http.request")
        .with_kind(SpanKind::Server);

    let span = span_builder.start_with_context(&tracer, &parent_ctx);

    // Set semantic attributes
    span.set_attribute(KeyValue::new("faas.trigger", "http"));
    span.set_attribute(KeyValue::new("faas.invocation_id", context.request_id.clone()));
    span.set_attribute(KeyValue::new("faas.parent_span", true));
    span.set_attribute(KeyValue::new("http.request.method",
        payload.pointer("/requestContext/http/method")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN")));
    span.set_attribute(KeyValue::new("url.path",
        payload.get("rawPath")
            .and_then(|v| v.as_str())
            .unwrap_or("/")));

    // Do actual work in span context
    let _guard = parent_ctx.with_span(span);

    // Process request...
    let body: RequestBody = serde_json::from_str(
        payload.get("body").and_then(|v| v.as_str()).unwrap_or("{}")
    )?;

    if body.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(body.delay_ms)).await;
    }

    // Return response
    Ok(json!({
        "statusCode": 200,
        "body": json!({
            "message": format!("Processed: {}", body.message),
            "request_id": context.request_id
        }).to_string()
    }))
}
```

---

## Mock Collector Capability Gaps

Based on `exporter_test.rs`, the mock-collector supports:
- `expect_span_with_name(name)` -> SpanAssertion
- `expect_span()` -> SpanAssertion
- `span_count()` -> usize
- `assert_exists()` / `assert_count(n)`

**Likely needed additions**:
- Access to span's `trace_id` and `parent_span_id` bytes
- Access to span's attributes for assertion
- `expect_metric_with_name(name)` -> MetricAssertion
- Metric value access

**Workaround**: If mock-collector lacks these, we can:
1. Extend mock-collector crate
2. Use lower-level access if available
3. For this test, verify span count + names as smoke test

---

## Edge Cases to Test Separately

After the main happy-path test works:

### Edge Case 1: Missing traceparent
- Enqueue event without traceparent header
- Function should create new root span
- Verify span has trace_id but empty parent_span_id

### Edge Case 2: Function Error
- Handler returns error
- Verify platform.runtimeDone.status = "error"
- Verify span has StatusCode::Error

### Edge Case 3: Cold Start Detection
- First invocation should have init_duration metric
- Second invocation should NOT have init_duration metric

### Edge Case 4: Extension Export Failure
- Point extension to unreachable endpoint
- Verify function still completes
- Verify ExportResult::Fallback behavior

---

## Synchronization Summary

| Checkpoint | Sync Method | Timeout |
|------------|-------------|---------|
| Collector ready | Start returns | N/A |
| Simulator ready | Builder returns | N/A |
| Extension registered | `simulator.wait_for(extension_count >= 1)` | 5s |
| OTLP receiver ready | HTTP health check | 2s |
| Invocation complete | `simulator.wait_for_invocation_complete()` | 10s |
| Extension ready | `simulator.wait_for_extensions_ready()` | 5s |
| Flush complete | Fixed sleep (FlushStrategy::End) | 500ms |
| Collector has data | `with_collector` (immediate check) | N/A |

---

## Test File Location

```
crates/lambda-otel-extension/tests/e2e_integration_test.rs
```

---

## Dependencies Required

```toml
[dev-dependencies]
tokio = { version = "1", features = ["full", "test-util"] }
mock-collector = { path = "../../mock-collector" }
lambda_runtime = "0.13"
serde_json = "1"
temp-env = "0.3"
hex = "0.4"
opentelemetry = "0.24"
opentelemetry-sdk = { version = "0.24", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.17", features = ["http-proto"] }
```

---

## Mock Collector API Improvements (Future Refactor)

Based on implementing the E2E tests, the following improvements to the `mock-collector` crate would make testing easier:

### 1. Async Wait-for-Data API

**Current**: Must poll or use fixed sleeps
```rust
// Workaround: fixed sleep then check
tokio::time::sleep(Duration::from_millis(500)).await;
collector.with_collector(|c| {
    assert!(c.span_count() >= 1);
}).await;
```

**Proposed**: Condition-based waiting
```rust
// Wait for specific condition with timeout
collector.wait_for(
    |c| c.span_count() >= 1,
    Duration::from_secs(5)
).await?;

// Or wait for specific span
collector.wait_for_span_with_name("http.request", Duration::from_secs(5)).await?;
```

**Benefit**: Eliminates flaky tests from timing races

### 2. Direct Trace ID/Span ID Access

**Current**: Access via `span.span()` then manual hex conversion
```rust
let span = matching_spans[0];
let span_trace_id = bytes_to_hex(&span.span().trace_id);
```

**Proposed**: Helper methods returning hex strings
```rust
let span = matching_spans[0];
assert_eq!(span.trace_id_hex(), trace_id);
assert_eq!(span.parent_span_id_hex(), parent_span_id);
```

**Benefit**: Less boilerplate in tests

### 3. Attribute Assertion Helpers

**Current**: Must chain `.with_attributes([...]).assert_exists()`
```rust
c.expect_span_with_name("http.request")
    .with_attributes([("faas.trigger", "http")])
    .assert_exists();
```

**Proposed**: Fluent attribute value getters
```rust
let span = c.get_span_with_name("http.request")?;
assert_eq!(span.get_string_attribute("faas.trigger"), Some("http"));
assert_eq!(span.get_bool_attribute("faas.parent_span"), Some(true));
```

**Benefit**: Better error messages on failure

### 4. Span Relationship Assertions

**Current**: Manual trace ID comparison
```rust
assert_eq!(span_trace_id, expected_trace_id);
assert_eq!(span_parent_id, expected_parent_span_id);
```

**Proposed**: Relationship helpers
```rust
// Assert span is child of specific parent
c.expect_span_with_name("http.request")
    .is_child_of_trace(trace_id)
    .has_parent_span(parent_span_id)
    .assert_exists();

// Or find span by trace relationship
let child = c.find_span_in_trace(trace_id)?;
```

**Benefit**: More expressive test assertions

### 5. Metric Value Access

**Current**: Only count-based assertions
```rust
let count = c.expect_metric_with_name("faas.invoke_duration").count();
```

**Proposed**: Value access
```rust
let metric = c.get_metric_with_name("faas.invoke_duration")?;
let value = metric.get_gauge_value()?;
assert!(value > 0.0);
```

**Benefit**: Can validate actual metric values

### 6. Clear/Reset Between Tests

**Current**: Must create new MockServer for each test
```rust
let collector = MockServer::builder()
    .protocol(MockProtocol::HttpBinary)
    .start()
    .await?;
```

**Proposed**: Clear collected data
```rust
collector.clear().await;  // Reset for next test scenario
```

**Benefit**: Faster tests, less resource usage

### 7. Subscription/Notification Pattern

**Current**: Pull-based checking only

**Proposed**: Async channel for new data
```rust
let mut rx = collector.subscribe_spans();
while let Some(span) = rx.recv().await {
    if span.name == "http.request" {
        // Process immediately
        break;
    }
}
```

**Benefit**: Event-driven tests without polling

### Implementation Priority

| Improvement | Priority | Complexity | Impact |
|-------------|----------|------------|--------|
| Async wait-for-data | HIGH | Medium | Eliminates timing flakes |
| Trace ID helpers | MEDIUM | Low | Reduces boilerplate |
| Attribute getters | MEDIUM | Low | Better assertions |
| Relationship assertions | MEDIUM | Medium | Expressive tests |
| Metric value access | LOW | Medium | Platform metrics validation |
| Clear/reset | LOW | Low | Test isolation |
| Subscription pattern | LOW | High | Advanced use cases |

---

## Lessons Learned

### OpenTelemetry SDK Flush Behavior

The OpenTelemetry SDK's batch exporter has specific behavior that affects testing:

1. **`force_flush()` is blocking**: It waits for the HTTP export to complete
2. **Calling from async context causes deadlock**: The blocking call prevents the Tokio runtime from executing the HTTP client
3. **Solution**: Use `tokio::task::spawn_blocking` to run flush in a blocking thread pool

```rust
// WRONG: Deadlocks
pub fn flush_tracer() {
    provider.force_flush();  // Blocks async runtime
}

// CORRECT: Uses blocking thread pool
pub async fn flush_tracer() {
    let provider = provider.clone();
    tokio::task::spawn_blocking(move || {
        provider.force_flush();
    }).await.ok();
}
```

### Extension Event Loop Timing

With `FlushStrategy::End`, the extension flushes when it receives the **next** INVOKE event, not when the current invocation ends. This means:

1. Single invocation tests won't see spans at collector until shutdown
2. Must use `graceful_shutdown()` to trigger final flush via SHUTDOWN event
3. Aborting the extension task skips the final flush

### Test Isolation with Global State

The Lambda function's tracer uses a global `OnceLock<TracerProvider>`:
- Initialized once per process with specific OTLP endpoint
- Tests must use same port or tracer will export to wrong endpoint
- Serial test execution (`#[serial]`) is required

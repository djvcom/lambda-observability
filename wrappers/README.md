# Handler wrappers

Wrapper scripts for Node.js and Python runtimes that signal the
[`opentelemetry-lambda-extension`](../crates/opentelemetry-lambda-extension)
the moment an invocation finishes.

## Why

The extension flushes telemetry in the post-invocation window — after the
response has been sent, but before the execution environment freezes. To do
this reliably it needs to know when the handler has finished. The
`platform.runtimeDone` telemetry event carries that information, but in
production it can arrive late or not at all, so the extension treats it only
as a backup signal.

These wrappers provide the primary signal: they wrap the function handler
and `POST /invocation/complete` to the extension's local receiver as soon as
the handler returns (or throws). The signal is best-effort — it is bounded
by a 50 ms timeout and never raises — so a missing or unhealthy extension
cannot fail the function.

Without a wrapper, the extension still works: it falls back to
`platform.runtimeDone`, and beyond that to a deadline-based fallback that
disables holding after the first timeout.

## Deployment

Ship the wrapper alongside the extension binary in a Lambda layer:

```
layer.zip
├── extensions/
│   └── opentelemetry-lambda-extension
├── otel-handler                # from wrappers/nodejs or wrappers/python
├── nodejs/
│   └── otel-wrapper.js         # Node.js functions
└── python/
    └── otel_wrapper.py         # Python functions
```

Then set the exec wrapper on the function:

```
AWS_LAMBDA_EXEC_WRAPPER=/opt/otel-handler
```

The `otel-handler` script records the configured handler in `ORIG_HANDLER`,
points `_HANDLER` at the wrapper module, and hands control back to the
runtime. The wrapper loads and invokes the original handler unchanged.

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `LAMBDA_OTEL_RECEIVER_HTTP_PORT` | `4318` | Port of the extension's local OTLP receiver |
| `LAMBDA_OTEL_COMPLETION_TIMEOUT_MS` | `50` | Upper bound on the time spent sending the completion signal |

The wrappers read the same port variable as the extension, so overriding the
receiver port keeps both in step.

## Scope

The wrappers only signal completion; they do not bootstrap an OpenTelemetry
SDK or instrument the handler. Instrument the function with the OTel SDK for
its language (exporting to `http://127.0.0.1:4318`) or, for Rust handlers,
with [`opentelemetry-lambda-tower`](../crates/opentelemetry-lambda-tower).

## Testing

A smoke test exercises both wrappers against a stub receiver and skips
gracefully when `node` or `python3` is not installed:

```bash
cargo test -p opentelemetry-lambda-extension --test wrapper_smoke_test -- --ignored
```

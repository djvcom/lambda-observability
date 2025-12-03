//! Fuzz target for X-Ray to W3C traceparent conversion.
//!
//! Run with: `cargo +nightly fuzz run fuzz_traceparent`

#![no_main]

use libfuzzer_sys::fuzz_target;
use opentelemetry_lambda_tower::extractors::http::{convert_xray_to_traceparent, parse_xray_trace_id};

fuzz_target!(|data: &str| {
    let _ = convert_xray_to_traceparent(data);
    let _ = parse_xray_trace_id(data);
});

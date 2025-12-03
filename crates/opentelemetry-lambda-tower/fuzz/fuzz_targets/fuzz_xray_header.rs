//! Fuzz target for X-Ray trace header parsing.
//!
//! Run with: `cargo +nightly fuzz run fuzz_xray_header`

#![no_main]

use libfuzzer_sys::fuzz_target;
use opentelemetry_lambda_tower::extractors::sqs::parse_xray_trace_header;

fuzz_target!(|data: &str| {
    let _ = parse_xray_trace_header(data);
});

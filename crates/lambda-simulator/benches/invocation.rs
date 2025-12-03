//! Benchmarks for invocation creation and processing.

use criterion::{Criterion, criterion_group, criterion_main};
use lambda_simulator::invocation::{Invocation, InvocationBuilder};
use serde_json::json;
use std::hint::black_box;

fn bench_invocation_new(c: &mut Criterion) {
    let payload = json!({"key": "value", "number": 42});

    c.bench_function("Invocation::new", |b| {
        b.iter(|| Invocation::new(black_box(payload.clone()), black_box(3000)))
    });
}

fn bench_invocation_builder(c: &mut Criterion) {
    let payload = json!({"key": "value", "number": 42});

    c.bench_function("InvocationBuilder::build", |b| {
        b.iter(|| {
            InvocationBuilder::new()
                .payload(black_box(payload.clone()))
                .timeout_ms(black_box(3000))
                .function_arn("arn:aws:lambda:us-east-1:123456789012:function:test")
                .build()
                .unwrap()
        })
    });
}

fn bench_invocation_deadline_ms(c: &mut Criterion) {
    let invocation = Invocation::new(json!({}), 3000);

    c.bench_function("Invocation::deadline_ms", |b| {
        b.iter(|| black_box(&invocation).deadline_ms())
    });
}

criterion_group!(
    benches,
    bench_invocation_new,
    bench_invocation_builder,
    bench_invocation_deadline_ms,
);
criterion_main!(benches);

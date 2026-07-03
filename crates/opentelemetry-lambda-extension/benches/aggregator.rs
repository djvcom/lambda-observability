//! Benchmarks for the signal aggregator's hot paths: pushing signals under
//! the shared byte/entry budget (including forced eviction) and draining
//! batches for export.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use opentelemetry_lambda_extension::aggregator::SignalAggregator;
use opentelemetry_lambda_extension::config::FlushConfig;
use opentelemetry_lambda_extension::receiver::Signal;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use std::hint::black_box;

fn make_trace_signal(span_name: &str, spans: usize) -> Signal {
    Signal::Traces(ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans: (0..spans)
                    .map(|i| Span {
                        name: format!("{span_name}-{i}"),
                        trace_id: vec![1; 16],
                        span_id: vec![2; 8],
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    })
}

fn bench_add(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let signals: Vec<Signal> = (0..100)
        .map(|i| make_trace_signal(&format!("bench-span-{i}"), 10))
        .collect();

    let mut group = c.benchmark_group("aggregator_add");
    group.throughput(Throughput::Elements(signals.len() as u64));

    group.bench_function("within_budget", |b| {
        b.iter_batched(
            || {
                (
                    SignalAggregator::new(FlushConfig::default()),
                    signals.clone(),
                )
            },
            |(aggregator, signals)| {
                runtime.block_on(async {
                    for signal in signals {
                        aggregator.add(signal).await;
                    }
                    black_box(aggregator.pending_bytes().await)
                })
            },
            BatchSize::SmallInput,
        )
    });

    group.bench_function("evicting", |b| {
        let config = FlushConfig {
            max_queue_bytes: 16 * 1024,
            ..Default::default()
        };
        b.iter_batched(
            || (SignalAggregator::new(config.clone()), signals.clone()),
            |(aggregator, signals)| {
                runtime.block_on(async {
                    for signal in signals {
                        aggregator.add(signal).await;
                    }
                    black_box(aggregator.dropped_count().await)
                })
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

fn bench_drain(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let signals: Vec<Signal> = (0..1_000)
        .map(|i| make_trace_signal(&format!("bench-span-{i}"), 10))
        .collect();

    let mut group = c.benchmark_group("aggregator_drain");
    group.throughput(Throughput::Elements(signals.len() as u64));

    group.bench_function("get_all_batches_1000_signals", |b| {
        b.iter_batched(
            || {
                let aggregator = SignalAggregator::new(FlushConfig::default());
                runtime.block_on(async {
                    for signal in &signals {
                        aggregator.add(signal.clone()).await;
                    }
                });
                aggregator
            },
            |aggregator| runtime.block_on(async { black_box(aggregator.get_all_batches().await) }),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_add, bench_drain);
criterion_main!(benches);

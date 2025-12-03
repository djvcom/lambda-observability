//! Benchmarks for telemetry event to OTLP signal conversion.

use criterion::{Criterion, criterion_group, criterion_main};
use opentelemetry_lambda_extension::conversion::{
    MetricsConverter, SpanConverter, TelemetryProcessor,
};
use opentelemetry_lambda_extension::telemetry::{
    ReportMetrics, ReportRecord, RuntimeDoneRecord, StartRecord, TelemetryEvent,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use std::hint::black_box;

fn make_start_record(request_id: &str) -> StartRecord {
    StartRecord {
        request_id: request_id.to_string(),
        version: Some("$LATEST".to_string()),
        tracing: None,
    }
}

fn make_runtime_done_record(request_id: &str) -> RuntimeDoneRecord {
    RuntimeDoneRecord {
        request_id: request_id.to_string(),
        status: "success".to_string(),
        metrics: None,
        tracing: None,
        spans: vec![],
    }
}

fn make_report_record(request_id: &str) -> ReportRecord {
    ReportRecord {
        request_id: request_id.to_string(),
        status: "success".to_string(),
        metrics: ReportMetrics {
            duration_ms: 100.5,
            billed_duration_ms: 200,
            memory_size_mb: 128,
            max_memory_used_mb: 64,
            init_duration_ms: None,
            restore_duration_ms: None,
        },
        tracing: None,
    }
}

fn bench_convert_report(c: &mut Criterion) {
    let converter = MetricsConverter::with_defaults();
    let record = make_report_record("test-request-id");
    let time = "2022-10-12T00:00:00.000Z";

    c.bench_function("MetricsConverter::convert_report", |b| {
        b.iter(|| converter.convert_report(black_box(&record), black_box(time)))
    });
}

fn bench_create_invocation_span(c: &mut Criterion) {
    let converter = SpanConverter::with_defaults();
    let start = make_start_record("test-request-id");
    let done = make_runtime_done_record("test-request-id");
    let start_time = "2022-10-12T00:00:00.000Z";
    let done_time = "2022-10-12T00:00:01.000Z";

    c.bench_function("SpanConverter::create_invocation_span", |b| {
        b.iter(|| {
            converter.create_invocation_span(
                black_box(&start),
                black_box(start_time),
                black_box(&done),
                black_box(done_time),
            )
        })
    });
}

fn bench_telemetry_processor(c: &mut Criterion) {
    let events = vec![
        TelemetryEvent::Start {
            time: "2022-10-12T00:00:00.000Z".to_string(),
            record: make_start_record("request-1"),
        },
        TelemetryEvent::RuntimeDone {
            time: "2022-10-12T00:00:01.000Z".to_string(),
            record: make_runtime_done_record("request-1"),
        },
        TelemetryEvent::Report {
            time: "2022-10-12T00:00:01.100Z".to_string(),
            record: make_report_record("request-1"),
        },
    ];

    c.bench_function("TelemetryProcessor::process_events", |b| {
        b.iter_batched(
            || (TelemetryProcessor::new(Resource::default()), events.clone()),
            |(mut processor, events)| processor.process_events(black_box(events)),
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_converter_with_resource(c: &mut Criterion) {
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};

    let resource = Resource {
        attributes: vec![
            KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("test-function".to_string())),
                }),
            },
            KeyValue {
                key: "faas.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("my-lambda".to_string())),
                }),
            },
        ],
        ..Default::default()
    };

    let converter = MetricsConverter::new(resource);
    let record = make_report_record("test-request-id");
    let time = "2022-10-12T00:00:00.000Z";

    c.bench_function("MetricsConverter::convert_report (with resource)", |b| {
        b.iter(|| converter.convert_report(black_box(&record), black_box(time)))
    });
}

criterion_group!(
    benches,
    bench_convert_report,
    bench_create_invocation_span,
    bench_telemetry_processor,
    bench_converter_with_resource,
);
criterion_main!(benches);

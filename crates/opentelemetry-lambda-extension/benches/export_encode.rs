//! Benchmarks for the export encode path: protobuf encoding and gzip
//! compression of trace batches at representative payload sizes.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flate2::Compression;
use flate2::write::GzEncoder;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;
use std::hint::black_box;
use std::io::Write;

fn make_span(index: usize) -> Span {
    Span {
        name: format!("benchmark-span-{index}"),
        trace_id: vec![1; 16],
        span_id: index.to_le_bytes().to_vec(),
        start_time_unix_nano: 1_700_000_000_000_000_000,
        end_time_unix_nano: 1_700_000_000_050_000_000,
        attributes: (0..8)
            .map(|attr| KeyValue {
                key: format!("attribute.key.{attr}"),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(format!(
                        "value-{index}-{attr}-with-a-realistic-payload"
                    ))),
                }),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// Builds a trace request whose encoded size is close to `target_bytes`.
fn make_request_of_size(target_bytes: usize) -> ExportTraceServiceRequest {
    let span_size = make_span(0).encoded_len();
    let spans = (target_bytes / span_size).max(1);
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans: (0..spans).map(make_span).collect(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn bench_encode_and_gzip(c: &mut Criterion) {
    let sizes = [
        ("100KiB", 100 * 1024),
        ("1MiB", 1024 * 1024),
        ("4MiB", 4 * 1024 * 1024),
    ];

    let mut encode_group = c.benchmark_group("export_encode");
    for (label, target) in sizes {
        let request = make_request_of_size(target);
        encode_group.throughput(Throughput::Bytes(request.encoded_len() as u64));
        encode_group.bench_with_input(BenchmarkId::new("protobuf", label), &request, |b, req| {
            b.iter(|| black_box(req.encode_to_vec()))
        });
    }
    encode_group.finish();

    let mut gzip_group = c.benchmark_group("export_gzip");
    for (label, target) in sizes {
        let encoded = make_request_of_size(target).encode_to_vec();
        gzip_group.throughput(Throughput::Bytes(encoded.len() as u64));
        gzip_group.bench_with_input(BenchmarkId::new("gzip", label), &encoded, |b, bytes| {
            b.iter(|| black_box(gzip(bytes)))
        });
    }
    gzip_group.finish();
}

criterion_group!(benches, bench_encode_and_gzip);
criterion_main!(benches);

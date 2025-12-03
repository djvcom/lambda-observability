//! Integration tests for the OTLP exporter using mock-collector.

use mock_collector::{MockServer, Protocol as MockProtocol};
use opentelemetry_lambda_extension::{
    BatchedSignal, Compression, ExportResult, ExporterConfig, OtlpExporter, Protocol,
};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use std::time::Duration;

fn make_trace_batch(span_name: &str) -> BatchedSignal {
    BatchedSignal::Traces(ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    name: span_name.to_string(),
                    trace_id: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
                    span_id: vec![1, 2, 3, 4, 5, 6, 7, 8],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    })
}

#[tokio::test]
async fn test_export_traces_to_mock_collector() {
    let server = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let endpoint = format!("http://{}", server.addr());

    let config = ExporterConfig {
        endpoint: Some(endpoint),
        protocol: Protocol::Http,
        timeout: Duration::from_secs(5),
        compression: Compression::None,
        ..Default::default()
    };

    let exporter = OtlpExporter::new(config).expect("Failed to create exporter");
    let batch = make_trace_batch("test-export-span");

    let result = exporter.export(batch).await;
    assert_eq!(result, ExportResult::Success);

    server
        .with_collector(|collector| {
            collector
                .expect_span_with_name("test-export-span")
                .assert_exists();
        })
        .await;

    server.shutdown().await.expect("Failed to shutdown server");
}

#[tokio::test]
async fn test_export_multiple_batches() {
    let server = MockServer::builder()
        .protocol(MockProtocol::HttpBinary)
        .start()
        .await
        .expect("Failed to start mock collector");

    let endpoint = format!("http://{}", server.addr());

    let config = ExporterConfig {
        endpoint: Some(endpoint),
        protocol: Protocol::Http,
        timeout: Duration::from_secs(5),
        compression: Compression::None,
        ..Default::default()
    };

    let exporter = OtlpExporter::new(config).expect("Failed to create exporter");

    for i in 0..3 {
        let batch = make_trace_batch(&format!("batch-{}-span", i));
        let result = exporter.export(batch).await;
        assert_eq!(result, ExportResult::Success);
    }

    server
        .with_collector(|collector| {
            collector.expect_span().assert_count(3);
            collector
                .expect_span_with_name("batch-0-span")
                .assert_exists();
            collector
                .expect_span_with_name("batch-1-span")
                .assert_exists();
            collector
                .expect_span_with_name("batch-2-span")
                .assert_exists();
        })
        .await;

    server.shutdown().await.expect("Failed to shutdown server");
}

#[tokio::test]
async fn test_export_with_json_protocol() {
    let server = MockServer::builder()
        .protocol(MockProtocol::HttpJson)
        .start()
        .await
        .expect("Failed to start mock collector");

    let endpoint = format!("http://{}", server.addr());

    let config = ExporterConfig {
        endpoint: Some(endpoint),
        protocol: Protocol::Http,
        timeout: Duration::from_secs(5),
        compression: Compression::None,
        ..Default::default()
    };

    let exporter = OtlpExporter::new(config).expect("Failed to create exporter");
    let batch = make_trace_batch("json-span");

    let _result = exporter.export(batch).await;

    server
        .with_collector(|collector| {
            let count = collector.span_count();
            if count > 0 {
                collector.expect_span_with_name("json-span").assert_exists();
            }
        })
        .await;

    server.shutdown().await.expect("Failed to shutdown server");
}

#[tokio::test]
async fn test_export_no_endpoint_skips() {
    let exporter = OtlpExporter::with_defaults().expect("Failed to create exporter");
    let batch = make_trace_batch("skipped-span");

    let result = exporter.export(batch).await;
    assert_eq!(result, ExportResult::Skipped);
}

#[tokio::test]
async fn test_export_to_unreachable_endpoint_falls_back() {
    let config = ExporterConfig {
        endpoint: Some("http://127.0.0.1:19999".to_string()),
        protocol: Protocol::Http,
        timeout: Duration::from_millis(100),
        ..Default::default()
    };

    let exporter = OtlpExporter::new(config).expect("Failed to create exporter");
    let batch = make_trace_batch("fallback-span");

    let result = exporter.export(batch).await;
    assert_eq!(result, ExportResult::Fallback);
}

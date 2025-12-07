//! Integration tests for the OTLP receiver.

mod common;

use common::wait_for_http_ready;
use opentelemetry_lambda_extension::{OtlpReceiver, ReceiverConfig, Signal};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_receiver_accepts_protobuf_traces() {
    let (signal_tx, mut signal_rx) = mpsc::channel(100);
    let cancel_token = CancellationToken::new();

    let config = ReceiverConfig {
        http_port: 14318,
        http_enabled: true,
        grpc_enabled: false,
        grpc_port: 14317,
    };

    let receiver = OtlpReceiver::new(config, signal_tx, cancel_token.clone());
    let (_handle, future) = receiver.start().await.expect("Failed to start receiver");
    let server_task = tokio::spawn(future);

    wait_for_http_ready(14318, Duration::from_secs(5))
        .await
        .expect("Receiver failed to start");

    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    name: "test-span".to_string(),
                    trace_id: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
                    span_id: vec![1, 2, 3, 4, 5, 6, 7, 8],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };

    let client = reqwest::Client::new();
    let response = client
        .post("http://127.0.0.1:14318/v1/traces")
        .header("Content-Type", "application/x-protobuf")
        .body(request.encode_to_vec())
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("Timeout waiting for signal")
        .expect("Channel closed");

    match signal {
        Signal::Traces(traces) => {
            assert_eq!(traces.resource_spans.len(), 1);
            assert_eq!(
                traces.resource_spans[0].scope_spans[0].spans[0].name,
                "test-span"
            );
        }
        _ => panic!("Expected Traces signal"),
    }

    cancel_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

#[tokio::test]
async fn test_receiver_accepts_json_traces() {
    let (signal_tx, mut signal_rx) = mpsc::channel(100);
    let cancel_token = CancellationToken::new();

    let config = ReceiverConfig {
        http_port: 14319,
        http_enabled: true,
        grpc_enabled: false,
        grpc_port: 14317,
    };

    let receiver = OtlpReceiver::new(config, signal_tx, cancel_token.clone());
    let (_handle, future) = receiver.start().await.expect("Failed to start receiver");
    let server_task = tokio::spawn(future);

    wait_for_http_ready(14319, Duration::from_secs(5))
        .await
        .expect("Receiver failed to start");

    let json = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"name":"json-span","traceId":"0102030405060708090a0b0c0d0e0f10","spanId":"0102030405060708"}]}]}]}"#;

    let client = reqwest::Client::new();
    let response = client
        .post("http://127.0.0.1:14319/v1/traces")
        .header("Content-Type", "application/json")
        .body(json)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("Timeout waiting for signal")
        .expect("Channel closed");

    match signal {
        Signal::Traces(traces) => {
            assert_eq!(traces.resource_spans.len(), 1);
            assert_eq!(
                traces.resource_spans[0].scope_spans[0].spans[0].name,
                "json-span"
            );
        }
        _ => panic!("Expected Traces signal"),
    }

    cancel_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

#[tokio::test]
async fn test_receiver_backpressure() {
    let (signal_tx, _signal_rx) = mpsc::channel(1);
    let cancel_token = CancellationToken::new();

    let config = ReceiverConfig {
        http_port: 14320,
        http_enabled: true,
        grpc_enabled: false,
        grpc_port: 14317,
    };

    let receiver = OtlpReceiver::new(config, signal_tx, cancel_token.clone());
    let (_handle, future) = receiver.start().await.expect("Failed to start receiver");
    let server_task = tokio::spawn(future);

    wait_for_http_ready(14320, Duration::from_secs(5))
        .await
        .expect("Receiver failed to start");

    let request = ExportTraceServiceRequest::default();
    let client = reqwest::Client::new();

    client
        .post("http://127.0.0.1:14320/v1/traces")
        .header("Content-Type", "application/x-protobuf")
        .body(request.encode_to_vec())
        .send()
        .await
        .expect("Failed to send first request");

    let response = client
        .post("http://127.0.0.1:14320/v1/traces")
        .header("Content-Type", "application/x-protobuf")
        .body(request.encode_to_vec())
        .send()
        .await
        .expect("Failed to send second request");

    // When queue is full, return 503 to signal backpressure to clients
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

    cancel_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

#[tokio::test]
async fn test_receiver_invalid_protobuf() {
    let (signal_tx, _signal_rx) = mpsc::channel(100);
    let cancel_token = CancellationToken::new();

    let config = ReceiverConfig {
        http_port: 14321,
        http_enabled: true,
        grpc_enabled: false,
        grpc_port: 14317,
    };

    let receiver = OtlpReceiver::new(config, signal_tx, cancel_token.clone());
    let (_handle, future) = receiver.start().await.expect("Failed to start receiver");
    let server_task = tokio::spawn(future);

    wait_for_http_ready(14321, Duration::from_secs(5))
        .await
        .expect("Receiver failed to start");

    let client = reqwest::Client::new();
    let response = client
        .post("http://127.0.0.1:14321/v1/traces")
        .header("Content-Type", "application/x-protobuf")
        .body("invalid protobuf data")
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    cancel_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

#[tokio::test]
async fn test_receiver_handles_metrics() {
    let (signal_tx, mut signal_rx) = mpsc::channel(100);
    let cancel_token = CancellationToken::new();

    let config = ReceiverConfig {
        http_port: 14322,
        http_enabled: true,
        grpc_enabled: false,
        grpc_port: 14317,
    };

    let receiver = OtlpReceiver::new(config, signal_tx, cancel_token.clone());
    let (_handle, future) = receiver.start().await.expect("Failed to start receiver");
    let server_task = tokio::spawn(future);

    wait_for_http_ready(14322, Duration::from_secs(5))
        .await
        .expect("Receiver failed to start");

    let json = r#"{"resourceMetrics":[]}"#;

    let client = reqwest::Client::new();
    let response = client
        .post("http://127.0.0.1:14322/v1/metrics")
        .header("Content-Type", "application/json")
        .body(json)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("Timeout waiting for signal")
        .expect("Channel closed");

    assert!(matches!(signal, Signal::Logs(_) | Signal::Metrics(_)));

    cancel_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

#[tokio::test]
async fn test_receiver_handles_logs() {
    let (signal_tx, mut signal_rx) = mpsc::channel(100);
    let cancel_token = CancellationToken::new();

    let config = ReceiverConfig {
        http_port: 14323,
        http_enabled: true,
        grpc_enabled: false,
        grpc_port: 14317,
    };

    let receiver = OtlpReceiver::new(config, signal_tx, cancel_token.clone());
    let (_handle, future) = receiver.start().await.expect("Failed to start receiver");
    let server_task = tokio::spawn(future);

    wait_for_http_ready(14323, Duration::from_secs(5))
        .await
        .expect("Receiver failed to start");

    let json = r#"{"resourceLogs":[]}"#;

    let client = reqwest::Client::new();
    let response = client
        .post("http://127.0.0.1:14323/v1/logs")
        .header("Content-Type", "application/json")
        .body(json)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("Timeout waiting for signal")
        .expect("Channel closed");

    assert!(matches!(signal, Signal::Logs(_)));

    cancel_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

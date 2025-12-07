//! Platform event to OTLP signal conversion.
//!
//! This module converts Lambda Telemetry API platform events into OpenTelemetry
//! signals (metrics and spans) following semantic conventions.

use crate::resource::semconv;
use crate::telemetry::{ReportRecord, RuntimeDoneRecord, StartRecord, TelemetryEvent};
use crate::tracing::XRayTraceHeader;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric::Data,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status, status};
use opentelemetry_semantic_conventions::SCHEMA_URL;
use std::time::{SystemTime, UNIX_EPOCH};

/// Scope name for instrumentation.
const SCOPE_NAME: &str = "lambda-otel-extension";
/// Scope version for instrumentation.
const SCOPE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Converts platform events to OTLP metrics.
pub struct MetricsConverter {
    resource: Resource,
}

impl MetricsConverter {
    /// Creates a new metrics converter with the given resource attributes.
    pub fn new(resource: Resource) -> Self {
        Self { resource }
    }

    /// Creates a converter with default resource attributes.
    pub fn with_defaults() -> Self {
        Self::new(Resource::default())
    }

    /// Sets the resource for this converter.
    pub fn set_resource(&mut self, resource: Resource) {
        self.resource = resource.clone();
    }

    /// Converts a report event to OTLP metrics.
    ///
    /// Generates the following metrics:
    /// - `faas.invocation.duration` - Duration of the invocation in milliseconds
    /// - `aws.lambda.billed_duration` - Billed duration in milliseconds
    /// - `aws.lambda.max_memory_used` - Maximum memory used in bytes
    /// - `aws.lambda.init_duration` (cold start only) - Init duration in milliseconds
    pub fn convert_report(&self, record: &ReportRecord, time: &str) -> ExportMetricsServiceRequest {
        let timestamp_nanos = parse_iso8601_to_nanos(time).unwrap_or_else(current_time_nanos);

        let mut metrics = vec![
            self.create_gauge_metric(
                "faas.invocation.duration",
                "Duration of the function invocation",
                "ms",
                record.metrics.duration_ms,
                timestamp_nanos,
                vec![kv_string(semconv::FAAS_INVOCATION_ID, &record.request_id)],
            ),
            self.create_gauge_metric(
                "aws.lambda.billed_duration",
                "Billed duration of the invocation",
                "ms",
                record.metrics.billed_duration_ms as f64,
                timestamp_nanos,
                vec![kv_string(semconv::FAAS_INVOCATION_ID, &record.request_id)],
            ),
            self.create_gauge_metric(
                "aws.lambda.max_memory_used",
                "Maximum memory used during invocation",
                "By",
                (record.metrics.max_memory_used_mb * 1024 * 1024) as f64,
                timestamp_nanos,
                vec![kv_string(semconv::FAAS_INVOCATION_ID, &record.request_id)],
            ),
        ];

        // Add init_duration for cold starts
        if let Some(init_duration) = record.metrics.init_duration_ms {
            metrics.push(self.create_gauge_metric(
                "aws.lambda.init_duration",
                "Cold start initialization duration",
                "ms",
                init_duration,
                timestamp_nanos,
                vec![kv_string(semconv::FAAS_INVOCATION_ID, &record.request_id)],
            ));
        }

        // Add restore_duration for SnapStart
        if let Some(restore_duration) = record.metrics.restore_duration_ms {
            metrics.push(self.create_gauge_metric(
                "aws.lambda.restore_duration",
                "SnapStart restore duration",
                "ms",
                restore_duration,
                timestamp_nanos,
                vec![kv_string(semconv::FAAS_INVOCATION_ID, &record.request_id)],
            ));
        }

        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(self.resource.clone()),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(
                        opentelemetry_proto::tonic::common::v1::InstrumentationScope {
                            name: SCOPE_NAME.to_string(),
                            version: SCOPE_VERSION.to_string(),
                            ..Default::default()
                        },
                    ),
                    metrics,
                    schema_url: SCHEMA_URL.to_string(),
                }],
                schema_url: SCHEMA_URL.to_string(),
            }],
        }
    }

    /// Creates a shutdown count metric.
    ///
    /// This metric is emitted when the extension receives a SHUTDOWN event,
    /// indicating the Lambda environment is being terminated. The metric
    /// includes the `faas.name` resource attribute to identify which function
    /// is shutting down.
    ///
    /// # Arguments
    ///
    /// * `shutdown_reason` - The reason for shutdown (e.g., "spindown", "timeout", "failure")
    pub fn create_shutdown_metric(&self, shutdown_reason: &str) -> ExportMetricsServiceRequest {
        let timestamp_nanos = current_time_nanos();

        let metric = Metric {
            name: "extension.shutdown_count".to_string(),
            description: "Count of extension shutdown events".to_string(),
            unit: "{count}".to_string(),
            data: Some(Data::Gauge(Gauge {
                data_points: vec![NumberDataPoint {
                    attributes: vec![kv_string("shutdown.reason", shutdown_reason)],
                    start_time_unix_nano: timestamp_nanos,
                    time_unix_nano: timestamp_nanos,
                    exemplars: vec![],
                    flags: 0,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(1),
                    ),
                }],
            })),
            metadata: vec![],
        };

        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(self.resource.clone()),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(
                        opentelemetry_proto::tonic::common::v1::InstrumentationScope {
                            name: SCOPE_NAME.to_string(),
                            version: SCOPE_VERSION.to_string(),
                            ..Default::default()
                        },
                    ),
                    metrics: vec![metric],
                    schema_url: SCHEMA_URL.to_string(),
                }],
                schema_url: SCHEMA_URL.to_string(),
            }],
        }
    }

    fn create_gauge_metric(
        &self,
        name: &str,
        description: &str,
        unit: &str,
        value: f64,
        timestamp_nanos: u64,
        attributes: Vec<KeyValue>,
    ) -> Metric {
        Metric {
            name: name.to_string(),
            description: description.to_string(),
            unit: unit.to_string(),
            data: Some(Data::Gauge(Gauge {
                data_points: vec![NumberDataPoint {
                    attributes,
                    start_time_unix_nano: timestamp_nanos,
                    time_unix_nano: timestamp_nanos,
                    exemplars: vec![],
                    flags: 0,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsDouble(
                            value,
                        ),
                    ),
                }],
            })),
            metadata: vec![],
        }
    }
}

/// Converts platform events to OTLP spans.
pub struct SpanConverter {
    resource: Resource,
}

impl SpanConverter {
    /// Creates a new span converter with the given resource attributes.
    pub fn new(resource: Resource) -> Self {
        Self { resource }
    }

    /// Creates a converter with default resource attributes.
    pub fn with_defaults() -> Self {
        Self::new(Resource::default())
    }

    /// Sets the resource for this converter.
    pub fn set_resource(&mut self, resource: Resource) {
        self.resource = resource.clone();
    }

    /// Creates an invocation span from start and runtime_done events.
    ///
    /// # Arguments
    ///
    /// * `start` - The platform.start event
    /// * `start_time` - ISO 8601 timestamp of start event
    /// * `runtime_done` - The platform.runtimeDone event
    /// * `done_time` - ISO 8601 timestamp of runtimeDone event
    pub fn create_invocation_span(
        &self,
        start: &StartRecord,
        start_time: &str,
        runtime_done: &RuntimeDoneRecord,
        done_time: &str,
    ) -> ExportTraceServiceRequest {
        let start_nanos = parse_iso8601_to_nanos(start_time).unwrap_or_else(current_time_nanos);
        let end_nanos = parse_iso8601_to_nanos(done_time).unwrap_or_else(current_time_nanos);

        // Extract trace context from X-Ray header if available
        let (trace_id, parent_span_id) = extract_trace_context(start);

        let span = Span {
            trace_id: trace_id.unwrap_or_else(generate_trace_id),
            span_id: generate_span_id(),
            parent_span_id: parent_span_id.unwrap_or_default(),
            name: "lambda.invoke".to_string(),
            kind: opentelemetry_proto::tonic::trace::v1::span::SpanKind::Server as i32,
            start_time_unix_nano: start_nanos,
            end_time_unix_nano: end_nanos,
            attributes: vec![
                kv_string(semconv::FAAS_INVOCATION_ID, &start.request_id),
                kv_string("faas.invocation.status", &runtime_done.status),
            ],
            dropped_attributes_count: 0,
            events: vec![],
            dropped_events_count: 0,
            links: vec![],
            dropped_links_count: 0,
            status: Some(Status {
                code: if runtime_done.status == "success" {
                    status::StatusCode::Unset as i32
                } else {
                    status::StatusCode::Error as i32
                },
                message: if runtime_done.status != "success" {
                    format!("Lambda invocation {}", runtime_done.status)
                } else {
                    String::new()
                },
            }),
            flags: 0,
            trace_state: String::new(),
        };

        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(self.resource.clone()),
                scope_spans: vec![ScopeSpans {
                    scope: Some(
                        opentelemetry_proto::tonic::common::v1::InstrumentationScope {
                            name: SCOPE_NAME.to_string(),
                            version: SCOPE_VERSION.to_string(),
                            ..Default::default()
                        },
                    ),
                    spans: vec![span],
                    schema_url: SCHEMA_URL.to_string(),
                }],
                schema_url: SCHEMA_URL.to_string(),
            }],
        }
    }
}

/// Extracts trace context from a start record's tracing info.
fn extract_trace_context(start: &StartRecord) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let Some(ref tracing) = start.tracing else {
        return (None, None);
    };

    let Some(ref value) = tracing.value else {
        return (None, None);
    };

    let Some(xray) = XRayTraceHeader::parse(value) else {
        return (None, None);
    };

    let Some(w3c) = xray.to_w3c() else {
        return (None, None);
    };

    let trace_id = w3c.trace_id_bytes().map(|b| b.to_vec());
    let span_id = w3c.span_id_bytes().map(|b| b.to_vec());

    (trace_id, span_id)
}

/// Generates a random trace ID per OpenTelemetry specification.
fn generate_trace_id() -> Vec<u8> {
    rand::random::<[u8; 16]>().to_vec()
}

/// Generates a random span ID per OpenTelemetry specification.
fn generate_span_id() -> Vec<u8> {
    rand::random::<[u8; 8]>().to_vec()
}

/// Creates a string key-value pair.
fn kv_string(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

/// Parses an ISO 8601 timestamp to nanoseconds since Unix epoch.
fn parse_iso8601_to_nanos(timestamp: &str) -> Option<u64> {
    // Simple ISO 8601 parsing (2022-10-12T00:00:00.000Z)
    let ts = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(ts.timestamp_nanos_opt()? as u64)
}

/// Returns the current time in nanoseconds since Unix epoch.
fn current_time_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Batch processor for telemetry events.
///
/// Collects related events (start + runtimeDone + report) and converts them
/// to OTLP signals when complete.
pub struct TelemetryProcessor {
    metrics_converter: MetricsConverter,
    span_converter: SpanConverter,
    pending_starts: std::collections::HashMap<String, (StartRecord, String)>,
}

impl TelemetryProcessor {
    /// Creates a new telemetry processor.
    pub fn new(resource: Resource) -> Self {
        Self {
            metrics_converter: MetricsConverter::new(resource.clone()),
            span_converter: SpanConverter::new(resource),
            pending_starts: std::collections::HashMap::new(),
        }
    }

    /// Creates a processor with default resource.
    pub fn with_defaults() -> Self {
        Self::new(Resource::default())
    }

    /// Sets the resource for this processor.
    pub fn set_resource(&mut self, resource: Resource) {
        self.metrics_converter.set_resource(resource.clone());
        self.span_converter.set_resource(resource);
    }

    /// Processes a batch of telemetry events.
    ///
    /// Returns generated OTLP signals (metrics and traces).
    pub fn process_events(
        &mut self,
        events: Vec<TelemetryEvent>,
    ) -> (
        Vec<ExportMetricsServiceRequest>,
        Vec<ExportTraceServiceRequest>,
    ) {
        let mut metrics = Vec::new();
        let mut traces = Vec::new();

        for event in events {
            match event {
                TelemetryEvent::Start { time, record } => {
                    self.pending_starts
                        .insert(record.request_id.clone(), (record, time));
                }
                TelemetryEvent::RuntimeDone { time, record } => {
                    if let Some((start_record, start_time)) =
                        self.pending_starts.remove(&record.request_id)
                    {
                        let trace = self.span_converter.create_invocation_span(
                            &start_record,
                            &start_time,
                            &record,
                            &time,
                        );
                        traces.push(trace);
                    }
                }
                TelemetryEvent::Report { time, record } => {
                    let metric = self.metrics_converter.convert_report(&record, &time);
                    metrics.push(metric);
                }
                _ => {
                    // Other events (init, fault, logs) are logged but not converted
                    tracing::trace!(?event, "Received non-invocation telemetry event");
                }
            }
        }

        (metrics, traces)
    }

    /// Clears any pending start events.
    ///
    /// Call this during shutdown to avoid memory leaks.
    pub fn clear_pending(&mut self) {
        self.pending_starts.clear();
    }

    /// Returns the number of pending start events.
    pub fn pending_count(&self) -> usize {
        self.pending_starts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{ReportMetrics, TracingRecord};

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

    #[test]
    fn test_convert_report_to_metrics() {
        let converter = MetricsConverter::with_defaults();
        let record = make_report_record("test-request-id");
        let time = "2022-10-12T00:00:00.000Z";

        let request = converter.convert_report(&record, time);

        assert_eq!(request.resource_metrics.len(), 1);
        let scope_metrics = &request.resource_metrics[0].scope_metrics;
        assert_eq!(scope_metrics.len(), 1);

        let metrics = &scope_metrics[0].metrics;
        assert_eq!(metrics.len(), 3); // duration, billed_duration, max_memory_used

        // Check metric names
        let names: Vec<_> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"faas.invocation.duration"));
        assert!(names.contains(&"aws.lambda.billed_duration"));
        assert!(names.contains(&"aws.lambda.max_memory_used"));
    }

    #[test]
    fn test_convert_report_with_init_duration() {
        let converter = MetricsConverter::with_defaults();
        let mut record = make_report_record("test-request-id");
        record.metrics.init_duration_ms = Some(500.0);

        let request = converter.convert_report(&record, "2022-10-12T00:00:00.000Z");

        let metrics = &request.resource_metrics[0].scope_metrics[0].metrics;
        assert_eq!(metrics.len(), 4); // includes init_duration

        let names: Vec<_> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"aws.lambda.init_duration"));
    }

    #[test]
    fn test_create_invocation_span() {
        let converter = SpanConverter::with_defaults();
        let start = make_start_record("test-request-id");
        let done = make_runtime_done_record("test-request-id");

        let request = converter.create_invocation_span(
            &start,
            "2022-10-12T00:00:00.000Z",
            &done,
            "2022-10-12T00:00:01.000Z",
        );

        assert_eq!(request.resource_spans.len(), 1);
        let spans = &request.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.name, "lambda.invoke");
        assert!(span.end_time_unix_nano > span.start_time_unix_nano);

        // Verify trace ID is valid (16 bytes, not all zeros)
        assert_eq!(span.trace_id.len(), 16);
        assert_ne!(span.trace_id, vec![0u8; 16]);

        // Verify span ID is valid (8 bytes)
        assert_eq!(span.span_id.len(), 8);
    }

    #[test]
    fn test_create_invocation_span_with_xray() {
        let converter = SpanConverter::with_defaults();
        let start = StartRecord {
            request_id: "test-request-id".to_string(),
            version: Some("$LATEST".to_string()),
            tracing: Some(TracingRecord {
                trace_type: Some("X-Amzn-Trace-Id".to_string()),
                value: Some(
                    "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
                        .to_string(),
                ),
                span_id: None,
            }),
        };
        let done = make_runtime_done_record("test-request-id");

        let request = converter.create_invocation_span(
            &start,
            "2022-10-12T00:00:00.000Z",
            &done,
            "2022-10-12T00:00:01.000Z",
        );

        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        // Verify trace ID was extracted from X-Ray header
        assert_eq!(span.trace_id.len(), 16);
        assert_ne!(span.trace_id, vec![0u8; 16]); // Not all zeros

        // Verify parent span ID was extracted
        assert_eq!(span.parent_span_id.len(), 8);
    }

    #[test]
    fn test_processor_collects_events() {
        let mut processor = TelemetryProcessor::with_defaults();

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

        let (metrics, traces) = processor.process_events(events);

        assert_eq!(metrics.len(), 1);
        assert_eq!(traces.len(), 1);
        assert_eq!(processor.pending_count(), 0);
    }

    #[test]
    fn test_processor_handles_out_of_order() {
        let mut processor = TelemetryProcessor::with_defaults();

        // Send start first
        let events1 = vec![TelemetryEvent::Start {
            time: "2022-10-12T00:00:00.000Z".to_string(),
            record: make_start_record("request-1"),
        }];

        let (metrics, traces) = processor.process_events(events1);
        assert_eq!(metrics.len(), 0);
        assert_eq!(traces.len(), 0);
        assert_eq!(processor.pending_count(), 1);

        // Send runtime_done
        let events2 = vec![TelemetryEvent::RuntimeDone {
            time: "2022-10-12T00:00:01.000Z".to_string(),
            record: make_runtime_done_record("request-1"),
        }];

        let (metrics, traces) = processor.process_events(events2);
        assert_eq!(metrics.len(), 0);
        assert_eq!(traces.len(), 1);
        assert_eq!(processor.pending_count(), 0);
    }

    #[test]
    fn test_parse_iso8601() {
        let ts = parse_iso8601_to_nanos("2022-10-12T00:00:00.000Z");
        assert!(ts.is_some());

        let invalid = parse_iso8601_to_nanos("invalid");
        assert!(invalid.is_none());
    }

    #[test]
    fn test_kv_string() {
        let kv = kv_string("key", "value");
        assert_eq!(kv.key, "key");

        match kv.value.unwrap().value.unwrap() {
            any_value::Value::StringValue(s) => assert_eq!(s, "value"),
            _ => panic!("Expected string value"),
        }
    }

    #[test]
    fn test_create_shutdown_metric() {
        use opentelemetry_proto::tonic::metrics::v1::number_data_point::Value;

        let converter = MetricsConverter::with_defaults();
        let request = converter.create_shutdown_metric("spindown");

        assert_eq!(request.resource_metrics.len(), 1);
        let scope_metrics = &request.resource_metrics[0].scope_metrics;
        assert_eq!(scope_metrics.len(), 1);

        let metrics = &scope_metrics[0].metrics;
        assert_eq!(metrics.len(), 1);

        let metric = &metrics[0];
        assert_eq!(metric.name, "extension.shutdown_count");
        assert_eq!(metric.unit, "{count}");

        // Check the metric has a value of 1
        if let Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(gauge)) =
            &metric.data
        {
            assert_eq!(gauge.data_points.len(), 1);
            let data_point = &gauge.data_points[0];

            // Check value is 1
            match data_point.value {
                Some(Value::AsInt(val)) => assert_eq!(val, 1),
                _ => panic!("Expected integer value of 1"),
            }

            // Check shutdown reason attribute
            let attrs: std::collections::HashMap<_, _> = data_point
                .attributes
                .iter()
                .map(|kv| (kv.key.as_str(), kv.value.as_ref()))
                .collect();
            assert!(attrs.contains_key("shutdown.reason"));
        } else {
            panic!("Expected Gauge metric");
        }
    }

    #[test]
    fn test_shutdown_metric_different_reasons() {
        let converter = MetricsConverter::with_defaults();

        for reason in &["spindown", "timeout", "failure"] {
            let request = converter.create_shutdown_metric(reason);
            let metric = &request.resource_metrics[0].scope_metrics[0].metrics[0];

            if let Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(gauge)) =
                &metric.data
            {
                let attr = &gauge.data_points[0].attributes[0];
                assert_eq!(attr.key, "shutdown.reason");

                if let Some(any_value::Value::StringValue(val)) =
                    attr.value.as_ref().and_then(|v| v.value.as_ref())
                {
                    assert_eq!(val, *reason);
                }
            }
        }
    }
}

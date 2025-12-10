//! SNS event extractor for notification triggers.
//!
//! Extracts trace context from SNS message attributes, checking:
//! 1. `message_attributes` for W3C `traceparent` (injected by OTel-instrumented producers)
//! 2. `message_attributes` for `AWSTraceHeader` in X-Ray format

use crate::extractor::TraceContextExtractor;
use aws_lambda_events::sns::{MessageAttribute, SnsEvent, SnsRecord};
use lambda_runtime::Context as LambdaContext;
use opentelemetry::Context;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::{
    Link, SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
};
use opentelemetry_semantic_conventions::attribute::{
    MESSAGING_BATCH_MESSAGE_COUNT, MESSAGING_DESTINATION_NAME, MESSAGING_MESSAGE_ID,
    MESSAGING_OPERATION_TYPE, MESSAGING_SYSTEM,
};
use std::collections::HashMap;
use tracing::Span;

/// Extractor for SNS notification events.
///
/// SNS events may carry trace context in two locations within `message_attributes`:
/// 1. W3C `traceparent`/`tracestate` - injected by OTel-instrumented producers
/// 2. `AWSTraceHeader` - X-Ray format set by AWS
///
/// This extractor checks both, preferring W3C format when available.
///
/// Per OpenTelemetry semantic conventions for messaging systems, this extractor:
/// - Does NOT set a parent context (returns current context)
/// - Creates span links for each message's trace context
///
/// This approach is appropriate because the async nature of message queues
/// means span links are more semantically correct than parent-child relationships.
///
/// # Example
///
/// ```ignore
/// use opentelemetry_lambda_tower::{OtelTracingLayer, SnsEventExtractor};
///
/// let layer = OtelTracingLayer::new(SnsEventExtractor::new());
/// ```
#[derive(Clone, Debug, Default)]
pub struct SnsEventExtractor;

impl SnsEventExtractor {
    /// Creates a new SNS event extractor.
    pub fn new() -> Self {
        Self
    }

    /// Extracts the topic name from an SNS topic ARN.
    ///
    /// ARN format: `arn:aws:sns:{region}:{account}:{topic-name}`
    fn topic_name_from_arn(arn: &str) -> Option<&str> {
        arn.rsplit(':').next()
    }
}

impl TraceContextExtractor<SnsEvent> for SnsEventExtractor {
    fn extract_context(&self, _event: &SnsEvent) -> Context {
        Context::current()
    }

    fn extract_links(&self, event: &SnsEvent) -> Vec<Link> {
        event
            .records
            .iter()
            .filter_map(extract_link_from_record)
            .collect()
    }

    fn trigger_type(&self) -> &'static str {
        "pubsub"
    }

    fn span_name(&self, event: &SnsEvent, lambda_ctx: &LambdaContext) -> String {
        let topic_name = event
            .records
            .first()
            .map(|r| &r.sns.topic_arn)
            .and_then(|arn| Self::topic_name_from_arn(arn))
            .unwrap_or(&lambda_ctx.env_config.function_name);

        format!("{} process", topic_name)
    }

    fn record_attributes(&self, event: &SnsEvent, span: &Span) {
        span.record(MESSAGING_SYSTEM, "aws_sns");
        span.record(MESSAGING_OPERATION_TYPE, "process");

        if let Some(record) = event.records.first() {
            if let Some(topic_name) = Self::topic_name_from_arn(&record.sns.topic_arn) {
                span.record(MESSAGING_DESTINATION_NAME, topic_name);
            }

            span.record(MESSAGING_MESSAGE_ID, record.sns.message_id.as_str());
        }

        span.record(MESSAGING_BATCH_MESSAGE_COUNT, event.records.len() as i64);
    }
}

/// Extracts a span link from an SNS record's message attributes.
///
/// Uses the globally configured propagator to extract trace context, then
/// falls back to parsing `AWSTraceHeader` in X-Ray format as a Lambda-specific default.
fn extract_link_from_record(record: &SnsRecord) -> Option<Link> {
    if let Some(span_context) =
        extract_trace_context_from_message_attributes(&record.sns.message_attributes)
    {
        return Some(Link::new(span_context, vec![], 0));
    }

    if let Some(trace_attr) = record.sns.message_attributes.get("AWSTraceHeader")
        && !trace_attr.value.is_empty()
        && let Some(span_context) = parse_xray_trace_header(&trace_attr.value)
    {
        return Some(Link::new(span_context, vec![], 0));
    }

    None
}

/// Extracts trace context from SNS message attributes using the global propagator.
///
/// The propagator determines which headers to look for (e.g. `traceparent` for W3C,
/// `X-Amzn-Trace-Id` for X-Ray, `X-B3-*` for Zipkin, etc.).
fn extract_trace_context_from_message_attributes(
    message_attributes: &HashMap<String, MessageAttribute>,
) -> Option<SpanContext> {
    let extractor = SnsMessageAttributeExtractor(message_attributes);
    let ctx =
        opentelemetry::global::get_text_map_propagator(|propagator| propagator.extract(&extractor));

    let span_context = ctx.span().span_context().clone();
    if span_context.is_valid() {
        Some(span_context)
    } else {
        None
    }
}

/// Adapter to extract trace context from SNS message attributes.
struct SnsMessageAttributeExtractor<'a>(&'a HashMap<String, MessageAttribute>);

impl Extractor for SnsMessageAttributeExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|attr| attr.value.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Parses an X-Ray trace header into a SpanContext.
///
/// X-Ray format: `Root=1-{epoch}-{random};Parent={span-id};Sampled={0|1}`
fn parse_xray_trace_header(header: &str) -> Option<SpanContext> {
    let mut trace_id_str = None;
    let mut parent_id_str = None;
    let mut sampled = false;

    for part in header.split(';') {
        let part = part.trim();
        if let Some(root) = part.strip_prefix("Root=") {
            trace_id_str = convert_xray_trace_id(root);
        } else if let Some(parent) = part.strip_prefix("Parent=") {
            parent_id_str = Some(parent.to_string());
        } else if part == "Sampled=1" {
            sampled = true;
        }
    }

    let trace_id_hex = trace_id_str?;
    let parent_id_hex = parent_id_str?;

    let trace_id_bytes = hex_to_bytes::<16>(&trace_id_hex)?;
    let trace_id = TraceId::from_bytes(trace_id_bytes);

    let span_id_bytes = hex_to_bytes::<8>(&parent_id_hex)?;
    let span_id = SpanId::from_bytes(span_id_bytes);

    let flags = if sampled {
        TraceFlags::SAMPLED
    } else {
        TraceFlags::default()
    };

    Some(SpanContext::new(
        trace_id,
        span_id,
        flags,
        true,
        TraceState::default(),
    ))
}

/// Converts X-Ray trace ID format to 32-character hex string.
fn convert_xray_trace_id(xray_id: &str) -> Option<String> {
    let parts: Vec<&str> = xray_id.split('-').collect();
    if parts.len() == 3 && parts[0] == "1" {
        let combined = format!("{}{}", parts[1], parts[2]);
        if combined.len() == 32 {
            return Some(combined);
        }
    }
    None
}

/// Converts a hex string to a fixed-size byte array.
fn hex_to_bytes<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != N * 2 {
        return None;
    }

    let mut bytes = [0u8; N];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let high = hex_char_to_nibble(chunk[0])?;
        let low = hex_char_to_nibble(chunk[1])?;
        bytes[i] = (high << 4) | low;
    }
    Some(bytes)
}

/// Converts a single hex character to its 4-bit value.
fn hex_char_to_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lambda_events::sns::{MessageAttribute, SnsMessage};
    use chrono::Utc;
    use std::collections::HashMap;

    fn create_test_sns_event_with_trace() -> SnsEvent {
        let mut attrs = HashMap::new();
        let mut trace_attr = MessageAttribute::default();
        trace_attr.data_type = "String".to_string();
        trace_attr.value =
            "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
                .to_string();
        attrs.insert("AWSTraceHeader".to_string(), trace_attr);

        let mut sns_msg = SnsMessage::default();
        sns_msg.sns_message_type = "Notification".to_string();
        sns_msg.message_id = "msg-123".to_string();
        sns_msg.topic_arn = "arn:aws:sns:us-east-1:123456789:my-topic".to_string();
        sns_msg.timestamp = Utc::now();
        sns_msg.signature_version = "1".to_string();
        sns_msg.signature = "sig".to_string();
        sns_msg.signing_cert_url = "https://cert".to_string();
        sns_msg.unsubscribe_url = "https://unsub".to_string();
        sns_msg.message = r#"{"test": "data"}"#.to_string();
        sns_msg.message_attributes = attrs;

        let mut record = SnsRecord::default();
        record.event_source = "aws:sns".to_string();
        record.event_version = "1.0".to_string();
        record.event_subscription_arn =
            "arn:aws:sns:us-east-1:123456789:my-topic:sub-123".to_string();
        record.sns = sns_msg;

        let mut event = SnsEvent::default();
        event.records = vec![record];
        event
    }

    #[test]
    fn test_trigger_type() {
        let extractor = SnsEventExtractor::new();
        assert_eq!(extractor.trigger_type(), "pubsub");
    }

    #[test]
    fn test_topic_name_from_arn() {
        assert_eq!(
            SnsEventExtractor::topic_name_from_arn("arn:aws:sns:us-east-1:123456789:my-topic"),
            Some("my-topic")
        );
    }

    #[test]
    fn test_extract_links_with_trace_header() {
        let extractor = SnsEventExtractor::new();
        let event = create_test_sns_event_with_trace();

        let links = extractor.extract_links(&event);

        assert_eq!(links.len(), 1);
        let link = &links[0];
        assert!(link.span_context.is_valid());
        assert_eq!(
            link.span_context.trace_id().to_string(),
            "5759e988bd862e3fe1be46a994272793"
        );
        assert_eq!(link.span_context.span_id().to_string(), "53995c3f42cd8ad8");
        assert!(link.span_context.is_sampled());
    }

    #[test]
    fn test_extract_links_no_trace_header() {
        let extractor = SnsEventExtractor::new();

        let mut sns_msg = SnsMessage::default();
        sns_msg.sns_message_type = "Notification".to_string();
        sns_msg.message_id = "msg-123".to_string();
        sns_msg.topic_arn = "arn:aws:sns:us-east-1:123456789:my-topic".to_string();
        sns_msg.timestamp = Utc::now();
        sns_msg.signature_version = "1".to_string();
        sns_msg.signature = "sig".to_string();
        sns_msg.signing_cert_url = "https://cert".to_string();
        sns_msg.unsubscribe_url = "https://unsub".to_string();
        sns_msg.message = r#"{"test": "data"}"#.to_string();
        sns_msg.message_attributes = HashMap::new();

        let mut record = SnsRecord::default();
        record.event_source = "aws:sns".to_string();
        record.event_version = "1.0".to_string();
        record.event_subscription_arn =
            "arn:aws:sns:us-east-1:123456789:my-topic:sub-123".to_string();
        record.sns = sns_msg;

        let mut event = SnsEvent::default();
        event.records = vec![record];

        let links = extractor.extract_links(&event);
        assert!(links.is_empty());
    }

    #[test]
    fn test_parse_xray_trace_header() {
        let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";

        let ctx = parse_xray_trace_header(header).unwrap();

        assert!(ctx.is_valid());
        assert_eq!(
            ctx.trace_id().to_string(),
            "5759e988bd862e3fe1be46a994272793"
        );
        assert_eq!(ctx.span_id().to_string(), "53995c3f42cd8ad8");
        assert!(ctx.is_sampled());
    }
}

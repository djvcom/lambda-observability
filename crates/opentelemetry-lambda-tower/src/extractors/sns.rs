//! SNS event extractor for notification triggers.
//!
//! Extracts trace context from SNS message attributes. SNS follows similar
//! patterns to SQS for trace propagation.

use crate::extractor::TraceContextExtractor;
use aws_lambda_events::sns::{SnsEvent, SnsRecord};
use lambda_runtime::Context as LambdaContext;
use opentelemetry::Context;
use opentelemetry::trace::{Link, SpanContext, SpanId, TraceFlags, TraceId, TraceState};
use opentelemetry_semantic_conventions::attribute::{
    MESSAGING_BATCH_MESSAGE_COUNT, MESSAGING_DESTINATION_NAME, MESSAGING_MESSAGE_ID,
    MESSAGING_OPERATION_TYPE, MESSAGING_SYSTEM,
};
use tracing::Span;

/// Extractor for SNS notification events.
///
/// SNS events can carry trace context in the `AWSTraceHeader` message
/// attribute using X-Ray format. This extractor:
///
/// 1. Does NOT set a parent context (returns current context)
/// 2. Creates span links for each message's trace context
///
/// This follows OpenTelemetry semantic conventions for messaging systems,
/// where the async nature of message queues means span links are more
/// appropriate than parent-child relationships.
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

/// Extracts a span link from an SNS record's AWSTraceHeader message attribute.
fn extract_link_from_record(record: &SnsRecord) -> Option<Link> {
    let trace_attr = record.sns.message_attributes.get("AWSTraceHeader")?;

    if trace_attr.value.is_empty() {
        return None;
    }

    let span_context = parse_xray_trace_header(&trace_attr.value)?;

    Some(Link::new(span_context, vec![], 0))
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
        attrs.insert(
            "AWSTraceHeader".to_string(),
            MessageAttribute {
                data_type: "String".to_string(),
                value: "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
                    .to_string(),
            },
        );

        SnsEvent {
            records: vec![SnsRecord {
                event_source: "aws:sns".to_string(),
                event_version: "1.0".to_string(),
                event_subscription_arn: "arn:aws:sns:us-east-1:123456789:my-topic:sub-123"
                    .to_string(),
                sns: SnsMessage {
                    sns_message_type: "Notification".to_string(),
                    message_id: "msg-123".to_string(),
                    topic_arn: "arn:aws:sns:us-east-1:123456789:my-topic".to_string(),
                    subject: None,
                    timestamp: Utc::now(),
                    signature_version: "1".to_string(),
                    signature: "sig".to_string(),
                    signing_cert_url: "https://cert".to_string(),
                    unsubscribe_url: "https://unsub".to_string(),
                    message: r#"{"test": "data"}"#.to_string(),
                    message_attributes: attrs,
                },
            }],
        }
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

        let event = SnsEvent {
            records: vec![SnsRecord {
                event_source: "aws:sns".to_string(),
                event_version: "1.0".to_string(),
                event_subscription_arn: "arn:aws:sns:us-east-1:123456789:my-topic:sub-123"
                    .to_string(),
                sns: SnsMessage {
                    sns_message_type: "Notification".to_string(),
                    message_id: "msg-123".to_string(),
                    topic_arn: "arn:aws:sns:us-east-1:123456789:my-topic".to_string(),
                    subject: None,
                    timestamp: Utc::now(),
                    signature_version: "1".to_string(),
                    signature: "sig".to_string(),
                    signing_cert_url: "https://cert".to_string(),
                    unsubscribe_url: "https://unsub".to_string(),
                    message: r#"{"test": "data"}"#.to_string(),
                    message_attributes: HashMap::new(),
                },
            }],
        };

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

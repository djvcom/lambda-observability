//! SQS event extractor for message queue triggers.
//!
//! Extracts trace context from SQS message system attributes using the
//! `AWSTraceHeader` attribute in X-Ray format.

use crate::extractor::TraceContextExtractor;
use aws_lambda_events::sqs::{SqsEvent, SqsMessage};
use lambda_runtime::Context as LambdaContext;
use opentelemetry::Context;
use opentelemetry::trace::{Link, SpanContext, SpanId, TraceFlags, TraceId, TraceState};
use opentelemetry_semantic_conventions::attribute::{
    MESSAGING_BATCH_MESSAGE_COUNT, MESSAGING_DESTINATION_NAME, MESSAGING_MESSAGE_ID,
    MESSAGING_OPERATION_TYPE, MESSAGING_SYSTEM,
};
use tracing::Span;

/// Extractor for SQS message events.
///
/// SQS events carry trace context in the `AWSTraceHeader` system attribute
/// using X-Ray format. This extractor:
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
/// use opentelemetry_lambda_tower::{OtelTracingLayer, SqsEventExtractor};
///
/// let layer = OtelTracingLayer::new(SqsEventExtractor::new());
/// ```
#[derive(Clone, Debug, Default)]
pub struct SqsEventExtractor;

impl SqsEventExtractor {
    /// Creates a new SQS event extractor.
    pub fn new() -> Self {
        Self
    }

    /// Extracts the queue name from an event source ARN.
    ///
    /// ARN format: `arn:aws:sqs:{region}:{account}:{queue-name}`
    fn queue_name_from_arn(arn: &str) -> Option<&str> {
        arn.rsplit(':').next()
    }
}

impl TraceContextExtractor<SqsEvent> for SqsEventExtractor {
    fn extract_context(&self, _event: &SqsEvent) -> Context {
        // For SQS, we don't set a parent context because:
        // 1. Messages may come from multiple different traces
        // 2. The async nature means parent-child doesn't make semantic sense
        // Instead, we use span links (see extract_links)
        Context::current()
    }

    fn extract_links(&self, event: &SqsEvent) -> Vec<Link> {
        event
            .records
            .iter()
            .filter_map(extract_link_from_message)
            .collect()
    }

    fn trigger_type(&self) -> &'static str {
        "pubsub"
    }

    fn span_name(&self, event: &SqsEvent, lambda_ctx: &LambdaContext) -> String {
        // Use "{queue_name} process" format per OTel messaging conventions
        let queue_name = event
            .records
            .first()
            .and_then(|r| r.event_source_arn.as_deref())
            .and_then(Self::queue_name_from_arn)
            .unwrap_or(&lambda_ctx.env_config.function_name);

        format!("{} process", queue_name)
    }

    fn record_attributes(&self, event: &SqsEvent, span: &Span) {
        span.record(MESSAGING_SYSTEM, "aws_sqs");
        span.record(MESSAGING_OPERATION_TYPE, "process");

        if let Some(record) = event.records.first()
            && let Some(ref arn) = record.event_source_arn
            && let Some(queue_name) = Self::queue_name_from_arn(arn)
        {
            span.record(MESSAGING_DESTINATION_NAME, queue_name);
        }

        span.record(MESSAGING_BATCH_MESSAGE_COUNT, event.records.len() as i64);

        if event.records.len() == 1
            && let Some(ref msg_id) = event.records[0].message_id
        {
            span.record(MESSAGING_MESSAGE_ID, msg_id.as_str());
        }
    }
}

/// Extracts a span link from an SQS message's AWSTraceHeader.
fn extract_link_from_message(message: &SqsMessage) -> Option<Link> {
    // AWSTraceHeader is in the system attributes, NOT message_attributes
    let trace_header = message.attributes.get("AWSTraceHeader")?;

    let span_context = parse_xray_trace_header(trace_header)?;

    Some(Link::new(span_context, vec![], 0))
}

/// Parses an X-Ray trace header into a SpanContext.
///
/// X-Ray format: `Root=1-{epoch}-{random};Parent={span-id};Sampled={0|1}`
///
/// # Example
///
/// ```
/// use opentelemetry_lambda_tower::extractors::sqs::parse_xray_trace_header;
///
/// let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";
/// let ctx = parse_xray_trace_header(header);
/// assert!(ctx.is_some());
/// ```
pub fn parse_xray_trace_header(header: &str) -> Option<SpanContext> {
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

    // Parse trace ID (32 hex chars = 16 bytes)
    let trace_id_bytes = hex_to_bytes::<16>(&trace_id_hex)?;
    let trace_id = TraceId::from_bytes(trace_id_bytes);

    // Parse parent/span ID (16 hex chars = 8 bytes)
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
        true, // is_remote
        TraceState::default(),
    ))
}

/// Converts X-Ray trace ID format to 32-character hex string.
///
/// X-Ray format: `1-{epoch_hex}-{random_hex}` (8 + 24 = 32 chars)
/// Returns: `{epoch_hex}{random_hex}`
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
    use std::collections::HashMap;

    fn create_test_sqs_event() -> SqsEvent {
        let mut attributes = HashMap::new();
        attributes.insert(
            "AWSTraceHeader".to_string(),
            "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
                .to_string(),
        );

        let mut message = SqsMessage::default();
        message.message_id = Some("msg-123".to_string());
        message.receipt_handle = Some("receipt-123".to_string());
        message.body = Some(r#"{"test": "data"}"#.to_string());
        message.attributes = attributes;
        message.message_attributes = HashMap::new();
        message.event_source = Some("aws:sqs".to_string());
        message.event_source_arn = Some("arn:aws:sqs:us-east-1:123456789:my-queue".to_string());
        message.aws_region = Some("us-east-1".to_string());

        let mut event = SqsEvent::default();
        event.records = vec![message];
        event
    }

    fn create_test_lambda_context() -> LambdaContext {
        LambdaContext::default()
    }

    #[test]
    fn test_trigger_type() {
        let extractor = SqsEventExtractor::new();
        assert_eq!(extractor.trigger_type(), "pubsub");
    }

    #[test]
    fn test_span_name_includes_queue() {
        let extractor = SqsEventExtractor::new();
        let event = create_test_sqs_event();
        let ctx = create_test_lambda_context();

        let name = extractor.span_name(&event, &ctx);
        assert_eq!(name, "my-queue process");
    }

    #[test]
    fn test_queue_name_from_arn() {
        assert_eq!(
            SqsEventExtractor::queue_name_from_arn("arn:aws:sqs:us-east-1:123456789:my-queue"),
            Some("my-queue")
        );
        assert_eq!(
            SqsEventExtractor::queue_name_from_arn(
                "arn:aws:sqs:eu-west-1:987654321:another-queue.fifo"
            ),
            Some("another-queue.fifo")
        );
    }

    #[test]
    fn test_extract_links_single_message() {
        let extractor = SqsEventExtractor::new();
        let event = create_test_sqs_event();

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
    fn test_extract_links_multiple_messages() {
        let extractor = SqsEventExtractor::new();

        let mut attrs1 = HashMap::new();
        attrs1.insert(
            "AWSTraceHeader".to_string(),
            "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
                .to_string(),
        );

        let mut attrs2 = HashMap::new();
        attrs2.insert(
            "AWSTraceHeader".to_string(),
            "Root=1-67890abc-def0123456789abcdef01234;Parent=1234567890abcdef;Sampled=0"
                .to_string(),
        );

        let mut msg1 = SqsMessage::default();
        msg1.attributes = attrs1;

        let mut msg2 = SqsMessage::default();
        msg2.attributes = attrs2;

        let mut event = SqsEvent::default();
        event.records = vec![msg1, msg2];

        let links = extractor.extract_links(&event);

        assert_eq!(links.len(), 2);
        // First link is sampled
        assert!(links[0].span_context.is_sampled());
        // Second link is not sampled
        assert!(!links[1].span_context.is_sampled());
    }

    #[test]
    fn test_extract_links_no_trace_header() {
        let extractor = SqsEventExtractor::new();

        let mut msg = SqsMessage::default();
        msg.attributes = HashMap::new();

        let mut event = SqsEvent::default();
        event.records = vec![msg];

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
        assert!(ctx.is_remote());
    }

    #[test]
    fn test_parse_xray_trace_header_unsampled() {
        let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=0";

        let ctx = parse_xray_trace_header(header).unwrap();
        assert!(!ctx.is_sampled());
    }

    #[test]
    fn test_parse_xray_trace_header_invalid() {
        assert!(parse_xray_trace_header("invalid").is_none());
        assert!(parse_xray_trace_header("Root=invalid;Parent=abc").is_none());
        assert!(parse_xray_trace_header("Root=1-abc-def").is_none());
    }

    #[test]
    fn test_convert_xray_trace_id() {
        assert_eq!(
            convert_xray_trace_id("1-5759e988-bd862e3fe1be46a994272793"),
            Some("5759e988bd862e3fe1be46a994272793".to_string())
        );
    }

    #[test]
    fn test_hex_to_bytes() {
        let bytes: [u8; 4] = hex_to_bytes("deadbeef").unwrap();
        assert_eq!(bytes, [0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_to_bytes_invalid() {
        assert!(hex_to_bytes::<4>("deadbee").is_none()); // too short
        assert!(hex_to_bytes::<4>("deadbeefx").is_none()); // too long
        assert!(hex_to_bytes::<4>("deadbeeg").is_none()); // invalid char
    }
}

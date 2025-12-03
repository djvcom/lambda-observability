//! X-Ray trace header parsing and W3C trace context conversion.
//!
//! This module provides utilities for parsing AWS X-Ray trace headers from the
//! Lambda Extensions API and converting them to W3C trace context format for
//! use with OpenTelemetry.

use std::fmt;

/// Parsed X-Ray trace header components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XRayTraceHeader {
    /// The X-Ray Root trace ID (e.g., "1-5759e988-bd862e3fe1be46a994272793").
    pub root: String,
    /// The parent segment ID (e.g., "53995c3f42cd8ad8").
    pub parent: Option<String>,
    /// Whether sampling is enabled (1) or disabled (0).
    pub sampled: Option<bool>,
}

impl XRayTraceHeader {
    /// Parses an X-Ray trace header string.
    ///
    /// The format is: `Root=1-{timestamp}-{random};Parent={parent_id};Sampled={0|1}`
    ///
    /// # Arguments
    ///
    /// * `header` - The X-Ray trace header string
    ///
    /// # Returns
    ///
    /// Returns `Some(XRayTraceHeader)` if parsing succeeds, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use opentelemetry_lambda_extension::XRayTraceHeader;
    ///
    /// let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";
    /// let parsed = XRayTraceHeader::parse(header).unwrap();
    /// assert_eq!(parsed.root, "1-5759e988-bd862e3fe1be46a994272793");
    /// assert_eq!(parsed.parent, Some("53995c3f42cd8ad8".to_string()));
    /// assert_eq!(parsed.sampled, Some(true));
    /// ```
    pub fn parse(header: &str) -> Option<Self> {
        let mut root = None;
        let mut parent = None;
        let mut sampled = None;

        for part in header.split(';') {
            let part = part.trim();
            if let Some((key, value)) = part.split_once('=') {
                match key {
                    "Root" => root = Some(value.to_string()),
                    "Parent" => parent = Some(value.to_string()),
                    "Sampled" => {
                        sampled = match value {
                            "1" => Some(true),
                            "0" => Some(false),
                            _ => None,
                        }
                    }
                    _ => {} // Ignore unknown keys
                }
            }
        }

        root.map(|root| Self {
            root,
            parent,
            sampled,
        })
    }

    /// Converts the X-Ray trace header to W3C trace context format.
    ///
    /// Returns a tuple of (trace_id, span_id, trace_flags) suitable for
    /// use with OpenTelemetry span context.
    ///
    /// # Returns
    ///
    /// Returns `Some((trace_id, span_id, sampled))` if conversion succeeds.
    /// - `trace_id` is a 32-character hex string (16 bytes)
    /// - `span_id` is a 16-character hex string (8 bytes)
    /// - `sampled` indicates whether the trace is sampled
    ///
    /// Returns `None` if the parent is not present or conversion fails.
    pub fn to_w3c(&self) -> Option<W3CTraceContext> {
        let parent = self.parent.as_ref()?;

        // X-Ray Root format: 1-{8 char timestamp}-{24 char random}
        // W3C trace-id: 32 hex chars = 16 bytes
        // We convert by combining the 8 char timestamp + 24 char random = 32 chars
        let trace_id = self.xray_root_to_trace_id()?;

        // X-Ray Parent is already a 16 char hex string (8 bytes)
        // W3C span-id: 16 hex chars = 8 bytes
        if parent.len() != 16 || !parent.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        Some(W3CTraceContext {
            trace_id,
            span_id: parent.clone(),
            sampled: self.sampled.unwrap_or(false),
        })
    }

    /// Converts X-Ray Root ID to W3C trace ID.
    ///
    /// X-Ray Root format: `1-{8 hex chars timestamp}-{24 hex chars random}`
    /// W3C trace-id: 32 hex characters
    ///
    /// We combine timestamp + random to form the trace ID.
    fn xray_root_to_trace_id(&self) -> Option<String> {
        let parts: Vec<&str> = self.root.split('-').collect();
        if parts.len() != 3 {
            return None;
        }

        let version = parts[0];
        let timestamp = parts[1];
        let random = parts[2];

        // Validate version
        if version != "1" {
            return None;
        }

        // Validate timestamp (8 hex chars)
        if timestamp.len() != 8 || !timestamp.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        // Validate random (24 hex chars)
        if random.len() != 24 || !random.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        // Combine to form 32-char trace ID
        Some(format!("{}{}", timestamp, random))
    }

    /// Returns the raw X-Ray trace header string.
    pub fn to_header_string(&self) -> String {
        let mut parts = vec![format!("Root={}", self.root)];

        if let Some(ref parent) = self.parent {
            parts.push(format!("Parent={}", parent));
        }

        if let Some(sampled) = self.sampled {
            parts.push(format!("Sampled={}", if sampled { "1" } else { "0" }));
        }

        parts.join(";")
    }
}

impl fmt::Display for XRayTraceHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_header_string())
    }
}

/// W3C trace context components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct W3CTraceContext {
    /// 32-character hex trace ID (16 bytes).
    pub trace_id: String,
    /// 16-character hex span ID (8 bytes).
    pub span_id: String,
    /// Whether the trace is sampled.
    pub sampled: bool,
}

impl W3CTraceContext {
    /// Converts trace ID to bytes.
    ///
    /// Returns a 16-byte array representation of the trace ID.
    pub fn trace_id_bytes(&self) -> Option<[u8; 16]> {
        let bytes = hex::decode(&self.trace_id).ok()?;
        if bytes.len() != 16 {
            return None;
        }
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&bytes);
        Some(arr)
    }

    /// Converts span ID to bytes.
    ///
    /// Returns an 8-byte array representation of the span ID.
    pub fn span_id_bytes(&self) -> Option<[u8; 8]> {
        let bytes = hex::decode(&self.span_id).ok()?;
        if bytes.len() != 8 {
            return None;
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes);
        Some(arr)
    }

    /// Returns the W3C traceparent header string.
    ///
    /// Format: `{version}-{trace-id}-{parent-id}-{trace-flags}`
    pub fn to_traceparent(&self) -> String {
        let flags = if self.sampled { "01" } else { "00" };
        format!("00-{}-{}-{}", self.trace_id, self.span_id, flags)
    }
}

impl fmt::Display for W3CTraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_traceparent())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn valid_timestamp() -> impl Strategy<Value = String> {
        "[0-9a-f]{8}".prop_map(|s| s.to_lowercase())
    }

    fn valid_random() -> impl Strategy<Value = String> {
        "[0-9a-f]{24}".prop_map(|s| s.to_lowercase())
    }

    fn valid_parent() -> impl Strategy<Value = String> {
        "[0-9a-f]{16}".prop_map(|s| s.to_lowercase())
    }

    proptest! {
        #[test]
        fn parse_roundtrips(
            timestamp in valid_timestamp(),
            random in valid_random(),
            parent in valid_parent(),
            sampled in prop::bool::ANY
        ) {
            let root = format!("1-{}-{}", timestamp, random);
            let header_str = format!(
                "Root={};Parent={};Sampled={}",
                root,
                parent,
                if sampled { "1" } else { "0" }
            );

            let parsed = XRayTraceHeader::parse(&header_str).unwrap();

            prop_assert_eq!(parsed.root, root);
            prop_assert_eq!(parsed.parent, Some(parent));
            prop_assert_eq!(parsed.sampled, Some(sampled));
        }

        #[test]
        fn w3c_conversion_produces_valid_ids(
            timestamp in valid_timestamp(),
            random in valid_random(),
            parent in valid_parent(),
        ) {
            let header = XRayTraceHeader {
                root: format!("1-{}-{}", timestamp, random),
                parent: Some(parent.clone()),
                sampled: Some(true),
            };

            let w3c = header.to_w3c().unwrap();

            prop_assert_eq!(w3c.trace_id.len(), 32);
            prop_assert_eq!(w3c.span_id.len(), 16);
            prop_assert_eq!(w3c.span_id, parent);
            prop_assert!(w3c.trace_id.chars().all(|c| c.is_ascii_hexdigit()));
        }

        #[test]
        fn trace_id_bytes_roundtrips(
            timestamp in valid_timestamp(),
            random in valid_random(),
            parent in valid_parent(),
        ) {
            let header = XRayTraceHeader {
                root: format!("1-{}-{}", timestamp, random),
                parent: Some(parent),
                sampled: Some(true),
            };

            let w3c = header.to_w3c().unwrap();
            let bytes = w3c.trace_id_bytes().unwrap();

            prop_assert_eq!(bytes.len(), 16);
            let hex_back = hex::encode(bytes);
            prop_assert_eq!(hex_back, w3c.trace_id);
        }
    }

    #[test]
    fn test_parse_full_header() {
        let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";
        let parsed = XRayTraceHeader::parse(header).unwrap();

        assert_eq!(parsed.root, "1-5759e988-bd862e3fe1be46a994272793");
        assert_eq!(parsed.parent, Some("53995c3f42cd8ad8".to_string()));
        assert_eq!(parsed.sampled, Some(true));
    }

    #[test]
    fn test_parse_header_unsampled() {
        let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=0";
        let parsed = XRayTraceHeader::parse(header).unwrap();

        assert_eq!(parsed.sampled, Some(false));
    }

    #[test]
    fn test_parse_header_no_parent() {
        let header = "Root=1-5759e988-bd862e3fe1be46a994272793;Sampled=1";
        let parsed = XRayTraceHeader::parse(header).unwrap();

        assert_eq!(parsed.root, "1-5759e988-bd862e3fe1be46a994272793");
        assert!(parsed.parent.is_none());
        assert_eq!(parsed.sampled, Some(true));
    }

    #[test]
    fn test_parse_header_root_only() {
        let header = "Root=1-5759e988-bd862e3fe1be46a994272793";
        let parsed = XRayTraceHeader::parse(header).unwrap();

        assert_eq!(parsed.root, "1-5759e988-bd862e3fe1be46a994272793");
        assert!(parsed.parent.is_none());
        assert!(parsed.sampled.is_none());
    }

    #[test]
    fn test_parse_invalid_header() {
        assert!(XRayTraceHeader::parse("").is_none());
        assert!(XRayTraceHeader::parse("Parent=123").is_none());
        assert!(XRayTraceHeader::parse("invalid").is_none());
    }

    #[test]
    fn test_to_w3c() {
        let header = XRayTraceHeader {
            root: "1-5759e988-bd862e3fe1be46a994272793".to_string(),
            parent: Some("53995c3f42cd8ad8".to_string()),
            sampled: Some(true),
        };

        let w3c = header.to_w3c().unwrap();

        // trace_id = timestamp (5759e988) + random (bd862e3fe1be46a994272793) = 32 chars
        assert_eq!(w3c.trace_id, "5759e988bd862e3fe1be46a994272793");
        assert_eq!(w3c.span_id, "53995c3f42cd8ad8");
        assert!(w3c.sampled);
    }

    #[test]
    fn test_to_w3c_no_parent() {
        let header = XRayTraceHeader {
            root: "1-5759e988-bd862e3fe1be46a994272793".to_string(),
            parent: None,
            sampled: Some(true),
        };

        assert!(header.to_w3c().is_none());
    }

    #[test]
    fn test_to_w3c_invalid_parent() {
        let header = XRayTraceHeader {
            root: "1-5759e988-bd862e3fe1be46a994272793".to_string(),
            parent: Some("invalid".to_string()),
            sampled: Some(true),
        };

        assert!(header.to_w3c().is_none());
    }

    #[test]
    fn test_to_w3c_invalid_root() {
        let header = XRayTraceHeader {
            root: "invalid-root".to_string(),
            parent: Some("53995c3f42cd8ad8".to_string()),
            sampled: Some(true),
        };

        assert!(header.to_w3c().is_none());
    }

    #[test]
    fn test_w3c_to_traceparent() {
        let ctx = W3CTraceContext {
            trace_id: "5759e988bd862e3fe1be46a994272793".to_string(),
            span_id: "53995c3f42cd8ad8".to_string(),
            sampled: true,
        };

        assert_eq!(
            ctx.to_traceparent(),
            "00-5759e988bd862e3fe1be46a994272793-53995c3f42cd8ad8-01"
        );
    }

    #[test]
    fn test_w3c_to_traceparent_unsampled() {
        let ctx = W3CTraceContext {
            trace_id: "5759e988bd862e3fe1be46a994272793".to_string(),
            span_id: "53995c3f42cd8ad8".to_string(),
            sampled: false,
        };

        assert_eq!(
            ctx.to_traceparent(),
            "00-5759e988bd862e3fe1be46a994272793-53995c3f42cd8ad8-00"
        );
    }

    #[test]
    fn test_trace_id_bytes() {
        let ctx = W3CTraceContext {
            trace_id: "5759e988bd862e3fe1be46a994272793".to_string(),
            span_id: "53995c3f42cd8ad8".to_string(),
            sampled: true,
        };

        let bytes = ctx.trace_id_bytes().unwrap();
        assert_eq!(bytes.len(), 16);
        assert_eq!(bytes[0], 0x57);
        assert_eq!(bytes[1], 0x59);
    }

    #[test]
    fn test_span_id_bytes() {
        let ctx = W3CTraceContext {
            trace_id: "5759e988bd862e3fe1be46a994272793".to_string(),
            span_id: "53995c3f42cd8ad8".to_string(),
            sampled: true,
        };

        let bytes = ctx.span_id_bytes().unwrap();
        assert_eq!(bytes.len(), 8);
        assert_eq!(bytes[0], 0x53);
        assert_eq!(bytes[1], 0x99);
    }

    #[test]
    fn test_to_header_string() {
        let header = XRayTraceHeader {
            root: "1-5759e988-bd862e3fe1be46a994272793".to_string(),
            parent: Some("53995c3f42cd8ad8".to_string()),
            sampled: Some(true),
        };

        assert_eq!(
            header.to_header_string(),
            "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1"
        );
    }

    #[test]
    fn test_display() {
        let header = XRayTraceHeader {
            root: "1-5759e988-bd862e3fe1be46a994272793".to_string(),
            parent: Some("53995c3f42cd8ad8".to_string()),
            sampled: Some(true),
        };

        let s = format!("{}", header);
        assert!(s.contains("Root="));
        assert!(s.contains("Parent="));
        assert!(s.contains("Sampled=1"));
    }
}

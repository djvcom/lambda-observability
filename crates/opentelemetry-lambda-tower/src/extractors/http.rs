//! HTTP event extractors for API Gateway.
//!
//! Provides trace context extraction from API Gateway events:
//! - [`ApiGatewayV2Extractor`] - HTTP API (v2) events
//! - [`ApiGatewayV1Extractor`] - REST API (v1) events
//! - [`HttpEventExtractor`] - Type alias for the most common v2 extractor

use crate::extractor::TraceContextExtractor;
use aws_lambda_events::apigw::{ApiGatewayProxyRequest, ApiGatewayV2httpRequest};
use http::HeaderMap;
use lambda_runtime::Context as LambdaContext;
use opentelemetry::Context;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_semantic_conventions::attribute::{
    CLIENT_ADDRESS, HTTP_REQUEST_METHOD, HTTP_ROUTE, NETWORK_PROTOCOL_VERSION, SERVER_ADDRESS,
    URL_PATH, URL_QUERY, URL_SCHEME, USER_AGENT_ORIGINAL,
};
use tracing::Span;

/// Type alias for the most common HTTP extractor (API Gateway HTTP API v2).
pub type HttpEventExtractor = ApiGatewayV2Extractor;

/// Extractor for API Gateway HTTP API (v2) events.
///
/// Extracts trace context from HTTP headers using the globally configured
/// OpenTelemetry propagator. Falls back to the `_X_AMZN_TRACE_ID` environment
/// variable if no valid trace context is found in headers.
///
/// Configure the propagator via `opentelemetry::global::set_text_map_propagator()`.
///
/// # Example
///
/// ```ignore
/// use opentelemetry_lambda_tower::{OtelTracingLayer, ApiGatewayV2Extractor};
///
/// let layer = OtelTracingLayer::new(ApiGatewayV2Extractor::new());
/// ```
#[derive(Clone, Debug, Default)]
pub struct ApiGatewayV2Extractor;

impl ApiGatewayV2Extractor {
    /// Creates a new extractor.
    ///
    /// Uses the globally configured OpenTelemetry propagator for trace context extraction.
    pub fn new() -> Self {
        Self
    }
}

impl TraceContextExtractor<ApiGatewayV2httpRequest> for ApiGatewayV2Extractor {
    fn extract_context(&self, event: &ApiGatewayV2httpRequest) -> Context {
        let extractor = HeaderMapExtractor(&event.headers);
        let ctx = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&extractor)
        });

        if ctx.span().span_context().is_valid() {
            return ctx;
        }

        if let Ok(xray_header) = std::env::var("_X_AMZN_TRACE_ID") {
            let env_extractor = XRayEnvExtractor::new(&xray_header);
            let xray_ctx = opentelemetry::global::get_text_map_propagator(|propagator| {
                propagator.extract(&env_extractor)
            });
            if xray_ctx.span().span_context().is_valid() {
                return xray_ctx;
            }
        }

        Context::current()
    }

    fn trigger_type(&self) -> &'static str {
        "http"
    }

    fn span_name(&self, event: &ApiGatewayV2httpRequest, lambda_ctx: &LambdaContext) -> String {
        // Use "{method} {route}" format per OTel conventions
        let method = event.request_context.http.method.as_str();

        // route_key contains the route pattern (e.g., "GET /users/{id}")
        // We want just the route part, or fall back to the raw path
        let route = event
            .route_key
            .as_deref()
            .and_then(|rk| rk.split_once(' ').map(|(_, route)| route))
            .or(event.raw_path.as_deref())
            .unwrap_or(&lambda_ctx.env_config.function_name);

        format!("{} {}", method, route)
    }

    fn record_attributes(&self, event: &ApiGatewayV2httpRequest, span: &Span) {
        span.record(
            HTTP_REQUEST_METHOD,
            event.request_context.http.method.as_str(),
        );

        if let Some(ref path) = event.raw_path {
            span.record(URL_PATH, path.as_str());
        }

        if let Some(ref route_key) = event.route_key {
            if let Some((_, route)) = route_key.split_once(' ') {
                span.record(HTTP_ROUTE, route);
            } else {
                span.record(HTTP_ROUTE, route_key.as_str());
            }
        }

        span.record(URL_SCHEME, "https");

        if let Some(ref qs) = event.raw_query_string
            && !qs.is_empty()
        {
            span.record(URL_QUERY, qs.as_str());
        }

        if let Some(ua) = event.headers.get("user-agent")
            && let Ok(ua_str) = ua.to_str()
        {
            span.record(USER_AGENT_ORIGINAL, ua_str);
        }

        if let Some(ref ip) = event.request_context.http.source_ip {
            span.record(CLIENT_ADDRESS, ip.as_str());
        }

        if let Some(host) = event.headers.get("host")
            && let Ok(host_str) = host.to_str()
        {
            span.record(SERVER_ADDRESS, host_str);
        }

        if let Some(ref protocol) = event.request_context.http.protocol {
            let version = extract_http_version(protocol);
            span.record(NETWORK_PROTOCOL_VERSION, version);
        }
    }
}

/// Extractor for API Gateway REST API (v1) events.
///
/// Similar to v2 but handles the different event structure.
/// Uses the globally configured OpenTelemetry propagator for trace context extraction.
#[derive(Clone, Debug, Default)]
pub struct ApiGatewayV1Extractor;

impl ApiGatewayV1Extractor {
    /// Creates a new extractor.
    ///
    /// Uses the globally configured OpenTelemetry propagator for trace context extraction.
    pub fn new() -> Self {
        Self
    }
}

impl TraceContextExtractor<ApiGatewayProxyRequest> for ApiGatewayV1Extractor {
    fn extract_context(&self, event: &ApiGatewayProxyRequest) -> Context {
        let extractor = HeaderMapExtractor(&event.headers);
        let ctx = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&extractor)
        });

        if ctx.span().span_context().is_valid() {
            return ctx;
        }

        if let Ok(xray_header) = std::env::var("_X_AMZN_TRACE_ID") {
            let env_extractor = XRayEnvExtractor::new(&xray_header);
            let xray_ctx = opentelemetry::global::get_text_map_propagator(|propagator| {
                propagator.extract(&env_extractor)
            });
            if xray_ctx.span().span_context().is_valid() {
                return xray_ctx;
            }
        }

        Context::current()
    }

    fn trigger_type(&self) -> &'static str {
        "http"
    }

    fn span_name(&self, event: &ApiGatewayProxyRequest, lambda_ctx: &LambdaContext) -> String {
        let method = event.http_method.as_str();

        // Use resource pattern for low-cardinality route
        let route = event
            .resource
            .as_deref()
            .or(event.path.as_deref())
            .unwrap_or(&lambda_ctx.env_config.function_name);

        format!("{} {}", method, route)
    }

    fn record_attributes(&self, event: &ApiGatewayProxyRequest, span: &Span) {
        span.record(HTTP_REQUEST_METHOD, event.http_method.as_str());

        if let Some(ref path) = event.path {
            span.record(URL_PATH, path.as_str());
        }

        if let Some(ref resource) = event.resource {
            span.record(HTTP_ROUTE, resource.as_str());
        }

        span.record(URL_SCHEME, "https");

        if let Some(ua) = event.headers.get("user-agent")
            && let Ok(ua_str) = ua.to_str()
        {
            span.record(USER_AGENT_ORIGINAL, ua_str);
        }

        if let Some(ref ip) = event.request_context.identity.source_ip {
            span.record(CLIENT_ADDRESS, ip.as_str());
        }

        if let Some(host) = event.headers.get("host")
            && let Ok(host_str) = host.to_str()
        {
            span.record(SERVER_ADDRESS, host_str);
        }

        if let Some(ref protocol) = event.request_context.protocol {
            let version = extract_http_version(protocol);
            span.record(NETWORK_PROTOCOL_VERSION, version);
        }
    }
}

/// Extracts the HTTP version from a protocol string.
///
/// Input formats: "HTTP/1.1", "HTTP/2.0", "HTTP/2", etc.
/// Returns: "1.1", "2", etc. (just the version part)
fn extract_http_version(protocol: &str) -> &str {
    protocol
        .strip_prefix("HTTP/")
        .map(|v| v.trim_end_matches(".0"))
        .unwrap_or(protocol)
}

/// Adapter to extract from http::HeaderMap using OTel's Extractor trait.
struct HeaderMapExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Adapter to extract traceparent from X-Ray environment variable format.
///
/// X-Ray format: `Root=1-{trace-id};Parent={span-id};Sampled={0|1}`
/// W3C format: `00-{trace-id}-{parent-id}-{flags}`
///
/// The converted traceparent is stored in the struct to satisfy the
/// `Extractor` trait's lifetime requirements.
struct XRayEnvExtractor {
    traceparent: Option<String>,
}

impl XRayEnvExtractor {
    fn new(xray: &str) -> Self {
        Self {
            traceparent: convert_xray_to_traceparent(xray),
        }
    }
}

impl Extractor for XRayEnvExtractor {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case("traceparent") {
            self.traceparent.as_deref()
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        if self.traceparent.is_some() {
            vec!["traceparent"]
        } else {
            vec![]
        }
    }
}

/// Converts X-Ray trace header to W3C traceparent format.
///
/// X-Ray: `Root=1-{epoch}-{random};Parent={span-id};Sampled=1`
/// W3C: `00-{trace-id}-{parent-id}-{flags}`
pub fn convert_xray_to_traceparent(xray: &str) -> Option<String> {
    let mut trace_id = None;
    let mut parent_id = None;
    let mut sampled = false;

    for part in xray.split(';') {
        if let Some(root) = part.strip_prefix("Root=") {
            trace_id = parse_xray_trace_id(root);
        } else if let Some(parent) = part.strip_prefix("Parent=") {
            parent_id = Some(parent.to_string());
        } else if part == "Sampled=1" {
            sampled = true;
        }
    }

    let trace = trace_id?;
    let parent = parent_id?;

    if parent.len() != 16 {
        return None;
    }

    let flags = if sampled { "01" } else { "00" };
    Some(format!("00-{}-{}-{}", trace, parent, flags))
}

/// Parses X-Ray trace ID format to 32-character hex string.
///
/// X-Ray format: `1-{epoch_hex}-{random_hex}`
/// OTel format: `{epoch_hex}{random_hex}` (32 chars total)
pub fn parse_xray_trace_id(root: &str) -> Option<String> {
    let parts: Vec<&str> = root.split('-').collect();
    if parts.len() == 3 && parts[0] == "1" {
        let trace_id = format!("{}{}", parts[1], parts[2]);
        if trace_id.len() == 32 {
            return Some(trace_id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lambda_events::apigw::{
        ApiGatewayV2httpRequestContext, ApiGatewayV2httpRequestContextHttpDescription,
    };
    use http::HeaderValue;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use serial_test::serial;

    fn create_test_v2_event() -> ApiGatewayV2httpRequest {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        let mut http_desc = ApiGatewayV2httpRequestContextHttpDescription::default();
        http_desc.method = http::Method::GET;
        http_desc.source_ip = Some("192.168.1.1".to_string());

        let mut request_context = ApiGatewayV2httpRequestContext::default();
        request_context.http = http_desc;

        let mut event = ApiGatewayV2httpRequest::default();
        event.headers = headers;
        event.raw_path = Some("/users/123".to_string());
        event.route_key = Some("GET /users/{id}".to_string());
        event.raw_query_string = Some("foo=bar".to_string());
        event.request_context = request_context;
        event
    }

    fn create_test_lambda_context() -> LambdaContext {
        LambdaContext::default()
    }

    #[test]
    fn test_trigger_type() {
        let extractor = ApiGatewayV2Extractor::new();
        assert_eq!(extractor.trigger_type(), "http");
    }

    #[test]
    fn test_span_name_from_route_v2() {
        let extractor = ApiGatewayV2Extractor::new();
        let event = create_test_v2_event();
        let ctx = create_test_lambda_context();

        let name = extractor.span_name(&event, &ctx);
        assert_eq!(name, "GET /users/{id}");
    }

    #[test]
    fn test_span_name_fallback_to_path() {
        let extractor = ApiGatewayV2Extractor::new();
        let mut event = create_test_v2_event();
        event.route_key = None;
        let ctx = create_test_lambda_context();

        let name = extractor.span_name(&event, &ctx);
        assert_eq!(name, "GET /users/123");
    }

    #[test]
    fn test_extract_no_trace_context() {
        let extractor = ApiGatewayV2Extractor::new();
        let event = create_test_v2_event();

        let ctx = extractor.extract_context(&event);

        // Should return a context, but span context may not be valid
        // (no parent trace)
        assert!(!ctx.span().span_context().is_valid());
    }

    #[test]
    #[serial]
    fn test_extract_traceparent_header() {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let extractor = ApiGatewayV2Extractor::new();
        let mut event = create_test_v2_event();

        event.headers.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );

        let ctx = extractor.extract_context(&event);

        assert!(ctx.span().span_context().is_valid());
        assert_eq!(
            ctx.span().span_context().trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
    }

    #[test]
    #[serial]
    fn test_extract_traceparent_case_insensitive() {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let extractor = ApiGatewayV2Extractor::new();
        let mut event = create_test_v2_event();

        event.headers.insert(
            "Traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );

        let ctx = extractor.extract_context(&event);
        assert!(ctx.span().span_context().is_valid());
    }

    #[test]
    fn test_extract_invalid_traceparent() {
        let extractor = ApiGatewayV2Extractor::new();
        let mut event = create_test_v2_event();

        event
            .headers
            .insert("traceparent", HeaderValue::from_static("invalid"));

        let ctx = extractor.extract_context(&event);

        assert!(!ctx.span().span_context().is_valid());
    }

    #[test]
    fn test_parse_xray_trace_id() {
        // Valid X-Ray trace ID
        let result = parse_xray_trace_id("1-5759e988-bd862e3fe1be46a994272793");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "5759e988bd862e3fe1be46a994272793");
    }

    #[test]
    fn test_parse_xray_trace_id_invalid() {
        assert!(parse_xray_trace_id("invalid").is_none());
        assert!(parse_xray_trace_id("1-abc").is_none());
    }

    #[test]
    fn test_convert_xray_to_traceparent_sampled() {
        let xray = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";
        let result = convert_xray_to_traceparent(xray);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap(),
            "00-5759e988bd862e3fe1be46a994272793-53995c3f42cd8ad8-01"
        );
    }

    #[test]
    fn test_convert_xray_to_traceparent_unsampled() {
        let xray = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=0";
        let result = convert_xray_to_traceparent(xray);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap(),
            "00-5759e988bd862e3fe1be46a994272793-53995c3f42cd8ad8-00"
        );
    }

    #[test]
    fn test_convert_xray_to_traceparent_missing_parent() {
        let xray = "Root=1-5759e988-bd862e3fe1be46a994272793;Sampled=1";
        assert!(convert_xray_to_traceparent(xray).is_none());
    }

    #[test]
    fn test_convert_xray_to_traceparent_invalid_parent() {
        let xray = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=tooshort;Sampled=1";
        assert!(convert_xray_to_traceparent(xray).is_none());
    }

    #[test]
    fn test_xray_env_extractor_valid() {
        let xray = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";
        let extractor = XRayEnvExtractor::new(xray);
        let traceparent = extractor.get("traceparent");
        assert!(traceparent.is_some());
        assert_eq!(
            traceparent.unwrap(),
            "00-5759e988bd862e3fe1be46a994272793-53995c3f42cd8ad8-01"
        );
    }

    #[test]
    fn test_xray_env_extractor_case_insensitive() {
        let xray = "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1";
        let extractor = XRayEnvExtractor::new(xray);
        assert!(extractor.get("Traceparent").is_some());
        assert!(extractor.get("TRACEPARENT").is_some());
    }

    #[test]
    fn test_extract_http_version_1_1() {
        assert_eq!(extract_http_version("HTTP/1.1"), "1.1");
    }

    #[test]
    fn test_extract_http_version_2_0() {
        assert_eq!(extract_http_version("HTTP/2.0"), "2");
    }

    #[test]
    fn test_extract_http_version_2() {
        assert_eq!(extract_http_version("HTTP/2"), "2");
    }

    #[test]
    fn test_extract_http_version_fallback() {
        assert_eq!(extract_http_version("unknown"), "unknown");
    }
}

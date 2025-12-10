//! Extractor for `lambda_http` crate integration.
//!
//! Provides trace context extraction from [`lambda_http::request::LambdaRequest`],
//! enabling seamless integration with the `lambda_http` crate's tower middleware pattern.
//!
//! # Example
//!
//! ```ignore
//! use lambda_http::{Body, Error, Request, Response};
//! use opentelemetry_lambda_tower::{LambdaHttpExtractor, OtelTracingLayer};
//! use tower::ServiceBuilder;
//!
//! async fn handler(event: Request) -> Result<Response<Body>, Error> {
//!     Ok(Response::builder().status(200).body("Hello".into())?)
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Error> {
//!     let tracing_layer = OtelTracingLayer::new(LambdaHttpExtractor::new());
//!
//!     let service = ServiceBuilder::new()
//!         .layer(tracing_layer)
//!         .layer_fn(lambda_http::Adapter::from)
//!         .service_fn(handler);
//!
//!     lambda_runtime::run(service).await
//! }
//! ```

use crate::extractor::TraceContextExtractor;
use http::HeaderMap;
use lambda_http::request::LambdaRequest;
use lambda_runtime::Context as LambdaContext;
use opentelemetry::Context;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_semantic_conventions::attribute::{
    CLIENT_ADDRESS, HTTP_REQUEST_METHOD, HTTP_ROUTE, NETWORK_PROTOCOL_VERSION, SERVER_ADDRESS,
    URL_PATH, URL_QUERY, URL_SCHEME, USER_AGENT_ORIGINAL,
};
use tracing::Span;

/// Extractor for `lambda_http::request::LambdaRequest`.
///
/// Handles all HTTP event types supported by `lambda_http`:
/// - API Gateway REST API (v1)
/// - API Gateway HTTP API (v2)
/// - Application Load Balancer (ALB)
/// - API Gateway WebSocket
///
/// Uses the globally configured OpenTelemetry propagator for trace context extraction,
/// supporting W3C TraceContext, B3, Jaeger, X-Ray, or any composite propagator.
/// Falls back to the `_X_AMZN_TRACE_ID` environment variable if no trace headers are found.
#[derive(Clone, Debug, Default)]
pub struct LambdaHttpExtractor;

impl LambdaHttpExtractor {
    /// Creates a new extractor.
    ///
    /// Uses the globally configured OpenTelemetry propagator for trace context extraction.
    /// Configure the propagator via `opentelemetry::global::set_text_map_propagator()`.
    pub fn new() -> Self {
        Self
    }

    fn extract_from_headers(&self, headers: &HeaderMap) -> Context {
        let extractor = HeaderMapExtractor(headers);
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
}

impl TraceContextExtractor<LambdaRequest> for LambdaHttpExtractor {
    fn extract_context(&self, event: &LambdaRequest) -> Context {
        match event {
            LambdaRequest::ApiGatewayV1(req) => self.extract_from_headers(&req.headers),
            LambdaRequest::ApiGatewayV2(req) => self.extract_from_headers(&req.headers),
            LambdaRequest::Alb(req) => self.extract_from_headers(&req.headers),
            LambdaRequest::WebSocket(req) => self.extract_from_headers(&req.headers),
            _ => Context::current(),
        }
    }

    fn trigger_type(&self) -> &'static str {
        "http"
    }

    fn span_name(&self, event: &LambdaRequest, lambda_ctx: &LambdaContext) -> String {
        match event {
            LambdaRequest::ApiGatewayV1(req) => {
                let method = req.http_method.as_str();
                let route = req
                    .resource
                    .as_deref()
                    .or(req.path.as_deref())
                    .unwrap_or(&lambda_ctx.env_config.function_name);
                format!("{} {}", method, route)
            }
            LambdaRequest::ApiGatewayV2(req) => {
                let method = req.request_context.http.method.as_str();
                let route = req
                    .route_key
                    .as_deref()
                    .and_then(|rk| rk.split_once(' ').map(|(_, route)| route))
                    .or(req.raw_path.as_deref())
                    .unwrap_or(&lambda_ctx.env_config.function_name);
                format!("{} {}", method, route)
            }
            LambdaRequest::Alb(req) => {
                let method = req.http_method.as_str();
                let path = req.path.as_deref().unwrap_or("/");
                format!("{} {}", method, path)
            }
            LambdaRequest::WebSocket(req) => {
                let route = req
                    .request_context
                    .route_key
                    .as_deref()
                    .unwrap_or("$default");
                format!("WebSocket {}", route)
            }
            _ => lambda_ctx.env_config.function_name.clone(),
        }
    }

    fn record_attributes(&self, event: &LambdaRequest, span: &Span) {
        match event {
            LambdaRequest::ApiGatewayV1(req) => {
                record_apigw_v1_attributes(req, span);
            }
            LambdaRequest::ApiGatewayV2(req) => {
                record_apigw_v2_attributes(req, span);
            }
            LambdaRequest::Alb(req) => {
                record_alb_attributes(req, span);
            }
            LambdaRequest::WebSocket(_req) => {
                span.record(URL_SCHEME, "wss");
            }
            _ => {}
        }
    }
}

fn record_apigw_v1_attributes(req: &aws_lambda_events::apigw::ApiGatewayProxyRequest, span: &Span) {
    span.record(HTTP_REQUEST_METHOD, req.http_method.as_str());

    if let Some(ref path) = req.path {
        span.record(URL_PATH, path.as_str());
    }

    if let Some(ref resource) = req.resource {
        span.record(HTTP_ROUTE, resource.as_str());
    }

    span.record(URL_SCHEME, "https");

    if let Some(ua) = req.headers.get("user-agent")
        && let Ok(ua_str) = ua.to_str()
    {
        span.record(USER_AGENT_ORIGINAL, ua_str);
    }

    if let Some(ref ip) = req.request_context.identity.source_ip {
        span.record(CLIENT_ADDRESS, ip.as_str());
    }

    if let Some(host) = req.headers.get("host")
        && let Ok(host_str) = host.to_str()
    {
        span.record(SERVER_ADDRESS, host_str);
    }

    if let Some(ref protocol) = req.request_context.protocol {
        let version = extract_http_version(protocol);
        span.record(NETWORK_PROTOCOL_VERSION, version);
    }
}

fn record_apigw_v2_attributes(
    req: &aws_lambda_events::apigw::ApiGatewayV2httpRequest,
    span: &Span,
) {
    span.record(
        HTTP_REQUEST_METHOD,
        req.request_context.http.method.as_str(),
    );

    if let Some(ref path) = req.raw_path {
        span.record(URL_PATH, path.as_str());
    }

    if let Some(ref route_key) = req.route_key {
        if let Some((_, route)) = route_key.split_once(' ') {
            span.record(HTTP_ROUTE, route);
        } else {
            span.record(HTTP_ROUTE, route_key.as_str());
        }
    }

    span.record(URL_SCHEME, "https");

    if let Some(ref qs) = req.raw_query_string
        && !qs.is_empty()
    {
        span.record(URL_QUERY, qs.as_str());
    }

    if let Some(ua) = req.headers.get("user-agent")
        && let Ok(ua_str) = ua.to_str()
    {
        span.record(USER_AGENT_ORIGINAL, ua_str);
    }

    if let Some(ref ip) = req.request_context.http.source_ip {
        span.record(CLIENT_ADDRESS, ip.as_str());
    }

    if let Some(host) = req.headers.get("host")
        && let Ok(host_str) = host.to_str()
    {
        span.record(SERVER_ADDRESS, host_str);
    }

    if let Some(ref protocol) = req.request_context.http.protocol {
        let version = extract_http_version(protocol);
        span.record(NETWORK_PROTOCOL_VERSION, version);
    }
}

fn record_alb_attributes(req: &aws_lambda_events::alb::AlbTargetGroupRequest, span: &Span) {
    span.record(HTTP_REQUEST_METHOD, req.http_method.as_str());

    if let Some(ref path) = req.path {
        span.record(URL_PATH, path.as_str());
    }

    span.record(URL_SCHEME, "https");

    if let Some(ua) = req.headers.get("user-agent")
        && let Ok(ua_str) = ua.to_str()
    {
        span.record(USER_AGENT_ORIGINAL, ua_str);
    }

    if let Some(host) = req.headers.get("host")
        && let Ok(host_str) = host.to_str()
    {
        span.record(SERVER_ADDRESS, host_str);
    }
}

fn extract_http_version(protocol: &str) -> &str {
    protocol
        .strip_prefix("HTTP/")
        .map(|v| v.trim_end_matches(".0"))
        .unwrap_or(protocol)
}

struct HeaderMapExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

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

fn convert_xray_to_traceparent(xray: &str) -> Option<String> {
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

fn parse_xray_trace_id(root: &str) -> Option<String> {
    let parts: Vec<&str> = root.split('-').collect();
    if parts.len() == 3 && parts[0] == "1" {
        let trace_id = format!("{}{}", parts[1], parts[2]);
        if trace_id.len() == 32 {
            return Some(trace_id);
        }
    }
    None
}

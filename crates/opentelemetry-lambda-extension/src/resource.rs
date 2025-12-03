//! Lambda resource attribute detection.
//!
//! This module provides an AWS Lambda resource detector that implements the
//! OpenTelemetry SDK's `ResourceDetector` trait. It detects Lambda runtime
//! environment attributes and builds properly typed `Resource` objects.
//!
//! The detector reads from standard Lambda environment variables and follows
//! OpenTelemetry semantic conventions for cloud and FaaS attributes.

use opentelemetry::{KeyValue, Value};
use opentelemetry_proto::tonic::resource::v1::Resource as ProtoResource;
use opentelemetry_sdk::resource::{Resource, ResourceDetector};
use opentelemetry_semantic_conventions::SCHEMA_URL;
use opentelemetry_semantic_conventions::attribute as semconv_attr;
use opentelemetry_semantic_conventions::resource as semconv_res;
use std::borrow::Cow;
use std::env;

/// Re-export semantic conventions for use by other modules.
pub mod semconv {
    pub use opentelemetry_semantic_conventions::attribute::{
        CLOUD_PLATFORM, CLOUD_PROVIDER, CLOUD_REGION, FAAS_COLDSTART, FAAS_INVOCATION_ID,
        FAAS_MAX_MEMORY, FAAS_NAME, FAAS_TRIGGER, FAAS_VERSION,
    };
    pub use opentelemetry_semantic_conventions::resource::{
        AWS_LOG_GROUP_NAMES, AWS_LOG_STREAM_NAMES, FAAS_INSTANCE, SERVICE_NAME,
        TELEMETRY_SDK_LANGUAGE, TELEMETRY_SDK_NAME, TELEMETRY_SDK_VERSION,
    };
}

/// Environment variable names for Lambda runtime.
const AWS_EXECUTION_ENV: &str = "AWS_EXECUTION_ENV";
const AWS_REGION: &str = "AWS_REGION";
const AWS_LAMBDA_FUNCTION_NAME: &str = "AWS_LAMBDA_FUNCTION_NAME";
const AWS_LAMBDA_FUNCTION_VERSION: &str = "AWS_LAMBDA_FUNCTION_VERSION";
const AWS_LAMBDA_FUNCTION_MEMORY_SIZE: &str = "AWS_LAMBDA_FUNCTION_MEMORY_SIZE";
const AWS_LAMBDA_LOG_GROUP_NAME: &str = "AWS_LAMBDA_LOG_GROUP_NAME";
const AWS_LAMBDA_LOG_STREAM_NAME: &str = "AWS_LAMBDA_LOG_STREAM_NAME";

/// AWS Lambda resource detector.
///
/// Detects Lambda runtime environment attributes from environment variables
/// and returns an OpenTelemetry `Resource` following semantic conventions.
///
/// This detector checks for the `AWS_EXECUTION_ENV` variable to confirm it's
/// running in a Lambda environment before collecting attributes.
///
/// # Environment Variables
///
/// The detector reads the following Lambda environment variables:
/// - `AWS_REGION` - Cloud region
/// - `AWS_LAMBDA_FUNCTION_NAME` - Function name (faas.name)
/// - `AWS_LAMBDA_FUNCTION_VERSION` - Function version (faas.version)
/// - `AWS_LAMBDA_FUNCTION_MEMORY_SIZE` - Memory in MB (converted to bytes for faas.max_memory)
/// - `AWS_LAMBDA_LOG_GROUP_NAME` - CloudWatch log group (may not be available to extensions)
/// - `AWS_LAMBDA_LOG_STREAM_NAME` - CloudWatch log stream (faas.instance, may not be available)
#[derive(Debug, Default)]
pub struct AwsLambdaDetector;

impl AwsLambdaDetector {
    /// Creates a new AWS Lambda detector.
    pub fn new() -> Self {
        Self
    }
}

impl ResourceDetector for AwsLambdaDetector {
    fn detect(&self) -> Resource {
        let execution_env = env::var(AWS_EXECUTION_ENV).ok();
        if !execution_env
            .as_ref()
            .map(|e| e.starts_with("AWS_Lambda_"))
            .unwrap_or(false)
        {
            return Resource::builder_empty().build();
        }

        let mut attributes = vec![
            KeyValue::new(semconv_attr::CLOUD_PROVIDER, "aws"),
            KeyValue::new(semconv_attr::CLOUD_PLATFORM, "aws_lambda"),
        ];

        if let Ok(region) = env::var(AWS_REGION) {
            attributes.push(KeyValue::new(semconv_attr::CLOUD_REGION, region));
        }

        if let Ok(name) = env::var(AWS_LAMBDA_FUNCTION_NAME) {
            attributes.push(KeyValue::new(semconv_attr::FAAS_NAME, name));
        }

        if let Ok(version) = env::var(AWS_LAMBDA_FUNCTION_VERSION) {
            attributes.push(KeyValue::new(semconv_attr::FAAS_VERSION, version));
        }

        if let Ok(memory) = env::var(AWS_LAMBDA_FUNCTION_MEMORY_SIZE)
            && let Ok(mb) = memory.parse::<i64>()
        {
            attributes.push(KeyValue::new(
                semconv_attr::FAAS_MAX_MEMORY,
                mb * 1024 * 1024,
            ));
        }

        if let Ok(log_group) = env::var(AWS_LAMBDA_LOG_GROUP_NAME) {
            use opentelemetry::StringValue;
            attributes.push(KeyValue::new(
                semconv_res::AWS_LOG_GROUP_NAMES,
                Value::Array(vec![StringValue::from(log_group)].into()),
            ));
        }

        if let Ok(log_stream) = env::var(AWS_LAMBDA_LOG_STREAM_NAME) {
            attributes.push(KeyValue::new(semconv_res::FAAS_INSTANCE, log_stream));
        }

        Resource::builder_empty()
            .with_schema_url(attributes.iter().cloned(), Cow::Borrowed(SCHEMA_URL))
            .build()
    }
}

/// Extension-specific resource detector.
///
/// Adds extension-specific attributes that distinguish the extension's
/// telemetry from the Lambda function's telemetry.
#[derive(Debug, Default)]
pub struct ExtensionDetector;

impl ExtensionDetector {
    /// Creates a new extension detector.
    pub fn new() -> Self {
        Self
    }
}

impl ResourceDetector for ExtensionDetector {
    fn detect(&self) -> Resource {
        let attributes = vec![
            KeyValue::new(semconv_res::SERVICE_NAME, "opentelemetry-lambda-extension"),
            KeyValue::new(
                semconv_res::TELEMETRY_SDK_NAME,
                "opentelemetry-lambda-extension",
            ),
            KeyValue::new(semconv_res::TELEMETRY_SDK_LANGUAGE, "rust"),
            KeyValue::new(
                semconv_res::TELEMETRY_SDK_VERSION,
                env!("CARGO_PKG_VERSION"),
            ),
        ];

        Resource::builder_empty()
            .with_attributes(attributes)
            .build()
    }
}

/// Detects Lambda environment and returns a configured Resource.
///
/// This convenience function creates a Resource by running the Lambda
/// detector and extension detector, merging their results.
pub fn detect_resource() -> Resource {
    let lambda_detector = AwsLambdaDetector::new();
    let extension_detector = ExtensionDetector::new();

    Resource::builder_empty()
        .with_detector(Box::new(lambda_detector))
        .with_detector(Box::new(extension_detector))
        .build()
}

/// Converts an SDK Resource to the protobuf Resource type for OTLP export.
pub fn to_proto_resource(resource: &Resource) -> ProtoResource {
    resource.into()
}

/// Builder for customising Lambda resource attributes.
///
/// This builder wraps the SDK's ResourceBuilder and provides a convenient
/// API for adding custom attributes alongside detected ones.
#[derive(Debug)]
#[must_use = "builders do nothing unless .build() is called"]
pub struct ResourceBuilder {
    inner: opentelemetry_sdk::resource::ResourceBuilder,
}

impl ResourceBuilder {
    /// Creates a new resource builder with Lambda detection enabled.
    pub fn new() -> Self {
        Self {
            inner: Resource::builder_empty()
                .with_detector(Box::new(AwsLambdaDetector::new()))
                .with_detector(Box::new(ExtensionDetector::new())),
        }
    }

    /// Detects Lambda environment attributes automatically.
    ///
    /// This is included by default in `new()`, but can be called again
    /// if you've created an empty builder.
    pub fn detect_lambda_environment(self) -> Self {
        Self {
            inner: self
                .inner
                .with_detector(Box::new(AwsLambdaDetector::new()))
                .with_detector(Box::new(ExtensionDetector::new())),
        }
    }

    /// Adds a custom string attribute.
    pub fn add_string(self, key: impl Into<Cow<'static, str>>, value: impl Into<String>) -> Self {
        Self {
            inner: self
                .inner
                .with_attribute(KeyValue::new(key.into(), value.into())),
        }
    }

    /// Adds a custom integer attribute.
    pub fn add_int(self, key: impl Into<Cow<'static, str>>, value: i64) -> Self {
        Self {
            inner: self.inner.with_attribute(KeyValue::new(key.into(), value)),
        }
    }

    /// Adds a custom boolean attribute.
    pub fn add_bool(self, key: impl Into<Cow<'static, str>>, value: bool) -> Self {
        Self {
            inner: self.inner.with_attribute(KeyValue::new(key.into(), value)),
        }
    }

    /// Builds the SDK Resource.
    pub fn build(self) -> Resource {
        self.inner.build()
    }

    /// Builds and converts to protobuf Resource.
    pub fn build_proto(self) -> ProtoResource {
        to_proto_resource(&self.build())
    }
}

impl Default for ResourceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value;

    fn get_string_value(resource: &Resource, key: &str) -> Option<String> {
        use opentelemetry::Key;
        let owned_key = Key::from(key.to_owned());
        resource.get(&owned_key).and_then(|v| match v {
            Value::String(s) => Some(s.to_string()),
            _ => None,
        })
    }

    fn get_int_value(resource: &Resource, key: &str) -> Option<i64> {
        use opentelemetry::Key;
        let owned_key = Key::from(key.to_owned());
        resource.get(&owned_key).and_then(|v| match v {
            Value::I64(i) => Some(i),
            _ => None,
        })
    }

    #[test]
    fn test_lambda_detector_outside_lambda() {
        // Without AWS_EXECUTION_ENV, should return empty resource
        temp_env::with_vars([(AWS_EXECUTION_ENV, None::<&str>)], || {
            let detector = AwsLambdaDetector::new();
            let resource = detector.detect();
            assert!(resource.is_empty());
        });
    }

    #[test]
    fn test_lambda_detector_in_lambda() {
        temp_env::with_vars(
            [
                (AWS_EXECUTION_ENV, Some("AWS_Lambda_nodejs18.x")),
                (AWS_REGION, Some("us-east-1")),
                (AWS_LAMBDA_FUNCTION_NAME, Some("test-function")),
                (AWS_LAMBDA_FUNCTION_VERSION, Some("$LATEST")),
                (AWS_LAMBDA_FUNCTION_MEMORY_SIZE, Some("128")),
            ],
            || {
                let detector = AwsLambdaDetector::new();
                let resource = detector.detect();

                assert_eq!(
                    get_string_value(&resource, semconv::CLOUD_PROVIDER),
                    Some("aws".to_string())
                );
                assert_eq!(
                    get_string_value(&resource, semconv::CLOUD_PLATFORM),
                    Some("aws_lambda".to_string())
                );
                assert_eq!(
                    get_string_value(&resource, semconv::CLOUD_REGION),
                    Some("us-east-1".to_string())
                );
                assert_eq!(
                    get_string_value(&resource, semconv::FAAS_NAME),
                    Some("test-function".to_string())
                );
                assert_eq!(
                    get_string_value(&resource, semconv::FAAS_VERSION),
                    Some("$LATEST".to_string())
                );
                assert_eq!(
                    get_int_value(&resource, semconv::FAAS_MAX_MEMORY),
                    Some(128 * 1024 * 1024)
                );
            },
        );
    }

    #[test]
    fn test_extension_detector() {
        let detector = ExtensionDetector::new();
        let resource = detector.detect();

        assert_eq!(
            get_string_value(&resource, semconv::SERVICE_NAME),
            Some("opentelemetry-lambda-extension".to_string())
        );
        assert_eq!(
            get_string_value(&resource, semconv::TELEMETRY_SDK_NAME),
            Some("opentelemetry-lambda-extension".to_string())
        );
        assert_eq!(
            get_string_value(&resource, semconv::TELEMETRY_SDK_LANGUAGE),
            Some("rust".to_string())
        );
        assert!(get_string_value(&resource, semconv::TELEMETRY_SDK_VERSION).is_some());
    }

    #[test]
    fn test_resource_builder_custom_attributes() {
        temp_env::with_vars([(AWS_EXECUTION_ENV, None::<&str>)], || {
            let resource = ResourceBuilder::new()
                .add_string("custom.string", "value")
                .add_int("custom.int", 42)
                .add_bool("custom.bool", true)
                .build();

            assert_eq!(
                get_string_value(&resource, "custom.string"),
                Some("value".to_string())
            );
            assert_eq!(get_int_value(&resource, "custom.int"), Some(42));
        });
    }

    #[test]
    fn test_detect_resource() {
        temp_env::with_vars([(AWS_EXECUTION_ENV, None::<&str>)], || {
            let resource = detect_resource();
            // Should at least have extension detector attributes
            assert!(get_string_value(&resource, semconv::SERVICE_NAME).is_some());
        });
    }

    #[test]
    fn test_to_proto_resource() {
        let resource = Resource::builder_empty()
            .with_attribute(KeyValue::new("test.key", "test.value"))
            .build();

        let proto = to_proto_resource(&resource);

        assert!(!proto.attributes.is_empty());
        let attr = &proto.attributes[0];
        assert_eq!(attr.key, "test.key");
        match &attr.value {
            Some(v) => match &v.value {
                Some(any_value::Value::StringValue(s)) => assert_eq!(s, "test.value"),
                _ => panic!("Expected string value"),
            },
            None => panic!("Expected value"),
        }
    }

    #[test]
    fn test_service_name_is_extension_not_function() {
        temp_env::with_vars(
            [
                (AWS_EXECUTION_ENV, Some("AWS_Lambda_nodejs18.x")),
                (AWS_LAMBDA_FUNCTION_NAME, Some("my-lambda-function")),
            ],
            || {
                let resource = detect_resource();

                let service_name = get_string_value(&resource, semconv::SERVICE_NAME);
                let faas_name = get_string_value(&resource, semconv::FAAS_NAME);

                assert_eq!(
                    service_name,
                    Some("opentelemetry-lambda-extension".to_string()),
                    "service.name should be the extension name, not the function name"
                );

                assert_eq!(
                    faas_name,
                    Some("my-lambda-function".to_string()),
                    "faas.name should be the Lambda function name"
                );
            },
        );
    }

    #[test]
    fn test_faas_instance_from_log_stream() {
        temp_env::with_vars(
            [
                (AWS_EXECUTION_ENV, Some("AWS_Lambda_nodejs18.x")),
                (
                    AWS_LAMBDA_LOG_STREAM_NAME,
                    Some("2024/01/01/[$LATEST]abc123"),
                ),
            ],
            || {
                let detector = AwsLambdaDetector::new();
                let resource = detector.detect();

                assert_eq!(
                    get_string_value(&resource, semconv::FAAS_INSTANCE),
                    Some("2024/01/01/[$LATEST]abc123".to_string())
                );
            },
        );
    }
}

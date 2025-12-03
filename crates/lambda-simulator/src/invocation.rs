//! Invocation lifecycle management and data structures.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Represents a Lambda invocation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invocation {
    /// Unique identifier for this invocation.
    pub request_id: String,

    /// The invocation payload (function input).
    pub payload: Value,

    /// Timestamp when the invocation was created.
    pub created_at: DateTime<Utc>,

    /// Deadline by which the invocation must complete.
    pub deadline: DateTime<Utc>,

    /// AWS request ID (for X-Ray tracing).
    pub aws_request_id: String,

    /// ARN of the function being invoked.
    pub invoked_function_arn: String,

    /// AWS X-Ray trace ID.
    pub trace_id: String,

    /// Client context (for mobile SDK).
    pub client_context: Option<String>,

    /// Cognito identity (for mobile SDK).
    pub cognito_identity: Option<String>,
}

impl Invocation {
    /// Creates a new invocation with default values.
    ///
    /// # Arguments
    ///
    /// * `payload` - The JSON payload for this invocation
    /// * `timeout_ms` - Timeout in milliseconds for this invocation
    ///
    /// # Returns
    ///
    /// A new `Invocation` instance with generated IDs and timestamps.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::json;
    /// use lambda_simulator::invocation::Invocation;
    ///
    /// let invocation = Invocation::new(json!({"key": "value"}), 3000);
    /// assert!(!invocation.request_id.is_empty());
    /// ```
    pub fn new(payload: Value, timeout_ms: u64) -> Self {
        let request_id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let deadline = created_at + chrono::Duration::milliseconds(timeout_ms as i64);

        let trace_id = Self::generate_trace_id(created_at);

        Self {
            request_id: request_id.clone(),
            payload,
            created_at,
            deadline,
            aws_request_id: request_id.clone(),
            invoked_function_arn: "arn:aws:lambda:us-east-1:123456789012:function:test-function"
                .to_string(),
            trace_id,
            client_context: None,
            cognito_identity: None,
        }
    }

    /// Generates an AWS X-Ray trace ID in the correct format.
    ///
    /// Format: Root=1-{8-hex-time}-{24-hex-random}
    /// Where time is Unix timestamp and random is 96 random bits.
    fn generate_trace_id(timestamp: DateTime<Utc>) -> String {
        let epoch_time = timestamp.timestamp() as u32;
        let random_bytes = Uuid::new_v4();
        let random_hex = format!("{:032x}", random_bytes.as_u128())
            .chars()
            .take(24)
            .collect::<String>();

        format!("Root=1-{:08x}-{}", epoch_time, random_hex)
    }

    /// Returns the deadline as milliseconds since Unix epoch.
    ///
    /// This is used for the `Lambda-Runtime-Deadline-Ms` header.
    pub fn deadline_ms(&self) -> i64 {
        self.deadline.timestamp_millis()
    }
}

/// Builder for creating invocations with custom properties.
#[derive(Debug, Default)]
#[must_use = "builders do nothing unless .build() is called"]
pub struct InvocationBuilder {
    payload: Option<Value>,
    timeout_ms: Option<u64>,
    function_arn: Option<String>,
    client_context: Option<String>,
    cognito_identity: Option<String>,
}

impl InvocationBuilder {
    /// Creates a new invocation builder.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::json;
    /// use lambda_simulator::invocation::InvocationBuilder;
    ///
    /// let invocation = InvocationBuilder::new()
    ///     .payload(json!({"key": "value"}))
    ///     .build();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the payload for the invocation.
    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Sets the timeout in milliseconds.
    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Sets the function ARN.
    pub fn function_arn(mut self, arn: impl Into<String>) -> Self {
        self.function_arn = Some(arn.into());
        self
    }

    /// Sets the client context.
    pub fn client_context(mut self, context: impl Into<String>) -> Self {
        self.client_context = Some(context.into());
        self
    }

    /// Sets the Cognito identity.
    pub fn cognito_identity(mut self, identity: impl Into<String>) -> Self {
        self.cognito_identity = Some(identity.into());
        self
    }

    /// Builds the invocation.
    ///
    /// # Errors
    ///
    /// Returns `BuilderError::MissingField` if no payload was provided.
    ///
    /// # Examples
    ///
    /// ```
    /// use lambda_simulator::InvocationBuilder;
    /// use serde_json::json;
    ///
    /// let invocation = InvocationBuilder::new()
    ///     .payload(json!({"key": "value"}))
    ///     .build()
    ///     .expect("Failed to build invocation");
    /// ```
    pub fn build(self) -> Result<Invocation, crate::error::BuilderError> {
        let payload = self
            .payload
            .ok_or_else(|| crate::error::BuilderError::MissingField("payload".to_string()))?;
        let timeout_ms = self.timeout_ms.unwrap_or(3000);

        let mut invocation = Invocation::new(payload, timeout_ms);

        if let Some(arn) = self.function_arn {
            invocation.invoked_function_arn = arn;
        }

        if let Some(context) = self.client_context {
            invocation.client_context = Some(context);
        }

        if let Some(identity) = self.cognito_identity {
            invocation.cognito_identity = Some(identity);
        }

        Ok(invocation)
    }
}

/// The status of an invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationStatus {
    /// Invocation is queued and waiting to be processed.
    Pending,

    /// Invocation has been sent to the runtime.
    InProgress,

    /// Invocation completed successfully.
    Success,

    /// Invocation failed with an error.
    Error,

    /// Invocation timed out.
    Timeout,
}

/// Response from a successful invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationResponse {
    /// The request ID this response is for.
    pub request_id: String,

    /// The response payload.
    pub payload: Value,

    /// Timestamp when the response was received.
    pub received_at: DateTime<Utc>,
}

/// Error response from a failed invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationError {
    /// The request ID this error is for.
    pub request_id: String,

    /// Error type.
    pub error_type: String,

    /// Error message.
    pub error_message: String,

    /// Stack trace if available.
    pub stack_trace: Option<Vec<String>>,

    /// Timestamp when the error was received.
    pub received_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn invocation_new_produces_valid_ids(timeout_ms in 1u64..=900000u64) {
            let invocation = Invocation::new(serde_json::json!({}), timeout_ms);

            prop_assert!(!invocation.request_id.is_empty());
            prop_assert!(!invocation.aws_request_id.is_empty());
            prop_assert_eq!(invocation.request_id, invocation.aws_request_id);
        }

        #[test]
        fn invocation_deadline_is_in_future(timeout_ms in 1u64..=900000u64) {
            let invocation = Invocation::new(serde_json::json!({}), timeout_ms);

            prop_assert!(invocation.deadline > invocation.created_at);
            prop_assert!(invocation.deadline_ms() > invocation.created_at.timestamp_millis());
        }

        #[test]
        fn invocation_trace_id_format_is_valid(timeout_ms in 1u64..=900000u64) {
            let invocation = Invocation::new(serde_json::json!({}), timeout_ms);

            prop_assert!(invocation.trace_id.starts_with("Root=1-"));
            let parts: Vec<_> = invocation.trace_id.split('-').collect();
            prop_assert_eq!(parts.len(), 3);
            prop_assert_eq!(parts[1].len(), 8);
            prop_assert_eq!(parts[2].len(), 24);
        }

        #[test]
        fn builder_produces_same_result_as_new(timeout_ms in 1u64..=900000u64) {
            let payload = serde_json::json!({"test": true});

            let from_builder = InvocationBuilder::new()
                .payload(payload.clone())
                .timeout_ms(timeout_ms)
                .build()
                .unwrap();

            prop_assert!(!from_builder.request_id.is_empty());
            prop_assert_eq!(from_builder.payload, payload);
        }

        #[test]
        fn builder_without_payload_fails(_timeout_ms in 1u64..=900000u64) {
            let result = InvocationBuilder::new().build();
            prop_assert!(result.is_err());
        }
    }
}

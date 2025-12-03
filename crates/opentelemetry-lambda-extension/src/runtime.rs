//! Extension runtime orchestrator.
//!
//! This module provides the main runtime that coordinates all extension components:
//! - Extensions API client for lifecycle management (via `lambda_extension` crate)
//! - Telemetry API subscription for platform events
//! - OTLP receiver for function telemetry
//! - Signal aggregation and export

use crate::config::Config;
use crate::receiver::{OtlpReceiver, Signal};
use crate::resource::{ResourceBuilder, detect_resource, to_proto_resource};
use crate::service::{EventsService, ExtensionState, TelemetryService};
use lambda_extension::{Extension, SharedService};
use opentelemetry_sdk::resource::Resource as SdkResource;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Extension runtime that orchestrates all components.
pub struct ExtensionRuntime {
    config: Config,
    cancel_token: CancellationToken,
    resource: SdkResource,
}

impl ExtensionRuntime {
    /// Creates a new extension runtime with the given configuration.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            cancel_token: CancellationToken::new(),
            resource: detect_resource(),
        }
    }

    /// Creates a runtime with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(Config::default())
    }

    /// Sets a custom resource for this runtime.
    pub fn with_resource(mut self, resource: SdkResource) -> Self {
        self.resource = resource;
        self
    }

    /// Returns a handle to the cancellation token.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Runs the extension runtime.
    ///
    /// This method uses the official `lambda_extension` crate with Tower services
    /// for proper lifecycle management. It:
    /// 1. Registers with the Extensions API
    /// 2. Subscribes to the Telemetry API for platform events
    /// 3. Starts the OTLP receiver for function telemetry
    /// 4. Runs the event loop until shutdown
    ///
    /// # Errors
    ///
    /// Returns an error if any component fails to start or if the extension
    /// cannot register with Lambda.
    pub async fn run(self) -> Result<(), RuntimeError> {
        tracing::debug!("Starting extension with lambda_extension crate");

        // Create shared state for services
        // Convert SDK Resource to proto Resource for internal use
        let proto_resource = to_proto_resource(&self.resource);
        let state = Arc::new(
            ExtensionState::new(self.config.clone(), proto_resource)
                .map_err(|e| RuntimeError::StateInit(Box::new(e)))?,
        );
        tracing::debug!("Extension state created");

        // Create Tower services
        let events_service = EventsService::new(Arc::clone(&state));
        let telemetry_service = TelemetryService::new(Arc::clone(&state));

        // Start OTLP receiver for function telemetry
        let signal_tx = {
            let aggregator = Arc::clone(&state.aggregator);
            let (tx, mut rx) = mpsc::channel::<Signal>(self.config.telemetry_api.buffer_size);

            // Spawn task to forward signals to aggregator
            tokio::spawn(async move {
                while let Some(signal) = rx.recv().await {
                    aggregator.add(signal).await;
                }
            });

            tx
        };

        let receiver = OtlpReceiver::new(
            self.config.receiver.clone(),
            signal_tx,
            self.cancel_token.clone(),
        );

        let (_receiver_handle, receiver_future) = receiver
            .start()
            .await
            .map_err(RuntimeError::ReceiverStart)?;

        let receiver_task = tokio::spawn(receiver_future);

        // Build and run the extension using lambda_extension
        // Note: Each with_* method returns a new type, so we must chain or branch.
        // We always enable telemetry processing when using this method since it's
        // the recommended path for proper lifecycle handling.
        tracing::debug!("Building Extension and starting run loop");
        let result = Extension::new()
            .with_events_processor(events_service)
            .with_telemetry_types(&["platform", "function", "extension"])
            .with_telemetry_processor(SharedService::new(telemetry_service))
            .run()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Extension run failed");
                RuntimeError::EventLoop(e)
            });
        tracing::debug!(?result, "Extension finished");

        self.cancel_token.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), receiver_task).await;

        result
    }
}

/// Errors from the extension runtime.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Failed to create extension state during initialisation.
    #[error("failed to create extension state")]
    StateInit(#[source] Box<crate::exporter::ExportError>),

    /// Failed to start OTLP receiver.
    #[error("failed to start OTLP receiver")]
    ReceiverStart(#[source] std::io::Error),

    /// Event loop encountered an error.
    #[error("event loop error")]
    EventLoop(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Builder for configuring the extension runtime.
#[must_use = "builders do nothing unless .build() is called"]
pub struct RuntimeBuilder {
    config: Config,
    resource: Option<SdkResource>,
}

impl RuntimeBuilder {
    /// Creates a new runtime builder with default configuration.
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            resource: None,
        }
    }

    /// Sets the configuration.
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Sets the OTLP exporter endpoint.
    pub fn exporter_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.exporter.endpoint = Some(endpoint.into());
        self
    }

    /// Sets the flush strategy.
    pub fn flush_strategy(mut self, strategy: crate::config::FlushStrategy) -> Self {
        self.config.flush.strategy = strategy;
        self
    }

    /// Disables the Telemetry API subscription.
    pub fn disable_telemetry_api(mut self) -> Self {
        self.config.telemetry_api.enabled = false;
        self
    }

    /// Sets a custom resource.
    pub fn resource(mut self, resource: SdkResource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Adds custom resource attributes.
    pub fn with_resource_attributes<F>(mut self, f: F) -> Self
    where
        F: FnOnce(ResourceBuilder) -> ResourceBuilder,
    {
        let builder = ResourceBuilder::new();
        self.resource = Some(f(builder).build());
        self
    }

    /// Builds the extension runtime.
    pub fn build(self) -> ExtensionRuntime {
        let mut runtime = ExtensionRuntime::new(self.config);
        if let Some(resource) = self.resource {
            runtime = runtime.with_resource(resource);
        }
        runtime
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_builder() {
        let runtime = RuntimeBuilder::new()
            .exporter_endpoint("http://localhost:4318")
            .flush_strategy(crate::config::FlushStrategy::End)
            .disable_telemetry_api()
            .build();

        assert_eq!(
            runtime.config.exporter.endpoint,
            Some("http://localhost:4318".to_string())
        );
        assert_eq!(
            runtime.config.flush.strategy,
            crate::config::FlushStrategy::End
        );
        assert!(!runtime.config.telemetry_api.enabled);
    }

    #[test]
    fn test_runtime_with_defaults() {
        let runtime = ExtensionRuntime::with_defaults();
        assert!(runtime.config.telemetry_api.enabled);
    }

    #[test]
    fn test_runtime_error_display() {
        use std::error::Error;

        let io_err = std::io::Error::new(std::io::ErrorKind::AddrInUse, "port in use");
        let err = RuntimeError::ReceiverStart(io_err);

        assert!(format!("{}", err).contains("receiver"));
        assert!(err.source().is_some());
    }

    #[test]
    fn test_cancellation_token() {
        let runtime = ExtensionRuntime::with_defaults();
        let token = runtime.cancellation_token();
        assert!(!token.is_cancelled());
    }
}

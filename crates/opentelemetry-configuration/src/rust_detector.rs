//! Rust-specific resource detection.
//!
//! This module provides:
//! - [`RustResourceDetector`] - Automatic detection of Rust runtime attributes
//! - [`RustBuildInfo`] - Struct for build-time rustc information
//! - [`emit_rustc_env`] - Build.rs helper to capture rustc version

use opentelemetry::KeyValue;
use opentelemetry_sdk::resource::{Resource, ResourceDetector};
use opentelemetry_semantic_conventions::resource::{
    PROCESS_RUNTIME_DESCRIPTION, PROCESS_RUNTIME_NAME, PROCESS_RUNTIME_VERSION,
};

/// Detects Rust runtime resource attributes.
///
/// Captures metadata available at runtime without requiring build.rs:
/// - `process.runtime.name` = "rust" (semantic convention)
/// - `rust.target_os`, `rust.target_arch`, `rust.target_family`
/// - `rust.debug` (true for debug builds)
/// - `process.executable.size` (binary size in bytes)
///
/// For rustc version and channel, use [`emit_rustc_env`] in build.rs combined
/// with the [`capture_rust_build_info!`](crate::capture_rust_build_info) macro.
pub struct RustResourceDetector;

impl ResourceDetector for RustResourceDetector {
    fn detect(&self) -> Resource {
        let mut attrs = vec![
            KeyValue::new(PROCESS_RUNTIME_NAME, "rust"),
            KeyValue::new("rust.target_os", std::env::consts::OS),
            KeyValue::new("rust.target_arch", std::env::consts::ARCH),
            KeyValue::new("rust.target_family", std::env::consts::FAMILY),
            KeyValue::new("rust.debug", cfg!(debug_assertions)),
        ];

        // Binary size
        if let Ok(exe_path) = std::env::current_exe()
            && let Ok(metadata) = std::fs::metadata(&exe_path)
        {
            attrs.push(KeyValue::new(
                "process.executable.size",
                metadata.len() as i64,
            ));
        }

        Resource::builder().with_attributes(attrs).build()
    }
}

/// Rust build-time information captured via build.rs.
///
/// Use [`emit_rustc_env`] in your build.rs and [`capture_rust_build_info!`](crate::capture_rust_build_info)
/// in your application code to populate this struct.
///
/// # Example
///
/// ```ignore
/// // In build.rs:
/// fn main() {
///     opentelemetry_configuration::emit_rustc_env();
/// }
///
/// // In main.rs:
/// let _guard = OtelSdkBuilder::new()
///     .service_name("my-service")
///     .with_rust_build_info(opentelemetry_configuration::capture_rust_build_info!())
///     .build()?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct RustBuildInfo {
    /// Rustc version (e.g., "1.84.0").
    pub rustc_version: Option<&'static str>,
    /// Rust release channel ("stable", "beta", "nightly").
    pub rust_channel: Option<&'static str>,
    /// Full rustc version string (e.g., "rustc 1.84.0 (9fc6b4312 2024-01-04)").
    pub rustc_version_full: Option<&'static str>,
}

impl RustBuildInfo {
    /// Converts to OpenTelemetry KeyValue pairs for resource attributes.
    ///
    /// Returns attributes using semantic conventions where applicable:
    /// - `process.runtime.version` for rustc version
    /// - `process.runtime.description` for full version string
    /// - `rust.channel` for release channel
    #[must_use]
    pub fn to_key_values(&self) -> Vec<KeyValue> {
        let mut attrs = Vec::new();

        if let Some(version) = self.rustc_version {
            attrs.push(KeyValue::new(PROCESS_RUNTIME_VERSION, version));
        }
        if let Some(channel) = self.rust_channel {
            attrs.push(KeyValue::new("rust.channel", channel));
        }
        if let Some(full) = self.rustc_version_full {
            attrs.push(KeyValue::new(PROCESS_RUNTIME_DESCRIPTION, full));
        }

        attrs
    }
}

/// Emits rustc version information as cargo environment variables.
///
/// Call this from your `build.rs` to capture rustc version at compile time.
/// The emitted environment variables can then be read using the
/// [`capture_rust_build_info!`](crate::capture_rust_build_info) macro.
///
/// # Environment Variables Emitted
///
/// - `RUSTC_VERSION` - The rustc version number (e.g., "1.84.0")
/// - `RUSTC_VERSION_FULL` - Full version string (e.g., "rustc 1.84.0 (9fc6b4312 2024-01-04)")
/// - `RUST_CHANNEL` - Release channel ("stable", "beta", or "nightly")
///
/// # Example
///
/// ```ignore
/// // In build.rs:
/// fn main() {
///     opentelemetry_configuration::emit_rustc_env();
/// }
/// ```
pub fn emit_rustc_env() {
    use std::process::Command;

    println!("cargo::rerun-if-env-changed=RUSTC");

    // Get rustc path (respect RUSTC env var if set)
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());

    // Get rustc version
    if let Ok(output) = Command::new(&rustc).arg("--version").output()
        && let Ok(version_str) = String::from_utf8(output.stdout)
    {
        let version_str = version_str.trim();

        // Full version string: "rustc 1.84.0 (9fc6b4312 2024-01-04)"
        println!("cargo::rustc-env=RUSTC_VERSION_FULL={version_str}");

        // Parse version number: "1.84.0"
        if let Some(version) = version_str.strip_prefix("rustc ")
            && let Some(ver) = version.split_whitespace().next()
        {
            println!("cargo::rustc-env=RUSTC_VERSION={ver}");
        }
    }

    // Get channel from rustc -vV
    if let Ok(output) = Command::new(&rustc).arg("-vV").output()
        && let Ok(verbose) = String::from_utf8(output.stdout)
    {
        for line in verbose.lines() {
            if let Some(release) = line.strip_prefix("release: ") {
                // Determine channel from version suffix
                let channel_name = if release.contains("nightly") {
                    "nightly"
                } else if release.contains("beta") {
                    "beta"
                } else {
                    "stable"
                };
                println!("cargo::rustc-env=RUST_CHANNEL={channel_name}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::resource::ResourceDetector;

    #[test]
    fn test_rust_detector_returns_resource() {
        let detector = RustResourceDetector;
        let resource = detector.detect();
        // Resource should have at least the basic attributes
        assert!(resource.iter().count() >= 5);
    }

    #[test]
    fn test_rust_detector_includes_runtime_name() {
        let detector = RustResourceDetector;
        let resource = detector.detect();

        let runtime_name = resource
            .iter()
            .find(|(k, _)| k.as_str() == PROCESS_RUNTIME_NAME);
        assert!(runtime_name.is_some());
    }

    #[test]
    fn test_rust_build_info_to_key_values_empty() {
        let info = RustBuildInfo::default();
        assert!(info.to_key_values().is_empty());
    }

    #[test]
    fn test_rust_build_info_to_key_values_with_data() {
        let info = RustBuildInfo {
            rustc_version: Some("1.84.0"),
            rust_channel: Some("stable"),
            rustc_version_full: Some("rustc 1.84.0"),
        };
        let kvs = info.to_key_values();
        assert_eq!(kvs.len(), 3);
    }

    #[test]
    fn test_rust_build_info_partial_data() {
        let info = RustBuildInfo {
            rustc_version: Some("1.84.0"),
            rust_channel: None,
            rustc_version_full: None,
        };
        let kvs = info.to_key_values();
        assert_eq!(kvs.len(), 1);
    }

    #[test]
    fn test_std_consts_not_empty() {
        // Verify that std::env::consts values are non-empty
        assert!(!std::env::consts::OS.is_empty());
        assert!(!std::env::consts::ARCH.is_empty());
        // FAMILY can be empty on some platforms, so we just check it exists
        let _ = std::env::consts::FAMILY;
    }
}

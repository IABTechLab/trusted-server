use error_stack::Report;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Test error types (platform-agnostic)
#[derive(Debug, derive_more::Display)]
pub enum TestError {
    // Runtime environment errors
    #[display("Failed to spawn runtime process")]
    RuntimeSpawn,

    #[display("Runtime not ready after timeout")]
    RuntimeNotReady,

    // Container errors
    #[display("Container failed to start: {reason}")]
    ContainerStart { reason: String },

    #[display("Container operation timed out")]
    ContainerTimeout,

    // HTTP errors
    #[display("HTTP request failed")]
    HttpRequest,

    #[display("Failed to parse response")]
    ResponseParse,

    // Assertion errors
    #[display("Script tag not found in HTML")]
    ScriptTagNotFound,

    #[display("Duplicate trustedserver-js script tags found in HTML")]
    DuplicateScriptTag,

    #[display("Script tag was injected when it should not have been")]
    UnexpectedScriptInjection,

    #[display("Response body did not match expected content")]
    UnexpectedContent,

    #[display("Invalid CSS selector")]
    InvalidSelector,

    #[display("Origin URL not rewritten in HTML attributes")]
    AttributeNotRewritten,

    // Resource errors
    #[display("No available port found")]
    NoPortAvailable,
}

impl core::error::Error for TestError {}

/// Result type used across integration tests.
pub type TestResult<T> = core::result::Result<T, Report<TestError>>;

/// Platform-agnostic process handle
pub struct RuntimeProcess {
    // Held for its Drop impl — the handle kills the child process on cleanup.
    #[allow(dead_code)]
    pub inner: Box<dyn RuntimeProcessHandle>,
    pub base_url: String,
}

/// Marker trait for type-erased runtime handles stored in [`RuntimeProcess`].
///
/// Concrete handle types own their cleanup through [`Drop`]; this trait exists
/// so tests can hold platform-specific processes behind a single boxed type.
pub trait RuntimeProcessHandle: Send + Sync {}

/// Trait defining how to run the trusted-server on different platforms.
///
/// The application configuration is loaded at runtime from the platform config
/// store. Test environments render a local Viceroy/Fastly config that projects
/// a canonical TOML payload into the fixed `ts-config` key before spawning the
/// runtime.
pub trait RuntimeEnvironment: Send + Sync {
    /// Platform identifier (e.g., "fastly", "cloudflare")
    fn id(&self) -> &'static str;

    /// Spawn runtime with the given WASM binary.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the process cannot be started.
    /// Returns [`TestError::RuntimeNotReady`] if the health check times out.
    fn spawn(&self, wasm_path: &Path) -> TestResult<RuntimeProcess>;

    /// Health check endpoint (may differ by platform)
    fn health_check_path(&self) -> &str {
        "/health"
    }

    /// Platform-specific environment variables
    // Provided for future runtime implementations; not yet called in-tree.
    #[allow(dead_code)]
    fn env_vars(&self) -> HashMap<String, String> {
        HashMap::new()
    }
}

/// Get path to WASM binary, respecting environment variable.
pub fn wasm_binary_path() -> PathBuf {
    std::env::var("WASM_BINARY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm")
        })
}

/// Get the fixed origin port used for Docker container port mapping.
///
/// This must match the origin URL stored in the integration-test application
/// config fixture that is projected into the local config store.
pub fn origin_port() -> u16 {
    match std::env::var("INTEGRATION_ORIGIN_PORT") {
        Ok(value) => value
            .parse()
            .expect("should parse INTEGRATION_ORIGIN_PORT as u16"),
        Err(_) => 8888,
    }
}

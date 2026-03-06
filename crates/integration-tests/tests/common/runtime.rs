use error_stack::Result;
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

    #[display("Failed to kill runtime process")]
    RuntimeKill,

    #[display("Failed to wait for runtime process")]
    RuntimeWait,

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

    #[display("Invalid CSS selector")]
    InvalidSelector,

    #[display("Origin URL not rewritten in HTML attributes")]
    AttributeNotRewritten,

    // Resource errors
    #[display("No available port found")]
    NoPortAvailable,
}

impl core::error::Error for TestError {}

/// Platform-agnostic process handle
pub struct RuntimeProcess {
    pub inner: Box<dyn RuntimeProcessHandle>,
    pub base_url: String,
}

/// Trait for runtime process lifecycle management
pub trait RuntimeProcessHandle: Send + Sync {
    fn kill(&mut self) -> Result<(), TestError>;
    fn wait(&mut self) -> Result<(), TestError>;
}

/// Trait defining how to run the trusted-server on different platforms.
///
/// The application configuration (origin URL, integrations, etc.) is baked
/// into the WASM binary at build time via `build.rs`. The runtime environment
/// only needs the WASM binary path and its own platform-specific config
/// (e.g. Viceroy's `fastly.toml` for KV stores and secret stores).
pub trait RuntimeEnvironment: Send + Sync {
    /// Platform identifier (e.g., "fastly", "cloudflare")
    fn id(&self) -> &'static str;

    /// Spawn runtime with the given WASM binary.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the process cannot be started.
    /// Returns [`TestError::RuntimeNotReady`] if the health check times out.
    fn spawn(&self, wasm_path: &Path) -> Result<RuntimeProcess, TestError>;

    /// Health check endpoint (may differ by platform)
    fn health_check_path(&self) -> &str {
        "/health"
    }

    /// Platform-specific environment variables
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
                .join("../../target/wasm32-wasip1/release/trusted-server-fastly.wasm")
        })
}

/// Get the fixed origin port used for Docker container port mapping.
///
/// This must match the port baked into the WASM binary via
/// `TRUSTED_SERVER__PUBLISHER__ORIGIN_URL` at build time.
pub fn origin_port() -> u16 {
    std::env::var("INTEGRATION_ORIGIN_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8888)
}

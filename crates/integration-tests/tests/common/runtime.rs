use error_stack::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

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

    #[display("Missing module: {module}")]
    MissingModule { module: String },

    #[display("Invalid CSS selector")]
    InvalidSelector,

    #[display("Element not found")]
    ElementNotFound,

    #[display("Attribute not rewritten")]
    AttributeNotRewritten,

    #[display("GDPR signal missing from response")]
    GdprSignalMissing,

    // Configuration errors
    #[display("Config parse error")]
    ConfigParse,

    #[display("Config write error")]
    ConfigWrite,

    #[display("Config serialization error")]
    ConfigSerialize,

    // Resource errors
    #[display("WASM binary not found")]
    WasmBinaryNotFound,

    #[display("No available port found")]
    NoPortAvailable,
}

impl core::error::Error for TestError {}

/// Configuration for runtime environments.
///
/// Holds the temp file so the config is not deleted while the runtime is alive.
pub struct RuntimeConfig {
    /// Handle to the temp config file — dropped when `RuntimeConfig` is dropped.
    _config_file: NamedTempFile,
    wasm_path: PathBuf,
}

impl RuntimeConfig {
    /// Create a new runtime configuration from a temp file and WASM binary path.
    pub fn new(config_file: NamedTempFile, wasm_path: PathBuf) -> Self {
        Self {
            _config_file: config_file,
            wasm_path,
        }
    }

    /// Path to the generated config file on disk.
    pub fn config_path(&self) -> &Path {
        self._config_file.path()
    }

    /// Path to the WASM binary.
    pub fn wasm_path(&self) -> &Path {
        &self.wasm_path
    }
}

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

/// Trait defining how to run the trusted-server on different platforms
pub trait RuntimeEnvironment: Send + Sync {
    /// Platform identifier (e.g., "fastly", "cloudflare")
    fn id(&self) -> &'static str;

    /// Spawn runtime with platform-specific configuration
    fn spawn(&self, config: &RuntimeConfig) -> Result<RuntimeProcess, TestError>;

    /// Platform-specific configuration template
    fn config_template(&self) -> &str;

    /// Health check endpoint (may differ by platform)
    fn health_check_path(&self) -> &str {
        "/health"
    }

    /// Platform-specific environment variables
    fn env_vars(&self) -> HashMap<String, String> {
        HashMap::new()
    }
}

/// Get path to WASM binary, respecting environment variable
pub fn wasm_binary_path() -> PathBuf {
    std::env::var("WASM_BINARY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/wasm32-wasip1/release/trusted-server-fastly.wasm")
        })
}

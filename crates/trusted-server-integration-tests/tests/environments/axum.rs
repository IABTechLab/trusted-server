use crate::common::config::integration_app_config_envelope;
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
use error_stack::ResultExt as _;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use trusted_server_core::config_payload::CONFIG_BLOB_KEY;

/// Default port the Axum dev server binds to when no `PORT` env var is supplied.
const AXUM_DEFAULT_PORT: u16 = 8787;

/// Axum native dev-server runtime environment.
///
/// Spawns the pre-built `trusted-server-axum` binary directly (no WASM, no
/// Viceroy). The binary must have been built before running integration tests:
///
/// ```sh
/// cargo build -p trusted-server-adapter-axum
/// ```
///
/// The WASM binary path argument is unused — it exists only to satisfy the
/// [`RuntimeEnvironment`] trait shared with Fastly.
pub struct AxumDevServer;

impl RuntimeEnvironment for AxumDevServer {
    fn id(&self) -> &'static str {
        "axum"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let binary = self.binary_path();
        let port = super::find_available_port().unwrap_or(AXUM_DEFAULT_PORT);

        let app_config = integration_app_config_envelope(origin_port())?;
        let config_segment = CONFIG_BLOB_KEY
            .to_ascii_uppercase()
            .replace(['-', '.', ' '], "_");
        let config_variable = format!("TRUSTED_SERVER_CONFIG_{config_segment}_{config_segment}");

        let mut child = Command::new(&binary)
            .env("PORT", port.to_string())
            .env(config_variable, app_config)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach(format!(
                "Failed to spawn trusted-server-axum binary at {}",
                binary.display()
            ))?;

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("axum: {line}");
                    }
                }
            });
        }

        let handle = AxumHandle { child };
        let base_url = format!("http://127.0.0.1:{port}");

        // The Axum dev server returns 403 at root (no publisher config in test env),
        // so we poll until we get any HTTP response rather than a specific status.
        wait_for_any_response(&base_url)?;

        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    fn health_check_path(&self) -> &str {
        "/health"
    }
}

impl AxumDevServer {
    /// Resolve the path to the compiled `trusted-server-axum` binary.
    ///
    /// Respects the `AXUM_BINARY_PATH` environment variable for CI overrides.
    /// Falls back to the workspace `target/debug/` directory.
    fn binary_path(&self) -> std::path::PathBuf {
        if let Ok(path) = std::env::var("AXUM_BINARY_PATH") {
            return std::path::PathBuf::from(path);
        }

        // CARGO_MANIFEST_DIR is crates/trusted-server-integration-tests -> go up two levels to workspace root
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/trusted-server-axum")
    }
}

/// Poll until the Axum dev server responds with any HTTP status code.
///
/// The Axum server returns 403 at root when no publisher config is present,
/// which is neither success nor 404, so the standard [`super::wait_for_ready`]
/// helper cannot be used. Any HTTP response means the server is up.
fn wait_for_any_response(base_url: &str) -> TestResult<()> {
    use error_stack::Report;

    let url = format!("{base_url}/");
    for _ in 0..30 {
        if reqwest::blocking::get(&url).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(Report::new(TestError::RuntimeNotReady)
        .attach(format!("Axum dev server at {base_url} not ready after 15s")))
}

/// Process handle for a running Axum dev-server instance.
///
/// Implements [`Drop`] to ensure the process is killed on test cleanup.
struct AxumHandle {
    child: Child,
}

impl RuntimeProcessHandle for AxumHandle {}

impl Drop for AxumHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

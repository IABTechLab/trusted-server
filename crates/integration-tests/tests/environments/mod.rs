pub mod fastly;

use crate::common::runtime::{RuntimeEnvironment, TestError, TestResult};
use error_stack::Report;
use std::time::Duration;

/// Runtime factory function type — avoids trait object static initialization issues.
type RuntimeFactory = fn() -> Box<dyn RuntimeEnvironment>;

/// Registry of all supported runtime environments.
///
/// Uses function pointers instead of `&[&dyn RuntimeEnvironment]` because trait
/// objects cannot be constructed in `const` context for types with non-trivial
/// constructors.
///
/// # Adding a new runtime
///
/// 1. Create `tests/environments/<platform>.rs`
/// 2. Implement [`RuntimeEnvironment`] trait
/// 3. Add factory closure here
pub static RUNTIME_ENVIRONMENTS: &[RuntimeFactory] = &[|| Box::new(fastly::FastlyViceroy)];

/// Readiness polling configuration for runtimes and frontend containers.
pub(crate) struct ReadyCheckOptions {
    pub(crate) max_attempts: usize,
    pub(crate) interval: Duration,
    pub(crate) fallback_to_root: bool,
    pub(crate) timeout_error: TestError,
    pub(crate) timeout_message: String,
}

/// Find an available TCP port for the runtime to bind to.
///
/// Binds to port 0, which asks the OS to assign a random available port,
/// then immediately closes the listener and returns the assigned port.
///
/// # Known limitation — TOCTOU race
///
/// There is an inherent time-of-check/time-of-use (TOCTOU) race between
/// closing the probe listener here and the caller binding the port. Another
/// process can claim the port in that window. In practice the window is
/// negligible — integration tests run with `--test-threads=1` and no other
/// test process competes for ports — but a flake is theoretically possible
/// on heavily loaded CI runners. If the spawn call returns an address-in-use
/// error, the caller should retry with a freshly allocated port.
///
/// # Errors
///
/// Returns [`TestError::NoPortAvailable`] if no port can be allocated.
pub fn find_available_port() -> TestResult<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|_| Report::new(TestError::NoPortAvailable))?;

    let port = listener
        .local_addr()
        .map_err(|_| Report::new(TestError::NoPortAvailable))?
        .port();

    Ok(port)
}

/// Poll a runtime's health endpoint until it responds with success.
///
/// Retries up to 30 times with 500ms delay between attempts (total ~15s).
///
/// # Arguments
///
/// * `base_url` - Runtime base URL, such as `http://127.0.0.1:7676`.
/// * `health_path` - Path to poll for readiness, such as `/health`.
/// * `fallback_to_root` - Whether a successful or 404 response from `/`
///   should also count as ready when the dedicated health endpoint is absent.
///
/// # Errors
///
/// Returns [`TestError::RuntimeNotReady`] if the runtime does not respond within timeout.
pub fn wait_for_ready(base_url: &str, health_path: &str, fallback_to_root: bool) -> TestResult<()> {
    wait_for_http_ready(
        base_url,
        health_path,
        ReadyCheckOptions {
            max_attempts: 30,
            interval: Duration::from_millis(500),
            fallback_to_root,
            timeout_error: TestError::RuntimeNotReady,
            timeout_message: format!("Runtime at {base_url} not ready after 15s"),
        },
    )
}

/// Poll an HTTP endpoint until it responds successfully.
///
/// When `fallback_to_root` is enabled, a successful or 404 response from the
/// base URL also counts as ready. This is useful for runtimes that proxy to an
/// origin but do not expose a dedicated health endpoint.
///
/// # Errors
///
/// Returns the configured timeout error if the endpoint does not become ready.
pub(crate) fn wait_for_http_ready(
    base_url: &str,
    health_path: &str,
    options: ReadyCheckOptions,
) -> TestResult<()> {
    let health_url = format!("{}{}", base_url, health_path);

    for _ in 0..options.max_attempts {
        if let Ok(resp) = reqwest::blocking::get(&health_url)
            && resp.status().is_success()
        {
            return Ok(());
        }

        if options.fallback_to_root
            && health_path != "/"
            && let Ok(resp) = reqwest::blocking::get(base_url)
            && (resp.status().is_success() || resp.status().as_u16() == 404)
        {
            return Ok(());
        }

        std::thread::sleep(options.interval);
    }

    Err(Report::new(options.timeout_error).attach(options.timeout_message))
}

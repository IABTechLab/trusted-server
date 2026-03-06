pub mod fastly;

use crate::common::runtime::{RuntimeEnvironment, TestError};

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
pub static RUNTIME_ENVIRONMENTS: &[RuntimeFactory] = &[
    || Box::new(fastly::FastlyViceroy),
];

/// Find an available TCP port for the runtime to bind to.
///
/// Binds to port 0, which asks the OS to assign a random available port,
/// then immediately closes the listener and returns the assigned port.
///
/// # Errors
///
/// Returns [`TestError::NoPortAvailable`] if no port can be allocated.
pub fn find_available_port() -> error_stack::Result<u16, TestError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|_| error_stack::report!(TestError::NoPortAvailable))?;

    let port = listener
        .local_addr()
        .map_err(|_| error_stack::report!(TestError::NoPortAvailable))?
        .port();

    Ok(port)
}

/// Poll a runtime's health endpoint until it responds with success.
///
/// Retries up to 30 times with 100ms delay between attempts (total ~3s).
/// Falls back to checking the root path if the health endpoint is not available.
///
/// # Errors
///
/// Returns [`TestError::RuntimeNotReady`] if the runtime does not respond within timeout.
pub fn wait_for_ready(base_url: &str, health_path: &str) -> error_stack::Result<(), TestError> {
    let health_url = format!("{}{}", base_url, health_path);

    for _ in 0..30 {
        if let Ok(resp) = reqwest::blocking::get(&health_url) {
            if resp.status().is_success() {
                return Ok(());
            }
        }

        // Fallback: try root path — a 404 means the server is responsive
        if let Ok(resp) = reqwest::blocking::get(base_url) {
            if resp.status().is_success() || resp.status().as_u16() == 404 {
                return Ok(());
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Err(error_stack::report!(TestError::RuntimeNotReady))
}

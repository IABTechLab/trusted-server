// Many items in common/environments/frameworks are defined for use by
// Docker-dependent integration tests that only run when Docker is available.
// The compiler sees them as unused when analysing the test binary alone.
#![allow(dead_code, unused_imports)]

mod common;
mod environments;
mod frameworks;

use common::runtime::{TestError, origin_port, wasm_binary_path};
use environments::RUNTIME_ENVIRONMENTS;
use error_stack::{Report, ResultExt as _};
use frameworks::{FRAMEWORKS, FrontendFramework};
use std::time::Duration;
use testcontainers::runners::SyncRunner as _;

/// Initialize the logger once for the test binary.
///
/// Called at the start of each test function. `try_init` is used so that
/// repeated calls (one per test) are harmless — only the first succeeds.
fn init_logger() {
    let _ = env_logger::try_init();
}

/// Test all combinations: frameworks x runtimes (matrix testing).
///
/// Iterates every registered runtime and framework, running all standard
/// and custom scenarios for each combination. Uses `--test-threads=1`
/// because all containers share the same fixed origin port.
#[test]
#[ignore = "requires Docker, Viceroy, and pre-built WASM binary"]
fn test_all_combinations() {
    init_logger();
    for runtime_factory in RUNTIME_ENVIRONMENTS {
        let runtime = runtime_factory();
        log::info!("Testing runtime: {}", runtime.id());

        for framework_factory in FRAMEWORKS {
            let framework = framework_factory();
            log::info!("  Testing framework: {}", framework.id());

            test_combination(runtime.as_ref(), framework.as_ref())
                .expect("should pass all scenarios for this combination");
        }
    }
}

/// Test a specific framework x runtime combination.
///
/// # Steps
///
/// 1. Start the frontend container mapped to the fixed origin port
/// 2. Spawn the runtime process (e.g. Viceroy) with the pre-built WASM binary
/// 3. Run standard scenarios (HTML injection, script serving, etc.)
/// 4. Run framework-specific custom scenarios
/// 5. Cleanup is automatic via [`Drop`] on both container and runtime
///
/// # Errors
///
/// Propagates any [`TestError`] from container startup, runtime spawn,
/// or scenario assertions, with full context chain.
fn test_combination(
    runtime: &dyn common::runtime::RuntimeEnvironment,
    framework: &dyn FrontendFramework,
) -> common::runtime::TestResult<()> {
    init_logger();
    let runtime_id = runtime.id();
    let framework_id = framework.id();
    let port = origin_port();

    // 1. Start frontend container mapped to the fixed origin port
    let container = framework
        .build_container(port)
        .attach(format!("runtime: {runtime_id}, framework: {framework_id}"))?
        .start()
        .change_context(TestError::ContainerStart {
            reason: format!("framework: {framework_id}"),
        })
        .attach(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    let origin_url = format!("http://127.0.0.1:{port}");

    // Wait for container to be ready
    wait_for_container(&origin_url, framework.health_check_path())?;

    // 2. Spawn runtime process with the pre-built WASM binary
    let wasm_path = wasm_binary_path();
    let process = runtime
        .spawn(&wasm_path)
        .attach(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    // 3. Run standard scenarios
    for scenario in framework.standard_scenarios() {
        scenario
            .run(&process.base_url, framework_id)
            .attach(format!(
                "runtime: {runtime_id}, framework: {framework_id}, scenario: {scenario:?}"
            ))?;
    }

    // 4. Run custom scenarios
    for scenario in framework.custom_scenarios() {
        scenario
            .run(&process.base_url, framework_id)
            .attach(format!(
                "runtime: {runtime_id}, framework: {framework_id}, custom scenario: {scenario:?}"
            ))?;
    }

    // Explicitly drop container to free the origin port for the next test
    drop(container);

    Ok(())
}

/// Wait for a Docker container's health endpoint to respond successfully.
///
/// Retries up to 60 times with 500ms delay (total ~30s). Uses a longer
/// budget than `wait_for_ready` because container cold-starts
/// include image pull, filesystem setup, and application init time.
///
/// Unlike `wait_for_ready`, this function does not fall back to the root path —
/// containers are expected to expose a reliable health endpoint.
///
/// # Errors
///
/// Returns [`TestError::ContainerTimeout`] if the health endpoint does not
/// respond with a success status within the timeout.
fn wait_for_container(base_url: &str, health_path: &str) -> common::runtime::TestResult<()> {
    let url = format!("{base_url}{health_path}");

    for _ in 0..60 {
        if let Ok(resp) = reqwest::blocking::get(&url)
            && resp.status().is_success()
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    Err(Report::new(TestError::ContainerTimeout)
        .attach(format!("Container at {base_url} not ready after 30s")))
}

// Individual test functions for faster iteration during development.

#[test]
#[ignore = "requires Docker, Viceroy, and pre-built WASM binary"]
fn test_wordpress_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Fastly");
}

#[test]
#[ignore = "requires Docker, Viceroy, and pre-built WASM binary"]
fn test_nextjs_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Fastly");
}

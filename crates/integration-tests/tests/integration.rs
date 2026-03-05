mod common;
mod environments;
mod frameworks;

use common::config::RuntimeConfigBuilder;
use common::runtime::{TestError, wasm_binary_path};
use environments::RUNTIME_ENVIRONMENTS;
use error_stack::ResultExt as _;
use frameworks::{FRAMEWORKS, FrontendFramework};
use std::time::Duration;
use testcontainers::runners::SyncRunner as _;

/// Test all combinations: frameworks x runtimes (matrix testing).
///
/// Iterates every registered runtime and framework, running all standard
/// and custom scenarios for each combination.
#[test]
fn test_all_combinations() {
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
/// 1. Start the frontend container (Docker via testcontainers)
/// 2. Generate platform-specific configuration pointing at the container
/// 3. Spawn the runtime process (e.g. Viceroy)
/// 4. Run standard scenarios (HTML injection, script serving, etc.)
/// 5. Run framework-specific custom scenarios
/// 6. Cleanup is automatic via [`Drop`] on both container and runtime
///
/// # Errors
///
/// Propagates any [`TestError`] from container startup, runtime spawn,
/// or scenario assertions, with full context chain.
fn test_combination(
    runtime: &dyn common::runtime::RuntimeEnvironment,
    framework: &dyn FrontendFramework,
) -> error_stack::Result<(), TestError> {
    let runtime_id = runtime.id();
    let framework_id = framework.id();

    // 1. Start frontend container
    let image = framework
        .build_container()
        .attach_printable(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    let container = image
        .start()
        .change_context(TestError::ContainerStart {
            reason: format!("framework: {framework_id}"),
        })
        .attach_printable(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    let host_port = container
        .get_host_port_ipv4(framework.container_port())
        .change_context(TestError::ContainerStart {
            reason: format!("could not get host port for framework: {framework_id}"),
        })?;

    let origin_url = format!("http://127.0.0.1:{host_port}");

    // Wait for container to be ready
    wait_for_container(&origin_url, framework.health_check_path())?;

    // 2. Generate platform-specific config
    let config = RuntimeConfigBuilder::new(runtime.config_template())
        .with_origin_url(origin_url)
        .with_integrations(vec!["prebid", "lockr"])
        .with_wasm_path(wasm_binary_path())
        .build()
        .attach_printable(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    // 3. Spawn runtime process
    let process = runtime
        .spawn(&config)
        .attach_printable(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    // 4. Run standard scenarios
    for scenario in framework.standard_scenarios() {
        scenario
            .run(&process.base_url, framework_id)
            .attach_printable(format!(
                "runtime: {runtime_id}, framework: {framework_id}, scenario: {scenario:?}"
            ))?;
    }

    // 5. Run custom scenarios
    for scenario in framework.custom_scenarios() {
        scenario
            .run(&process.base_url, framework_id)
            .attach_printable(format!(
                "runtime: {runtime_id}, framework: {framework_id}, custom scenario: {scenario:?}"
            ))?;
    }

    Ok(())
}

/// Wait for a container's health check endpoint to respond.
fn wait_for_container(base_url: &str, health_path: &str) -> error_stack::Result<(), TestError> {
    let url = format!("{base_url}{health_path}");

    for _ in 0..60 {
        if let Ok(resp) = reqwest::blocking::get(&url) {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    Err(error_stack::report!(TestError::ContainerTimeout)
        .attach_printable(format!("Container at {base_url} not ready after 30s")))
}

// Individual test functions for faster iteration during development.

#[test]
fn test_wordpress_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Fastly");
}

#[test]
fn test_nextjs_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Fastly");
}

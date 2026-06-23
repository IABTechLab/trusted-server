mod common;
mod environments;
mod frameworks;

use common::runtime::{RuntimeEnvironment, TestError, origin_port, wasm_binary_path};
use environments::{RUNTIME_ENVIRONMENTS, ReadyCheckOptions, wait_for_http_ready};
use error_stack::ResultExt as _;
use frameworks::scenarios::EcScenario;
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
    wait_for_http_ready(
        &origin_url,
        framework.health_check_path(),
        ReadyCheckOptions {
            max_attempts: 60,
            interval: Duration::from_millis(500),
            fallback_to_root: false,
            timeout_error: TestError::ContainerTimeout,
            timeout_message: format!("Container at {origin_url} not ready after 30s"),
        },
    )?;

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

#[test]
#[ignore = "requires Docker, the `wrangler` CLI in $PATH, and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts `wrangler dev` automatically"]
fn test_wordpress_cloudflare() {
    let runtime = environments::cloudflare::CloudflareWorkers;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Cloudflare Workers");
}

#[test]
#[ignore = "requires Docker, the `wrangler` CLI in $PATH, and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts `wrangler dev` automatically"]
fn test_nextjs_cloudflare() {
    let runtime = environments::cloudflare::CloudflareWorkers;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Cloudflare Workers");
}

#[test]
#[ignore = "requires Docker and pre-built trusted-server-axum binary"]
fn test_wordpress_axum() {
    let runtime = environments::axum::AxumDevServer;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Axum");
}

#[test]
#[ignore = "requires Docker and pre-built trusted-server-axum binary"]
fn test_nextjs_axum() {
    let runtime = environments::axum::AxumDevServer;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Axum");
}

// ---------------------------------------------------------------------------
// EC identity lifecycle tests (no frontend framework container needed)
// ---------------------------------------------------------------------------

/// Runs all EC lifecycle scenarios against a standalone Viceroy instance.
///
/// Unlike framework tests, these use a minimal TCP origin server instead
/// of a Docker container. The scenarios use a pre-seeded EC row plus
/// explicit consent cookies because Viceroy's local HTTP runtime does not
/// satisfy the production browser bot-gate fingerprint requirements for
/// minting new ECs end-to-end.
#[test]
#[ignore = "requires Viceroy and pre-built WASM binary"]
fn test_ec_lifecycle_fastly() {
    init_logger();
    let port = origin_port();

    // Start a minimal origin server so organic route proxying succeeds.
    let _origin = common::ec::MinimalOrigin::start(port);
    log::info!("EC lifecycle tests: minimal origin running on port {port}");

    let runtime = environments::fastly::FastlyViceroy;
    let wasm_path = wasm_binary_path();

    let process = runtime
        .spawn(&wasm_path)
        .expect("should spawn Viceroy for EC tests");

    log::info!(
        "EC lifecycle tests: Viceroy running at {}",
        process.base_url
    );

    // EdgeZero entry-point canary. This same test runs in two CI jobs: the
    // legacy `integration-tests` job (default Viceroy config, legacy_main) and
    // the `integration-tests-edgezero` job (EdgeZero config store, edgezero_main).
    // Only assert the canary when the job opted into the EdgeZero path via
    // EXPECT_EDGEZERO_ENTRY_POINT; on the legacy path TRACE is proxied (not 405ed)
    // and the scenarios still validate legacy behavior. The canary guards against
    // the EdgeZero job silently greening on legacy if the config store cannot be
    // read (main() falls back to legacy_main).
    if std::env::var("EXPECT_EDGEZERO_ENTRY_POINT").as_deref() == Ok("true") {
        common::ec::assert_edgezero_entry_point(&process.base_url)
            .expect("EdgeZero entry-point probe request failed");
    }

    for scenario in EcScenario::all() {
        log::info!("  Running EC scenario: {scenario:?}");
        let result = scenario.run(&process.base_url);
        assert!(
            result.is_ok(),
            "EC scenario {scenario:?} should succeed: {result:?}"
        );
    }
}

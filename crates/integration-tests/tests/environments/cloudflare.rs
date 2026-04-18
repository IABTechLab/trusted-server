use crate::common::runtime::{RuntimeEnvironment, RuntimeProcess, TestError, TestResult};
use error_stack::Report;
use std::path::Path;

/// Cloudflare Workers runtime environment.
///
/// Not runnable in the current integration test harness — Cloudflare Workers
/// requires a Wrangler dev server (`wrangler dev`) which is not automated here.
///
/// All integration tests for this environment are marked `#[ignore]`. To run
/// them locally, start Wrangler first:
///
/// ```sh
/// # Build the WASM binary
/// cargo build -p trusted-server-adapter-cloudflare --release --target wasm32-unknown-unknown --features cloudflare
///
/// # Start the dev server (in crates/trusted-server-adapter-cloudflare/)
/// wrangler dev
/// ```
///
/// Then run with `-- --ignored test_wordpress_cloudflare`.
pub struct CloudflareWorkers;

impl RuntimeEnvironment for CloudflareWorkers {
    fn id(&self) -> &'static str {
        "cloudflare"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        Err(Report::new(TestError::RuntimeSpawn).attach(
            "Cloudflare Workers integration tests require a running `wrangler dev` instance \
             and are not automated in the CI harness. Run with --ignored to skip.",
        ))
    }

    fn health_check_path(&self) -> &str {
        "/.well-known/trusted-server.json"
    }
}

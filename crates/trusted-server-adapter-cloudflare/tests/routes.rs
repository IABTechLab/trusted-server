//! Smoke tests for the Cloudflare adapter route wiring.
//!
//! Runs on the host target (no Workers runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking and that
//! the crate compiles cleanly on the host target. Does not exercise
//! the platform layer or outbound network calls.

use edgezero_core::app::Hooks as _;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;

#[test]
fn routes_build_without_panic() {
    // build_state() may fail (no real settings in CI) — startup_error_router
    // is the fallback. Either way, routes() must not panic.
    let _router = TrustedServerApp::routes();
}

#[test]
fn crate_compiles_on_host_target() {
    // Ensures the cfg-gated shim keeps the crate host-compilable.
}

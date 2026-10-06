#[cfg(not(target_arch = "wasm32"))]
mod ad_templates;
#[cfg(not(target_arch = "wasm32"))]
mod app_config;
#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod prebid_bundle;
#[cfg(not(target_arch = "wasm32"))]
mod run;
#[cfg(not(target_arch = "wasm32"))]
mod tls;
#[cfg(not(target_arch = "wasm32"))]
mod url_guard;

#[cfg(not(target_arch = "wasm32"))]
pub use run::{RunOutcome, run_from_env};

// Public commands let native integration tests exercise the shared proxy.
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
// Console output wrappers. Gated to non-wasm hosts rather than the proxy's
// macOS/Linux targets: `ts dev sandbox-probe` builds on every host target and
// needs it too.
#[cfg(not(target_arch = "wasm32"))]
mod output;

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
pub use run::{RunOutcome, run_from_env};

// Public commands let native integration tests exercise the shared proxy.
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod output;

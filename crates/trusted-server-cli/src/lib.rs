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

// Every `ts` subcommand's implementation lives under `commands/<name>`. The
// `ts dev` group is available on every host target; its only subcommand,
// `ts dev proxy`, is macOS-only (CA trust via the login keychain, Safari
// automation via `networksetup`, a native TLS / networking stack) and its
// dependencies are scoped to macOS in `Cargo.toml`. `commands` is `pub` so the
// macOS-gated `tests/proxy_e2e.rs` integration suite can exercise the proxy
// internals.
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
// Console output wrappers. Gated to non-wasm hosts rather than macOS: the
// macOS-only proxy was its first consumer, but `ts dev sandbox-probe` builds
// on every host target and needs it too.
#[cfg(not(target_arch = "wasm32"))]
mod output;

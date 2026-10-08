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

// Every `ts` subcommand's implementation lives under `commands/<name>`. The
// `ts dev` group is available on every host target; `ts dev lint` and
// `ts dev install-hooks` are pure-Rust (gitoxide) and cross-host, while
// `ts dev proxy` is macOS/Linux-only (CA trust via the login keychain, Safari
// automation via `networksetup`, a native TLS / networking stack) and its
// dependencies are scoped to those targets in `Cargo.toml`. `commands` is
// `pub` so the gated `tests/proxy_e2e.rs` integration suite can exercise the
// shared proxy internals.
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
// Console output wrappers, cross-host: `ts dev lint` uses the `write_*`
// helpers on every target, and `ts dev sandbox-probe` builds on every host
// target and needs them too. The `info` / `warn` helpers are proxy-facing.
#[cfg(not(target_arch = "wasm32"))]
mod output;

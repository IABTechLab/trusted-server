#[cfg(not(target_arch = "wasm32"))]
mod config_init;
#[cfg(not(target_arch = "wasm32"))]
mod run;

#[cfg(not(target_arch = "wasm32"))]
pub use run::run_from_env;

// The `ts dev` command group is available on every host target; its only
// subcommand, `ts dev proxy`, is macOS-only (CA trust via the login keychain,
// Safari automation via `networksetup`, a native TLS / networking stack) and its
// dependencies are scoped to macOS in `Cargo.toml`. `commands` is `pub` so the
// macOS-gated `tests/proxy_e2e.rs` integration suite can exercise the proxy
// internals.
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
#[cfg(target_os = "macos")]
mod output;

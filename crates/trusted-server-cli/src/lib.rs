#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod prebid_bundle;
#[cfg(not(target_arch = "wasm32"))]
mod run;

#[cfg(not(target_arch = "wasm32"))]
pub use run::run_from_env;

// Every `ts` subcommand's implementation lives under `commands/<name>`. The
// `ts dev` group is available on every host target; `ts dev lint` and
// `ts dev install-hooks` are pure-Rust (gitoxide) and cross-host, while
// `ts dev proxy` is macOS-only (CA trust via the login keychain, Safari
// automation via `networksetup`, a native TLS / networking stack) and its
// dependencies are scoped to macOS in `Cargo.toml`. `commands` is `pub` so the
// macOS-gated `tests/proxy_e2e.rs` integration suite can exercise the proxy
// internals.
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
// `output` is cross-host: `ts dev lint` uses its `write_*` helpers on every
// target; the `info` / `warn` helpers are macOS-only (proxy-facing).
#[cfg(not(target_arch = "wasm32"))]
mod output;

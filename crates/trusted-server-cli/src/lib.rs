#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "CLI tests use panic-on-failure helpers"
    )
)]

#[cfg(not(target_arch = "wasm32"))]
mod audit;
#[cfg(not(target_arch = "wasm32"))]
mod config_init;
#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod prebid_bundle;
#[cfg(not(target_arch = "wasm32"))]
mod run;

#[cfg(not(target_arch = "wasm32"))]
pub use run::run_from_env;

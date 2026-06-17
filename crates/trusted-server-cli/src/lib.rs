#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
    )
)]

#[cfg(not(target_arch = "wasm32"))]
mod args;
#[cfg(not(target_arch = "wasm32"))]
mod config_command;
#[cfg(not(target_arch = "wasm32"))]
mod edgezero_delegate;
#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod run;

#[cfg(not(target_arch = "wasm32"))]
pub use run::{run_from_env, run_with_io};

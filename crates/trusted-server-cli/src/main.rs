#[cfg(target_os = "macos")]
use clap::Parser as _;
#[cfg(target_os = "macos")]
use trusted_server_cli::Cli;

#[cfg(target_os = "macos")]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    std::process::exit(Cli::parse().run());
}

// `ts dev proxy` is macOS-only (its deps are macOS-scoped in `Cargo.toml`), so
// on other targets the crate is an empty shell; this trivial entry point just
// keeps the binary target's shape valid.
#[cfg(not(target_os = "macos"))]
fn main() {}

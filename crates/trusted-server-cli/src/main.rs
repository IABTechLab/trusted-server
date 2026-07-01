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
// on other targets the crate is an empty shell. Fail loudly with a clear message
// and a nonzero exit rather than silently succeeding, so a run on an unsupported
// platform is never mistaken for a working command.
#[cfg(not(target_os = "macos"))]
fn main() {
    trusted_server_cli::output::error(
        "`ts dev proxy` is supported on macOS only (it uses the macOS login keychain, \
         `networksetup`, and Safari automation). Build and run it on macOS.",
    );
    std::process::exit(2);
}

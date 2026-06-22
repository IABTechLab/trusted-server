use clap::Parser as _;
use trusted_server_cli::Cli;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    std::process::exit(Cli::parse().run());
}

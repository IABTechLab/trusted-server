use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::process::ExitCode;

use trusted_server_core::runtime_config::load_runtime_config;

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        let _ = writeln!(
            io::stderr(),
            "usage: ts-config-canonicalize <trusted-server.toml>"
        );
        return ExitCode::from(2);
    };

    let toml = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            let _ = writeln!(io::stderr(), "failed to read {path}: {error}");
            return ExitCode::from(1);
        }
    };

    let loaded = match load_runtime_config(&toml) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(io::stderr(), "failed to canonicalize config: {error:?}");
            return ExitCode::from(1);
        }
    };

    let _ = writeln!(io::stderr(), "config hash: {}", loaded.config_hash);
    let _ = write!(io::stdout(), "{}", loaded.canonical_toml);
    ExitCode::SUCCESS
}

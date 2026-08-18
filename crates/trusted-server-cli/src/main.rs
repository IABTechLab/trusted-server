#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use std::process;

    edgezero_cli::init_cli_logger();
    match trusted_server_cli::run_from_env() {
        Ok(outcome) if outcome.exit_code() != 0 => process::exit(outcome.exit_code()),
        Ok(_) => {}
        Err(err) => {
            log::error!("[ts] {err}");
            process::exit(2);
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

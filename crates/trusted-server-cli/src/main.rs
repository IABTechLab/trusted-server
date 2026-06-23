#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use std::process;

    edgezero_cli::init_cli_logger();
    if let Err(err) = trusted_server_cli::run_from_env() {
        log::error!("[ts] {err}");
        process::exit(2);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

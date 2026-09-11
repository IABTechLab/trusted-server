#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use std::process;

    // Dependencies such as chromiumoxide instrument their internals with
    // `tracing`. Without a subscriber, tracing's log-compatibility fallback
    // forwards tolerated CDP decode warnings into the CLI's user-facing logger.
    // Trusted Server uses `log` for intentional operator output, so install a
    // no-op tracing subscriber to keep dependency diagnostics out of stdout and
    // stderr without changing the process-wide `log` level.
    let _ = tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::default());
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

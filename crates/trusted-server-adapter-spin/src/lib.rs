//! Fermyon Spin adapter for Trusted Server.

pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::{IntoResponse, Request};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_service;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_service]
// FORCED: edgezero_adapter_spin::run_app returns anyhow::Result — EdgeZero SDK constraint, not a project choice.
async fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    // Install a real `log` backend before dispatch. `edgezero_adapter_spin`
    // only calls a no-op `init_logger`, so without this every `log::error!`
    // (including the startup-error diagnostics) is silently dropped on Spin.
    logging::init();
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(req).await
}

/// Minimal stderr [`log`] backend for the Spin component.
///
/// Spin captures component stderr to `.spin/logs/<component>_stderr.txt`
/// (local) and to the Fermyon Cloud log stream, so routing `log` records to
/// stderr makes startup and request diagnostics visible.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
mod logging {
    use std::io::Write as _;
    use std::sync::Once;

    struct StderrLogger;

    impl log::Log for StderrLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            // Write to the stderr handle directly rather than via `eprintln!`
            // so the component's `print_stderr` lint stays enforced elsewhere.
            let _ = writeln!(
                std::io::stderr(),
                "[{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }

        fn flush(&self) {}
    }

    static LOGGER: StderrLogger = StderrLogger;
    static INIT: Once = Once::new();

    /// Installs [`StderrLogger`] as the global `log` backend exactly once.
    ///
    /// Runs before `run_app`, so this wins the global slot and the adapter's
    /// own no-op `init_logger` becomes the harmless second `set_logger` (whose
    /// `Err` it already ignores).
    pub(crate) fn init() {
        INIT.call_once(|| {
            if log::set_logger(&LOGGER).is_ok() {
                log::set_max_level(log::LevelFilter::Info);
            }
        });
    }
}

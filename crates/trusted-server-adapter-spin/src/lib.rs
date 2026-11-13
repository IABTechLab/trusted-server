//! Fermyon Spin adapter for Trusted Server.

// Unit tests may buffer bodies with `Body::into_bytes`; production code must not.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use std::io::Write as _;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::Request;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_service;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_service]
// FORCED: edgezero_adapter_spin::run_app returns anyhow::Result — EdgeZero SDK constraint, not a project choice.
async fn handle(req: Request) -> anyhow::Result<edgezero_adapter_spin::SpinResponse> {
    install_logger();
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(req).await
}

/// Writes warn and error records to stderr, which Spin keeps as the component log.
///
/// `EdgeZero`'s Spin `init_logger` installs no backend, so
/// [`app::TrustedServerApp`] owns logging and the entry point installs this.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
struct StderrLogger;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(
                std::io::stderr(),
                "{} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// Installs [`StderrLogger`] at warn, once per component instance, before
/// `EdgeZero` assembles the app.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
fn install_logger() {
    static LOGGER: StderrLogger = StderrLogger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

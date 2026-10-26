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
    #[cfg(feature = "test-utils")]
    install_fixture_logger();
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(req).await
}

#[cfg(all(feature = "test-utils", feature = "spin", target_arch = "wasm32"))]
use std::io::Write as _;

#[cfg(all(feature = "test-utils", feature = "spin", target_arch = "wasm32"))]
struct FixtureLogger;

#[cfg(all(feature = "test-utils", feature = "spin", target_arch = "wasm32"))]
impl log::Log for FixtureLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(std::io::stderr(), "{}", record.args());
        }
    }

    fn flush(&self) {}
}

#[cfg(all(feature = "test-utils", feature = "spin", target_arch = "wasm32"))]
fn install_fixture_logger() {
    static LOGGER: FixtureLogger = FixtureLogger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

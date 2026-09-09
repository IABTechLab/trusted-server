//! Spin-specific logging backend.

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
use std::io::Write as _;

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
use log::{Level, Log, Metadata, Record};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use log::{LevelFilter, SetLoggerError};

/// Target reserved for startup diagnostics consumed by the Spin smoke oracle.
pub(crate) const STARTUP_DIAGNOSTIC_TARGET: &str = "trusted_server_adapter_spin::startup";

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
struct SpinStartupLogger;

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
impl Log for SpinStartupLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() == Level::Error && metadata.target() == STARTUP_DIAGNOSTIC_TARGET
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            // Spin's supported component-log channel is stdout/stderr. EdgeZero's
            // Spin logger initializer is a no-op, so this adapter backend owns the
            // write while callers continue to use the `log` facade.
            let _ = writeln!(std::io::stderr().lock(), "{}", record.args());
        }
    }

    fn flush(&self) {}
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
static SPIN_STARTUP_LOGGER: SpinStartupLogger = SpinStartupLogger;

/// Installs the target-filtered logger used by the startup smoke oracle.
///
/// A repeated request may find the process-global logger already installed;
/// that is the expected steady state.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
pub(crate) fn init_logger() -> Result<(), SetLoggerError> {
    log::set_logger(&SPIN_STARTUP_LOGGER)?;
    log::set_max_level(LevelFilter::Error);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_enables_only_the_startup_error_target() {
        let startup_error = Metadata::builder()
            .level(Level::Error)
            .target(STARTUP_DIAGNOSTIC_TARGET)
            .build();
        let startup_warning = Metadata::builder()
            .level(Level::Warn)
            .target(STARTUP_DIAGNOSTIC_TARGET)
            .build();
        let unrelated_error = Metadata::builder()
            .level(Level::Error)
            .target("trusted_server_adapter_spin::app")
            .build();

        assert!(SPIN_STARTUP_LOGGER.enabled(&startup_error));
        assert!(!SPIN_STARTUP_LOGGER.enabled(&startup_warning));
        assert!(!SPIN_STARTUP_LOGGER.enabled(&unrelated_error));
    }
}

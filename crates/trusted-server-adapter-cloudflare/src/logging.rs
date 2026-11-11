//! Minimal Workers console backend for existing application diagnostics.

use log::{Level, LevelFilter, Log, Metadata, Record};

struct WorkerLogger;

static LOGGER: WorkerLogger = WorkerLogger;

impl Log for WorkerLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        match record.level() {
            Level::Error | Level::Warn => {
                worker::console_error!("[{}] {}", record.target(), record.args());
            }
            _ => {
                worker::console_log!("[{}] {}", record.target(), record.args());
            }
        }
    }

    fn flush(&self) {}
}

/// Install the console backend once, preserving any previously installed logger.
pub(crate) fn initialize() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

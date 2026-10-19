//! The Worker's logger. It writes each record through the console method of
//! the record's level, so the Workers runtime keeps the line at that level.

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The most detailed level the Worker writes. Debug and trace lines stay out,
/// because some of them carry auction payloads and consent strings.
pub const MAX_LEVEL: LevelFilter = LevelFilter::Info;

/// The console method a record is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleMethod {
    Error,
    Warn,
    Log,
    Debug,
}

/// `console.error` and `console.warn` for those levels, `console.log` for
/// `info`, and `console.debug` for `debug` and `trace`.
const fn console_method(level: Level) -> ConsoleMethod {
    match level {
        Level::Error => ConsoleMethod::Error,
        Level::Warn => ConsoleMethod::Warn,
        Level::Info => ConsoleMethod::Log,
        Level::Debug | Level::Trace => ConsoleMethod::Debug,
    }
}

/// The line written for `record`, its level and target before its message.
fn line(record: &Record<'_>) -> String {
    format!("{} [{}] {}", record.level(), record.target(), record.args())
}

/// Writes each record at [`MAX_LEVEL`] or above to the console.
struct ConsoleLogger;

impl Log for ConsoleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= MAX_LEVEL
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            write(console_method(record.level()), &line(record));
        }
    }

    fn flush(&self) {}
}

#[cfg(target_arch = "wasm32")]
fn write(method: ConsoleMethod, line: &str) {
    match method {
        ConsoleMethod::Error => worker::console_error!("{line}"),
        ConsoleMethod::Warn => worker::console_warn!("{line}"),
        ConsoleMethod::Log => worker::console_log!("{line}"),
        ConsoleMethod::Debug => worker::console_debug!("{line}"),
    }
}

/// Off the Workers runtime, which only the native tests are, a line goes
/// nowhere.
#[cfg(not(target_arch = "wasm32"))]
fn write(_method: ConsoleMethod, _line: &str) {}

static LOGGER: ConsoleLogger = ConsoleLogger;

/// Installs the Worker's logger at [`MAX_LEVEL`], unless a logger is
/// installed already. The entry point calls this for every request, so only
/// the first call in an instance installs the logger and a later call changes
/// nothing.
pub fn init_logger() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(MAX_LEVEL);
    }
}

#[cfg(test)]
mod tests {
    use log::{Level, Log as _, Metadata, Record};

    use super::{ConsoleLogger, ConsoleMethod, console_method, line};

    #[test]
    fn each_level_is_written_with_the_console_method_for_it() {
        let methods = [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ]
        .map(console_method);

        assert_eq!(
            methods,
            [
                ConsoleMethod::Error,
                ConsoleMethod::Warn,
                ConsoleMethod::Log,
                ConsoleMethod::Debug,
                ConsoleMethod::Debug,
            ],
            "should write each level with the console method for it"
        );
    }

    #[test]
    fn a_line_gives_the_level_and_target_before_the_message() {
        let written = line(
            &Record::builder()
                .level(Level::Warn)
                .target("trusted_server_adapter_cloudflare::app")
                .args(format_args!("a route enables the Image Optimizer"))
                .build(),
        );

        assert_eq!(
            written,
            "WARN [trusted_server_adapter_cloudflare::app] a route enables the Image Optimizer",
            "should give the level and target before the message"
        );
    }

    #[test]
    fn info_and_above_are_written_and_debug_and_trace_are_not() {
        let writes = |level| ConsoleLogger.enabled(&Metadata::builder().level(level).build());

        assert!(
            writes(Level::Error) && writes(Level::Warn) && writes(Level::Info),
            "should write info, warnings and errors"
        );
        assert!(
            !writes(Level::Debug) && !writes(Level::Trace),
            "should leave debug and trace lines out"
        );
    }
}

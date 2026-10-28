//! The component's logger. It writes each record as one line to standard
//! error, which Spin keeps as the component's log.

use std::io::Write;

use log::{LevelFilter, Log, Metadata, Record};

/// The most detailed level the component writes. Debug and trace lines stay
/// out, because some of them carry auction payloads and consent strings.
pub const MAX_LEVEL: LevelFilter = LevelFilter::Info;

/// Writes `record` to `out` as one line, its level and target before its
/// message. The line is formatted first, so it reaches `out` in one write.
fn write_line(out: &mut impl Write, record: &Record<'_>) -> std::io::Result<()> {
    let line = format!(
        "{} [{}] {}\n",
        record.level(),
        record.target(),
        record.args()
    );
    out.write_all(line.as_bytes())
}

/// Writes each record at [`MAX_LEVEL`] or above to standard error.
struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= MAX_LEVEL
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            // A line standard error cannot take is dropped, because nothing
            // is left to report that to.
            let _ = write_line(&mut std::io::stderr().lock(), record);
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

static LOGGER: StderrLogger = StderrLogger;

/// Installs the component's logger at [`MAX_LEVEL`], unless a logger is
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

    use super::{StderrLogger, write_line};

    #[test]
    fn a_record_is_written_as_one_line_with_its_level_and_target() {
        let mut out = Vec::new();

        write_line(
            &mut out,
            &Record::builder()
                .level(Level::Warn)
                .target("trusted_server_adapter_spin::app")
                .args(format_args!("a source names another host"))
                .build(),
        )
        .expect("should write to memory");

        assert_eq!(
            String::from_utf8(out).expect("should write text"),
            "WARN [trusted_server_adapter_spin::app] a source names another host\n",
            "should give the level and target before the message and end the line"
        );
    }

    #[test]
    fn info_and_above_are_written_and_debug_and_trace_are_not() {
        let writes = |level| StderrLogger.enabled(&Metadata::builder().level(level).build());

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

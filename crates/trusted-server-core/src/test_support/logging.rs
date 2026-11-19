//! Thread-local capture of warn and error records behind the single process-wide test logger.

use std::cell::RefCell;
use std::sync::Once;

thread_local! {
    static RECORDS: RefCell<Option<Vec<CapturedLog>>> = const { RefCell::new(None) };
}

/// One warn or error record a [`LogCapture`] recorded.
pub struct CapturedLog {
    /// Level of the record, [`log::Level::Warn`] or [`log::Level::Error`].
    pub level: log::Level,
    /// Log target of the record.
    pub target: String,
    /// Rendered log message.
    pub message: String,
}

struct TestLogger;

impl log::Log for TestLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            RECORDS.with(|records| {
                if let Some(records) = records.borrow_mut().as_mut() {
                    records.push(CapturedLog {
                        level: record.level(),
                        target: record.target().to_owned(),
                        message: record.args().to_string(),
                    });
                }
            });
        }
    }

    fn flush(&self) {}
}

/// Records the warn and error records logged on the current thread until it is dropped.
///
/// A test that claims a warning reads [`LogCapture::take_warnings`], so the same message
/// logged at error does not satisfy it. A test that claims something about every record,
/// such as that nothing was logged or that no record carries a secret, reads
/// [`LogCapture::take_all`] and checks each record's level itself.
///
/// The first capture installs one process-wide logger that keeps only warn and error
/// records, so a test binary that installs another logger cannot use it. Captures do not
/// nest.
pub struct LogCapture;

impl LogCapture {
    /// Starts capturing the warn and error records logged on this thread.
    ///
    /// # Panics
    ///
    /// Panics if another logger is already installed or a capture is already active on this
    /// thread.
    #[must_use]
    pub fn start() -> Self {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            log::set_logger(&TestLogger).expect("should install the shared test logger");
            // Leave the level enabled: restoring it races captures on other threads.
            log::set_max_level(log::LevelFilter::Warn);
        });
        RECORDS.with(|records| {
            assert!(records.borrow().is_none(), "should not nest log captures");
            *records.borrow_mut() = Some(Vec::new());
        });
        Self
    }

    /// Returns the warn-level records captured since the start or the last take.
    ///
    /// Error-level records captured in the same span are discarded.
    ///
    /// # Panics
    ///
    /// Panics if the capture is no longer active on this thread.
    #[must_use]
    pub fn take_warnings(&self) -> Vec<CapturedLog> {
        let mut records = self.take_all();
        records.retain(|record| record.level == log::Level::Warn);
        records
    }

    /// Returns every warn and error record captured since the start or the last take.
    ///
    /// # Panics
    ///
    /// Panics if the capture is no longer active on this thread.
    #[must_use]
    pub fn take_all(&self) -> Vec<CapturedLog> {
        RECORDS.with(|records| {
            std::mem::take(
                records
                    .borrow_mut()
                    .as_mut()
                    .expect("should have an active log capture"),
            )
        })
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        RECORDS.with(|records| *records.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_warnings_keeps_only_warn_records_and_take_all_keeps_both() {
        let capture = LogCapture::start();

        log::warn!("example warning");
        log::error!("example error");
        let warnings = capture.take_warnings();
        log::warn!("example warning");
        log::error!("example error");
        let all = capture.take_all();

        let warning_view: Vec<(log::Level, &str)> = warnings
            .iter()
            .map(|record| (record.level, record.message.as_str()))
            .collect();
        assert_eq!(
            warning_view,
            [(log::Level::Warn, "example warning")],
            "should return only the warn-level record"
        );
        let all_view: Vec<(log::Level, &str)> = all
            .iter()
            .map(|record| (record.level, record.message.as_str()))
            .collect();
        assert_eq!(
            all_view,
            [
                (log::Level::Warn, "example warning"),
                (log::Level::Error, "example error")
            ],
            "should return the warn and the error record in order"
        );
        assert!(
            capture.take_all().is_empty(),
            "should drain the records on each take"
        );
    }
}

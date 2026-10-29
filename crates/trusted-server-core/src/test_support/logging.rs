//! Thread-local warning capture behind the single process-wide test logger.

use std::cell::RefCell;
use std::sync::Once;

thread_local! {
    static WARNINGS: RefCell<Option<Vec<CapturedWarning>>> = const { RefCell::new(None) };
}

pub(crate) struct CapturedWarning {
    pub target: String,
    pub message: String,
}

struct TestLogger;

impl log::Log for TestLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() == log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            WARNINGS.with(|warnings| {
                if let Some(warnings) = warnings.borrow_mut().as_mut() {
                    warnings.push(CapturedWarning {
                        target: record.target().to_owned(),
                        message: record.args().to_string(),
                    });
                }
            });
        }
    }

    fn flush(&self) {}
}

pub(crate) struct WarningCapture;

impl WarningCapture {
    pub fn start() -> Self {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            log::set_logger(&TestLogger).expect("should install the shared test logger");
            // Leave the level enabled: restoring it races captures on other threads.
            log::set_max_level(log::LevelFilter::Warn);
        });
        WARNINGS.with(|warnings| {
            assert!(
                warnings.borrow().is_none(),
                "should not nest warning captures"
            );
            *warnings.borrow_mut() = Some(Vec::new());
        });
        Self
    }

    pub fn take(&self) -> Vec<CapturedWarning> {
        WARNINGS.with(|warnings| {
            std::mem::take(
                warnings
                    .borrow_mut()
                    .as_mut()
                    .expect("should have an active warning capture"),
            )
        })
    }
}

impl Drop for WarningCapture {
    fn drop(&mut self) {
        WARNINGS.with(|warnings| *warnings.borrow_mut() = None);
    }
}

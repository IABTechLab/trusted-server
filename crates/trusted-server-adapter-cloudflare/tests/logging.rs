//! The Worker's logger, installed once for an instance at `info` and above.
//!
//! A logger is global to a process, so these tests have a binary of their
//! own, in which nothing else installs one.

use edgezero_core::app::Hooks as _;
use log::{Level, LevelFilter, Metadata};
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
use trusted_server_adapter_cloudflare::logging::init_logger;

/// Whether the installed logger writes a record at `level`.
fn writes(level: Level) -> bool {
    log::logger().enabled(&Metadata::builder().level(level).build())
}

/// The entry point installs the logger for every request, so a second call
/// keeps the first logger rather than failing.
#[test]
fn the_logger_is_installed_once_at_info_and_above() {
    assert_eq!(
        log::max_level(),
        LevelFilter::Off,
        "no logger should be installed before the first call"
    );

    init_logger();
    init_logger();

    assert_eq!(
        log::max_level(),
        LevelFilter::Info,
        "should write info and above"
    );
    assert!(
        writes(Level::Error) && writes(Level::Warn) && writes(Level::Info),
        "the installed logger should write info, warnings and errors"
    );
    assert!(
        !writes(Level::Debug) && !writes(Level::Trace),
        "the installed logger should leave debug and trace lines out"
    );
}

/// The adapter installs its own logger, so `EdgeZero`'s `run_app` installs
/// none.
#[test]
fn the_app_owns_its_logging() {
    assert!(
        TrustedServerApp::owns_logging(),
        "should tell EdgeZero that the adapter installs its own logger"
    );
}

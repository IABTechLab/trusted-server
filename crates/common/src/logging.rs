use log::LevelFilter;

/// Initialize logging for the application
/// Should be called once at the start of main()
pub fn init_logging() {
    // Initialize Fastly logging if available, otherwise use env_logger for tests
    #[cfg(target_arch = "wasm32")]
    {
        log_fastly::init_simple("trusted-server", LevelFilter::Info);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        env_logger::builder().filter_level(LevelFilter::Info).init();
    }
}

/// Log level helper to determine if debug logging is enabled
pub fn is_debug_enabled() -> bool {
    log::log_enabled!(log::Level::Debug)
}

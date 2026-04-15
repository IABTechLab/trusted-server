use chrono::{SecondsFormat, Utc};
use log_fastly::Logger;

/// Extracts the final `::` segment from a Rust module path for use as a log label.
///
/// Falls back to the full target string when the input contains no separator or
/// when the separator appears at the trailing position (e.g. `"foo::"`), which
/// would otherwise produce an empty label in log output.
fn target_label(target: &str) -> &str {
    match target.rsplit_once("::") {
        Some((head, "")) => head,
        Some((_, last)) => last,
        None => target,
    }
}

/// Initialises the Fastly-backed `fern` logger and installs it as the global logger.
///
/// Log records are forwarded to the `tslog` Fastly endpoint and echoed to stdout.
/// Each line is prefixed with an RFC 3339 timestamp, level, and the final segment
/// of the record's target module path.
///
/// # Panics
///
/// Panics if the logger cannot be built or if a global logger has already been set.
pub(crate) fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Info)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                record.level(),
                target_label(record.target()),
                message
            ));
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("should initialize logger");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_label_extracts_correct_segment() {
        assert_eq!(
            target_label("trusted_server_adapter_fastly::proxy"),
            "proxy",
            "should handle standard single-separator case"
        );
        assert_eq!(
            target_label("foo::bar::baz"),
            "baz",
            "should handle multiple separators"
        );
        assert_eq!(
            target_label("no_separators_here"),
            "no_separators_here",
            "should handle inputs without ::"
        );
        assert_eq!(target_label(""), "", "should handle empty strings");
        assert_eq!(
            target_label("trailing::"),
            "trailing",
            "should strip separator when trailing segment is empty"
        );
    }
}

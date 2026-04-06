use chrono::{Local, SecondsFormat};
use log_fastly::Logger;

pub(crate) fn target_label(target: &str) -> &str {
    target.rsplit_once("::").map_or(target, |(_, last)| last)
}

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
                Local::now().to_rfc3339_opts(SecondsFormat::Millis, true),
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
        assert_eq!(target_label("trailing::"), "", "should handle trailing ::");
    }
}

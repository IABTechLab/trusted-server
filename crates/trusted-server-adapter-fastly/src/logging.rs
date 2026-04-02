use chrono::{Local, SecondsFormat};
use log_fastly::Logger;

pub(crate) fn target_label(target: &str) -> &str {
    target.split("::").last().unwrap_or(target)
}

#[allow(dead_code)]
pub(crate) fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Info)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
        .format(|out, _message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                Local::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                record.level(),
                target_label(record.target()),
                record.args()
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
    fn target_label_uses_last_target_segment() {
        assert_eq!(
            target_label("trusted_server_adapter_fastly::proxy"),
            "proxy",
            "should use the final target segment"
        );
    }
}

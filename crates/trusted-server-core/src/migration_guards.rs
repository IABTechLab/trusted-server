fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn migrated_utility_and_handler_modules_do_not_depend_on_fastly_request_response_types() {
    let sources = [
        ("auth.rs", include_str!("auth.rs")),
        ("cookies.rs", include_str!("cookies.rs")),
        ("synthetic.rs", include_str!("synthetic.rs")),
        ("http_util.rs", include_str!("http_util.rs")),
        ("geo.rs", include_str!("geo.rs")),
        ("publisher.rs", include_str!("publisher.rs")),
        ("proxy.rs", include_str!("proxy.rs")),
        ("auction/formats.rs", include_str!("auction/formats.rs")),
        ("auction/endpoints.rs", include_str!("auction/endpoints.rs")),
        (
            "request_signing/endpoints.rs",
            include_str!("request_signing/endpoints.rs"),
        ),
        (
            "consent/extraction.rs",
            include_str!("consent/extraction.rs"),
        ),
        ("consent/mod.rs", include_str!("consent/mod.rs")),
    ];
    let banned_patterns = [
        "fastly::Request",
        "fastly::Response",
        "fastly::http::Method",
        "fastly::http::StatusCode",
        "fastly::mime::APPLICATION_JSON",
    ];

    for (path, source) in sources {
        let uncommented = strip_line_comments(source);
        for banned in banned_patterns {
            assert!(
                !uncommented.contains(banned),
                "{path} should not reference `{banned}` after PR11 migration"
            );
        }
    }
}

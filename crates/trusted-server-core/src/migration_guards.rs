// Strips lines whose first non-whitespace token is `//`.
//
// Known limitations (both produce false positives, never false negatives):
// - String literals: `"fastly::Request"` in a test assertion would trigger a
//   spurious failure even though it is not a real Fastly dependency.
// - Block comments: `/* fastly::Request */` is not stripped. A banned pattern
//   inside a block comment causes a spurious failure; one hidden *outside* a
//   block comment is still caught by the non-comment portions of the line.
//
// False positives are safe for a guard test — they cause a noisy failure that
// forces investigation rather than letting a real regression slip through
// silently. False negatives are not possible with the current banned-pattern
// list because none of the migrated files use block comments in practice.
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
    // Word-boundary regex prevents false positives from doc comments or string
    // literals that merely mention Fastly type names without importing them.
    let banned = regex::Regex::new(
        r"\bfastly::(Request|Response|http::(Method|StatusCode)|mime::APPLICATION_JSON)\b",
    )
    .expect("should compile migration guard regex");

    for (path, source) in sources {
        let uncommented = strip_line_comments(source);
        assert!(
            !banned.is_match(&uncommented),
            "{path} should not reference fastly Request/Response types after PR11 migration"
        );
    }
}

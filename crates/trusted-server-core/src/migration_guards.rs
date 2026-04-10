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
fn migrated_utility_modules_do_not_depend_on_fastly_request_response_types() {
    let sources = [
        ("auth.rs", include_str!("auth.rs")),
        ("cookies.rs", include_str!("cookies.rs")),
        ("synthetic.rs", include_str!("synthetic.rs")),
        ("http_util.rs", include_str!("http_util.rs")),
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

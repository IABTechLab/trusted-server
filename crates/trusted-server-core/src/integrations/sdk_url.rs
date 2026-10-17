//! Shared input policy for SDK URL ownership checks.

use url::Url;

/// Parse an HTTP(S) SDK URL with an explicit authority, or a protocol-relative URL.
///
/// Trims ASCII whitespace and rejects relative paths and shorthand schemes.
/// Browser/Rust ownership cases live in test/fixtures/sdk-url-ownership.json.
pub(super) fn parse_sdk_network_url(url: &str) -> Option<Url> {
    let value = url.trim_matches([' ', '\t', '\n', '\r', '\x0c', '\x0b']);
    let parsed = if value.starts_with("//") {
        Url::parse(&format!("https:{value}"))
    } else {
        if !value.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }) {
            return None;
        }
        Url::parse(value)
    };
    parsed.ok()
}

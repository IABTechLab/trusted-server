//! Shared input policy for SDK URL ownership checks.

use url::Url;

/// Parse an HTTP(S) SDK URL with an explicit authority, or a protocol-relative URL.
///
/// Applies browser C0/space trimming, removes tab/CR/LF, and accepts backslash
/// authority separators. Literal NULs, relative paths, and base-dependent shorthand
/// stay unowned; HTML replaces NULs rather than treating them as URL whitespace.
/// Browser/Rust ownership cases live in crates/trusted-server-js/lib/test/fixtures/sdk-url-ownership.json.
pub(super) fn parse_sdk_network_url(url: &str) -> Option<Url> {
    if url.contains('\0') {
        return None;
    }
    let value = url
        .trim_matches(|character| character <= '\u{20}')
        .replace(['\t', '\n', '\r'], "");
    let has_authority = |value: &str| matches!(value.as_bytes(), [b'/' | b'\\', b'/' | b'\\', ..]);
    let parsed = if has_authority(&value) {
        Url::parse(&format!("https:{value}"))
    } else {
        let (scheme, remainder) = value.split_once(':')?;
        if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
            || !has_authority(remainder)
        {
            return None;
        }
        Url::parse(&value)
    };
    parsed.ok()
}

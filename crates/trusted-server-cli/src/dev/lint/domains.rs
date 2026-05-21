//! `ts dev lint domains` — URL-host linter.
//!
//! Design: docs/superpowers/specs/2026-05-18-check-domains-design.md

use core::error::Error;
use std::collections::HashSet;
use std::sync::OnceLock;

use derive_more::Display;
use regex::Regex;

/// Integration proxies and loopback hosts that must match exactly.
/// Subdomains are NOT allowed (e.g., `anything.api.privacy-center.org`
/// is disallowed). See spec §"Exact-match hosts" for the policy.
pub const EXACT_HOSTS: &[&str] = &[
    // Loopback
    "127.0.0.1",
    "::1",
    "localhost",
    // didomi
    "api.privacy-center.org",
    "sdk.privacy-center.org",
    // sourcepoint
    "cdn.privacy-mgmt.com",
    // lockr
    "aim.loc.kr",
    "identity.loc.kr",
    // datadome
    "js.datadome.co",
    "api-js.datadome.co",
    // aps / Amazon
    "aax.amazon-adsystem.com",
    "aax-events.amazon-adsystem.com",
    // permutive
    "api.permutive.com",
    "secure-signals.permutive.app",
    "cdn.permutive.com",
    // Google Tag Manager / Analytics
    "www.googletagmanager.com",
    "www.google-analytics.com",
    "analytics.google.com",
    // adserver mock
    "securepubads.g.doubleclick.net",
    "origin-mocktioneer.cdintel.com",
    // Prebid CDN
    "cdn.prebid.org",
    // Fastly platform
    "api.fastly.com",
];

/// Hosts where exact match AND any subdomain (`*.host`) is allowed.
/// See spec §"Subdomain-permitting hosts" and §"Allowlist
/// Maintenance Policy" for the bar to add an entry here.
pub const SUBDOMAIN_HOSTS: &[&str] = &[
    // IANA RFC 2606 reserved
    "example.com",
    "example.net",
    "example.org",
    // Permutive: runtime host is {organization_id}.edge.permutive.app
    "edge.permutive.app",
];

/// Well-known documentation and specification sources. Exact-match,
/// allowed in every scanned file. See spec §"Reference / doc hosts"
/// for the curated list (seeded from a sampling; expected to grow
/// during Stage 1 doc cleanup).
pub const REFERENCE_HOSTS: &[&str] = &[
    // Git / GitHub
    "github.com",
    "docs.github.com",
    "help.github.com",
    "token.actions.githubusercontent.com",
    // Git commit conventions
    "chris.beams.io",
    // Rust
    "docs.rs",
    "doc.rust-lang.org",
    "crates.io",
    // Web / W3C standards
    "www.w3.org",
    "schema.org",
    // Versioning / changelogs
    "semver.org",
    "keepachangelog.com",
    // IAB Tech Lab
    "iab.com",
    "iabtechlab.com",
    "iabtechlab.github.io",
    "iabeurope.github.io",
    // Specs (supply chain)
    "in-toto.io",
    "rslstandard.org",
    // Specs (other)
    "webassembly.org",
    // Fastly docs
    "www.fastly.com",
    "developer.fastly.com",
    "manage.fastly.com",
    // Cloudflare docs
    "developers.cloudflare.com",
    // Vendor docs
    "docs.datadome.co",
    "docs.prebid.org",
    // Tooling docs
    "vitepress.dev",
    "playwright.dev",
    "testcontainers.com",
    "grafana.com",
    "docsearch.algolia.com",
];

/// IANA RFC 2606 reserved TLDs. Any host ending in one of these is allowed.
pub const RESERVED_TLDS: &[&str] = &[".example", ".test", ".invalid", ".localhost"];

/// Errors raised by the domains linter.
#[derive(Debug, Display)]
pub enum DomainsLintError {
    /// Opening the git repository failed.
    #[display("failed to open git repository")]
    OpenRepo,
    /// Reading the git index failed.
    #[display("failed to read git index")]
    Index,
    /// Computing a blob or tree diff failed.
    #[display("failed to compute diff")]
    Diff,
    /// A git reference could not be resolved.
    #[display("failed to resolve reference `{_0}`")]
    Reference(String),
    /// No merge-base exists between the base ref and HEAD.
    #[display("failed to compute merge-base of `{base}` and HEAD")]
    MergeBase {
        /// The base reference that was requested.
        base: String,
    },
    /// A file could not be read.
    #[display("failed to read file `{}`", _0.display())]
    ReadFile(std::path::PathBuf),
    /// An explicitly-named path does not exist.
    #[display("path not found: `{}`", _0.display())]
    PathNotFound(std::path::PathBuf),
    /// An explicitly-named path could not be read for permission reasons.
    #[display("permission denied reading `{}`", _0.display())]
    PermissionDenied(std::path::PathBuf),
    /// More than one scan mode was requested at once.
    #[display("invalid mode combination")]
    InvalidMode,
    /// Failure writing a warning to stderr (broken pipe, etc.).
    ///
    /// Used by the in-module [`warn`] helper so collectors can call
    /// [`crate::output::write_stderr_line`] and still return
    /// `Report<DomainsLintError>` consistently.
    #[display("I/O error writing warning to stderr")]
    WriteWarning,
}

impl Error for DomainsLintError {}

/// In-module warning helper.
///
/// Wraps the CLI's [`crate::output::write_stderr_line`] (which
/// returns `Report<CliError>`) so callers inside `domains` can stay
/// on `Report<DomainsLintError>` without inventing custom `?`
/// conversions at every call site.
///
/// # Errors
///
/// Returns [`DomainsLintError::WriteWarning`] if writing to stderr
/// fails (e.g., a broken pipe).
#[allow(dead_code)]
fn warn(msg: impl Into<String>) -> Result<(), error_stack::Report<DomainsLintError>> {
    use error_stack::ResultExt as _;
    crate::output::write_stderr_line(msg.into()).change_context(DomainsLintError::WriteWarning)
}

/// Normalise an extracted URL host: strip bracketed-IPv6 `[ ]` and
/// lowercase. Pure function; no I/O.
fn normalise_host(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('[').trim_end_matches(']');
    trimmed.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_lowercases() {
        assert_eq!(normalise_host("EXAMPLE.COM"), "example.com");
        assert_eq!(normalise_host("Foo.Example.Com"), "foo.example.com");
    }

    #[test]
    fn normalise_strips_ipv6_brackets() {
        assert_eq!(normalise_host("[::1]"), "::1");
        assert_eq!(normalise_host("[2001:DB8::1]"), "2001:db8::1");
    }

    #[test]
    fn normalise_passthrough_for_plain_hosts() {
        assert_eq!(normalise_host("test.com"), "test.com");
        assert_eq!(normalise_host("127.0.0.1"), "127.0.0.1");
    }
}

/// Decide whether a normalised host is allowed.
///
/// Order: per-line suppression set, reserved-TLD suffix, exact match
/// against [`EXACT_HOSTS`] and [`REFERENCE_HOSTS`], then the subdomain
/// rule against [`SUBDOMAIN_HOSTS`].
fn is_allowed(host: &str, suppressed_on_line: &HashSet<String>) -> bool {
    if suppressed_on_line.contains(host) {
        return true;
    }
    if RESERVED_TLDS.iter().any(|t| host.ends_with(t)) {
        return true;
    }
    if EXACT_HOSTS.iter().any(|e| host == *e) {
        return true;
    }
    if REFERENCE_HOSTS.iter().any(|e| host == *e) {
        return true;
    }
    if SUBDOMAIN_HOSTS
        .iter()
        .any(|e| host == *e || host.ends_with(&format!(".{e}")))
    {
        return true;
    }
    false
}

#[cfg(test)]
mod allow_check_tests {
    use super::*;

    fn nothing_suppressed() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn exact_match_allows() {
        assert!(is_allowed("api.fastly.com", &nothing_suppressed()));
        assert!(is_allowed("127.0.0.1", &nothing_suppressed()));
    }

    #[test]
    fn exact_only_rejects_subdomain() {
        // EXACT_HOSTS entries are exact-only: a subdomain of an
        // exact host is NOT allowed.
        assert!(!is_allowed("v2.api.fastly.com", &nothing_suppressed()));
        assert!(!is_allowed(
            "anything.api.privacy-center.org",
            &nothing_suppressed()
        ));
    }

    #[test]
    fn subdomain_list_allows_apex_and_subdomains() {
        assert!(is_allowed("example.com", &nothing_suppressed()));
        assert!(is_allowed("foo.example.com", &nothing_suppressed()));
        assert!(is_allowed("a.b.example.com", &nothing_suppressed()));
        assert!(is_allowed("example.net", &nothing_suppressed()));
        assert!(is_allowed("assets.example.net", &nothing_suppressed()));
    }

    #[test]
    fn lookalike_attack_rejected() {
        // example.com.evil.com is not a subdomain of example.com.
        assert!(!is_allowed("example.com.evil.com", &nothing_suppressed()));
        assert!(!is_allowed("notexample.com", &nothing_suppressed()));
    }

    #[test]
    fn reserved_tld_allows() {
        assert!(is_allowed("testlight.example", &nothing_suppressed()));
        assert!(is_allowed("something.test", &nothing_suppressed()));
        assert!(is_allowed("thing.invalid", &nothing_suppressed()));
        assert!(is_allowed("my.localhost", &nothing_suppressed()));
    }

    #[test]
    fn reference_hosts_allowed_everywhere() {
        assert!(is_allowed("github.com", &nothing_suppressed()));
        assert!(is_allowed("docs.rs", &nothing_suppressed()));
        // But NOT subdomains of REFERENCE_HOSTS (exact-match).
        assert!(!is_allowed("other.github.com", &nothing_suppressed()));
    }

    #[test]
    fn suppression_set_allows() {
        let mut suppressed = HashSet::new();
        suppressed.insert("evil.com".to_string());
        assert!(is_allowed("evil.com", &suppressed));
    }

    #[test]
    fn rejects_unrelated_host() {
        assert!(!is_allowed("test.com", &nothing_suppressed()));
        assert!(!is_allowed("1.2.3.4", &nothing_suppressed()));
        assert!(!is_allowed("192.168.1.1", &nothing_suppressed()));
    }
}

/// Regex for absolute `http(s)://` URLs. Case-insensitive; the host
/// must start with an alphanumeric character so placeholders like
/// `https://...` are rejected.
fn absolute_url_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)https?://(\[[0-9a-fA-F:]+\]|[A-Za-z0-9][A-Za-z0-9.\-]*)")
            .expect("should compile absolute URL regex")
    })
}

/// Extract and normalise every host from absolute URLs on `line`.
fn extract_absolute_hosts(line: &str) -> Vec<String> {
    absolute_url_regex()
        .captures_iter(line)
        .filter_map(|c| c.get(1).map(|m| normalise_host(m.as_str())))
        .collect()
}

#[cfg(test)]
mod absolute_url_tests {
    use super::*;

    #[test]
    fn extracts_plain() {
        assert_eq!(
            extract_absolute_hosts("see https://example.com/path here"),
            vec!["example.com"]
        );
    }

    #[test]
    fn extracts_bracketed_ipv6() {
        assert_eq!(extract_absolute_hosts("dial http://[::1]:8080/"), vec!["::1"]);
    }

    #[test]
    fn extracts_uppercase_normalised() {
        assert_eq!(
            extract_absolute_hosts("HTTPS://Example.COM/x"),
            vec!["example.com"]
        );
    }

    #[test]
    fn rejects_dots_only_placeholder() {
        assert!(extract_absolute_hosts("see https://... for an example").is_empty());
    }

    #[test]
    fn handles_punctuation_wrapping() {
        for s in [
            "\"https://example.com\",",
            "(https://example.com)",
            "<https://example.com>",
        ] {
            assert_eq!(extract_absolute_hosts(s), vec!["example.com"], "input: {s}");
        }
    }

    #[test]
    fn extracts_multiple_per_line() {
        assert_eq!(
            extract_absolute_hosts("see [a](https://github.com/x) and [b](https://example.com/y)"),
            vec!["github.com", "example.com"]
        );
    }
}

/// Regex for protocol-relative `//host/...` URLs. The `//` must be
/// preceded by a boundary character (start-of-line, whitespace,
/// quote, paren, `=`, `<`, `>`, `{`, `,`, `[`, `]`, backtick) — but
/// NOT `:`, which would double-match the `//` in an absolute URL.
/// The host requires a dotted TLD-like suffix to filter out code
/// comment dividers.
fn protocol_relative_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?i)(?:^|[\s"'(=<>{,\[\]`])//([A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,})"#)
            .expect("should compile protocol-relative URL regex")
    })
}

/// Extract and normalise every host from protocol-relative URLs.
fn extract_protocol_relative_hosts(line: &str) -> Vec<String> {
    protocol_relative_regex()
        .captures_iter(line)
        .filter_map(|c| c.get(1).map(|m| normalise_host(m.as_str())))
        .collect()
}

#[cfg(test)]
mod protocol_relative_tests {
    use super::*;

    #[test]
    fn extracts_after_quote() {
        assert_eq!(
            extract_protocol_relative_hosts("src=\"//www.googletagmanager.com/gtm.js\""),
            vec!["www.googletagmanager.com"]
        );
    }

    #[test]
    fn extracts_after_start_of_line() {
        assert_eq!(
            extract_protocol_relative_hosts("//cdn.example.evil/foo"),
            vec!["cdn.example.evil"]
        );
    }

    #[test]
    fn extracts_template_literal_backtick() {
        assert_eq!(
            extract_protocol_relative_hosts("`//cdn.example.evil/${path}`"),
            vec!["cdn.example.evil"]
        );
    }

    #[test]
    fn extracts_json_object_value() {
        assert_eq!(
            extract_protocol_relative_hosts("{\"src\": \"//cdn.example.evil/x\"}"),
            vec!["cdn.example.evil"]
        );
    }

    #[test]
    fn does_not_match_colon_prefix() {
        // http://foo.com — // is preceded by ':', NOT in the boundary class.
        assert!(extract_protocol_relative_hosts("http://foo.com/x").is_empty());
    }

    #[test]
    fn does_not_match_code_comment_divider() {
        // The trailing TLD-like constraint (.{2,}) filters this out;
        // "comment text" has no dotted-suffix.
        assert!(extract_protocol_relative_hosts("// comment text").is_empty());
    }
}

/// Regex for the per-line suppression marker. The comment introducer
/// (`//`, `#`, `<!--`, or `*` + whitespace) must be preceded by
/// start-of-line or whitespace — this is what makes the marker
/// bypass-resistant against `allow-domain` substrings inside URLs.
fn suppression_marker_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?im)(?:^|\s)(?://|\#|<!--|\*\s)\s*allow-domain:\s*([A-Za-z0-9.\-:\[\],\s]+?)(?:-->|$)",
        )
        .expect("should compile suppression marker regex")
    })
}

/// Result of parsing a line for a suppression marker.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LineSuppression {
    /// Hosts listed in the marker (post-trim, lowercased).
    pub suppressed: HashSet<String>,
}

/// Parse the `allow-domain:` marker on `line`, if present. Splits the
/// captured host list on `,`, trims each entry, lowercases, and
/// drops empties.
fn parse_suppression_marker(line: &str) -> LineSuppression {
    let mut out = LineSuppression::default();
    let Some(caps) = suppression_marker_regex().captures(line) else {
        return out;
    };
    let Some(m) = caps.get(1) else {
        return out;
    };
    for host in m.as_str().split(',') {
        let host = host.trim();
        if !host.is_empty() {
            out.suppressed.insert(host.to_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod suppression_tests {
    use super::*;

    fn parse(line: &str) -> HashSet<String> {
        parse_suppression_marker(line).suppressed
    }

    #[test]
    fn single_host_after_slash_comment() {
        let got = parse("let x = \"https://evil.com\"; // allow-domain: evil.com");
        let expected: HashSet<String> = ["evil.com".to_string()].into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn html_comment_form_with_trailing_space() {
        // Captured group includes trailing space before --> ; trim handles it.
        let got = parse("<!-- allow-domain: test.com   -->");
        let expected: HashSet<String> = ["test.com".to_string()].into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn hash_comment_form() {
        let got = parse("upstream = \"https://evil.com\"  # allow-domain: evil.com");
        let expected: HashSet<String> = ["evil.com".to_string()].into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn multi_host_with_whitespace() {
        let got = parse("// allow-domain: a.com ,  b.com , c.com");
        let expected: HashSet<String> = ["a.com", "b.com", "c.com"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn bypass_attempt_url_path_lookalike_not_suppressed() {
        // 'allow-domain' inside a URL path is NOT a comment.
        let got = parse("fetch(\"https://evil.com/allow-domain\")");
        assert!(got.is_empty(), "URL-path content must not suppress: {got:?}");
    }

    #[test]
    fn bypass_attempt_pathological_host_named_allow_domain() {
        // https://allow-domain:8080/path — the // is preceded by ':',
        // not whitespace/SOL, so the marker anchor fails.
        let got = parse("let x = \"https://allow-domain:8080/path\";");
        assert!(got.is_empty(), "pathological host must not suppress: {got:?}");
    }
}

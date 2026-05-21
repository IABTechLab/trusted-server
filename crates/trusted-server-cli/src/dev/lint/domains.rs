//! `ts dev lint domains` — URL-host linter.
//!
//! Design: docs/superpowers/specs/2026-05-18-check-domains-design.md

// The pure-function layer (allowlist constants, host extraction,
// scan_line) and the DomainsLintError variants are exercised by the
// inline #[cfg(test)] modules but are not yet reachable from a
// non-test build. Phase 4 (diff collectors) and Phase 5
// (domains::run + clap wiring) make them live; this allow is
// removed in Phase 5.
#![allow(dead_code)]

use core::error::Error;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use derive_more::Display;
use error_stack::{Report, ResultExt as _};
use gix::ObjectId;
use gix::bstr::BString;
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
    if EXACT_HOSTS.contains(&host) {
        return true;
    }
    if REFERENCE_HOSTS.contains(&host) {
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
            .map(ToString::to_string)
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

/// One reported violation on a scanned line.
#[derive(Debug, PartialEq, Eq)]
pub struct LineViolation {
    /// The disallowed host.
    pub host: String,
}

/// Result of scanning one source line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LineScanOutcome {
    /// Disallowed hosts found on the line (after suppression).
    pub violations: Vec<LineViolation>,
    /// Hosts the line's `allow-domain:` marker listed but that would
    /// not have been a violation anyway. The caller emits these as a
    /// stderr warning.
    pub unused_suppressions: Vec<String>,
}

/// Scan one source line; return violations and any unused
/// suppression-marker entries.
///
/// Composes [`parse_suppression_marker`], [`extract_absolute_hosts`],
/// [`extract_protocol_relative_hosts`], and [`is_allowed`].
pub fn scan_line(line: &str) -> LineScanOutcome {
    let suppression = parse_suppression_marker(line);
    let mut hosts = extract_absolute_hosts(line);
    hosts.extend(extract_protocol_relative_hosts(line));

    // Hosts that WOULD be flagged WITHOUT any suppression. A marker
    // entry that does not match one of these is "unused" — it
    // suppresses nothing and warrants a warning.
    let empty_suppression: HashSet<String> = HashSet::new();
    let disallowed_without_suppression: HashSet<&String> = hosts
        .iter()
        .filter(|h| !is_allowed(h, &empty_suppression))
        .collect();

    let mut unused: Vec<String> = suppression
        .suppressed
        .iter()
        .filter(|listed| {
            !disallowed_without_suppression
                .iter()
                .any(|h| h.as_str() == listed.as_str())
        })
        .cloned()
        .collect();
    unused.sort();

    let violations = hosts
        .into_iter()
        .filter(|h| !is_allowed(h, &suppression.suppressed))
        .map(|host| LineViolation { host })
        .collect();

    LineScanOutcome {
        violations,
        unused_suppressions: unused,
    }
}

#[cfg(test)]
mod scan_line_tests {
    use super::*;

    fn hosts(line: &str) -> Vec<String> {
        scan_line(line)
            .violations
            .into_iter()
            .map(|v| v.host)
            .collect()
    }

    #[test]
    fn allowed_passes_clean() {
        for line in [
            "see https://example.com",
            "see https://foo.example.com",
            "see https://api.privacy-center.org",
            "dial http://127.0.0.1:8080/",
            "see https://github.com/x/y",
            "see https://testlight.example",
            "//www.googletagmanager.com/gtm.js",
        ] {
            assert!(hosts(line).is_empty(), "should be clean: {line}");
        }
    }

    #[test]
    fn disallowed_reports() {
        assert_eq!(hosts("see https://test.com"), vec!["test.com"]);
        assert_eq!(hosts("see https://partner.com"), vec!["partner.com"]);
    }

    #[test]
    fn suppression_with_correct_host_passes() {
        let out = scan_line("https://evil.com // allow-domain: evil.com");
        assert!(out.violations.is_empty());
        assert!(out.unused_suppressions.is_empty());
    }

    #[test]
    fn suppression_with_wrong_host_still_reports_and_warns() {
        let out = scan_line("https://evil.com // allow-domain: other.com");
        assert_eq!(
            out.violations
                .into_iter()
                .map(|v| v.host)
                .collect::<Vec<_>>(),
            vec!["evil.com"]
        );
        assert_eq!(
            out.unused_suppressions,
            vec!["other.com"],
            "other.com was listed but never appeared on the line"
        );
    }

    #[test]
    fn multi_host_suppression_applied_to_violations() {
        let out = scan_line(
            "x = \"https://evil.com\"; y = \"https://bad.org\"; \
             // allow-domain: evil.com, bad.org",
        );
        assert!(
            out.violations.is_empty(),
            "both hosts should be suppressed: {out:?}"
        );
        assert!(out.unused_suppressions.is_empty());
    }

    #[test]
    fn multi_host_suppression_partial_match_warns_for_unused() {
        let out = scan_line("\"https://evil.com\" // allow-domain: evil.com, ghost.com");
        assert!(out.violations.is_empty(), "evil.com should be suppressed");
        assert_eq!(out.unused_suppressions, vec!["ghost.com"]);
    }

    #[test]
    fn jsdoc_star_suppression_form() {
        let out = scan_line(" * fetch(\"https://evil.com\") * allow-domain: evil.com");
        assert!(
            out.violations.is_empty(),
            "jsdoc-style suppression should apply: {out:?}"
        );
    }

    #[test]
    fn multiple_disallowed_on_one_line() {
        let got = hosts("<a href=\"https://test.com\">x</a><a href=\"https://partner.com\">y</a>");
        assert_eq!(got, vec!["test.com", "partner.com"]);
    }

    #[test]
    fn bypass_attempt_reports() {
        // fetch("https://evil.com/allow-domain") — substring inside URL,
        // not a comment, so suppression does NOT apply.
        assert_eq!(
            hosts("fetch(\"https://evil.com/allow-domain\")"),
            vec!["evil.com"]
        );
    }

    #[test]
    fn unused_warning_only_when_marker_present() {
        let out = scan_line("see https://example.com");
        assert!(out.unused_suppressions.is_empty());
    }

    #[test]
    fn unused_warning_fires_for_already_allowed_listed_host() {
        // example.com is extracted but already allowed → would never
        // have been a violation → the marker entry was unnecessary.
        let out = scan_line("see https://example.com // allow-domain: example.com");
        assert!(out.violations.is_empty(), "example.com is already allowed");
        assert_eq!(
            out.unused_suppressions,
            vec!["example.com"],
            "marker listed an already-allowed host; it suppresses nothing"
        );
    }
}

// === Diff and path collectors (Phase 4) ===

/// One added line collected from a diff or file scan.
#[derive(Debug)]
pub(crate) struct DiffLine {
    /// Path for display and reporting. Built via
    /// `String::from_utf8_lossy` for non-UTF-8 sources.
    pub path: PathBuf,
    /// 1-based line number within the new-side file.
    pub line_no: usize,
    /// The line's text content.
    pub content: String,
}

/// Whether a repo-relative path should be scanned.
///
/// Stub implementation — replaced with the real extension /
/// path-exclusion filter in Task 4.5.
fn path_is_scanned(_rel_path: &str) -> bool {
    true
}

/// Read a blob's bytes from the object database.
fn read_blob(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>, Report<DomainsLintError>> {
    let obj = repo
        .find_object(id)
        .change_context(DomainsLintError::Diff)?;
    Ok(obj.data.clone())
}

/// Walk a tree recursively into a `path → blob_id` map.
fn tree_blob_map(tree: &gix::Tree<'_>) -> Result<HashMap<BString, ObjectId>, Report<DomainsLintError>> {
    let mut map = HashMap::new();
    let entries = tree
        .traverse()
        .breadthfirst
        .files()
        .change_context(DomainsLintError::Diff)?;
    for entry in entries {
        if entry.mode.is_blob() {
            map.insert(entry.filepath, entry.oid);
        }
    }
    Ok(map)
}

/// Compute the new-side added lines between two blob contents.
///
/// Returns `(1-based line number, content)` for every inserted line.
fn added_lines(old: Option<&[u8]>, new: &[u8]) -> Vec<(usize, String)> {
    use gix::diff::blob::{Algorithm, Diff, InternedInput};

    let old_text = old
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let new_text = String::from_utf8_lossy(new).into_owned();

    let input = InternedInput::new(old_text.as_str(), new_text.as_str());
    let diff = Diff::compute(Algorithm::Myers, &input);

    let new_lines: Vec<&str> = new_text.lines().collect();
    let mut out = Vec::new();
    for hunk in diff.hunks() {
        for token_idx in hunk.after.clone() {
            let content = new_lines
                .get(token_idx as usize)
                .copied()
                .unwrap_or("")
                .to_string();
            out.push((token_idx as usize + 1, content));
        }
    }
    out
}

/// Convert a raw byte path to a display `PathBuf`, lossy-decoding
/// non-UTF-8 bytes. Returns `(path, was_lossy)`.
fn bytes_to_pathbuf(raw: &[u8]) -> (PathBuf, bool) {
    match std::str::from_utf8(raw) {
        Ok(s) => (PathBuf::from(s), false),
        Err(_) => {
            let lossy = String::from_utf8_lossy(raw).into_owned();
            (PathBuf::from(&lossy), true)
        }
    }
}

/// Collect added lines staged in the index relative to the HEAD tree.
///
/// # Errors
///
/// Returns [`DomainsLintError`] if the repository, its index, or a
/// blob cannot be read.
pub(crate) fn staged_added_lines(
    repo_path: &Path,
) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(repo_path).change_context(DomainsLintError::OpenRepo)?;

    // HEAD tree → blob map. An empty repo (no commits) has no HEAD;
    // treat that as an empty map (everything in the index is added).
    let head_map: HashMap<BString, ObjectId> = match repo.head_commit() {
        Ok(commit) => {
            let tree_id = commit.tree_id().change_context(DomainsLintError::OpenRepo)?;
            let tree = repo
                .find_tree(tree_id)
                .change_context(DomainsLintError::OpenRepo)?;
            tree_blob_map(&tree)?
        }
        Err(_) => HashMap::new(),
    };

    let index = repo.index().change_context(DomainsLintError::Index)?;
    let mut index_map: HashMap<BString, ObjectId> = HashMap::new();
    for entry in index.entries() {
        if entry.mode.contains(gix::index::entry::Mode::FILE) {
            index_map.insert(entry.path(&index).to_owned(), entry.id);
        }
    }

    let mut all_paths: Vec<&BString> = index_map.keys().chain(head_map.keys()).collect();
    all_paths.sort();
    all_paths.dedup();

    let mut out = Vec::new();
    for raw_path in all_paths {
        let head_id = head_map.get(raw_path);
        let index_id = index_map.get(raw_path);
        let (old_bytes, new_bytes) = match (head_id, index_id) {
            (Some(h), Some(i)) if h == i => continue, // unchanged
            (Some(h), Some(i)) => (Some(read_blob(&repo, *h)?), read_blob(&repo, *i)?),
            (None, Some(i)) => (None, read_blob(&repo, *i)?),
            (Some(_), None) => continue, // deletion — no added lines
            (None, None) => continue,
        };

        let (path, was_lossy) = bytes_to_pathbuf(raw_path);
        let path_str = path.to_string_lossy();
        if !path_is_scanned(&path_str) {
            continue;
        }
        if was_lossy {
            // Staged mode reports non-UTF-8 paths (unlike full-repo
            // mode, which skips them) — see spec test 25.
            warn(format!(
                "warning: staged path is not valid UTF-8; displaying lossy: {}",
                path.display()
            ))?;
        }

        for (line_no, content) in added_lines(old_bytes.as_deref(), &new_bytes) {
            out.push(DiffLine {
                path: path.clone(),
                line_no,
                content,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod staged_added_lines_tests {
    use super::*;
    use crate::dev::lint::test_support;

    #[test]
    fn reports_added_line_with_new_side_line_number() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("a.txt"), "alpha\nbeta\ngamma\n")
            .expect("should write initial file");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "initial");

        std::fs::write(temp.path().join("a.txt"), "alpha\nNEW LINE\nbeta\ngamma\n")
            .expect("should write modification");
        test_support::stage_all(&repo);

        let lines = staged_added_lines(temp.path()).expect("should collect staged lines");
        let added: Vec<_> = lines
            .iter()
            .map(|l| {
                (
                    l.path.to_string_lossy().into_owned(),
                    l.line_no,
                    l.content.clone(),
                )
            })
            .collect();

        assert_eq!(added, vec![("a.txt".to_string(), 2, "NEW LINE".to_string())]);
    }

    /// Spec test case 25: staged scan must NOT skip non-UTF-8 paths.
    ///
    /// Gated to Linux: macOS (APFS/HFS+) rejects non-UTF-8 byte
    /// sequences in filenames with `EILSEQ`, so the scenario cannot
    /// be constructed there. Linux ext4/CI runners permit it.
    #[cfg(target_os = "linux")]
    #[test]
    fn reports_non_utf8_staged_path_lossy() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());

        std::fs::write(temp.path().join("readme.txt"), "hi\n")
            .expect("should write readme");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "initial");

        let non_utf8_name =
            std::ffi::OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f, 0x2e, 0x72, 0x73]);
        let bad_file = temp.path().join(non_utf8_name);
        std::fs::write(&bad_file, "let x = \"https://test.com\";\n")
            .expect("should write non-utf8-named file");
        test_support::stage_all(&repo);

        let lines = staged_added_lines(temp.path())
            .expect("should collect staged lines even with non-UTF-8 path");
        assert!(
            !lines.is_empty(),
            "non-UTF-8 staged paths must be reported, not skipped"
        );
        assert!(
            lines.iter().any(|l| l.content.contains("https://test.com")),
            "must surface the URL for scanning: {lines:?}"
        );
    }
}

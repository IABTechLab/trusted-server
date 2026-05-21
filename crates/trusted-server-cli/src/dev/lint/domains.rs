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

/// File extensions whose contents are scanned. See spec
/// §"File extensions scanned".
const SCANNED_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "mjs", "cjs", "toml", "yml", "yaml", "json", "md", "css", "html",
];

/// Lockfile basenames excluded by exact match. See spec
/// §"Always excluded (paths)".
const EXCLUDED_LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "pnpm-lock.json",
    "yarn.lock",
    "npm-shrinkwrap.json",
];

/// Path components that exclude any path containing them.
const EXCLUDED_DIR_COMPONENTS: &[&str] = &["node_modules", "target", "dist", ".git", ".worktrees"];

/// The linter's own source file — excluded so its allowlist
/// constants and doc comments cannot self-flag.
const SELF_PATH: &str = "crates/trusted-server-cli/src/dev/lint/domains.rs";

/// Whether a repo-relative path (using `/` separators) should be
/// scanned. See spec §"File extensions scanned" and
/// §"Always excluded (paths)".
fn path_is_scanned(rel_path: &str) -> bool {
    // Self-exclude.
    if rel_path == SELF_PATH {
        return false;
    }
    // Excluded directory components (whole-segment match).
    let components: Vec<&str> = rel_path.split('/').collect();
    if components
        .iter()
        .any(|c| EXCLUDED_DIR_COMPONENTS.contains(c))
    {
        return false;
    }
    // `.claude/worktrees/` — two-segment exclusion.
    if components.windows(2).any(|w| w == [".claude", "worktrees"]) {
        return false;
    }
    // Publisher-capture HTML fixtures: the narrow
    // trusted-server-core/src/integrations/**/fixtures/** path.
    if rel_path.contains("crates/trusted-server-core/src/integrations/")
        && rel_path.contains("/fixtures/")
    {
        return false;
    }

    let basename = components.last().copied().unwrap_or("");
    // Excluded lockfiles (exact basename).
    if EXCLUDED_LOCKFILES.contains(&basename) {
        return false;
    }
    // Dockerfile and Dockerfile.* are scanned (no extension).
    if basename == "Dockerfile" || basename.starts_with("Dockerfile.") {
        return true;
    }
    // `.env*` files are scanned.
    if basename.starts_with(".env") {
        return true;
    }
    // Otherwise scan by extension.
    match basename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => SCANNED_EXTENSIONS.contains(&ext),
        _ => false,
    }
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

    collect_added_from_maps(&repo, &head_map, &index_map)
}

/// Walk two `path → blob_id` maps, classify each path, and blob-diff
/// added/modified entries into [`DiffLine`]s.
///
/// Shared by [`staged_added_lines`] (HEAD-tree vs index) and
/// [`changed_vs_added_lines`] (merge-base tree vs HEAD tree). Both
/// modes scan blob content, so a non-UTF-8 path is reported lossy
/// with a stderr warning rather than skipped (full-repo mode skips —
/// see [`full_repo_lines`]).
fn collect_added_from_maps(
    repo: &gix::Repository,
    old_map: &HashMap<BString, ObjectId>,
    new_map: &HashMap<BString, ObjectId>,
) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let mut all_paths: Vec<&BString> = new_map.keys().chain(old_map.keys()).collect();
    all_paths.sort();
    all_paths.dedup();

    let mut out = Vec::new();
    for raw_path in all_paths {
        let old_id = old_map.get(raw_path);
        let new_id = new_map.get(raw_path);
        let (old_bytes, new_bytes) = match (old_id, new_id) {
            (Some(o), Some(n)) if o == n => continue, // unchanged
            (Some(o), Some(n)) => (Some(read_blob(repo, *o)?), read_blob(repo, *n)?),
            (None, Some(n)) => (None, read_blob(repo, *n)?),
            (Some(_), None) => continue, // deletion — no added lines
            (None, None) => continue,
        };

        let (path, was_lossy) = bytes_to_pathbuf(raw_path);
        let path_str = path.to_string_lossy();
        if !path_is_scanned(&path_str) {
            continue;
        }
        if was_lossy {
            // Staged / changed-vs modes report non-UTF-8 paths
            // (unlike full-repo mode, which skips them) — spec test 25.
            warn(format!(
                "warning: path is not valid UTF-8; displaying lossy: {}",
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

/// Resolve a base reference to an object id, trying four candidate
/// forms in order: the name as given, then `refs/heads/<name>`,
/// `refs/remotes/origin/<name>`, and `refs/tags/<name>`.
///
/// # Errors
///
/// Returns [`DomainsLintError::Reference`] if no candidate resolves.
fn resolve_base_ref(
    repo: &gix::Repository,
    reference: &str,
) -> Result<ObjectId, Report<DomainsLintError>> {
    let candidates = [
        reference.to_string(),
        format!("refs/heads/{reference}"),
        format!("refs/remotes/origin/{reference}"),
        format!("refs/tags/{reference}"),
    ];
    for candidate in &candidates {
        if let Ok(mut r) = repo.find_reference(candidate.as_str())
            && let Ok(id) = r.peel_to_id()
        {
            return Ok(id.detach());
        }
    }
    Err(Report::new(DomainsLintError::Reference(reference.to_string())))
}

/// Collect added lines on `HEAD` relative to the merge-base of
/// `reference` and `HEAD` — the CI/PR scan mode.
///
/// # Errors
///
/// Returns [`DomainsLintError`] if the repository cannot be opened,
/// the base ref does not resolve, no merge-base exists, or a tree or
/// blob cannot be read.
pub(crate) fn changed_vs_added_lines(
    repo_path: &Path,
    reference: &str,
) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(repo_path).change_context(DomainsLintError::OpenRepo)?;
    let head_id = repo
        .head_id()
        .change_context(DomainsLintError::OpenRepo)?
        .detach();
    let base_id = resolve_base_ref(&repo, reference)?;
    let merge_base = repo
        .merge_base(base_id, head_id)
        .change_context_lazy(|| DomainsLintError::MergeBase {
            base: reference.to_string(),
        })?
        .detach();

    let base_map = tree_blob_map(&commit_tree(&repo, merge_base)?)?;
    let head_map = tree_blob_map(&commit_tree(&repo, head_id)?)?;
    collect_added_from_maps(&repo, &base_map, &head_map)
}

/// Resolve a commit id to its tree object.
fn commit_tree(
    repo: &gix::Repository,
    commit_id: ObjectId,
) -> Result<gix::Tree<'_>, Report<DomainsLintError>> {
    let tree_id = repo
        .find_commit(commit_id)
        .change_context(DomainsLintError::Diff)?
        .tree_id()
        .change_context(DomainsLintError::Diff)?
        .detach();
    repo.find_tree(tree_id)
        .change_context(DomainsLintError::Diff)
}

#[cfg(test)]
mod staged_added_lines_tests {
    use super::*;
    use crate::dev::lint::test_support;

    #[test]
    fn reports_added_line_with_new_side_line_number() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("a.rs"), "alpha\nbeta\ngamma\n")
            .expect("should write initial file");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "initial");

        std::fs::write(temp.path().join("a.rs"), "alpha\nNEW LINE\nbeta\ngamma\n")
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

        assert_eq!(added, vec![("a.rs".to_string(), 2, "NEW LINE".to_string())]);
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

#[cfg(test)]
mod changed_vs_tests {
    use super::*;
    use crate::dev::lint::test_support;

    /// Build a two-branch fixture: `main` with a base commit, then a
    /// `feature` branch that adds a line containing a disallowed URL.
    /// Returns the tempdir (kept alive by the caller).
    fn two_branch_fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());

        std::fs::write(temp.path().join("a.rs"), "let ok = 1;\n")
            .expect("should write base file");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "base");

        test_support::create_and_checkout_branch(&repo, "feature");
        std::fs::write(
            temp.path().join("a.rs"),
            "let ok = 1;\nlet bad = \"https://test.com\";\n",
        )
        .expect("should write feature change");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "feature change");

        temp
    }

    #[test]
    fn reports_lines_added_by_feature_branch() {
        let temp = two_branch_fixture();
        let lines = changed_vs_added_lines(temp.path(), "main")
            .expect("should compute changed-vs added lines");
        let added: Vec<_> = lines
            .iter()
            .map(|l| (l.line_no, l.content.clone()))
            .collect();
        assert_eq!(
            added,
            vec![(2, "let bad = \"https://test.com\";".to_string())],
            "should report only the line the feature branch added"
        );
    }

    #[test]
    fn resolves_via_remote_tracking_ref_fallback() {
        let temp = two_branch_fixture();
        let repo = gix::open(temp.path()).expect("should open repo");

        // Move refs/heads/main → refs/remotes/origin/main so the
        // bare name "main" only resolves via the fallback chain.
        let main_id = repo
            .find_reference("refs/heads/main")
            .expect("refs/heads/main should exist")
            .peel_to_id()
            .expect("should peel main")
            .detach();
        repo.reference(
            "refs/remotes/origin/main",
            main_id,
            gix::refs::transaction::PreviousValue::Any,
            "seed remote-tracking ref",
        )
        .expect("should create remote-tracking ref");

        use gix::refs::transaction::{Change, RefEdit, RefLog};
        let delete = RefEdit {
            change: Change::Delete {
                expected: gix::refs::transaction::PreviousValue::Any,
                log: RefLog::AndReference,
            },
            name: "refs/heads/main"
                .try_into()
                .expect("valid ref name"),
            deref: false,
        };
        repo.edit_reference(delete)
            .expect("should delete refs/heads/main");

        // resolve_base_ref must now fall through to
        // refs/remotes/origin/main.
        let lines = changed_vs_added_lines(temp.path(), "main")
            .expect("should resolve via remote-tracking fallback");
        assert_eq!(
            lines.len(),
            1,
            "fallback resolution should still find the feature change"
        );
        assert!(lines[0].content.contains("https://test.com"));
    }
}

/// Emit a "skipping" warning for a path that is being excluded from
/// a full-repo scan.
fn warn_skip(path: &Path, reason: &str) -> Result<(), Report<DomainsLintError>> {
    warn(format!("note: skipping {}: {reason}", path.display()))
}

/// Like [`warn_skip`] but for a raw byte path that is not valid UTF-8.
fn warn_skip_bytes(bytes: &[u8], reason: &str) -> Result<(), Report<DomainsLintError>> {
    warn(format!(
        "note: skipping {}: {reason}",
        String::from_utf8_lossy(bytes)
    ))
}

/// Scan every line of every tracked file in the working tree —
/// the full-repo audit mode.
///
/// Reads working-tree content (not committed blobs), so it reports
/// the current local state including unstaged edits. Tracked files
/// that are missing, symlinks, non-regular, non-UTF-8-named, or
/// binary are skipped with a stderr warning.
///
/// # Errors
///
/// Returns [`DomainsLintError`] if the repository or its index
/// cannot be opened, the repository has no work directory, or a
/// scanned file fails to read for a reason other than binary
/// content.
pub(crate) fn full_repo_lines(
    repo_path: &Path,
) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(repo_path).change_context(DomainsLintError::OpenRepo)?;
    let work_dir = repo
        .workdir()
        .ok_or_else(|| Report::new(DomainsLintError::OpenRepo))?
        .to_path_buf();
    let index = repo.index().change_context(DomainsLintError::Index)?;

    let mut out = Vec::new();
    for entry in index.entries() {
        let raw = entry.path(&index);

        // Case 4: non-UTF-8 path — skip (full-repo mode does not
        // lossy-report; that is staged/changed-vs behavior).
        let Ok(rel_str) = std::str::from_utf8(raw) else {
            warn_skip_bytes(raw, "non-UTF-8 path")?;
            continue;
        };
        if !path_is_scanned(rel_str) {
            continue;
        }

        let path = work_dir.join(rel_str);
        // Case 1: tracked but missing from the working tree.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warn_skip(&path, "tracked but missing from working tree")?;
                continue;
            }
            Err(e) => {
                warn_skip(&path, &format!("metadata error: {e}"))?;
                continue;
            }
        };
        // Case 2: symlink — not followed.
        if meta.file_type().is_symlink() {
            warn_skip(&path, "symlink not followed")?;
            continue;
        }
        // Case 3: non-regular file (FIFO, socket, device).
        if !meta.file_type().is_file() {
            warn_skip(&path, "non-regular file")?;
            continue;
        }
        // Case 5: binary content.
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                warn_skip(&path, "binary content")?;
                continue;
            }
            Err(e) => {
                return Err(Report::new(DomainsLintError::ReadFile(path.clone()))
                    .attach(e.to_string()));
            }
        };

        for (i, line) in content.lines().enumerate() {
            out.push(DiffLine {
                path: PathBuf::from(rel_str),
                line_no: i + 1,
                content: line.to_string(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod full_repo_tests {
    use super::*;
    use crate::dev::lint::test_support;

    /// A clean tracked file is scanned line-by-line.
    #[test]
    fn scans_tracked_file_lines() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("a.rs"), "one\ntwo\nthree\n")
            .expect("should write file");
        test_support::stage_all(&repo);

        let lines = full_repo_lines(temp.path()).expect("should scan repo");
        let texts: Vec<_> = lines.iter().map(|l| l.content.clone()).collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
    }

    /// Case 1: a tracked file removed from the working tree is
    /// skipped, not a hard error.
    #[test]
    fn skips_tracked_but_missing_file() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("a.rs"), "kept\n").expect("should write a");
        std::fs::write(temp.path().join("gone.rs"), "removed\n").expect("should write gone");
        test_support::stage_all(&repo);
        std::fs::remove_file(temp.path().join("gone.rs")).expect("should remove gone");

        let lines = full_repo_lines(temp.path()).expect("should scan repo despite missing file");
        let texts: Vec<_> = lines.iter().map(|l| l.content.clone()).collect();
        assert_eq!(texts, vec!["kept"], "missing file is skipped, kept file scanned");
    }

    /// Case 2: a tracked path that became a symlink is skipped.
    #[cfg(unix)]
    #[test]
    fn skips_symlink() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("real.rs"), "real\n").expect("should write real");
        std::fs::write(temp.path().join("link.rs"), "placeholder\n")
            .expect("should write placeholder");
        test_support::stage_all(&repo);

        // Replace link.rs on disk with a symlink; the index entry
        // stays a regular file.
        std::fs::remove_file(temp.path().join("link.rs")).expect("should remove placeholder");
        std::os::unix::fs::symlink("real.rs", temp.path().join("link.rs"))
            .expect("should create symlink");

        let lines = full_repo_lines(temp.path()).expect("should scan repo");
        let texts: Vec<_> = lines.iter().map(|l| l.content.clone()).collect();
        assert_eq!(texts, vec!["real"], "symlink is skipped, real file scanned");
    }

    /// Case 5: a binary file is skipped, not a hard error.
    #[test]
    fn skips_binary_file() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("text.rs"), "hello\n").expect("should write text");
        // 0xff 0xfe is not a valid UTF-8 sequence — read_to_string
        // rejects it with ErrorKind::InvalidData. (A NUL byte would
        // NOT work: NUL is valid UTF-8.)
        std::fs::write(temp.path().join("data.json"), b"{\"x\":\xff\xfe}")
            .expect("should write binary");
        test_support::stage_all(&repo);

        let lines = full_repo_lines(temp.path()).expect("should scan repo despite binary file");
        let texts: Vec<_> = lines.iter().map(|l| l.content.clone()).collect();
        assert_eq!(texts, vec!["hello"], "binary file is skipped, text file scanned");
    }
}

#[cfg(test)]
mod path_is_scanned_tests {
    use super::*;

    #[test]
    fn scanned_paths() {
        for p in [
            "foo.rs",
            "foo.html",
            "foo.css",
            "Dockerfile",
            "Dockerfile.prod",
            "crates/trusted-server-core/src/html_processor.test.html",
            "crates/js/lib/src/core/templates/iframe.html",
            ".env.dev",
            "crates/integration-tests/fixtures/frameworks/nextjs/app/page.tsx",
            "crates/integration-tests/fixtures/frameworks/nextjs/Dockerfile",
            "crates/integration-tests/fixtures/frameworks/wordpress/Dockerfile",
            "README.md",
            "CHANGELOG.md",
            "CONTRIBUTING.md",
            "docs/guide/onboarding.md",
            "docs/superpowers/specs/2026-05-18-check-domains-design.md",
        ] {
            assert!(path_is_scanned(p), "should be scanned: {p}");
        }
    }

    #[test]
    fn not_scanned_paths() {
        for p in [
            "crates/trusted-server-core/src/integrations/nextjs/fixtures/inlined-data-escaped.html",
            "crates/trusted-server-core/src/integrations/google_tag_manager/fixtures/captured.html",
            "node_modules/foo.js",
            ".worktrees/x/y.rs",
            ".claude/worktrees/x/y.rs",
            "package-lock.json",
            "pnpm-lock.yaml",
            "Cargo.lock",
            "crates/trusted-server-cli/src/dev/lint/domains.rs",
            "foo.markdown",
            "foo.MD",
            "target/debug/build.rs",
            "image.png",
        ] {
            assert!(!path_is_scanned(p), "should NOT be scanned: {p}");
        }
    }
}

/// Scan explicitly-named paths in full.
///
/// Policy filters (extension/path exclusion, symlink, non-regular,
/// binary content) warn and skip. Access failures on a user-named
/// path are hard errors: a missing path or a permission failure
/// almost always means a typo or a real environment problem the
/// user should know about.
///
/// # Errors
///
/// Returns [`DomainsLintError::PathNotFound`] /
/// [`DomainsLintError::PermissionDenied`] /
/// [`DomainsLintError::ReadFile`] if a named path cannot be accessed.
pub(crate) fn explicit_path_lines(
    paths: &[PathBuf],
) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let mut out = Vec::new();
    for path in paths {
        let path_str = path.to_string_lossy();
        if !path_is_scanned(&path_str) {
            warn(format!(
                "note: {} is not in scanned extensions or is excluded; skipping",
                path.display()
            ))?;
            continue;
        }

        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => return Err(io_error_to_report(&e, path)),
        };
        if meta.file_type().is_symlink() {
            warn_skip(path, "symlink not followed")?;
            continue;
        }
        if !meta.file_type().is_file() {
            warn_skip(path, "non-regular file")?;
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                warn_skip(path, "binary content")?;
                continue;
            }
            Err(e) => return Err(io_error_to_report(&e, path)),
        };

        for (i, line) in content.lines().enumerate() {
            out.push(DiffLine {
                path: path.clone(),
                line_no: i + 1,
                content: line.to_string(),
            });
        }
    }
    Ok(out)
}

/// Map an [`std::io::Error`] on a user-named path to the matching
/// [`DomainsLintError`] variant.
fn io_error_to_report(err: &std::io::Error, path: &Path) -> Report<DomainsLintError> {
    match err.kind() {
        std::io::ErrorKind::NotFound => {
            Report::new(DomainsLintError::PathNotFound(path.to_path_buf()))
        }
        std::io::ErrorKind::PermissionDenied => {
            Report::new(DomainsLintError::PermissionDenied(path.to_path_buf()))
        }
        _ => Report::new(DomainsLintError::ReadFile(path.to_path_buf())).attach(err.to_string()),
    }
}

#[cfg(test)]
mod explicit_path_tests {
    use super::*;

    #[test]
    fn scans_a_valid_file() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let file = temp.path().join("a.rs");
        std::fs::write(&file, "one\ntwo\n").expect("should write file");

        let lines = explicit_path_lines(&[file]).expect("should scan named file");
        let texts: Vec<_> = lines.iter().map(|l| l.content.clone()).collect();
        assert_eq!(texts, vec!["one", "two"]);
    }

    #[test]
    fn skips_excluded_extension() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let file = temp.path().join("image.png");
        std::fs::write(&file, "not really a png").expect("should write file");

        let lines = explicit_path_lines(&[file]).expect("should skip excluded extension");
        assert!(lines.is_empty(), "excluded extension yields no lines");
    }

    #[test]
    fn skips_excluded_path() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let dir = temp.path().join("node_modules");
        std::fs::create_dir(&dir).expect("should create node_modules");
        let file = dir.join("pkg.js");
        std::fs::write(&file, "let x = 1;\n").expect("should write file");

        let lines = explicit_path_lines(&[file]).expect("should skip node_modules path");
        assert!(lines.is_empty(), "node_modules path yields no lines");
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlink() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let real = temp.path().join("real.rs");
        std::fs::write(&real, "real\n").expect("should write real");
        let link = temp.path().join("link.rs");
        std::os::unix::fs::symlink(&real, &link).expect("should create symlink");

        let lines = explicit_path_lines(&[link]).expect("should skip symlink");
        assert!(lines.is_empty(), "symlink yields no lines");
    }

    #[test]
    fn missing_path_is_hard_error() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let missing = temp.path().join("nope.rs");

        let err = explicit_path_lines(&[missing]).expect_err("missing path should error");
        assert!(
            matches!(err.current_context(), DomainsLintError::PathNotFound(_)),
            "should be PathNotFound: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_is_hard_error() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("should create tempdir");
        let file = temp.path().join("secret.rs");
        std::fs::write(&file, "secret\n").expect("should write file");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))
            .expect("should chmod 000");

        let result = explicit_path_lines(std::slice::from_ref(&file));
        // Restore perms so the tempdir can be cleaned up.
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644));

        let err = result.expect_err("permission-denied path should error");
        assert!(
            matches!(err.current_context(), DomainsLintError::PermissionDenied(_)),
            "should be PermissionDenied: {err:?}"
        );
    }
}

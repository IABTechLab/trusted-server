//! `ts dev lint domains` — URL-host linter.
//!
//! Design: docs/superpowers/specs/2026-05-18-check-domains-design.md

use core::error::Error;

use derive_more::Display;

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

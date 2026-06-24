//! Pure request-rewriting logic: rule matching and header outcomes (spec §8).

/// A rewrite-target authority: host plus a resolved port and its scheme default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    /// Hostname only — never used with a port for SNI.
    host: String,
    /// Resolved port (explicit, or the scheme default).
    pub port: u16,
    /// Scheme default for this authority (443 for TLS, 80 for plaintext).
    default_port: u16,
}

/// Errors from parsing/validating rules.
#[derive(Debug, derive_more::Display)]
pub enum RuleError {
    /// The `--map FROM=TO` value was not `FROM=TO`.
    #[display("expected FROM=TO, got `{value}`")]
    Map { value: String },
    /// The authority port was not a valid `u16`.
    #[display("invalid port in `{value}`")]
    Port { value: String },
    /// The authority host was empty.
    #[display("empty host in `{value}`")]
    EmptyHost { value: String },
}

impl core::error::Error for RuleError {}

impl Authority {
    /// Parses `HOST[:PORT]`, defaulting the port from `plaintext` (80) or TLS (443).
    ///
    /// # Errors
    ///
    /// Returns [`RuleError`] on an empty host or an unparseable port.
    pub fn parse(raw: &str, plaintext: bool) -> Result<Self, RuleError> {
        let default_port = if plaintext { 80 } else { 443 };
        let (host, port) = match raw.rsplit_once(':') {
            Some((h, p)) => {
                if p.is_empty() {
                    return Err(RuleError::Port {
                        value: raw.to_string(),
                    });
                }
                let port = p.parse::<u16>().map_err(|_| RuleError::Port {
                    value: raw.to_string(),
                })?;
                (h, port)
            }
            None => (raw, default_port),
        };
        if host.is_empty() {
            return Err(RuleError::EmptyHost {
                value: raw.to_string(),
            });
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
            default_port,
        })
    }

    /// The bare hostname (for SNI and connection target).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the port equals this authority's scheme default (443 TLS / 80
    /// plaintext) — so `:port` is omitted from the `Host` header.
    #[must_use]
    pub fn is_default_port(&self) -> bool {
        self.port == self.default_port
    }

    /// `host`, plus `:port` only when the port is non-default — for the `Host` header.
    #[must_use]
    pub fn host_with_port(&self) -> String {
        if self.is_default_port() {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// A single rewrite rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Production hostname to match (stored lowercase, port-stripped).
    pub from: String,
    /// Upstream target — kept a hostname so the SNI/certificate stay valid; the
    /// actual connection address may be pinned via `--resolve`.
    pub to: Authority,
    /// `--rewrite-host`: when true, send `Host: TO`; otherwise (default) send
    /// `Host: FROM`. The TLS SNI is always the `TO` host either way.
    pub rewrite_host: bool,
    /// Connect to the upstream over plaintext HTTP.
    pub plaintext: bool,
}

/// An ordered set of rules; first match wins.
#[derive(Debug, Clone, Default)]
pub struct RuleTable(pub Vec<Rule>);

impl RuleTable {
    /// Returns the first rule matching `host`, comparing case-insensitively and
    /// ignoring any `:port`.
    #[must_use]
    pub fn first_match(&self, host: &str) -> Option<&Rule> {
        let needle = host
            .rsplit_once(':')
            .map_or(host, |(h, _)| h)
            .to_ascii_lowercase();
        self.0
            .iter()
            .find(|r| r.from.to_ascii_lowercase() == needle)
    }
}

/// The header/SNI decisions for a matched rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// SNI to present upstream — always the `TO` host; never carries a port.
    pub sni: String,
    /// Value for the upstream `Host` header.
    pub host_header: String,
    /// Value for the `X-Orig-Host` header (always FROM).
    pub orig_host: String,
    /// Whether the upstream leg is TLS (`!plaintext`).
    pub scheme_is_tls: bool,
}

/// Computes the rewrite outcome for a matched rule (spec §8.3).
#[must_use]
pub fn rewrite_for(rule: &Rule) -> RewriteOutcome {
    // SNI is always the TO host (the connection address may be pinned separately
    // via `--resolve`); only the `Host` header depends on `rewrite_host`.
    let host_header = if rule.rewrite_host {
        rule.to.host_with_port()
    } else {
        rule.from.clone()
    };
    RewriteOutcome {
        sni: rule.to.host().to_string(),
        host_header,
        orig_host: rule.from.clone(),
        scheme_is_tls: !rule.plaintext,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, to: &str, rewrite_host: bool, plaintext: bool) -> Rule {
        Rule {
            from: from.to_string(),
            to: Authority::parse(to, plaintext).expect("should parse authority"),
            rewrite_host,
            plaintext,
        }
    }

    #[test]
    fn authority_defaults_port_443_for_tls() {
        let a = Authority::parse("staging.example.net", false).expect("should parse");
        assert_eq!(a.host(), "staging.example.net", "should keep host");
        assert_eq!(a.port, 443, "should default to 443 for TLS");
        assert!(a.is_default_port(), "443 is default for TLS");
        assert_eq!(
            a.host_with_port(),
            "staging.example.net",
            "default port omitted"
        );
    }

    #[test]
    fn authority_defaults_port_80_for_plaintext() {
        let a = Authority::parse("localhost", true).expect("should parse");
        assert_eq!(a.port, 80, "should default to 80 for plaintext");
        assert_eq!(a.host_with_port(), "localhost", "default port omitted");
    }

    #[test]
    fn authority_keeps_non_default_port_in_host_header_only() {
        let a = Authority::parse("localhost:3000", true).expect("should parse");
        assert_eq!(a.port, 3000, "should parse explicit port");
        assert!(!a.is_default_port(), "3000 is not default");
        assert_eq!(a.host(), "localhost", "SNI host must exclude port");
        assert_eq!(
            a.host_with_port(),
            "localhost:3000",
            "Host header includes non-default port"
        );
    }

    #[test]
    fn is_default_port_is_scheme_relative() {
        // TLS authority on :80 is NOT default — :80 must appear in Host.
        let tls_80 = Authority::parse("host.example.com:80", false).expect("parse");
        assert!(!tls_80.is_default_port(), "80 is not the TLS default");
        assert_eq!(
            tls_80.host_with_port(),
            "host.example.com:80",
            "Host keeps :80 for TLS"
        );
        // Plaintext authority on :443 is NOT default — :443 must appear in Host.
        let plain_443 = Authority::parse("host.example.com:443", true).expect("parse");
        assert!(
            !plain_443.is_default_port(),
            "443 is not the plaintext default"
        );
        assert_eq!(
            plain_443.host_with_port(),
            "host.example.com:443",
            "Host keeps :443 for plaintext"
        );
    }

    #[test]
    fn matching_is_case_insensitive_and_port_stripped() {
        let table = RuleTable(vec![rule(
            "www.example-publisher.com",
            "to.edgecompute.app",
            false,
            false,
        )]);
        let m = table
            .first_match("WWW.Example-Publisher.COM:443")
            .expect("should match");
        assert_eq!(
            m.from, "www.example-publisher.com",
            "match ignores case and port"
        );
        assert!(
            table.first_match("other.example.com").is_none(),
            "unmatched host returns None"
        );
    }

    #[test]
    fn first_match_wins() {
        let table = RuleTable(vec![
            rule("a.example.com", "first.edgecompute.app", false, false),
            rule("a.example.com", "second.edgecompute.app", false, false),
        ]);
        assert_eq!(
            table
                .first_match("a.example.com")
                .expect("should match")
                .to
                .host(),
            "first.edgecompute.app"
        );
    }

    #[test]
    fn rewrite_default_preserves_from_host_and_sets_sni_to_to() {
        let r = rule(
            "www.example-publisher.com",
            "to.edgecompute.app:8443",
            false,
            false,
        );
        let out = rewrite_for(&r);
        assert_eq!(
            out.sni, "to.edgecompute.app",
            "SNI is TO host only, no port"
        );
        assert_eq!(
            out.host_header, "www.example-publisher.com",
            "default Host is FROM"
        );
        assert_eq!(
            out.orig_host, "www.example-publisher.com",
            "X-Orig-Host is FROM"
        );
        assert!(out.scheme_is_tls, "TLS rule yields a TLS outcome");
    }

    #[test]
    fn rewrite_host_uses_to_authority_with_port() {
        let r = rule("www.example-publisher.com", "localhost:3000", true, true);
        let out = rewrite_for(&r);
        assert_eq!(out.sni, "localhost", "SNI never carries a port");
        assert_eq!(
            out.host_header, "localhost:3000",
            "rewrite-host sends TO host:port"
        );
        assert_eq!(
            out.orig_host, "www.example-publisher.com",
            "X-Orig-Host stays FROM"
        );
        assert!(
            !out.scheme_is_tls,
            "plaintext rule yields a non-TLS outcome"
        );
    }

    #[test]
    fn rejects_empty_or_missing_port() {
        let err =
            Authority::parse("host.example.com:", true).expect_err("should reject trailing colon");
        assert!(
            matches!(err, RuleError::Port { .. }),
            "trailing colon should be a Port error, got: {err}"
        );
    }
}

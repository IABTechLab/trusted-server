//! Validated destination grants for creative proxy identity disclosure.

use std::collections::HashSet;

use error_stack::Report;
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::consent::allows_proxy_ec_disclosure;
use crate::ec::{EcContext, is_valid_ec_id};
use crate::error::TrustedServerError;

/// One explicit HTTPS origin and path grant for creative EC disclosure.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProxyIdentityEndpoint {
    /// Stable operator-chosen diagnostic identifier.
    pub id: String,
    /// Exact HTTPS DNS origin, with an optional port.
    pub origin: String,
    /// Exact case-sensitive path selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_exact: Option<String>,
    /// Segment-boundary path prefix selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
}

/// Immutable validated creative identity destination policy.
#[derive(Debug, Clone, Default)]
pub struct CompiledProxyPolicy {
    endpoints: Vec<CompiledEndpoint>,
}

#[derive(Debug, Clone)]
struct CompiledEndpoint {
    rule: ProxyIdentityEndpoint,
    host: String,
    port: u16,
}

/// Select whether a proxy contract may disclose canonical query identity.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub enum ProxyQueryIdentity {
    /// Never disclose identity through the proxy query.
    #[default]
    Disabled,
    /// Consult explicit creative identity endpoint grants and request consent.
    CreativeEndpoints,
}

/// Effective destination permission and identity delivery contract for one hop.
#[derive(Debug, Clone, Copy)]
pub struct ProxyHopDecision<'a> {
    /// Whether the creative route may fetch this parsed target.
    pub fetch_allowed: bool,
    /// Explicit recipient grant, absent when fetching or disclosure is denied.
    pub endpoint: Option<&'a ProxyIdentityEndpoint>,
}

/// Privacy accumulated across one creative fetch and all of its redirects.
#[derive(Debug, Default)]
pub(crate) struct ProxyChainState {
    private: bool,
}

impl ProxyChainState {
    pub(crate) fn visit(&mut self, decision: &ProxyHopDecision<'_>) {
        self.private |= decision.endpoint.is_some();
    }

    pub(crate) fn is_private(&self) -> bool {
        self.private
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ProxyDisclosureReason {
    NoCanonicalCookie,
    ConsentWithheld,
    Eligible,
}

/// Request-local canonical cookie identity authorized by current consent.
#[derive(Debug, Copy, Clone)]
pub struct ProxyDisclosureContext<'a> {
    identity: Option<&'a str>,
    reason: ProxyDisclosureReason,
}

impl<'a> ProxyDisclosureContext<'a> {
    pub(crate) fn from_ec_context(context: &'a EcContext) -> Self {
        let cookie = context
            .existing_cookie_ec_id()
            .filter(|identity| is_valid_ec_id(identity));
        let reason = match cookie {
            None => ProxyDisclosureReason::NoCanonicalCookie,
            Some(_) if allows_proxy_ec_disclosure(context.consent()) => {
                ProxyDisclosureReason::Eligible
            }
            Some(_) => ProxyDisclosureReason::ConsentWithheld,
        };
        Self {
            identity: cookie.filter(|_| reason == ProxyDisclosureReason::Eligible),
            reason,
        }
    }

    pub(crate) fn reason(&self) -> ProxyDisclosureReason {
        self.reason
    }

    pub(crate) fn identity(&self) -> Option<&'a str> {
        self.identity
    }
}

impl CompiledProxyPolicy {
    /// Validate endpoint selectors and compile an immutable matching policy.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for invalid or duplicate endpoint rules.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::proxy_policy::CompiledProxyPolicy;
    /// let policy = CompiledProxyPolicy::compile(&[]).expect("should compile empty policy");
    /// ```
    pub fn compile(
        endpoints: &[ProxyIdentityEndpoint],
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut ids = HashSet::new();
        let mut selectors = HashSet::new();
        let mut compiled = Vec::with_capacity(endpoints.len());
        for endpoint in endpoints {
            if endpoint.id.is_empty()
                || endpoint.id.len() > 64
                || !endpoint
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(invalid_rule(
                    "identity endpoint id must be 1–64 ASCII letters, digits, underscores or hyphens",
                ));
            }
            let invalid = |message: &str| {
                invalid_rule(&format!("identity endpoint `{}`: {message}", endpoint.id))
            };
            if !ids.insert(endpoint.id.as_str()) {
                return Err(invalid("duplicate id"));
            }
            let origin = validated_origin(&endpoint.origin)
                .ok_or_else(|| invalid("origin must be an exact HTTPS DNS origin"))?;
            let (exact, path) = match (&endpoint.path_exact, &endpoint.path_prefix) {
                (Some(path), None) => (true, path),
                (None, Some(path)) if path == "/" || path.ends_with('/') => (false, path),
                _ => {
                    return Err(invalid(
                        "exactly one path_exact or slash-terminated path_prefix is required",
                    ));
                }
            };
            if !eligible_path(path) {
                return Err(invalid(
                    "path must be absolute without escapes, dot segments, whitespace, query or fragment",
                ));
            }
            let mut serialized = origin.clone();
            serialized.set_path(path);
            if serialized.path() != path {
                return Err(invalid(
                    "path must retain its bytes after URL serialization",
                ));
            }
            let host = origin
                .host_str()
                .ok_or_else(|| invalid("origin must have a DNS host"))?
                .to_owned();
            let port = origin.port_or_known_default().unwrap_or(443);
            if !selectors.insert((host.clone(), port, exact, path.clone())) {
                return Err(invalid("duplicate normalized selector"));
            }
            let mut rule = endpoint.clone();
            rule.origin = origin.origin().ascii_serialization();
            compiled.push(CompiledEndpoint { rule, host, port });
        }
        Ok(Self {
            endpoints: compiled,
        })
    }

    /// Evaluate fetch permission and recipient rules for a canonical creative target.
    ///
    /// No current-reader consent or identity is assumed by this configuration decision.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::proxy_policy::CompiledProxyPolicy;
    /// use url::Url;
    /// let policy = CompiledProxyPolicy::default();
    /// let target = Url::parse("https://example.com/asset").expect("should parse URL");
    /// assert!(policy.evaluate(&target, &[]).fetch_allowed);
    /// ```
    #[must_use]
    pub fn evaluate(&self, target: &Url, allowed_domains: &[String]) -> ProxyHopDecision<'_> {
        let fetch_allowed = matches!(target.scheme(), "http" | "https")
            && target
                .host_str()
                .is_some_and(|host| crate::proxy::is_host_permitted(allowed_domains, host))
            && target.username().is_empty()
            && target.password().is_none();
        ProxyHopDecision {
            fetch_allowed,
            endpoint: fetch_allowed
                .then(|| self.matching_endpoint(target))
                .flatten(),
        }
    }

    /// Return the exact or longest-prefix rule matching a canonical target.
    ///
    /// Query parameters do not participate in matching. Percent-escaped paths,
    /// credentials, and non-HTTPS targets cannot receive an identity grant.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::proxy_policy::CompiledProxyPolicy;
    /// use url::Url;
    /// let policy = CompiledProxyPolicy::default();
    /// let target = Url::parse("https://example.com/event").expect("should parse URL");
    /// assert!(policy.matching_endpoint(&target).is_none());
    /// ```
    #[must_use]
    pub fn matching_endpoint(&self, target: &Url) -> Option<&ProxyIdentityEndpoint> {
        if target.scheme() != "https"
            || !target.username().is_empty()
            || target.password().is_some()
            || !eligible_path(target.path())
        {
            return None;
        }
        self.endpoints
            .iter()
            .filter(|endpoint| {
                target.host_str() == Some(endpoint.host.as_str())
                    && target.port_or_known_default() == Some(endpoint.port)
                    && match (&endpoint.rule.path_exact, &endpoint.rule.path_prefix) {
                        (Some(path), _) => target.path() == path,
                        (_, Some(prefix)) => target.path().starts_with(prefix),
                        _ => false,
                    }
            })
            .max_by_key(|endpoint| {
                (
                    endpoint.rule.path_exact.is_some(),
                    endpoint
                        .rule
                        .path_exact
                        .as_ref()
                        .or(endpoint.rule.path_prefix.as_ref())
                        .map_or(0, String::len),
                )
            })
            .map(|endpoint| &endpoint.rule)
    }
}

fn invalid_rule(message: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("proxy.identity_endpoints: {message}"),
    })
}

fn eligible_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '%' | '\\' | '?' | '#')
        })
        && !path.split('/').any(|segment| matches!(segment, "." | ".."))
}

fn validated_origin(raw: &str) -> Option<Url> {
    if !raw.is_ascii()
        || raw.bytes().any(|byte| {
            byte.is_ascii_whitespace()
                || byte.is_ascii_control()
                || matches!(byte, b'\\' | b'%' | b'?' | b'#' | b'@')
        })
    {
        return None;
    }
    let (scheme, remainder) = raw.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = remainder.strip_suffix('/').unwrap_or(remainder);
    if authority.contains('/') || authority.ends_with(':') {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let Host::Domain(host) = url.host()? else {
        return None;
    };
    if url.scheme() != "https"
        || url.port() == Some(0)
        || host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return None;
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;

    fn rule(
        id: &str,
        origin: &str,
        exact: Option<&str>,
        prefix: Option<&str>,
    ) -> ProxyIdentityEndpoint {
        ProxyIdentityEndpoint {
            id: id.to_owned(),
            origin: origin.to_owned(),
            path_exact: exact.map(str::to_owned),
            path_prefix: prefix.map(str::to_owned),
        }
    }

    #[test]
    fn proxy_policy_rejects_selectors_changed_by_url_serialization() {
        for path in ["/café", "/a<b", "/a{b"] {
            let endpoint = rule("unreachable", "https://example.com", Some(path), None);
            assert!(
                CompiledProxyPolicy::compile(&[endpoint]).is_err(),
                "should reject unreachable selector {path}"
            );
        }
    }

    #[test]
    fn identity_policy_disclosure_uses_only_valid_cookie_and_current_consent() {
        let canonical = format!("{}.Ab1234", "a".repeat(64));
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let context = EcContext::new_for_test(Some(canonical.clone()), consent.clone());
        assert_eq!(
            ProxyDisclosureContext::from_ec_context(&context).identity(),
            Some(canonical.as_str()),
            "should disclose valid canonical cookie with affirmative current consent"
        );
        for denied in [
            EcContext::new_for_test(None, consent.clone()),
            EcContext::new_for_test(Some("arbitrary-header-value".to_owned()), consent.clone()),
            EcContext::new_for_test(
                Some(canonical),
                ConsentContext {
                    gpc: true,
                    ..consent
                },
            ),
        ] {
            assert!(
                ProxyDisclosureContext::from_ec_context(&denied)
                    .identity()
                    .is_none(),
                "should withhold missing invalid or denied identity"
            );
        }
    }

    #[test]
    fn identity_policy_exact_wins_then_longest_prefix_independent_of_order() {
        let rules = vec![
            rule("root", "https://EXAMPLE.com:443/", None, Some("/")),
            rule("events", "https://example.com", None, Some("/events/")),
            rule("exact", "https://example.com", Some("/events/pixel"), None),
        ];
        for rules in [rules.clone(), rules.into_iter().rev().collect()] {
            let policy =
                CompiledProxyPolicy::compile(&rules).expect("should compile overlapping rules");
            for (path, expected) in [
                ("/events/pixel?x=1", "exact"),
                ("/events/a", "events"),
                ("/events", "root"),
                ("/eventual", "root"),
            ] {
                let target =
                    Url::parse(&format!("https://example.com{path}")).expect("should parse target");
                assert_eq!(
                    policy
                        .matching_endpoint(&target)
                        .map(|rule| rule.id.as_str()),
                    Some(expected),
                    "should choose most specific selector for {path}"
                );
            }
            for target in [
                "http://example.com/events/pixel",
                "https://example.com:444/events/pixel",
                "https://other.example.com/events/pixel",
                "https://user@example.com/events/pixel",
                "https://example.com/events/%70ixel",
            ] {
                assert!(
                    policy
                        .matching_endpoint(&Url::parse(target).expect("should parse target"))
                        .is_none(),
                    "should deny unmatched or ambiguous target {target}"
                );
            }
        }
    }

    #[test]
    fn identity_policy_rejects_invalid_origins() {
        for origin in [
            "http://example.com",
            "https://example.com/a",
            "https://example.com/.",
            "https://example.com/../",
            "https://example.com?x",
            "https://example.com#x",
            "https://user@example.com",
            "https://@example.com",
            "https://127.0.0.1",
            "https://[::1]",
            "https://*.example.com",
            "https://éxample.example.com",
            "https://example.com.",
            "https://a..example.com",
            "https://example.com:0",
            " https://example.com",
            "https://example.com\\",
            "https://exa%mple.example.com",
            "https://example.com:",
        ] {
            assert!(
                CompiledProxyPolicy::compile(&[rule("rule", origin, Some("/event"), None)])
                    .is_err(),
                "should reject origin {origin}"
            );
        }
    }

    #[test]
    fn identity_policy_rejects_invalid_paths_and_rule_ids() {
        for path in [
            "event", "/a?x", "/a#x", "/a\\b", "/a b", "/a\tb", "/%61", "/a/./b", "/a/../b", "/.",
            "/..",
        ] {
            assert!(
                CompiledProxyPolicy::compile(&[rule(
                    "rule",
                    "https://example.com",
                    Some(path),
                    None
                )])
                .is_err(),
                "should reject path {path:?}"
            );
        }
        for id in ["", "with space", "é", &"a".repeat(65)] {
            assert!(
                CompiledProxyPolicy::compile(&[rule(
                    id,
                    "https://example.com",
                    Some("/event"),
                    None
                )])
                .is_err(),
                "should reject id {id:?}"
            );
        }
        for (exact, prefix) in [
            (None, None),
            (Some("/a"), Some("/a/")),
            (None, Some("/events")),
        ] {
            assert!(
                CompiledProxyPolicy::compile(&[rule("rule", "https://example.com", exact, prefix)])
                    .is_err(),
                "should reject invalid selector shape"
            );
        }
    }

    #[test]
    fn identity_policy_rejects_duplicate_ids_and_normalized_selectors() {
        for rules in [
            vec![
                rule("same", "https://example.com", Some("/a"), None),
                rule("same", "https://example.com", Some("/b"), None),
            ],
            vec![
                rule("one", "https://EXAMPLE.com:443/", Some("/a"), None),
                rule("two", "https://example.com", Some("/a"), None),
            ],
        ] {
            assert!(
                CompiledProxyPolicy::compile(&rules).is_err(),
                "should reject duplicate identifiers or selectors"
            );
        }
        assert!(
            CompiledProxyPolicy::default()
                .matching_endpoint(
                    &Url::parse("https://example.com/a").expect("should parse target")
                )
                .is_none(),
            "should deny all identity recipients by default"
        );
    }

    #[test]
    fn identity_policy_config_rejects_unknown_fields() {
        let value = serde_json::json!({"id":"event","origin":"https://example.com","path_exact":"/event","cached":true});
        assert!(
            serde_json::from_value::<ProxyIdentityEndpoint>(value).is_err(),
            "should reject unknown rule fields"
        );
    }
}

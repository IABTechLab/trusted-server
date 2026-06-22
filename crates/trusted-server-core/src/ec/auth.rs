//! Shared Bearer-token authentication helpers for EC partner endpoints.
//!
//! Used by both `/_ts/api/v1/identify` and `/_ts/api/v1/batch-sync` so
//! authentication hardening stays consistent across endpoints.

use edgezero_core::body::Body as EdgeBody;
use http::Request;

use super::partner::hash_api_key;
use super::registry::{PartnerConfig, PartnerRegistry};

/// Authenticates a request via Bearer token, returning the matching partner.
pub(super) fn authenticate_bearer<'r>(
    registry: &'r PartnerRegistry,
    req: &Request<EdgeBody>,
) -> Option<&'r PartnerConfig> {
    let header_value = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())?;
    let token = parse_bearer_token(header_value)?;
    let key_hash = hash_api_key(token);
    registry.find_by_api_key_hash(&key_hash)
}

fn parse_bearer_token(header_value: &str) -> Option<&str> {
    let mut parts = header_value.split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;

    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }

    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    const VALID_API_TOKEN: &str = "auth-test-token-32-bytes-minimum";

    fn make_test_partner(source_domain: &str, api_token: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(api_token.to_owned()),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    #[test]
    fn parse_bearer_token_accepts_case_insensitive_scheme() {
        assert_eq!(parse_bearer_token("Bearer tok"), Some("tok"));
        assert_eq!(parse_bearer_token("bearer tok"), Some("tok"));
        assert_eq!(parse_bearer_token("BEARER tok"), Some("tok"));
    }

    #[test]
    fn parse_bearer_token_rejects_invalid_shapes() {
        assert_eq!(parse_bearer_token("Bearer"), None);
        assert_eq!(parse_bearer_token("Bearer "), None);
        assert_eq!(parse_bearer_token("Basic abc"), None);
        assert_eq!(parse_bearer_token("Bearer a b"), None);
    }

    #[test]
    fn authenticate_bearer_returns_none_for_missing_header() {
        let registry = PartnerRegistry::empty();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.example.com/_ts/api/v1/identify")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let result = authenticate_bearer(&registry, &req);
        assert!(result.is_none(), "should return None without auth header");
    }

    #[test]
    fn authenticate_bearer_returns_none_for_malformed_header() {
        let registry = PartnerRegistry::empty();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.example.com/_ts/api/v1/identify")
            .header("authorization", "Basic dXNlcjpwYXNz")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let result = authenticate_bearer(&registry, &req);
        assert!(
            result.is_none(),
            "should return None for non-Bearer auth scheme"
        );
    }

    #[test]
    fn authenticate_bearer_returns_matching_partner_for_valid_token() {
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.example.com/_ts/api/v1/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .body(EdgeBody::empty())
            .expect("should build test request");

        let result = authenticate_bearer(&registry, &req).expect("should authenticate partner");
        assert_eq!(
            result.source_domain, "ssp.example.com",
            "should return the matching partner"
        );
    }
}

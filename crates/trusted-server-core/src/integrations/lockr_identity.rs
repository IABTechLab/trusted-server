//! Lockr as a capture-only identity provider (spec 12.2).
//!
//! Reads the Lockr API responses Trusted Server already proxies. Capture is
//! revoke-only: a `revoke-consent` call opts the visitor out of Lockr's
//! sources, and a later token issue clears that opt-out. The tokens it
//! reads are not stored; browser ingest carries Lockr's IDs to KV.

use std::collections::BTreeMap;

use error_stack::Report;
use http::Method;
use serde::Deserialize;

use crate::ec::identity::{
    CaptureInput, Captures, EidProvenance, IdentityProvider, IdentityProviderConfig,
    IdentityProviderError, ProviderDescriptor, Resolution, ResolvedEid, UnixTime,
};
use crate::integrations::lockr::{LOCKR_INTEGRATION_ID, LockrConfig};
use crate::integrations::vendor_eid::VendorEid;

/// The proxied Lockr API prefix.
const LOCKR_API: &str = "/integrations/lockr/api/publisher/app/v2/identityLockr";

/// The Lockr API calls captured, by their last path segment.
const CAPTURED_ENDPOINTS: &[&str] = &[
    "page-view",
    "generate-tokens",
    "refresh-tokens",
    "sync-no-hem-ids",
    "revoke-consent",
];

/// The endpoint that reports a consent revocation.
const REVOKE_ENDPOINT: &str = "revoke-consent";

/// Lockr's token key names and the sources they carry. Only confirmed
/// mappings are listed.
const KEY_SOURCES: &[(&str, &str)] = &[
    ("id5id", "id5-sync.com"),
    ("_lr_env", "liveramp.com"),
    ("__uid2_advertising_token", "uidapi.com"),
];

impl IdentityProviderConfig for LockrConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut captures = Captures::new();
        for endpoint in CAPTURED_ENDPOINTS {
            captures = captures.with_path(Method::POST, &format!("{LOCKR_API}/{endpoint}"));
        }
        let mut builder =
            ProviderDescriptor::builder(LOCKR_INTEGRATION_ID.parse()?).with_captures(captures);
        for (_, domain) in KEY_SOURCES {
            builder = builder.with_source(domain.parse()?);
        }
        builder
            .with_provenance(
                &"id5-sync.com".parse()?,
                EidProvenance::new()
                    .with_inserter("Lockr-For-Publishers")
                    .with_mm(2),
            )
            .build()
    }
}

/// The Lockr capture provider.
pub struct LockrIdentityProvider {
    descriptor: ProviderDescriptor,
}

impl LockrIdentityProvider {
    /// Builds the provider from Lockr's validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the descriptor is
    /// invalid.
    pub fn new(config: &LockrConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
        })
    }
}

#[derive(Deserialize)]
struct TokenReply {
    #[serde(rename = "aimTokens", default)]
    aim_tokens: Vec<AimToken>,
    #[serde(default)]
    ids: BTreeMap<String, LockrId>,
}

#[derive(Deserialize)]
struct AimToken {
    key_name: String,
    /// Milliseconds since the epoch.
    identity_expires: Option<u64>,
}

#[derive(Deserialize)]
struct LockrId {
    eid: VendorEid,
}

impl IdentityProvider for LockrIdentityProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if input
            .path()
            .rsplit('/')
            .next()
            .is_some_and(|endpoint| endpoint == REVOKE_ENDPOINT)
        {
            return Ok(Resolution::opted_out());
        }
        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        let eids: Vec<ResolvedEid> = reply
            .ids
            .iter()
            .filter_map(|(key, entry)| {
                let (_, domain) = KEY_SOURCES.iter().find(|(name, _)| name == key)?;
                let source = input.source_named(domain)?;
                if !source.matches(&entry.eid.source) {
                    return None;
                }
                let expires = reply
                    .aim_tokens
                    .iter()
                    .find(|token| token.key_name == *key)
                    .and_then(|token| token.identity_expires)
                    .map(UnixTime::from_millis);
                if expires.is_some_and(|at| at <= input.now()) {
                    return None;
                }
                let eid = entry.eid.to_resolved(source)?;
                Some(match expires {
                    Some(at) => eid.with_expires_at(at),
                    None => eid,
                })
            })
            .collect();
        Ok(if eids.is_empty() {
            Resolution::no_match()
        } else {
            Resolution::found(eids)
        })
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::ec::identity::When;
    use crate::ec::identity::test_support::{CaptureFixture, ConformanceCases, ProviderHarness};

    fn config() -> LockrConfig {
        serde_json::from_value(json!({ "app_id": "app-1", "capture_identity": true }))
            .expect("should parse the Lockr config")
    }

    fn provider() -> LockrIdentityProvider {
        LockrIdentityProvider::new(&config()).expect("should build the provider")
    }

    fn path(endpoint: &str) -> String {
        format!("{LOCKR_API}/{endpoint}")
    }

    #[test]
    fn a_revocation_opts_out() {
        let provider = provider();

        let resolution = ProviderHarness::new(&provider)
            .capture(&CaptureFixture::new(Method::POST, &path("revoke-consent")))
            .expect("should capture");

        assert!(resolution.is_opted_out(), "should opt the visitor out");
    }

    #[test]
    fn reads_complete_eids_for_known_keys() {
        let provider = provider();
        let fixture = CaptureFixture::new(Method::POST, &path("generate-tokens")).with_response(
            StatusCode::OK,
            &json!({
                "aimTokens": [{ "key_name": "id5id", "identity_expires": 1_770_000_000_000_u64 }],
                "ids": {
                    "id5id": { "eid": {
                        "source": "id5-sync.com",
                        "uids": [{ "id": "ID5*example", "atype": 1 }],
                        "inserter": "inserter.example.com"
                    }},
                    "unknown": { "eid": {
                        "source": "other.example.com",
                        "uids": [{ "id": "ignored" }]
                    }}
                }
            }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        let eids = resolution.eids();
        assert_eq!(eids.len(), 1, "should keep only mapped keys");
        assert_eq!(
            eids[0].source().as_str(),
            "id5-sync.com",
            "should map id5id"
        );
        assert_eq!(
            eids[0].inserter(),
            Some("inserter.example.com"),
            "should keep the EID fields"
        );
    }

    #[test]
    fn skips_an_expired_token() {
        let provider = provider();
        let fixture = CaptureFixture::new(Method::POST, &path("refresh-tokens")).with_response(
            StatusCode::OK,
            &json!({
                "aimTokens": [{ "key_name": "id5id", "identity_expires": 1_000_u64 }],
                "ids": { "id5id": { "eid": {
                    "source": "id5-sync.com",
                    "uids": [{ "id": "ID5*expired" }]
                }}}
            }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        assert!(resolution.is_no_match(), "should skip the expired token");
    }

    fn tokens(path_endpoint: &str, body: &serde_json::Value) -> CaptureFixture {
        CaptureFixture::new(Method::POST, &path(path_endpoint)).with_response(StatusCode::OK, body)
    }

    #[test]
    fn captures_exactly_the_proxied_identity_endpoints() {
        let descriptor = config().build_descriptor().expect("should build");
        let captures = descriptor.captures().expect("should capture");

        for endpoint in CAPTURED_ENDPOINTS {
            assert!(
                captures.matches(&Method::POST, &path(endpoint)),
                "should capture POST {endpoint}"
            );
            assert!(
                !captures.matches(&Method::GET, &path(endpoint)),
                "should not capture GET {endpoint}"
            );
        }
        assert!(
            !captures.matches(&Method::POST, &path("other")),
            "should not capture another endpoint"
        );
        assert!(
            !captures.matches(&Method::POST, &format!("{}/x", path("revoke-consent"))),
            "should not capture a longer path"
        );
        assert!(descriptor.resolves().is_none(), "should never resolve");
    }

    #[test]
    fn a_revocation_opts_out_whatever_the_body() {
        let provider = provider();
        let fixture = tokens(
            "revoke-consent",
            &json!({ "ids": { "id5id": { "eid": { "source": "id5-sync.com", "uids": [{ "id": "x" }] } } } }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        assert!(resolution.is_opted_out(), "should opt out");
        assert!(
            resolution.eids().is_empty(),
            "should report no ID on a revocation"
        );
    }

    #[test]
    fn maps_each_known_key_to_its_source() {
        let provider = provider();
        let fixture = tokens(
            "page-view",
            &json!({ "ids": {
                "id5id": { "eid": { "source": "id5-sync.com", "uids": [{ "id": "id5" }] } },
                "_lr_env": { "eid": { "source": "liveramp.com", "uids": [{ "id": "env" }] } },
                "__uid2_advertising_token": { "eid": { "source": "uidapi.com", "uids": [{ "id": "uid2" }] } }
            }}),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        let mut sources: Vec<&str> = resolution
            .eids()
            .iter()
            .map(|eid| eid.source().as_str())
            .collect();
        sources.sort_unstable();
        assert_eq!(
            sources,
            ["id5-sync.com", "liveramp.com", "uidapi.com"],
            "should map all three"
        );
    }

    #[test]
    fn skips_an_entry_whose_source_does_not_match_its_key() {
        let provider = provider();
        let fixture = tokens(
            "sync-no-hem-ids",
            &json!({ "ids": { "id5id": { "eid": { "source": "uidapi.com", "uids": [{ "id": "x" }] } } } }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        assert!(
            resolution.is_no_match(),
            "should not file a UID under another key's source"
        );
    }

    #[test]
    fn a_token_without_an_expiry_keeps_no_expiry_and_a_future_one_keeps_it() {
        let provider = provider();
        let fixture = tokens(
            "generate-tokens",
            &json!({
                "aimTokens": [{ "key_name": "_lr_env", "identity_expires": 9_999_999_999_000_u64 }],
                "ids": {
                    "id5id": { "eid": { "source": "id5-sync.com", "uids": [{ "id": "id5" }] } },
                    "_lr_env": { "eid": { "source": "liveramp.com", "uids": [{ "id": "env" }] } }
                }
            }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        for eid in resolution.eids() {
            match eid.source().as_str() {
                "id5-sync.com" => assert_eq!(eid.expiry(), None, "should not invent an expiry"),
                "liveramp.com" => assert_eq!(
                    eid.expiry(),
                    Some(When::At(UnixTime::from_millis(9_999_999_999_000))),
                    "should keep the token expiry"
                ),
                other => panic!("should not resolve {other}"),
            }
        }
        assert_eq!(resolution.eids().len(), 2, "should resolve both");
    }

    #[test]
    fn a_body_it_cannot_read_is_no_match_not_an_error() {
        let provider = provider();
        for body in [
            json!("text"),
            json!([1, 2]),
            json!({ "ids": { "id5id": { "eid": 5 } } }),
        ] {
            let resolution = ProviderHarness::new(&provider)
                .capture(&tokens("page-view", &body))
                .expect("should not fail the proxied call");

            assert!(resolution.is_no_match(), "should be no match for {body}");
        }
    }

    #[test]
    fn an_entry_without_a_uid_is_skipped() {
        let provider = provider();
        let fixture = tokens(
            "page-view",
            &json!({ "ids": { "id5id": { "eid": { "source": "id5-sync.com", "uids": [{ "id": "" }] } } } }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        assert!(resolution.is_no_match(), "should skip an empty UID");
    }

    #[test]
    fn declares_lockr_provenance_for_id5() {
        let descriptor = config().build_descriptor().expect("should build");

        let provenance = descriptor
            .provenance(&"id5-sync.com".parse().expect("should parse"))
            .expect("should declare ID5 provenance");

        assert_eq!(
            provenance.inserter(),
            Some("Lockr-For-Publishers"),
            "should name Lockr"
        );
        assert_eq!(provenance.mm(), Some(2), "should mark the match method");
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance(
            &ConformanceCases::default()
                .with_capture_case(CaptureFixture::new(Method::POST, &path("revoke-consent"))),
        );
    }
}

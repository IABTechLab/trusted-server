//! Today's pull sync, modelled as an identity provider.
//!
//! A pull-enabled `[[ec.partners]]` entry becomes one [`GenericPullProvider`]:
//! a `GET` of the partner's `pull_sync_url` with the EC ID, answered with one
//! UID. The coordinator owns eligibility, pacing, consent and write-back.

use error_stack::{Report, ResultExt as _};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::descriptor::{ConsentPolicy, ProviderDescriptor, Resolves};
use super::exchange::{ProviderRequest, ProviderResponse};
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::ResolveInput;
use super::provider::IdentityProvider;
use super::resolution::{IdentityProviderError, Resolution, ResolvedEid, ResolvedUid};
use crate::ec::partner::normalize_partner_source_domain;
use crate::ec::prebid_eids::is_valid_eid_uid;
use crate::ec::pull_sync::{build_pull_request_url, validated_pull_sync_url};
use crate::error::TrustedServerError;
use crate::redacted::Redacted;
use crate::settings::EcPartner;

/// Longest provider ID, from `^[a-z][a-z0-9_-]{0,62}$`.
const MAX_PROVIDER_ID_LENGTH: usize = 63;

/// Readable characters kept when the ID is cut, leaving room for `-` and the
/// 8 hash characters.
const CUT_READABLE_LENGTH: usize = MAX_PROVIDER_ID_LENGTH - 9;

/// Derives the provider ID of the pull partner with `source_domain`.
///
/// It is `pull-` plus the normalized domain with `.` replaced by `-`. A
/// domain that already contains `-` also gets `-` and the first 8 hex digits
/// of the SHA-256 of the domain, so `a-b.example.com` and `a.b.example.com`
/// differ. An ID over 63 characters is cut to 54 readable characters, then
/// `-` and the hash.
#[must_use]
pub(crate) fn generic_pull_id(source_domain: &str) -> IdentityProviderId {
    let domain = normalize_partner_source_domain(source_domain)
        .unwrap_or_else(|_| source_domain.to_ascii_lowercase());
    let readable: String = format!("pull-{domain}")
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let digest = Sha256::digest(domain.as_bytes());
    let hash = hex::encode(&digest[..4]);

    let id = if domain.contains('-') {
        format!("{readable}-{hash}")
    } else {
        readable.clone()
    };
    let id = if id.len() > MAX_PROVIDER_ID_LENGTH {
        format!("{}-{hash}", &readable[..CUT_READABLE_LENGTH])
    } else {
        id
    };
    id.parse()
        .expect("should derive a valid provider ID from a normalized domain")
}

/// The descriptors of every pull-enabled partner.
///
/// Reads no secret, so it works on unresolved deploy-time settings.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when a pull-enabled partner
/// cannot be described, for example a `pull_sync_allowed_domains` entry that
/// is not a hostname.
pub(crate) fn generic_pull_descriptors(
    partners: &[EcPartner],
) -> Result<Vec<ProviderDescriptor>, Report<TrustedServerError>> {
    partners
        .iter()
        .filter(|partner| partner.pull_sync_enabled)
        .map(|partner| {
            describe(partner).change_context(TrustedServerError::Configuration {
                message: format!(
                    "ec.partners: pull sync of '{}' cannot be described as an identity provider",
                    partner.source_domain
                ),
            })
        })
        .collect()
}

fn describe(partner: &EcPartner) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
    let mut resolves = Resolves::new().with_ec_id();
    for host in &partner.pull_sync_allowed_domains {
        resolves = resolves.with_host(&host.trim().trim_end_matches('.').to_ascii_lowercase());
    }
    let source: SourceDomain = partner.source_domain.parse()?;
    ProviderDescriptor::builder(generic_pull_id(&partner.source_domain))
        .with_source(source)
        .with_resolves(resolves)
        .with_consent_policy(ConsentPolicy::eid_persistence_only())
        .build()
}

/// The answer a pull partner gives.
#[derive(Debug, Deserialize)]
struct PullAnswer {
    uid: Option<String>,
}

/// One pull-enabled partner, called as an identity provider.
pub(crate) struct GenericPullProvider {
    descriptor: ProviderDescriptor,
    source: SourceDomain,
    pull_sync_url: Option<String>,
    allowed_domains: Vec<String>,
    token: Option<Redacted<String>>,
}

impl GenericPullProvider {
    /// Builds the provider of `partner`.
    ///
    /// The descriptor reads only `source_domain`, `pull_sync_url` and
    /// `pull_sync_allowed_domains`, which are not secret, so this works on
    /// unresolved deploy-time settings. The token is kept for
    /// [`request`](IdentityProvider::request) only.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the partner cannot be
    /// described.
    pub(crate) fn from_partner(partner: &EcPartner) -> Result<Self, Report<IdentityProviderError>> {
        let descriptor = describe(partner)?;
        let source = descriptor.primary_source().clone();
        Ok(Self {
            descriptor,
            source,
            pull_sync_url: partner.pull_sync_url.clone(),
            allowed_domains: partner.pull_sync_allowed_domains.clone(),
            token: partner.ts_pull_token.clone(),
        })
    }
}

impl IdentityProvider for GenericPullProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let Some(mut url) =
            validated_pull_sync_url(self.pull_sync_url.as_deref(), &self.allowed_domains)
        else {
            return Ok(None);
        };
        let Some(token) = self
            .token
            .as_ref()
            .filter(|token| !token.expose().trim().is_empty())
        else {
            log::warn!(
                "Pull sync: partner '{}' enabled but missing ts_pull_token",
                self.source
            );
            return Ok(None);
        };

        // The allowlist match ignores a trailing dot, and the descriptor's
        // hosts carry none, so call the dotless spelling.
        let host = url
            .host_str()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_owned();
        if url.host_str() != Some(host.as_str()) && url.set_host(Some(&host)).is_err() {
            return Ok(None);
        }
        let url: Url = build_pull_request_url(url, input.ec_id()?);
        Ok(Some(ProviderRequest::get(&url).with_bearer(token)))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        _input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        // A 404 is the partner's "no user" answer, as pull sync treats it.
        if response.status() == http::StatusCode::NOT_FOUND {
            return Ok(Resolution::no_match());
        }
        let answer: PullAnswer = response.success_json()?;
        match answer.uid {
            Some(uid) if is_valid_eid_uid(&uid) => Ok(Resolution::found([ResolvedEid::new(
                &self.source,
                ResolvedUid::new(uid),
            )])),
            _ => Ok(Resolution::no_match()),
        }
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::ec::identity::test_support::{ProviderHarness, TEST_EC_ID, ec_partner};
    use crate::settings::{ProviderSettings, Settings};
    use crate::test_support::tests::create_test_settings;

    const TOKEN: &str = "example-token-value-0123456789abcdef";

    fn partner() -> crate::settings::EcPartner {
        ec_partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull",
            TOKEN,
        )
    }

    #[test]
    fn calls_the_partner_url_with_the_ec_id_and_bearer() {
        let provider = GenericPullProvider::from_partner(&partner()).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);

        let request = harness
            .request()
            .expect("should build")
            .expect("should not skip");

        assert_eq!(
            request.url().host_str(),
            Some("sync.ssp.example.com"),
            "should call the partner host"
        );
        assert_eq!(
            request
                .url()
                .query_pairs()
                .find(|(name, _)| name == "ec_id")
                .map(|(_, value)| value.into_owned()),
            Some(TEST_EC_ID.to_owned()),
            "should pass the EC ID"
        );
        assert_eq!(
            request.headers()["authorization"],
            format!("Bearer {TOKEN}"),
            "should send the bearer token"
        );
        assert_eq!(request.method(), http::Method::GET, "should use GET");

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "uid": "partner-uid" }))
            .expect("should parse the answer");
        assert_eq!(resolution.eids().len(), 1, "should return one entry");
        let eid = &resolution.eids()[0];
        assert_eq!(
            eid.source().as_str(),
            "ssp.example.com",
            "should name the source"
        );
        assert_eq!(eid.uids().len(), 1, "should return one UID");
        assert_eq!(
            eid.uids()[0].id(),
            "partner-uid",
            "should return the partner's UID"
        );
        assert_eq!(
            (
                eid.uids()[0].atype(),
                eid.uids()[0].ext(),
                eid.inserter(),
                eid.matcher(),
                eid.mm(),
                eid.ext()
            ),
            (None, None, None, None, None, None),
            "should set nothing beyond the UID, so it is stored as one entry with one UID"
        );
    }

    #[test]
    fn keeps_the_query_the_partner_url_already_has() {
        let partner = ec_partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull?x=1",
            TOKEN,
        );
        let provider = GenericPullProvider::from_partner(&partner).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);

        let request = harness
            .request()
            .expect("should build")
            .expect("should not skip");

        let query = request.url().query().expect("should have a query");
        assert!(query.contains("x=1"), "should keep the existing query");
        assert!(query.contains("ec_id="), "should append the EC ID");
        assert!(!query.contains("ip="), "should not forward the client IP");
    }

    #[test]
    fn not_found_and_null_uid_are_no_match_as_today() {
        let provider = GenericPullProvider::from_partner(&partner()).expect("should build");
        let oversized = "x".repeat(513);
        for (name, status, body) in [
            ("404", StatusCode::NOT_FOUND, json!({})),
            (
                "404 with a body",
                StatusCode::NOT_FOUND,
                json!({ "uid": "u" }),
            ),
            ("null uid", StatusCode::OK, json!({ "uid": null })),
            ("missing uid", StatusCode::OK, json!({})),
            ("empty uid", StatusCode::OK, json!({ "uid": "" })),
            ("blank uid", StatusCode::OK, json!({ "uid": "  " })),
            ("oversized uid", StatusCode::OK, json!({ "uid": oversized })),
        ] {
            let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);
            harness
                .request()
                .expect("should build")
                .expect("should not skip");
            let resolution = harness
                .respond(status, &body)
                .expect("should not be an error");
            assert!(resolution.is_no_match(), "should be no_match for {name}");
        }
    }

    #[test]
    fn a_uid_at_the_size_limit_is_kept() {
        let provider = GenericPullProvider::from_partner(&partner()).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);
        harness
            .request()
            .expect("should build")
            .expect("should not skip");

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "uid": "x".repeat(512) }))
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].uids()[0].id().len(),
            512,
            "should keep a UID of MAX_UID_LENGTH"
        );
    }

    #[test]
    fn other_failures_are_errors_not_no_match() {
        let provider = GenericPullProvider::from_partner(&partner()).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);
        harness
            .request()
            .expect("should build")
            .expect("should not skip");

        let rejected = harness
            .respond(StatusCode::INTERNAL_SERVER_ERROR, &json!({}))
            .expect_err("should reject a 500");
        assert!(
            matches!(
                rejected.current_context(),
                IdentityProviderError::Rejected { status } if *status == StatusCode::INTERNAL_SERVER_ERROR
            ),
            "should report Rejected"
        );
        let malformed = harness
            .respond_raw(StatusCode::OK, &[], b"not json")
            .expect_err("should reject a body that is not JSON");
        assert!(
            matches!(
                malformed.current_context(),
                IdentityProviderError::Malformed
            ),
            "should report Malformed"
        );
    }

    #[test]
    fn skips_without_a_valid_url_or_token() {
        let mut http_url = partner();
        http_url.pull_sync_url = Some("http://sync.ssp.example.com/pull".to_owned());
        let mut other_host = partner();
        other_host.pull_sync_url = Some("https://evil.example.net/pull".to_owned());
        let mut no_url = partner();
        no_url.pull_sync_url = None;
        let mut no_token = partner();
        no_token.ts_pull_token = None;

        for (name, partner) in [
            ("http scheme", http_url),
            ("host outside the allowlist", other_host),
            ("no URL", no_url),
            ("no token", no_token),
        ] {
            let provider = GenericPullProvider::from_partner(&partner).expect("should build");
            let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);

            let request = harness.request().expect("should not be an error");

            assert!(request.is_none(), "should skip for {name}");
        }
    }

    #[test]
    fn accepts_an_allowlist_match_after_normalization() {
        let mut partner = partner();
        partner.pull_sync_url = Some("https://SYNC.SSP.EXAMPLE.COM./pull".to_owned());
        partner.pull_sync_allowed_domains = vec!["Sync.Ssp.Example.Com.".to_owned()];
        let provider = GenericPullProvider::from_partner(&partner).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);

        let request = harness.request().expect("should build");

        assert!(request.is_some(), "should call a host the allowlist names");
    }

    #[test]
    fn a_stored_record_makes_the_source_a_refresh_and_the_call_still_goes_out() {
        let provider = GenericPullProvider::from_partner(&partner()).expect("should build");
        let source = provider.descriptor().primary_source().clone();
        let stored = crate::ec::identity::test_support::stored_for_test(&source, "stored-uid");
        let mut harness = ProviderHarness::new(&provider)
            .with_ec_id(TEST_EC_ID)
            .with_stored(stored);

        let request = harness.request().expect("should build");

        assert!(request.is_some(), "should refresh a stored source");
    }

    #[test]
    fn the_descriptor_reads_no_secret() {
        let mut partner = partner();
        partner.ts_pull_token = Some(crate::redacted::Redacted::new("secret-key-name".to_owned()));
        let with_key_name = GenericPullProvider::from_partner(&partner).expect("should build");
        let with_token = GenericPullProvider::from_partner(&self::partner()).expect("should build");

        assert_eq!(
            with_key_name.descriptor(),
            with_token.descriptor(),
            "should describe the same provider whatever the token holds"
        );
        let descriptor = with_token.descriptor();
        assert_eq!(
            descriptor.id().as_str(),
            "pull-ssp-example-com",
            "should derive the ID"
        );
        assert_eq!(descriptor.sources().len(), 1, "should claim one source");
        assert_eq!(
            descriptor.primary_source().as_str(),
            "ssp.example.com",
            "should claim the partner's source domain"
        );
        let resolves = descriptor.resolves().expect("should resolve");
        assert_eq!(
            resolves.hosts(),
            ["sync.ssp.example.com".to_owned()],
            "should allow only the allowlisted hosts"
        );
        assert!(resolves.gets_ec_id(), "should get the EC ID");
        assert!(descriptor.captures().is_none(), "should not capture");
        assert!(
            descriptor.consent_policy().is_eid_persistence_only(),
            "should require the EID persistence consent pull sync requires today"
        );
    }

    #[test]
    fn descriptors_cover_only_pull_enabled_partners() {
        let mut disabled = ec_partner("off.example.com", "https://sync.off.example.com/p", TOKEN);
        disabled.pull_sync_enabled = false;
        let partners = [partner(), disabled];

        let descriptors = generic_pull_descriptors(&partners).expect("should build");

        assert_eq!(
            descriptors.len(),
            1,
            "should skip a partner without pull sync"
        );
        assert_eq!(
            descriptors[0].id().as_str(),
            "pull-ssp-example-com",
            "should describe the pull-enabled partner"
        );
    }

    #[test]
    fn provider_ids_are_derived_and_bounded() {
        assert_eq!(
            generic_pull_id("ssp.example.com").as_str(),
            "pull-ssp-example-com",
            "should keep a dot-only domain readable"
        );
        let long = format!("{}.example.com", "a".repeat(70));
        let id = generic_pull_id(&long);
        assert!(
            id.as_str().len() <= 63,
            "should bound the ID to 63 characters"
        );
        assert_ne!(
            id,
            generic_pull_id(&format!("{}.example.com", "a".repeat(71))),
            "should keep long IDs unique by hash"
        );
        assert_ne!(
            generic_pull_id("a-b.example.com"),
            generic_pull_id("a.b.example.com"),
            "should not collide when a domain already contains a hyphen"
        );
        assert_eq!(
            generic_pull_id("SSP.Example.Com."),
            generic_pull_id("ssp.example.com"),
            "should derive the ID from the normalized domain"
        );
        let hyphenated_long = format!("{}-b.example.com", "a".repeat(70));
        assert!(
            generic_pull_id(&hyphenated_long).as_str().len() <= 63,
            "should bound a long hyphenated domain too"
        );
    }

    #[test]
    fn generic_pull_pacing_comes_from_its_provider_table() {
        let mut settings: Settings = create_test_settings();
        let id = generic_pull_id("ssp.example.com");

        let defaults = settings.ec.identity.provider(&id);
        assert_eq!(
            defaults.refresh_after_sec, 86_400,
            "should default the refresh"
        );
        assert_eq!(
            defaults.max_calls_per_minute, 1,
            "should default the pacing"
        );

        settings.ec.identity.providers.insert(
            id.as_str().to_owned(),
            ProviderSettings {
                max_calls_per_minute: 2,
                ..ProviderSettings::default()
            },
        );
        let configured = settings.ec.identity.provider(&id);
        assert_eq!(
            configured.max_calls_per_minute, 2,
            "should read the pacing from the pull provider's table"
        );
    }
}

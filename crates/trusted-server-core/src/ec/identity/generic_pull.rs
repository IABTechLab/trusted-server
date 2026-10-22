//! `GenericPullProvider`: today's pull sync partners on the provider trait.
//!
//! The protocol is unchanged: `GET <pull_sync_url>?ec_id=<id>` with the
//! partner's bearer token, answered with `{"uid": "..."}`. Its sources are
//! unowned, so it only fills missing IDs, as pull sync always has.

use error_stack::Report;
use http::StatusCode;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::config::{ProviderMode, ProviderSettings};
use super::descriptor::{ConsentPolicy, ProviderDescriptor, Resolves};
use super::exchange::{ProviderRequest, ProviderResponse};
use super::ids::IdentityProviderId;
use super::input::ResolveInput;
use super::provider::IdentityProvider;
use super::resolution::{IdentityProviderError, Resolution, ResolvedId};
use crate::ec::kv_types::MAX_UID_LENGTH;
use crate::ec::pull_sync::{build_pull_request_url, validated_pull_sync_url};
use crate::ec::registry::PartnerConfig;
use crate::redacted::Redacted;

/// Longest provider ID.
const MAX_ID_LENGTH: usize = 63;
/// Hex digits of the hash suffix a cut ID gets.
const ID_HASH_SUFFIX_LENGTH: usize = 8;

/// `pull-` plus the source domain with dots replaced by `-`. An ID that
/// would exceed 63 characters is cut and given an 8-digit hash suffix of the
/// full domain, so IDs stay unique and valid.
#[must_use]
pub(crate) fn generic_pull_id(source_domain: &str) -> IdentityProviderId {
    let full = format!("pull-{}", source_domain.replace('.', "-"));
    let id = if full.len() <= MAX_ID_LENGTH {
        full
    } else {
        let digest = Sha256::digest(source_domain.as_bytes());
        let suffix = &hex::encode(digest)[..ID_HASH_SUFFIX_LENGTH];
        let keep = MAX_ID_LENGTH - ID_HASH_SUFFIX_LENGTH - 1;
        format!("{}-{suffix}", full[..keep].trim_end_matches('-'))
    };
    id.parse()
        .expect("should derive a valid provider id from a normalized source domain")
}

/// The per-minute burst cap the limiter enforces for an hourly legacy limit:
/// `max(1, ceil(limit / 60))`.
#[must_use]
pub(crate) fn legacy_rate_limit_per_minute(pull_sync_rate_limit: u32) -> u32 {
    pull_sync_rate_limit.div_ceil(60).max(1)
}

/// A pull partner's settings when no `[ec.identity.providers.<id>]` table
/// overrides them.
#[must_use]
pub(crate) fn generic_pull_settings(partner: &PartnerConfig) -> ProviderSettings {
    ProviderSettings {
        mode: ProviderMode::AfterSend,
        max_calls_per_minute: legacy_rate_limit_per_minute(partner.pull_sync_rate_limit),
        refresh_after_sec: partner.pull_sync_ttl_sec.max(1),
        ..ProviderSettings::default()
    }
}

/// One `[[ec.partners]]` entry with `pull_sync_enabled = true`.
pub(crate) struct GenericPullProvider {
    descriptor: ProviderDescriptor,
    partner: PartnerConfig,
    token: Option<Redacted<String>>,
}

#[derive(Deserialize)]
struct PullReply {
    uid: Option<String>,
}

impl GenericPullProvider {
    /// Builds the provider from a partner.
    ///
    /// # Errors
    ///
    /// `Config` when the partner's source or allowed domains cannot form a
    /// valid descriptor.
    pub(crate) fn from_partner(
        partner: &PartnerConfig,
    ) -> Result<Self, Report<IdentityProviderError>> {
        let mut resolves = Resolves::new().with_ec_id();
        for host in &partner.pull_sync_allowed_domains {
            resolves = resolves.with_host(&host.trim().trim_end_matches('.').to_ascii_lowercase());
        }
        let descriptor = ProviderDescriptor::builder(generic_pull_id(&partner.source_domain))
            .with_source(partner.source_domain.parse()?)
            .with_resolves(resolves)
            .with_consent_policy(ConsentPolicy::eid_persistence_only())
            .build()?;
        Ok(Self {
            descriptor,
            partner: partner.clone(),
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
        let (Some(url), Some(token)) =
            (validated_pull_sync_url(&self.partner), self.token.as_ref())
        else {
            return Ok(None);
        };
        let url = build_pull_request_url(url, input.ec_id()?);
        Ok(Some(ProviderRequest::get(&url).with_bearer(token)))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Resolution::no_match());
        }
        let reply: PullReply = response.success_json()?;
        let uid = reply
            .uid
            .filter(|uid| !uid.trim().is_empty() && uid.len() <= MAX_UID_LENGTH);
        Ok(match uid {
            Some(uid) => Resolution::found([ResolvedId::new(input.primary_source(), uid)]),
            None => Resolution::no_match(),
        })
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::resolution::IdentityProviderError;
    use crate::ec::identity::test_support::ProviderHarness;
    use crate::ec::registry::PartnerRegistry;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    const TOKEN: &str = "example-token-value-0123456789abcdef";

    fn ec_id() -> String {
        format!("{}.ABC123", "a".repeat(64))
    }

    fn partner(source: &str, url: &str, allowed: &[&str]) -> EcPartner {
        EcPartner {
            name: format!("Partner {source}"),
            source_domain: source.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: None,
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: true,
            pull_sync_url: Some(url.to_owned()),
            pull_sync_allowed_domains: allowed.iter().map(|domain| (*domain).to_owned()).collect(),
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: 65,
            ts_pull_token: Some(Redacted::new(TOKEN.to_owned())),
        }
    }

    fn provider_for(partner_config: &EcPartner) -> GenericPullProvider {
        let registry = PartnerRegistry::from_config(std::slice::from_ref(partner_config))
            .expect("should build registry");
        GenericPullProvider::from_partner(
            registry
                .get(&partner_config.source_domain)
                .expect("should hold the partner"),
        )
        .expect("should build the provider")
    }

    fn standard() -> GenericPullProvider {
        provider_for(&partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull",
            &["sync.ssp.example.com"],
        ))
    }

    #[test]
    fn calls_the_partner_url_with_the_ec_id_and_bearer() {
        let provider = standard();
        let mut harness = ProviderHarness::new(&provider).with_ec_id(&ec_id());

        let request = harness
            .request()
            .expect("should build")
            .expect("should not skip");

        assert_eq!(request.method(), http::Method::GET, "GET");
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
            Some(ec_id()),
            "should pass the EC ID"
        );
        assert_eq!(
            request.headers()["authorization"],
            format!("Bearer {TOKEN}").as_str(),
            "should send the bearer token"
        );

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "uid": "partner-uid" }))
            .expect("should parse");
        assert_eq!(resolution.ids().len(), 1, "one ID");
        assert_eq!(resolution.ids()[0].uid(), "partner-uid", "uid");
        assert_eq!(
            resolution.ids()[0].source().as_str(),
            "ssp.example.com",
            "for the partner source"
        );
    }

    #[test]
    fn response_mapping_matches_pull_sync() {
        let provider = standard();
        let mut harness = ProviderHarness::new(&provider).with_ec_id(&ec_id());
        harness.request().expect("should build");

        assert!(
            harness
                .respond_raw(StatusCode::NOT_FOUND, &[], b"")
                .expect("404 is not an error")
                .is_no_match(),
            "404 is no_match"
        );
        for body in [json!({ "uid": null }), json!({ "uid": "  " }), json!({})] {
            assert!(
                harness
                    .respond(StatusCode::OK, &body)
                    .expect("should parse")
                    .is_no_match(),
                "a null, blank or missing uid is no_match: {body}"
            );
        }
        assert!(
            harness
                .respond(StatusCode::OK, &json!({ "uid": "x".repeat(513) }))
                .expect("should parse")
                .is_no_match(),
            "an oversized uid is no_match"
        );
        let error = harness
            .respond(StatusCode::INTERNAL_SERVER_ERROR, &json!({}))
            .expect_err("should reject");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Rejected { status } if *status == StatusCode::INTERNAL_SERVER_ERROR),
            "5xx is Rejected"
        );
        let error = harness
            .respond_raw(StatusCode::OK, &[], b"not json")
            .expect_err("should reject");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "bad JSON is Malformed"
        );
    }

    #[test]
    fn descriptor_matches_the_partner() {
        let provider = standard();
        let descriptor = provider.descriptor();
        assert_eq!(descriptor.id().as_str(), "pull-ssp-example-com", "id");
        assert_eq!(
            descriptor.primary_source().as_str(),
            "ssp.example.com",
            "one source"
        );
        assert_eq!(
            descriptor
                .resolves()
                .map(|resolves| resolves.hosts().to_vec()),
            Some(vec!["sync.ssp.example.com".to_owned()]),
            "allowed hosts"
        );
        assert!(
            descriptor.consent_policy().is_eid_persistence_only(),
            "blocks exactly what pull sync blocks"
        );
        assert!(
            !descriptor.is_expiring(descriptor.primary_source()),
            "IDs do not expire"
        );
    }

    #[test]
    fn provider_ids_are_derived_and_bounded() {
        assert_eq!(
            generic_pull_id("ssp.example.com").as_str(),
            "pull-ssp-example-com",
            "simple"
        );
        let long = format!("{}.example.com", "a".repeat(60));
        let id = generic_pull_id(&long);
        assert!(id.as_str().len() <= 63, "bounded");
        assert_ne!(
            id,
            generic_pull_id(&format!("{}.example.com", "a".repeat(61))),
            "unique by hash"
        );
    }

    #[test]
    fn legacy_hourly_limit_maps_to_per_minute() {
        assert_eq!(legacy_rate_limit_per_minute(0), 1, "at least one");
        assert_eq!(legacy_rate_limit_per_minute(59), 1, "rounds up");
        assert_eq!(
            legacy_rate_limit_per_minute(65),
            2,
            "65/hr is 2/min, as the limiter does today"
        );
        let base = generic_pull_settings(
            PartnerRegistry::from_config(&[partner(
                "ssp.example.com",
                "https://sync.ssp.example.com/pull",
                &["sync.ssp.example.com"],
            )])
            .expect("should build registry")
            .get("ssp.example.com")
            .expect("should hold the partner"),
        );
        assert_eq!(
            base.max_calls_per_minute, 2,
            "base settings carry the legacy limit"
        );
        assert_eq!(
            base.mode,
            crate::ec::identity::config::ProviderMode::AfterSend,
            "after send"
        );
    }

    #[test]
    fn catalog_holds_only_pull_enabled_partners() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut no_pull = partner("dsp.example.com", "https://sync.dsp.example.com/pull", &[]);
        no_pull.pull_sync_enabled = false;
        no_pull.pull_sync_url = None;
        no_pull.ts_pull_token = None;
        let partners = vec![
            partner(
                "ssp.example.com",
                "https://sync.ssp.example.com/pull",
                &["sync.ssp.example.com"],
            ),
            no_pull,
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build");
        let catalog = ProviderCatalog::from_partners(&registry).expect("should build");
        let ids: Vec<_> = catalog
            .providers()
            .map(|provider| provider.descriptor().id().to_string())
            .collect();
        assert_eq!(
            ids,
            vec!["pull-ssp-example-com".to_owned()],
            "pull partners only"
        );
        assert_eq!(
            catalog
                .provider_settings(
                    &settings.ec.identity,
                    &"pull-ssp-example-com".parse().expect("should parse id")
                )
                .max_calls_per_minute,
            2,
            "base settings without an operator table"
        );
    }
}

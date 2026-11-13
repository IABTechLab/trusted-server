//! Prebid `SharedID` identity provider: reads the publisher common ID from
//! the first-party cookie Prebid's `sharedIdSystem` writes.
//!
//! Prebid stores the ID as a bare UUID in the cookie named by the
//! publisher's `storage.name`. The provider reads the cookie named in
//! `integrations.prebid.managed_user_ids[].storage.name` for the `sharedId`
//! (or `pubCommonId`) module; without one it reads `_sharedid`, then the
//! legacy `_pubcid`. It is sent as a device-based (agent type 1) EID under
//! `pubcid.org` unless another source is configured. The opt-out cookie
//! `_pubcid_optout` reports an opt-out, which removes a stored `SharedID`.
//! No server call is made.
//!
//! `SharedID` is a publisher first-party ID with no vendor server. With
//! `generate = true`, when the cookies hold none and no live `SharedID` is
//! stored, Trusted Server generates one itself: a random UUID v4, stored in
//! the row only. No cookie is set, so Prebid's own cookie is never touched;
//! when Prebid later writes one, the cookie's ID replaces the generated one.
//! An expired generated ID the row still holds is renewed with the same
//! value, so it stays stable for as long as the row lives.
//! Generation is off by default: Prebid may keep its opt-out in localStorage,
//! which the server cannot see, so only a publisher who knows its opt-out
//! lives in the `_pubcid_optout` cookie should turn it on.

use std::borrow::Cow;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderEid, ProviderError, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};
use uuid::Uuid;

use crate::common::{
    CookieReader, MAX_TTL, bounded_expiry, cookie_capabilities, device_eid, is_usable_id,
};

/// Settings key naming the cookies that hold the `SharedID`.
pub const COOKIE_NAMES_SETTING: &str = "cookie_names";

/// Cookie Prebid sets when the user opts out of `SharedID`.
const OPT_OUT_COOKIE: &str = "_pubcid_optout";

/// The Prebid `SharedID` provider kind.
pub struct SharedId;

/// Settings for a `sharedid` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedIdSettings {
    /// Cookies holding the `SharedID`, read in order. Defaults to Prebid's
    /// configured `storage.name`, else `_sharedid` then `_pubcid`.
    #[serde(default = "SharedIdSettings::default_cookie_names")]
    pub cookie_names: Vec<String>,
    /// Whether to generate a `SharedID` when the cookies hold none. Off by
    /// default, because a localStorage opt-out is invisible to the server.
    #[serde(default)]
    pub generate: bool,
}

impl SharedIdSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["_sharedid".to_owned(), "_pubcid".to_owned()]
    }
}

/// A built Prebid `SharedID` provider.
#[derive(Debug)]
pub struct SharedIdProvider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    cookies: CookieReader,
    generate: bool,
}

impl ProviderKind for SharedId {
    const KIND: &'static str = "sharedid";
    type Settings = SharedIdSettings;
    type Provider = SharedIdProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: SharedIdSettings,
    ) -> Result<SharedIdProvider, Report<ProviderConfigError>> {
        if settings.cookie_names.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("a sharedid provider needs at least one cookie name"));
        }
        let (capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        Ok(SharedIdProvider {
            capabilities,
            source,
            cookies: CookieReader::own(common, settings.cookie_names),
            generate: settings.generate,
        })
    }
}

impl IdentityProvider for SharedIdProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// The request carries no EC ID, so it may run on the navigation that
    /// mints the EC.
    fn sends_ec_id(&self) -> bool {
        false
    }

    fn build_request(
        &self,
        _lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        Err(Report::new(ProviderError::Request).attach("sharedid makes no lookup"))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Err(Report::new(ProviderError::InvalidResponse).attach("sharedid makes no lookup"))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        if cookies.get(OPT_OUT_COOKIE).is_some() {
            return CookieRead::OptedOut;
        }
        let Some(id) = self
            .cookies
            .read(cookies, |value| is_usable_id(value).then_some(value.trim()))
        else {
            return CookieRead::Absent;
        };
        CookieRead::Found(vec![device_eid(
            &self.source,
            Cow::Borrowed(id),
            bounded_expiry(None),
        )])
    }

    fn generate(&self, lookup: &LookupInput<'_>) -> Option<Vec<ProviderEid<'static>>> {
        if !self.generate {
            return None;
        }
        // An expired ID the row still holds is renewed rather than replaced,
        // so the generated ID stays stable for as long as the row lives.
        let id = lookup
            .current
            .iter()
            .filter(|(source, _)| *source == self.source.as_str())
            .flat_map(|(_, eid)| eid.uids())
            .map(|uid| uid.id().as_str().to_owned())
            .next()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        Some(vec![device_eid(
            &self.source,
            Cow::Owned(id),
            // A generated ID has no cookie to renew it, so it keeps the
            // longest lifetime rather than changing every day.
            bounded_expiry(Some(MAX_TTL)),
        )])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::ec::eid_types::{
        AgentType, EidFields, Expiry, ProviderId, UidValue, UnixSeconds,
    };
    use trusted_server_core::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvUid};
    use trusted_server_core::ec::provider::{OwnedEntries, ProviderExpiry, build_kind};
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider() -> Box<dyn IdentityProvider> {
        provider_with(&json!({}))
    }

    fn provider_with(settings: &serde_json::Value) -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "sharedid",
            "kind": "sharedid",
            "sources": ["sharedid.example.com"],
            "settings": settings,
        }))
        .expect("should parse provider config");
        build_kind::<SharedId>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn read(header: &str) -> Option<String> {
        let eids = provider()
            .read_request_cookies(RequestCookies::new(header))
            .into_found()?;
        assert_eq!(
            eids[0].source, "sharedid.example.com",
            "should use the source"
        );
        let uid = &eids[0].uids[0];
        assert_eq!(uid.atype, Some(1), "should be device-based");
        Some(uid.id.to_string())
    }

    #[test]
    fn reads_sharedid_before_pubcid() {
        assert_eq!(
            read("_pubcid=pubcid-example; _sharedid=sharedid-example").as_deref(),
            Some("sharedid-example"),
            "should prefer _sharedid"
        );
        assert_eq!(
            read("_pubcid=pubcid-example").as_deref(),
            Some("pubcid-example"),
            "should fall back to _pubcid"
        );
    }

    #[test]
    fn defaults_the_source_to_pubcid_org() {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "sharedid",
            "kind": "sharedid",
        }))
        .expect("should parse provider config");

        let provider = build_kind::<SharedId>(&config, &ProviderSecrets::default())
            .expect("should build provider");
        let eids = provider
            .read_request_cookies(RequestCookies::new("_sharedid=sharedid-example"))
            .into_found()
            .expect("should read the cookie");

        assert_eq!(eids[0].source, "pubcid.org", "should default to pubcid.org");
        assert_eq!(eids[0].uids[0].atype, Some(1), "should be device-based");
    }

    #[test]
    fn honours_opt_out_and_ignores_blank_ids() {
        assert!(
            matches!(
                provider().read_request_cookies(RequestCookies::new(
                    "_sharedid=sharedid-example; _pubcid_optout=1"
                )),
                CookieRead::OptedOut
            ),
            "should report the opt-out cookie"
        );
        for header in ["_sharedid=", "_sharedid=0", "other=1"] {
            assert!(read(header).is_none(), "should ignore {header}");
        }
    }

    fn generate_with(provider: &dyn IdentityProvider) -> Option<Vec<ProviderEid<'static>>> {
        generate_holding(provider, OwnedEntries::empty())
    }

    fn generate_holding(
        provider: &dyn IdentityProvider,
        current: OwnedEntries<'_>,
    ) -> Option<Vec<ProviderEid<'static>>> {
        let consent = trusted_server_core::consent::ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: trusted_server_core::ec::provider::ClientSignals {
                ip: None,
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current,
            cookies: RequestCookies::default(),
            hashed_email: None,
            now: trusted_server_core::ec::eid_types::UnixSeconds::new(1_700_000_000),
        };
        provider.generate(&lookup)
    }

    #[test]
    fn generates_nothing_by_default() {
        assert!(
            generate_with(&*provider()).is_none(),
            "should not generate without generate = true"
        );
    }

    #[test]
    fn renews_an_expired_generated_id_the_row_still_holds() {
        // Arrange: the row holds the generated ID, expired at 1000.
        let provider = provider_with(&json!({ "generate": true }));
        let uid = KvUid::new(
            UidValue::try_from("11111111-2222-4333-8444-555555555555").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("sharedid")),
            Expiry::At(UnixSeconds::new(1000)),
        );
        let mut entry = KvEntry::tombstone(1);
        entry.ids.insert(
            "sharedid.example.com".to_owned(),
            KvPartnerId::from_eids(
                vec![KvEid::new(EidFields::default(), vec![uid])],
                UnixSeconds::new(500),
            )
            .expect("should build record"),
        );
        let sources = [SourceDomain::try_from("sharedid.example.com").expect("should parse")];
        let owner = ProviderId::new("sharedid");

        // Act
        let eids = generate_holding(
            &*provider,
            OwnedEntries::new(Some(&entry), &sources, &owner),
        )
        .expect("should generate an ID");

        // Assert
        assert_eq!(
            eids[0].uids[0].id, "11111111-2222-4333-8444-555555555555",
            "should keep the held ID rather than mint a new one"
        );
    }

    #[test]
    fn generates_a_random_uuid_when_none_is_held() {
        let provider = provider_with(&json!({ "generate": true }));

        let first = generate_with(&*provider).expect("should generate an ID");
        let second = generate_with(&*provider).expect("should generate an ID");

        let uid = &first[0].uids[0];
        assert_eq!(
            first[0].source, "sharedid.example.com",
            "should use the source"
        );
        assert_eq!(uid.atype, Some(1), "should be device-based");
        assert!(
            Uuid::parse_str(&uid.id).is_ok_and(|id| id.get_version_num() == 4),
            "should be a UUID v4"
        );
        assert_ne!(uid.id, second[0].uids[0].id, "should be random");
        assert!(
            matches!(uid.expiry, ProviderExpiry::After(ttl) if ttl == MAX_TTL),
            "should keep a generated ID for the longest TTL"
        );
    }
}

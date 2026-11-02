//! UID2 identity provider: reads the UID2 advertising token from the
//! browser's own first-party cookies.
//!
//! The UID2 SDK stores its identity in the `__uid_2` cookie as
//! percent-encoded JSON. Only `advertising_token` and `identity_expires`
//! (milliseconds) are read: the token is stored with agent type 3 until
//! `identity_expires`, and the coordinator drops it once that time has
//! passed. The refresh token and response key are never read, so they are
//! never stored, and no refresh is made server side.
//!
//! When `__uid_2` yields nothing, Prebid's `__uid2_advertising_token`
//! cookie (or each name in `cookie_names`) is read as a raw token with the shared default TTL. Lockr AIM can
//! write the token into a cookie whose name comes from its own
//! configuration; list that name in `cookie_names`.
//!
//! Parsing is lenient: an undecodable cookie, a missing token or expiry,
//! or a blank or opt-out token reads as no ID.

use std::borrow::Cow;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::{SourceDomain, UnixSeconds};
use trusted_server_core::ec::provider::{
    IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderEid, ProviderError, ProviderExpiry, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};

use crate::common::{
    bounded_expiry, cookie_capabilities, cookie_eid, is_usable_id, percent_decoded,
};

/// The UID2 provider kind.
pub struct Uid2;

/// Settings for a `uid2` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uid2Settings {
    /// Cookie holding the UID2 SDK identity JSON.
    #[serde(default = "Uid2Settings::default_identity_cookie_name")]
    pub identity_cookie_name: String,
    /// Cookies holding a raw advertising token, read in order after the
    /// identity cookie.
    #[serde(default = "Uid2Settings::default_cookie_names")]
    pub cookie_names: Vec<String>,
}

impl Uid2Settings {
    fn default_identity_cookie_name() -> String {
        "__uid_2".to_owned()
    }

    fn default_cookie_names() -> Vec<String> {
        vec!["__uid2_advertising_token".to_owned()]
    }
}

/// A built UID2 provider.
#[derive(Debug)]
pub struct Uid2Provider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    identity_cookie_name: String,
    cookie_names: Box<[String]>,
}

impl ProviderKind for Uid2 {
    const KIND: &'static str = "uid2";
    type Settings = Uid2Settings;
    type Provider = Uid2Provider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: Uid2Settings,
    ) -> Result<Uid2Provider, Report<ProviderConfigError>> {
        let (capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        Ok(Uid2Provider {
            capabilities,
            source,
            identity_cookie_name: settings.identity_cookie_name,
            cookie_names: settings.cookie_names.into_boxed_slice(),
        })
    }
}

/// The fields of the `__uid_2` cookie this provider reads.
#[derive(Deserialize)]
struct IdentityCookie {
    advertising_token: String,
    identity_expires: u64,
}

/// Decodes one `__uid_2` value into the token and its expiry.
fn identity(value: &str) -> Option<(String, ProviderExpiry)> {
    let decoded = percent_decoded(value);
    let cookie: IdentityCookie = serde_json::from_str(&decoded).ok()?;
    if !is_usable_id(&cookie.advertising_token) {
        return None;
    }
    let expires = UnixSeconds::new(cookie.identity_expires / 1000);
    Some((cookie.advertising_token, ProviderExpiry::At(expires)))
}

/// Decodes one raw advertising token cookie.
fn raw_token(value: &str) -> Option<(String, ProviderExpiry)> {
    let token = percent_decoded(value);
    let token = token.trim();
    is_usable_id(token).then(|| (token.to_owned(), bounded_expiry(None)))
}

impl IdentityProvider for Uid2Provider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        _lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        Err(Report::new(ProviderError::Request).attach("uid2 makes no lookup"))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Err(Report::new(ProviderError::InvalidResponse).attach("uid2 makes no lookup"))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> Option<Vec<ProviderEid<'cookie>>> {
        let (token, expiry) = cookies
            .get(&self.identity_cookie_name)
            .and_then(identity)
            .or_else(|| {
                self.cookie_names
                    .iter()
                    .filter_map(|name| cookies.get(name))
                    .find_map(raw_token)
            })?;
        Some(vec![cookie_eid(&self.source, Cow::Owned(token), expiry)])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::ec::provider::build_kind;
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider(settings: &serde_json::Value) -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "uid2",
            "kind": "uid2",
            "sources": ["uid2.example.com"],
            "settings": settings,
        }))
        .expect("should parse provider config");
        build_kind::<Uid2>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn read(provider: &dyn IdentityProvider, header: &str) -> Option<(String, ProviderExpiry)> {
        let eids = provider.read_request_cookies(RequestCookies::new(header))?;
        assert_eq!(eids[0].source, "uid2.example.com", "should use the source");
        let uid = &eids[0].uids[0];
        assert_eq!(uid.atype, Some(3), "should be person-based");
        Some((uid.id.to_string(), uid.expiry))
    }

    fn identity_cookie(identity: &serde_json::Value) -> String {
        format!("__uid_2={}", urlencoding::encode(&identity.to_string()))
    }

    #[test]
    fn reads_only_the_advertising_token_until_identity_expiry() {
        let header = identity_cookie(&json!({
            "advertising_token": "AgAAAAexampletoken+/=",
            "refresh_token": "example-refresh-token",
            "identity_expires": 1_700_000_000_123_u64,
            "refresh_from": 1_699_990_000_000_u64,
            "refresh_expires": 1_702_000_000_000_u64,
            "refresh_response_key": "example-response-key",
        }));

        let (token, expiry) = read(&*provider(&json!({})), &header).expect("should read");

        assert_eq!(
            token, "AgAAAAexampletoken+/=",
            "should store only the token"
        );
        assert!(
            matches!(expiry, ProviderExpiry::At(at) if at.get() == 1_700_000_000),
            "should expire at identity_expires"
        );
    }

    #[test]
    fn falls_back_to_the_raw_token_cookie() {
        let header = "__uid_2=%7Bbroken; __uid2_advertising_token=AgAAAAexampletoken";

        let (token, expiry) = read(&*provider(&json!({})), header).expect("should read");

        assert_eq!(token, "AgAAAAexampletoken", "should read the raw token");
        assert!(
            matches!(expiry, ProviderExpiry::After(_)),
            "should use the default TTL"
        );
    }

    #[test]
    fn reads_configured_cookie_names() {
        let provider = provider(&json!({ "cookie_names": ["example_lockr_uid2"] }));

        let read = read(&*provider, "example_lockr_uid2=AgAAAAexampletoken");

        assert_eq!(
            read.map(|(token, _)| token).as_deref(),
            Some("AgAAAAexampletoken"),
            "should read the configured cookie"
        );
    }

    #[test]
    fn ignores_missing_malformed_and_opt_out_cookies() {
        let provider = provider(&json!({}));
        for header in [
            String::new(),
            identity_cookie(&json!({ "advertising_token": "tok" })),
            identity_cookie(&json!({ "identity_expires": 1 })),
            identity_cookie(&json!({ "advertising_token": "", "identity_expires": 1 })),
            "__uid2_advertising_token=0".to_owned(),
        ] {
            assert!(
                read(&*provider, &header).is_none(),
                "should ignore {header:?}"
            );
        }
    }
}

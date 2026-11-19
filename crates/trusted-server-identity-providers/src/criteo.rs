//! Criteo identity provider: reads the Criteo ID from the first-party
//! cookie Prebid's `criteoIdSystem` writes.
//!
//! Prebid stores Criteo's bid ID in `cto_bidid` with `encodeURIComponent`
//! and sends the decoded value as the `criteo.com` EID with agent type 1,
//! so the cookie is percent-decoded before it is stored. Like Prebid's
//! `normalizeBidId`, a `{"criteoId":…}` value is unwrapped. No server call
//! is made.

use std::borrow::Cow;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderError, ProviderKind, ProviderRequest, ProviderResponse,
    RequestCookies,
};

use crate::common::{
    CookieReader, bounded_expiry, cookie_capabilities, device_eid, is_usable_id, percent_decoded,
};

/// The Criteo provider kind.
pub struct Criteo;

/// Settings for a `criteo` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriteoSettings {
    /// Cookies holding the Criteo bid ID, read in order.
    #[serde(default = "CriteoSettings::default_cookie_names")]
    pub cookie_names: Vec<String>,
}

impl CriteoSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["cto_bidid".to_owned()]
    }
}

/// A built Criteo provider.
#[derive(Debug)]
pub struct CriteoProvider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    cookies: CookieReader,
}

impl ProviderKind for Criteo {
    const KIND: &'static str = "criteo";
    type Settings = CriteoSettings;
    type Provider = CriteoProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: CriteoSettings,
    ) -> Result<CriteoProvider, Report<ProviderConfigError>> {
        if settings.cookie_names.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("a criteo provider needs at least one cookie name"));
        }
        let (capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        Ok(CriteoProvider {
            capabilities,
            source,
            cookies: CookieReader::own(common, settings.cookie_names),
        })
    }
}

impl IdentityProvider for CriteoProvider {
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
        Err(Report::new(ProviderError::Request).attach("criteo makes no lookup"))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Err(Report::new(ProviderError::InvalidResponse).attach("criteo makes no lookup"))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        let Some(id) = self.cookies.read(cookies, |value| {
            let id = bid_id(&percent_decoded(value))?;
            is_usable_id(&id).then(|| id.trim().to_owned())
        }) else {
            return CookieRead::Absent;
        };
        CookieRead::Found(vec![device_eid(
            &self.source,
            Cow::Owned(id),
            bounded_expiry(None),
        )])
    }
}

/// The bid ID in a decoded `cto_bidid` value, unwrapping the
/// `{"criteoId":…}` form Prebid's `normalizeBidId` accepts.
fn bid_id(decoded: &str) -> Option<String> {
    if decoded.trim_start().starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(decoded).ok()?;
        return value["criteoId"].as_str().map(str::to_owned);
    }
    Some(decoded.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::ec::provider::build_kind;
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider() -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "criteo",
            "kind": "criteo",
            "sources": ["criteo.example.com"],
        }))
        .expect("should parse provider config");
        build_kind::<Criteo>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    #[test]
    fn stores_the_decoded_bid_id() {
        let eids = provider()
            .read_request_cookies(RequestCookies::new("cto_bidid=bidid-example%3D%3D"))
            .into_found()
            .expect("should read the bid ID");

        assert_eq!(
            eids[0].source, "criteo.example.com",
            "should use the source"
        );
        let uid = &eids[0].uids[0];
        assert_eq!(
            uid.id, "bidid-example==",
            "should store the decoded value Prebid sends"
        );
        assert_eq!(uid.atype, Some(1), "should be device-based");
    }

    #[test]
    fn unwraps_a_criteo_id_object() {
        let eids = provider()
            .read_request_cookies(RequestCookies::new(
                "cto_bidid=%7B%22criteoId%22%3A%22bidid-example%22%7D",
            ))
            .into_found()
            .expect("should read the bid ID");

        assert_eq!(
            eids[0].uids[0].id, "bidid-example",
            "should store the wrapped bid ID"
        );
    }

    #[test]
    fn ignores_missing_and_blank_cookies() {
        for header in ["cto_bidid=", "cto_bidid=0", "cto_bundle=bundle-example"] {
            assert!(
                provider()
                    .read_request_cookies(RequestCookies::new(header))
                    .into_found()
                    .is_none(),
                "should ignore {header}"
            );
        }
    }
}

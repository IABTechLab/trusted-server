//! Prebid `SharedID` identity provider: reads the publisher common ID from
//! the first-party cookie Prebid's `sharedIdSystem` writes.
//!
//! Prebid stores the ID as a bare UUID in `_sharedid` (the publisher's
//! configured name) or the legacy `_pubcid`. It is sent as a device-based
//! (agent type 1) EID. The opt-out cookie `_pubcid_optout` suppresses it.
//! No server call is made.

use std::borrow::Cow;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderEid, ProviderError, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};

use crate::common::{CookieReader, bounded_expiry, cookie_capabilities, device_eid, is_usable_id};

/// Cookie Prebid sets when the user opts out of `SharedID`.
const OPT_OUT_COOKIE: &str = "_pubcid_optout";

/// The Prebid `SharedID` provider kind.
pub struct SharedId;

/// Settings for a `sharedid` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedIdSettings {
    /// Cookies holding the `SharedID`, read in order.
    #[serde(default = "SharedIdSettings::default_cookie_names")]
    pub cookie_names: Vec<String>,
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
        })
    }
}

impl IdentityProvider for SharedIdProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
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
    ) -> Option<Vec<ProviderEid<'cookie>>> {
        if cookies.get(OPT_OUT_COOKIE).is_some() {
            return None;
        }
        let id = self
            .cookies
            .read(cookies, |value| is_usable_id(value).then_some(value.trim()))?;
        Some(vec![device_eid(
            &self.source,
            Cow::Borrowed(id),
            bounded_expiry(None),
        )])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::ec::provider::build_kind;
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider() -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "sharedid",
            "kind": "sharedid",
            "sources": ["sharedid.example.com"],
        }))
        .expect("should parse provider config");
        build_kind::<SharedId>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn read(header: &str) -> Option<String> {
        let eids = provider().read_request_cookies(RequestCookies::new(header))?;
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
    fn honours_opt_out_and_ignores_blank_ids() {
        assert!(
            read("_sharedid=sharedid-example; _pubcid_optout=1").is_none(),
            "should honour the opt-out cookie"
        );
        for header in ["_sharedid=", "_sharedid=0", "other=1"] {
            assert!(read(header).is_none(), "should ignore {header}");
        }
    }
}

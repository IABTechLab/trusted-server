//! Unified ID identity provider: reads The Trade Desk ID (TDID) from the
//! first-party cookie Prebid's `unifiedIdSystem` writes.
//!
//! Prebid stores a percent-encoded JSON object such as `{"TDID":"…"}` in
//! `pbjs-unifiedid` and sends the TDID as the `adserver.org` EID with agent
//! type 1, UID `ext` `{"rtiPartner":"TDID"}`, and `inserter`, `matcher` set
//! to `adserver.org` with `mm` 4. No server call is made.

use std::borrow::Cow;

use error_stack::{Report, ResultExt as _};
use serde::Deserialize;
use serde_json::value::RawValue;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    CookieRead, EidLevelFields, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities,
    ProviderCommon, ProviderConfigError, ProviderError, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};

use crate::common::{
    CookieReader, bounded_expiry, cookie_capabilities, device_eid, is_usable_id, percent_decoded,
};

/// `inserter` and `matcher` Prebid sets on the TDID EID.
const TDID_INSERTER: &str = "adserver.org";

/// `mm` (match method) Prebid sets on the TDID EID.
const TDID_MATCH_METHOD: i32 = 4;

/// UID `ext` Prebid puts on the TDID.
const TDID_UID_EXT: &str = r#"{"rtiPartner":"TDID"}"#;

/// The Unified ID provider kind.
pub struct UnifiedId;

/// Settings for a `unifiedid` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnifiedIdSettings {
    /// Cookies holding the Unified ID object, read in order.
    #[serde(default = "UnifiedIdSettings::default_cookie_names")]
    pub cookie_names: Vec<String>,
}

impl UnifiedIdSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["pbjs-unifiedid".to_owned()]
    }
}

/// A built Unified ID provider.
#[derive(Debug)]
pub struct UnifiedIdProvider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    cookies: CookieReader,
    uid_ext: &'static RawValue,
}

impl ProviderKind for UnifiedId {
    const KIND: &'static str = "unifiedid";
    type Settings = UnifiedIdSettings;
    type Provider = UnifiedIdProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: UnifiedIdSettings,
    ) -> Result<UnifiedIdProvider, Report<ProviderConfigError>> {
        if settings.cookie_names.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("a unifiedid provider needs at least one cookie name"));
        }
        let (mut capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        capabilities.shape.eid_level_fields = EidLevelFields::Preserved;
        let uid_ext = serde_json::from_str::<&'static RawValue>(TDID_UID_EXT)
            .change_context(ProviderConfigError::Invalid)?;
        Ok(UnifiedIdProvider {
            capabilities,
            source,
            cookies: CookieReader::own(common, settings.cookie_names),
            uid_ext,
        })
    }
}

/// The field of the Unified ID cookie this provider reads.
#[derive(Deserialize)]
struct UnifiedIdCookie {
    #[serde(rename = "TDID")]
    tdid: String,
}

/// Decodes one cookie value into the TDID.
fn tdid(value: &str) -> Option<String> {
    let cookie: UnifiedIdCookie = serde_json::from_str(&percent_decoded(value)).ok()?;
    is_usable_id(&cookie.tdid).then_some(cookie.tdid)
}

impl IdentityProvider for UnifiedIdProvider {
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
        Err(Report::new(ProviderError::Request).attach("unifiedid makes no lookup"))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Err(Report::new(ProviderError::InvalidResponse).attach("unifiedid makes no lookup"))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        let Some(id) = self.cookies.read(cookies, tdid) else {
            return CookieRead::Absent;
        };
        let mut eid = device_eid(&self.source, Cow::Owned(id), bounded_expiry(None));
        eid.inserter = Some(Cow::Borrowed(TDID_INSERTER));
        eid.matcher = Some(Cow::Borrowed(TDID_INSERTER));
        eid.mm = Some(TDID_MATCH_METHOD);
        for uid in &mut eid.uids {
            uid.ext = Some(Cow::Borrowed(self.uid_ext));
        }
        CookieRead::Found(vec![eid])
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
            "id": "unifiedid",
            "kind": "unifiedid",
            "sources": ["unifiedid.example.com"],
        }))
        .expect("should parse provider config");
        build_kind::<UnifiedId>(&config, &ProviderSecrets::default())
            .expect("should build provider")
    }

    fn header(cookie: &serde_json::Value) -> String {
        format!(
            "pbjs-unifiedid={}",
            urlencoding::encode(&cookie.to_string())
        )
    }

    #[test]
    fn reads_the_tdid_with_prebid_fields() {
        let cookie = json!({
            "TDID": "tdid-example",
            "TDID_LOOKUP": "TRUE",
            "TDID_CREATED_AT": "2026-01-01T00:00:00",
        });
        let header = header(&cookie);

        let eids = provider()
            .read_request_cookies(RequestCookies::new(&header))
            .into_found()
            .expect("should read the TDID");

        let eid = &eids[0];
        assert_eq!(eid.source, "unifiedid.example.com", "should use the source");
        assert_eq!(
            eid.inserter.as_deref(),
            Some("adserver.org"),
            "should set inserter"
        );
        assert_eq!(
            eid.matcher.as_deref(),
            Some("adserver.org"),
            "should set matcher"
        );
        assert_eq!(eid.mm, Some(4), "should set mm");
        let uid = &eid.uids[0];
        assert_eq!(uid.id, "tdid-example", "should store only the TDID");
        assert_eq!(uid.atype, Some(1), "should be device-based");
        assert_eq!(
            uid.ext.as_deref().map(RawValue::get),
            Some(r#"{"rtiPartner":"TDID"}"#),
            "should mark the rtiPartner"
        );
    }

    #[test]
    fn ignores_missing_malformed_and_blank_ids() {
        for cookie in [
            json!({}),
            json!({ "TDID": "" }),
            json!({ "TDID": "0" }),
            json!({ "TDID": 5 }),
        ] {
            assert!(
                provider()
                    .read_request_cookies(RequestCookies::new(&header(&cookie)))
                    .into_found()
                    .is_none(),
                "should ignore {cookie}"
            );
        }
        assert!(
            provider()
                .read_request_cookies(RequestCookies::new("pbjs-unifiedid={broken"))
                .into_found()
                .is_none(),
            "should ignore malformed JSON"
        );
    }
}

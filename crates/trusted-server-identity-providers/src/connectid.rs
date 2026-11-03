//! Yahoo `ConnectID` identity provider: reads `ConnectID` from Yahoo's own
//! first-party cookie.
//!
//! Yahoo's `ConnectID` module stores a JSON object in the `connectId` cookie
//! (optionally percent-encoded) with `connectId`, `lastSynced` and `ttl`,
//! both in milliseconds. The ID is stored with agent type 3 until
//! `lastSynced + ttl`, with the TTL capped at [`MAX_TTL`]; the coordinator
//! drops it once that time has passed. A missing `ttl` uses the shared
//! default TTL. No server call is made.
//!
//! This kind has no Lockr capability: Lockr AIM is not documented to
//! deliver `ConnectID`, so a `lockr` settings table is rejected.
//!
//! The cookie's `he` and `puid` fields are never read, so they are never
//! stored. Parsing is lenient: an undecodable cookie, a missing
//! `connectId` or `lastSynced`, or a blank or opt-out ID reads as no ID.

use std::borrow::Cow;
use std::time::Duration;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::{SourceDomain, UnixSeconds};
use trusted_server_core::ec::provider::{
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderError, ProviderExpiry, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};

use crate::common::{
    CookieReader, DEFAULT_TTL, cookie_capabilities, cookie_eid, is_usable_id, percent_decoded,
};

/// Longest time a `ConnectID` is kept after its last sync.
pub const MAX_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// The Yahoo `ConnectID` provider kind.
pub struct ConnectId;

/// Settings for a `connectid` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectIdSettings {
    /// Cookies holding the `ConnectID` object, read in order; the first
    /// holding a usable ID wins.
    #[serde(default = "ConnectIdSettings::default_cookie_names")]
    pub cookie_names: Vec<String>,
}

impl ConnectIdSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["connectId".to_owned()]
    }
}

/// A built Yahoo `ConnectID` provider.
#[derive(Debug)]
pub struct ConnectIdProvider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    cookies: CookieReader,
}

impl ProviderKind for ConnectId {
    const KIND: &'static str = "connectid";
    type Settings = ConnectIdSettings;
    type Provider = ConnectIdProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: ConnectIdSettings,
    ) -> Result<ConnectIdProvider, Report<ProviderConfigError>> {
        if settings.cookie_names.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("a connectid provider needs at least one cookie name"));
        }
        let (capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        Ok(ConnectIdProvider {
            capabilities,
            source,
            cookies: CookieReader::own(common, settings.cookie_names),
        })
    }
}

/// The fields of the `connectId` cookie this provider reads.
#[derive(Deserialize)]
struct ConnectIdCookie {
    #[serde(rename = "connectId")]
    connect_id: String,
    #[serde(rename = "lastSynced")]
    last_synced_ms: f64,
    #[serde(default)]
    ttl: Option<f64>,
}

/// Milliseconds as a duration; negative or non-finite values are zero.
fn from_millis(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms / 1000.0).unwrap_or(Duration::ZERO)
}

/// Decodes one cookie value into the ID and its expiry.
fn connect_id(value: &str) -> Option<(String, ProviderExpiry)> {
    let decoded = percent_decoded(value);
    let cookie: ConnectIdCookie = serde_json::from_str(&decoded).ok()?;
    if !is_usable_id(&cookie.connect_id) {
        return None;
    }
    let ttl = cookie.ttl.map_or(DEFAULT_TTL, from_millis).min(MAX_TTL);
    let expires = from_millis(cookie.last_synced_ms).saturating_add(ttl);
    Some((
        cookie.connect_id,
        ProviderExpiry::At(UnixSeconds::new(expires.as_secs())),
    ))
}

impl IdentityProvider for ConnectIdProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        _lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        Err(Report::new(ProviderError::Request).attach("connectid makes no lookup"))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Err(Report::new(ProviderError::InvalidResponse).attach("connectid makes no lookup"))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        let Some((id, expiry)) = self.cookies.read(cookies, connect_id) else {
            return CookieRead::Absent;
        };
        CookieRead::Found(vec![cookie_eid(&self.source, Cow::Owned(id), expiry)])
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
            "id": "connectid",
            "kind": "connectid",
            "sources": ["connectid.example.com"],
        }))
        .expect("should parse provider config");
        build_kind::<ConnectId>(&config, &ProviderSecrets::default())
            .expect("should build provider")
    }

    fn read(cookie: &serde_json::Value) -> Option<(String, ProviderExpiry)> {
        let header = format!("connectId={}", urlencoding::encode(&cookie.to_string()));
        let eids = provider()
            .read_request_cookies(RequestCookies::new(&header))
            .into_found()?;
        assert_eq!(
            eids[0].source, "connectid.example.com",
            "should use the source"
        );
        let uid = &eids[0].uids[0];
        assert_eq!(uid.atype, Some(3), "should be person-based");
        Some((uid.id.to_string(), uid.expiry))
    }

    fn expires_at(read: Option<(String, ProviderExpiry)>) -> u64 {
        match read.expect("should read the ID").1 {
            ProviderExpiry::At(at) => at.get(),
            other => panic!("should expire at a time, got {other:?}"),
        }
    }

    #[test]
    fn reads_the_id_until_last_sync_plus_ttl() {
        let cookie = json!({
            "connectId": "cid-example-1",
            "he": "example-hashed-email",
            "puid": "example-puid",
            "lastSynced": 1_700_000_000_000_u64,
            "lastUsed": 1_700_000_100_000_u64,
            "ttl": 86_400_000,
        });

        let read = read(&cookie);

        assert_eq!(
            read.as_ref().map(|(id, _)| id.as_str()),
            Some("cid-example-1"),
            "should store only connectId"
        );
        assert_eq!(
            expires_at(read),
            1_700_086_400,
            "should expire at lastSynced + ttl"
        );
    }

    #[test]
    fn caps_the_ttl_and_defaults_a_missing_one() {
        let long = json!({ "connectId": "cid", "lastSynced": 0, "ttl": 1e15 });
        let missing = json!({ "connectId": "cid", "lastSynced": 0 });

        assert_eq!(
            expires_at(read(&long)),
            MAX_TTL.as_secs(),
            "should cap at 14 days"
        );
        assert_eq!(
            expires_at(read(&missing)),
            DEFAULT_TTL.as_secs(),
            "should use the default"
        );
    }

    #[test]
    fn ignores_missing_malformed_and_opt_out_cookies() {
        for cookie in [
            json!({}),
            json!({ "connectId": "cid" }),
            json!({ "connectId": "", "lastSynced": 1 }),
            json!({ "connectId": "0", "lastSynced": 1 }),
            json!({ "connectId": 5, "lastSynced": 1 }),
            json!("not an object"),
        ] {
            assert!(read(&cookie).is_none(), "should ignore {cookie}");
        }
        assert!(
            provider()
                .read_request_cookies(RequestCookies::new("connectId={broken"))
                .into_found()
                .is_none(),
            "should ignore malformed JSON"
        );
    }
}

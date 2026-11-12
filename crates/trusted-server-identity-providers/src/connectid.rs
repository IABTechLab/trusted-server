//! Yahoo `ConnectID` identity provider: reads `ConnectID` from Yahoo's own
//! first-party cookie.
//!
//! Yahoo's `ConnectID` module stores a JSON object in the `connectId` cookie
//! (optionally percent-encoded) with `connectId`, `lastSynced` and `ttl`,
//! both in milliseconds. The ID is stored with agent type 3 until
//! `lastSynced + ttl`, with the TTL capped at [`MAX_TTL`]; the coordinator
//! drops it once that time has passed. As in Prebid's `connectIdSystem`, a
//! missing or zero `ttl` means [`MAX_TTL`], a missing `lastSynced` means
//! the ID is fresh (stored for [`MAX_TTL`]), and the key may also be spelled
//! `connectid`. No server call is made.
//!
//! Yahoo's opt-out (`connectIdOptOut` = `1`) is kept in local storage, which
//! a server never sees, so it is not honoured here: a `ConnectID` cookie left
//! behind after an opt-out is still read until it expires.
//!
//! This kind has no Lockr capability: Lockr AIM is not documented to
//! deliver `ConnectID`, so a `lockr` settings table is rejected.
//!
//! The cookie's `he` and `puid` fields are never read, so they are never
//! stored. Parsing is lenient: an undecodable cookie, a missing
//! `connectId`, or a blank or placeholder (`0`) ID reads as no ID.

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
    CookieReader, cookie_capabilities, cookie_eid, is_expired, is_usable_id, percent_decoded,
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
    #[serde(rename = "connectId", alias = "connectid")]
    connect_id: String,
    #[serde(rename = "lastSynced", default)]
    last_synced_ms: Option<f64>,
    #[serde(default)]
    ttl: Option<f64>,
}

/// Milliseconds as a duration; negative or non-finite values are zero.
fn from_millis(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms / 1000.0).unwrap_or(Duration::ZERO)
}

/// Decodes one cookie value into the ID and its expiry.
fn connect_id(value: &str, now: UnixSeconds) -> Option<(String, ProviderExpiry)> {
    let decoded = percent_decoded(value);
    let cookie: ConnectIdCookie = serde_json::from_str(&decoded).ok()?;
    if !is_usable_id(&cookie.connect_id) {
        return None;
    }
    let Some(last_synced_ms) = cookie.last_synced_ms else {
        return Some((cookie.connect_id, ProviderExpiry::After(MAX_TTL)));
    };
    let ttl = cookie
        .ttl
        .filter(|ttl| *ttl > 0.0)
        .map_or(MAX_TTL, from_millis)
        .min(MAX_TTL);
    let expires = from_millis(last_synced_ms).saturating_add(ttl);
    let expiry = ProviderExpiry::At(UnixSeconds::new(expires.as_secs()));
    // An expired cookie never hides a later live one.
    (!is_expired(&expiry, now)).then_some((cookie.connect_id, expiry))
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
        let Some((id, expiry)) = self
            .cookies
            .read(cookies, |value| connect_id(value, cookies.now()))
        else {
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

    /// Request time the tests read cookies at.
    const NOW: UnixSeconds = UnixSeconds::new(1_700_000_000);

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
            .read_request_cookies(RequestCookies::new(&header).at(NOW))
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
            _ => panic!("should expire at a time"),
        }
    }

    #[test]
    fn reads_the_id_until_last_sync_plus_ttl() {
        let cookie = json!({
            "connectId": "cid-example-1",
            "he": "example-hashed-email",
            "puid": "example-puid",
            "lastSynced": 1_800_000_000_000_u64,
            "lastUsed": 4_700_000_100_000_u64,
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
            1_800_086_400,
            "should expire at lastSynced + ttl"
        );
    }

    #[test]
    fn caps_the_ttl_and_defaults_a_missing_or_zero_one() {
        // A far-future sync keeps the cookie live whatever today's date is.
        let synced_secs = 1_800_000_000_u64;
        let long = json!({ "connectId": "cid", "lastSynced": synced_secs * 1000, "ttl": 1e15 });
        let missing = json!({ "connectId": "cid", "lastSynced": synced_secs * 1000 });
        let zero = json!({ "connectId": "cid", "lastSynced": synced_secs * 1000, "ttl": 0 });

        assert_eq!(
            expires_at(read(&long)),
            synced_secs + MAX_TTL.as_secs(),
            "should cap at 14 days"
        );
        for cookie in [missing, zero] {
            assert_eq!(
                expires_at(read(&cookie)),
                synced_secs + MAX_TTL.as_secs(),
                "should use Prebid's 14 days for {cookie}"
            );
        }
    }

    #[test]
    fn reads_a_lowercase_key_and_a_missing_last_sync_as_fresh() {
        let lowercase = json!({ "connectid": "cid", "lastSynced": 1_800_000_000_000_u64 });
        let unsynced = json!({ "connectId": "cid" });

        assert_eq!(
            read(&lowercase).map(|(id, _)| id).as_deref(),
            Some("cid"),
            "should accept the lowercase key"
        );
        assert!(
            matches!(read(&unsynced), Some((_, ProviderExpiry::After(ttl))) if ttl == MAX_TTL),
            "should store an unsynced ID for 14 days"
        );
    }

    #[test]
    fn an_expired_cookie_does_not_hide_a_later_live_one() {
        let expired = json!({ "connectId": "cid-old", "lastSynced": 1000, "ttl": 1000 });
        let live = json!({ "connectId": "cid-live", "lastSynced": 1_800_000_000_000_u64 });
        let header = format!(
            "connectId={}; connectId={}",
            urlencoding::encode(&expired.to_string()),
            urlencoding::encode(&live.to_string())
        );

        let eids = provider()
            .read_request_cookies(RequestCookies::new(&header).at(NOW))
            .into_found()
            .expect("should read the live cookie");

        assert_eq!(
            eids[0].uids[0].id, "cid-live",
            "should skip the expired cookie"
        );
    }

    #[test]
    fn ignores_missing_malformed_and_opt_out_cookies() {
        for cookie in [
            json!({}),
            json!({ "connectId": "", "lastSynced": 1 }),
            json!({ "connectId": "0", "lastSynced": 1 }),
            json!({ "connectId": 5, "lastSynced": 1 }),
            json!("not an object"),
        ] {
            assert!(read(&cookie).is_none(), "should ignore {cookie}");
        }
        assert!(
            provider()
                .read_request_cookies(RequestCookies::new("connectId={broken").at(NOW))
                .into_found()
                .is_none(),
            "should ignore malformed JSON"
        );
    }
}

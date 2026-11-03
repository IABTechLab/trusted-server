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
//! When `__uid_2` yields nothing, the cookies in `cookie_names` (default
//! Prebid's `__uid2_advertising_token`) are read. Prebid's UID2 module
//! stores percent-encoded JSON there, `{"originalToken":…,"latestToken":…}`:
//! only `latestToken.advertising_token` is stored, until
//! `latestToken.identity_expires` (milliseconds) or with the shared default
//! TTL when that is missing. Any other value is read as a raw token with the
//! shared default TTL. With the Lockr capability enabled
//! (`[settings.lockr]`), the cookies Lockr AIM writes the raw token into
//! (default key `__uid2_advertising_token`) are read last, with the same
//! parser.
//!
//! A `latestToken` of `"optout"` in any of those cookies reports an opt-out:
//! no lookup is made and stored UID2 UIDs are removed.
//!
//! Parsing is lenient: an undecodable cookie, a missing token or expiry,
//! or a blank or opt-out token reads as no ID. A JSON value is never stored
//! as a token.

use std::borrow::Cow;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::{SourceDomain, UnixSeconds};
use trusted_server_core::ec::provider::{
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderError, ProviderExpiry, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};

use crate::common::{
    CookieReader, LockrSettings, ReadsLockr, bounded_expiry, cookie_capabilities, cookie_eid,
    is_usable_id, percent_decoded,
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
    /// The Lockr capability; see [`LockrSettings`].
    #[serde(default)]
    pub lockr: LockrSettings,
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
    cookies: CookieReader,
}

impl ReadsLockr for Uid2 {
    const LOCKR_COOKIE_NAMES: &'static [&'static str] = &["__uid2_advertising_token"];
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
            cookies: CookieReader::with_lockr::<Self>(
                common,
                settings.cookie_names,
                settings.lockr,
            )?,
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

/// `latestToken` value Prebid stores after an opt-out.
const PREBID_OPT_OUT: &str = "optout";

/// The fields of Prebid's stored tokens this provider reads.
#[derive(Deserialize)]
struct PrebidTokens {
    #[serde(rename = "latestToken")]
    latest_token: serde_json::Value,
}

/// The fields of Prebid's `latestToken` identity this provider reads.
#[derive(Deserialize)]
struct PrebidIdentity {
    advertising_token: String,
    #[serde(default)]
    identity_expires: Option<u64>,
}

/// One token cookie's reading.
enum TokenCookie {
    /// A usable token and its expiry.
    Token(String, ProviderExpiry),
    /// Prebid recorded an opt-out.
    OptedOut,
}

/// Decodes one token cookie: Prebid's stored tokens JSON or a raw token.
///
/// A JSON value is never taken as a raw token.
fn token_cookie(value: &str) -> Option<TokenCookie> {
    let decoded = percent_decoded(value);
    let decoded = decoded.trim();
    if decoded.starts_with('{') {
        return prebid_tokens(decoded);
    }
    is_usable_id(decoded).then(|| TokenCookie::Token(decoded.to_owned(), bounded_expiry(None)))
}

/// Reads `latestToken` from Prebid's stored tokens JSON.
fn prebid_tokens(json: &str) -> Option<TokenCookie> {
    let tokens: PrebidTokens = serde_json::from_str(json).ok()?;
    if tokens.latest_token.as_str() == Some(PREBID_OPT_OUT) {
        return Some(TokenCookie::OptedOut);
    }
    let identity: PrebidIdentity = serde_json::from_value(tokens.latest_token).ok()?;
    if !is_usable_id(&identity.advertising_token) {
        return None;
    }
    let expiry = identity.identity_expires.map_or_else(
        || bounded_expiry(None),
        |ms| ProviderExpiry::At(UnixSeconds::new(ms / 1000)),
    );
    Some(TokenCookie::Token(identity.advertising_token, expiry))
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
    ) -> CookieRead<'cookie> {
        let opted_out = self.cookies.any(cookies, |value| {
            matches!(token_cookie(value), Some(TokenCookie::OptedOut))
        });
        if opted_out {
            return CookieRead::OptedOut;
        }
        let token = cookies
            .get(&self.identity_cookie_name)
            .and_then(identity)
            .or_else(|| {
                self.cookies
                    .read(cookies, |value| match token_cookie(value)? {
                        TokenCookie::Token(token, expiry) => Some((token, expiry)),
                        TokenCookie::OptedOut => None,
                    })
            });
        let Some((token, expiry)) = token else {
            return CookieRead::Absent;
        };
        CookieRead::Found(vec![cookie_eid(&self.source, Cow::Owned(token), expiry)])
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
        let eids = provider
            .read_request_cookies(RequestCookies::new(header))
            .into_found()?;
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

    fn prebid_cookie(tokens: &serde_json::Value) -> String {
        format!(
            "__uid2_advertising_token={}",
            urlencoding::encode(&tokens.to_string())
        )
    }

    #[test]
    fn reads_the_latest_token_from_prebid_stored_tokens() {
        let header = prebid_cookie(&json!({
            "originalToken": {
                "advertising_token": "AgAAAAoriginaltoken",
                "identity_expires": 1_600_000_000_000_u64,
            },
            "latestToken": {
                "advertising_token": "AgAAAAlatesttoken",
                "refresh_token": "example-refresh-token",
                "refresh_response_key": "example-response-key",
                "identity_expires": 1_700_000_000_456_u64,
                "refresh_from": 1_699_990_000_000_u64,
                "refresh_expires": 1_702_000_000_000_u64,
            },
        }));

        let (token, expiry) = read(&*provider(&json!({})), &header).expect("should read");

        assert_eq!(
            token, "AgAAAAlatesttoken",
            "should store only the latest advertising token"
        );
        assert!(
            matches!(expiry, ProviderExpiry::At(at) if at.get() == 1_700_000_000),
            "should expire at the latest identity_expires"
        );
    }

    #[test]
    fn reports_a_prebid_opt_out() {
        let provider = provider(&json!({}));
        let own = identity_cookie(&json!({
            "advertising_token": "AgAAAAowntoken",
            "identity_expires": 1_700_000_000_000_u64,
        }));
        let opt_out = prebid_cookie(&json!({
            "originalToken": { "advertising_token": "AgAAAAoriginaltoken" },
            "latestToken": "optout",
        }));

        let header = format!("{own}; {opt_out}");

        let read = provider.read_request_cookies(RequestCookies::new(&header));

        assert!(
            matches!(read, CookieRead::OptedOut),
            "should report the opt-out over any token"
        );
    }

    #[test]
    fn never_stores_json_as_a_token() {
        let provider = provider(&json!({}));
        for header in [
            "__uid2_advertising_token=%7Bbroken".to_owned(),
            "__uid2_advertising_token={\"latestToken\":{}}".to_owned(),
            prebid_cookie(&json!({ "originalToken": {} })),
            prebid_cookie(&json!({ "latestToken": { "advertising_token": "" } })),
            prebid_cookie(&json!({ "latestToken": 7 })),
        ] {
            assert!(
                matches!(
                    provider.read_request_cookies(RequestCookies::new(&header)),
                    CookieRead::Absent
                ),
                "should read nothing from {header:?}"
            );
        }
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
    fn reads_the_lockr_cookie_last() {
        let enabled = provider(&json!({
            "lockr": { "enabled": true, "cookie_names": ["example_lockr_uid2"] },
        }));
        let disabled = provider(&json!({
            "lockr": { "cookie_names": ["example_lockr_uid2"] },
        }));
        let own = identity_cookie(&json!({
            "advertising_token": "AgAAAAowntoken",
            "identity_expires": 1_700_000_000_000_u64,
        }));

        let both = read(&*enabled, &format!("example_lockr_uid2=AgAAAAlockr; {own}"));
        let lockr_only = read(&*enabled, "example_lockr_uid2=AgAAAAlockr");
        let ignored = read(&*disabled, "example_lockr_uid2=AgAAAAlockr");

        assert_eq!(
            both.map(|(token, _)| token).as_deref(),
            Some("AgAAAAowntoken"),
            "should prefer the identity cookie"
        );
        assert_eq!(
            lockr_only.map(|(token, _)| token).as_deref(),
            Some("AgAAAAlockr"),
            "should read the Lockr cookie as a raw token"
        );
        assert!(ignored.is_none(), "should ignore Lockr while disabled");
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

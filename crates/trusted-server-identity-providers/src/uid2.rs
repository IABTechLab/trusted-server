//! UID2 identity provider: reads the UID2 advertising token from the
//! browser's own first-party cookies, and refreshes it server side when an
//! `endpoint` is configured.
//!
//! The UID2 SDK stores its identity in the `__uid_2` cookie as
//! percent-encoded JSON. `advertising_token` and `identity_expires`
//! (milliseconds) are read: the token is stored with agent type 3 until
//! `identity_expires`, and the coordinator drops it once that time has
//! passed.
//!
//! With an `endpoint` (UID2's `POST /v2/token/refresh`), the cookie's
//! `refresh_token` and `refresh_response_key` are also kept with the token as
//! private refresh state, and the source becomes due at `refresh_from`. The
//! refresh posts the refresh token, which needs no API key and no email, and
//! decrypts the reply with the response key (AES-256-GCM, a 12-byte IV
//! first). A refresh replying `optout` removes the stored token. No token is
//! ever generated: that needs an email or phone number, which is out of
//! scope. Without an `endpoint` the refresh state is not stored.
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
//! A `{"status":"optout"}` identity in `__uid_2` (the UID2 SDK's opt-out)
//! or a `latestToken` of `"optout"` in any of those cookies reports an
//! opt-out: no lookup is made and stored UID2 UIDs are removed.
//!
//! UID2 is not offered where GDPR applies: there the provider is skipped
//! for region, so its cookies are not read, no refresh is made and the UIDs
//! the row holds for it are removed.
//!
//! Parsing is lenient: an undecodable cookie, a missing token or expiry,
//! or a blank or placeholder (`0`) token reads as no ID. A JSON value is never stored
//! as a token.

use std::borrow::Cow;

use aes_gcm::aead::{Aead as _, KeyInit as _};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use error_stack::Report;
use http::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use trusted_server_core::ec::eid_types::{SourceDomain, UnixSeconds};
use trusted_server_core::ec::provider::{
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderEid, ProviderError, ProviderExpiry, ProviderKind, ProviderRefresh,
    ProviderRequest, ProviderResponse, RequestCookies,
};
use url::Url;

use crate::common::{
    CookieReader, LockrSettings, ReadsLockr, bounded_expiry, cookie_capabilities, cookie_eid,
    is_expired, is_usable_id, percent_decoded,
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
    /// UID2's token refresh endpoint; `None` disables server refresh.
    refresh_endpoint: Option<Url>,
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
            refresh_endpoint: common.endpoint.cloned(),
        })
    }
}

/// A UID2 identity: the `__uid_2` cookie and the `body` of a decrypted
/// refresh reply share these fields.
#[derive(Deserialize)]
struct Identity {
    advertising_token: String,
    identity_expires: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    refresh_response_key: Option<String>,
    #[serde(default)]
    refresh_from: Option<u64>,
}

/// The refresh state kept with a stored token.
#[derive(Serialize, Deserialize)]
struct RefreshState {
    /// The refresh token posted to the refresh endpoint.
    token: String,
    /// Base64 AES-256 key that decrypts the refresh reply.
    key: String,
}

/// A usable token with its expiry and, when present, its refresh state.
struct Token {
    token: String,
    expiry: ProviderExpiry,
    refresh: Option<(RefreshState, ProviderExpiry)>,
}

impl Identity {
    /// The token, with refresh state when the identity carries all of it.
    fn into_token(self) -> Option<Token> {
        if !is_usable_id(&self.advertising_token) {
            return None;
        }
        let refresh = match (
            self.refresh_token,
            self.refresh_response_key,
            self.refresh_from,
        ) {
            (Some(token), Some(key), Some(from)) if !token.is_empty() && !key.is_empty() => Some((
                RefreshState { token, key },
                ProviderExpiry::At(UnixSeconds::new(from / 1000)),
            )),
            _ => None,
        };
        Some(Token {
            token: self.advertising_token,
            expiry: ProviderExpiry::At(UnixSeconds::new(self.identity_expires / 1000)),
            refresh,
        })
    }
}

/// Decodes one `__uid_2` value.
fn identity(value: &str) -> Option<Token> {
    let decoded = percent_decoded(value);
    serde_json::from_str::<Identity>(&decoded)
        .ok()?
        .into_token()
}

/// A decrypted refresh reply.
#[derive(Deserialize)]
struct RefreshReply {
    status: String,
    #[serde(default)]
    body: Option<Identity>,
}

/// Status of a refresh reply for a user who opted out.
const REFRESH_OPT_OUT: &str = "optout";

/// Length of the AES-GCM IV that starts a refresh reply.
const IV_BYTES: usize = 12;

/// Decrypts a base64 refresh reply with the base64 `key`.
fn decrypt_reply(body: &[u8], key: &str) -> Option<Vec<u8>> {
    let key = BASE64.decode(key).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let envelope = BASE64.decode(body.trim_ascii()).ok()?;
    if envelope.len() <= IV_BYTES {
        return None;
    }
    let (iv, ciphertext) = envelope.split_at(IV_BYTES);
    let nonce = Nonce::try_from(iv).ok()?;
    cipher.decrypt(&nonce, ciphertext).ok()
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

    /// UID2 is not offered where GDPR applies, so none of its sources is
    /// collected or kept there.
    fn excluded_where_gdpr_applies(&self, _source: &SourceDomain) -> bool {
        true
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        let Some(endpoint) = &self.refresh_endpoint else {
            return Err(Report::new(ProviderError::Request)
                .attach("uid2 refreshes only with an endpoint and never generates tokens"));
        };
        let state = lookup
            .current
            .iter()
            .flat_map(|(_, eid)| eid.uids())
            .find_map(|uid| uid.refresh())
            .and_then(|refresh| serde_json::from_str::<RefreshState>(&refresh.state).ok())
            .ok_or_else(|| {
                Report::new(ProviderError::Request).attach("no stored uid2 refresh token")
            })?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        Ok(ProviderRequest {
            method: http::Method::POST,
            url: endpoint.clone(),
            headers,
            body: Bytes::from(state.token),
            context: Some(state.key),
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        let key = response.context.ok_or_else(|| {
            Report::new(ProviderError::InvalidResponse).attach("no uid2 response key")
        })?;
        let plaintext = decrypt_reply(response.body, key).ok_or_else(|| {
            Report::new(ProviderError::InvalidResponse).attach("uid2 reply not decrypted")
        })?;
        let reply: RefreshReply = serde_json::from_slice(&plaintext).map_err(|error| {
            Report::new(ProviderError::InvalidResponse).attach(format!("uid2 reply: {error}"))
        })?;
        if reply.status == REFRESH_OPT_OUT {
            return Ok(LookupOutcome::OptedOut);
        }
        let token = reply
            .body
            .filter(|_| reply.status == "success")
            .and_then(Identity::into_token)
            .ok_or_else(|| {
                Report::new(ProviderError::InvalidResponse)
                    .attach(format!("uid2 refresh status {}", reply.status))
            })?;
        Ok(LookupOutcome::Found(vec![self.eid(token)]))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        // The UID2 SDK records an opt-out as `{"status":"optout"}` in its
        // identity cookie, Prebid as a `latestToken` of `"optout"`; either
        // one, in any duplicate cookie, wins over every token.
        let opted_out = cookies.get_all(&self.identity_cookie_name).any(|value| {
            serde_json::from_str::<serde_json::Value>(&percent_decoded(value))
                .is_ok_and(|identity| identity["status"] == REFRESH_OPT_OUT)
        }) || self.cookies.any(cookies, |value| {
            matches!(token_cookie(value), Some(TokenCookie::OptedOut))
        });
        if opted_out {
            return CookieRead::OptedOut;
        }
        // An expired identity never hides a later live one.
        let token = cookies
            .get_all(&self.identity_cookie_name)
            .filter_map(identity)
            .find(|token| !is_expired(&token.expiry))
            .or_else(|| {
                self.cookies
                    .read(cookies, |value| match token_cookie(value)? {
                        TokenCookie::Token(token, expiry) if !is_expired(&expiry) => Some(Token {
                            token,
                            expiry,
                            refresh: None,
                        }),
                        TokenCookie::Token(..) | TokenCookie::OptedOut => None,
                    })
            });
        let Some(token) = token else {
            return CookieRead::Absent;
        };
        CookieRead::Found(vec![self.eid(token)])
    }
}

impl Uid2Provider {
    /// Builds the entry for `token`, keeping its refresh state only when
    /// server refresh is enabled.
    fn eid<'any>(&self, token: Token) -> ProviderEid<'any> {
        let mut eid = cookie_eid(&self.source, Cow::Owned(token.token), token.expiry);
        if self.refresh_endpoint.is_some()
            && let Some((state, from)) = token.refresh
            && let Ok(state) = serde_json::to_string(&state)
        {
            for uid in &mut eid.uids {
                uid.refresh = Some(ProviderRefresh {
                    state: Cow::Owned(state.clone()),
                    from,
                });
            }
        }
        eid
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::ec::provider::build_kind;
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, UidValue};
    use trusted_server_core::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvRefresh, KvUid};
    use trusted_server_core::ec::provider::{ClientSignals, OwnedEntries};

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
    fn is_not_run_where_gdpr_applies() {
        let provider = provider(&json!({}));
        let source = SourceDomain::try_from("uid2.example.com").expect("should build source");

        assert!(
            provider.excluded_where_gdpr_applies(&source),
            "should not run UID2 where GDPR applies"
        );
    }

    #[test]
    fn reads_only_the_advertising_token_until_identity_expiry() {
        let header = identity_cookie(&json!({
            "advertising_token": "AgAAAAexampletoken+/=",
            "refresh_token": "example-refresh-token",
            "identity_expires": 4_700_000_000_123_u64,
            "refresh_from": 4_699_990_000_000_u64,
            "refresh_expires": 4_702_000_000_000_u64,
            "refresh_response_key": "example-response-key",
        }));

        let (token, expiry) = read(&*provider(&json!({})), &header).expect("should read");

        assert_eq!(
            token, "AgAAAAexampletoken+/=",
            "should store only the token"
        );
        assert!(
            matches!(expiry, ProviderExpiry::At(at) if at.get() == 4_700_000_000),
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
                "identity_expires": 4_700_000_000_456_u64,
                "refresh_from": 4_699_990_000_000_u64,
                "refresh_expires": 4_702_000_000_000_u64,
            },
        }));

        let (token, expiry) = read(&*provider(&json!({})), &header).expect("should read");

        assert_eq!(
            token, "AgAAAAlatesttoken",
            "should store only the latest advertising token"
        );
        assert!(
            matches!(expiry, ProviderExpiry::At(at) if at.get() == 4_700_000_000),
            "should expire at the latest identity_expires"
        );
    }

    #[test]
    fn reports_a_prebid_opt_out() {
        let provider = provider(&json!({}));
        let own = identity_cookie(&json!({
            "advertising_token": "AgAAAAowntoken",
            "identity_expires": 4_700_000_000_000_u64,
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
    fn reports_an_sdk_opt_out_over_a_fallback_token() {
        let provider = provider(&json!({}));
        let live = identity_cookie(&json!({
            "advertising_token": "AgAAAAowntoken",
            "identity_expires": 4_700_000_000_000_u64,
        }));
        let opt_out = identity_cookie(&json!({ "status": "optout" }));
        let fallback = prebid_cookie(&json!({
            "latestToken": {
                "advertising_token": "AgAAAAlatesttoken",
                "identity_expires": 4_700_000_000_000_u64,
            },
        }));
        let header = format!("{live}; {opt_out}; {fallback}");

        let read = provider.read_request_cookies(RequestCookies::new(&header));

        assert!(
            matches!(read, CookieRead::OptedOut),
            "should report the SDK opt-out over every token"
        );
    }

    #[test]
    fn reports_an_opt_out_in_a_later_duplicate_cookie() {
        let provider = provider(&json!({}));
        let token = prebid_cookie(&json!({
            "latestToken": {
                "advertising_token": "AgAAAAlatesttoken",
                "identity_expires": 4_700_000_000_000_u64,
            },
        }));
        let opt_out = prebid_cookie(&json!({ "latestToken": "optout" }));
        let header = format!("{token}; {opt_out}");

        let read = provider.read_request_cookies(RequestCookies::new(&header));

        assert!(
            matches!(read, CookieRead::OptedOut),
            "should see the opt-out in a later cookie of the same name"
        );
    }

    #[test]
    fn an_expired_identity_does_not_hide_a_later_live_token() {
        let provider = provider(&json!({}));
        let expired = identity_cookie(&json!({
            "advertising_token": "AgAAAAexpiredtoken",
            "identity_expires": 1_000_u64,
        }));
        let live = prebid_cookie(&json!({
            "latestToken": {
                "advertising_token": "AgAAAAlivetoken",
                "identity_expires": 4_700_000_000_000_u64,
            },
        }));
        let header = format!("{expired}; {live}");

        let read = read(&*provider, &header);

        assert_eq!(
            read.map(|(token, _)| token).as_deref(),
            Some("AgAAAAlivetoken"),
            "should skip the expired identity"
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
            "identity_expires": 4_700_000_000_000_u64,
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

    const REFRESH_ENDPOINT: &str = "https://uid2.example.com/v2/token/refresh";

    fn refreshing_provider() -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "uid2",
            "kind": "uid2",
            "sources": ["uid2.example.com"],
            "endpoint": REFRESH_ENDPOINT,
            "allowed_domains": ["uid2.example.com"],
        }))
        .expect("should parse provider config");
        build_kind::<Uid2>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn sdk_identity_header() -> String {
        identity_cookie(&json!({
            "advertising_token": "AgAAAAexampletoken",
            "refresh_token": "example-refresh-token",
            "identity_expires": 4_700_000_000_000_u64,
            "refresh_from": 4_699_990_000_000_u64,
            "refresh_expires": 4_702_000_000_000_u64,
            "refresh_response_key": BASE64.encode([7_u8; 32]),
        }))
    }

    fn refresh_of(
        provider: &dyn IdentityProvider,
        header: &str,
    ) -> Option<(String, ProviderExpiry)> {
        let eids = provider
            .read_request_cookies(RequestCookies::new(header))
            .into_found()
            .expect("should read the token");
        let refresh = eids[0].uids[0].refresh.as_ref()?;
        Some((refresh.state.to_string(), refresh.from))
    }

    #[test]
    fn keeps_the_refresh_state_when_refresh_is_enabled() {
        let (state, from) = refresh_of(&*refreshing_provider(), &sdk_identity_header())
            .expect("should keep the refresh state");

        let state: RefreshState = serde_json::from_str(&state).expect("should parse the state");
        assert_eq!(
            state.token, "example-refresh-token",
            "should keep the refresh token"
        );
        assert!(
            matches!(from, ProviderExpiry::At(at) if at.get() == 4_699_990_000),
            "should ask for a refresh at refresh_from"
        );
    }

    #[test]
    fn stores_no_refresh_state_without_an_endpoint() {
        assert!(
            refresh_of(&*provider(&json!({})), &sdk_identity_header()).is_none(),
            "should not keep the refresh token when refresh is disabled"
        );
    }

    fn stored_entry() -> KvEntry {
        let state = serde_json::to_string(&RefreshState {
            token: "example-refresh-token".to_owned(),
            key: BASE64.encode([7_u8; 32]),
        })
        .expect("should serialize the state");
        let uid = KvUid::new(
            UidValue::try_from("AgAAAAexampletoken".to_owned()).expect("should build uid"),
            AgentType::try_from(3).expect("should build atype"),
            None,
            Some(ProviderId::new("uid2")),
            Expiry::At(UnixSeconds::new(4_700_000_000)),
        )
        .with_refresh(KvRefresh {
            state,
            from: UnixSeconds::new(4_699_990_000),
        });
        let record = KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(4_699_995_000),
        )
        .expect("should keep the live uid");
        let mut entry = KvEntry::tombstone(1);
        entry.consent.ok = true;
        entry.ids.insert("uid2.example.com".to_owned(), record);
        entry
    }

    #[test]
    fn refresh_posts_the_stored_refresh_token() {
        let entry = stored_entry();
        let sources = [SourceDomain::try_from("uid2.example.com").expect("should build source")];
        let owner = ProviderId::new("uid2");
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: None,
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &sources,
            current: OwnedEntries::new(Some(&entry), &sources, &owner),
            cookies: RequestCookies::default(),
            hashed_email: None,
        };

        let request = refreshing_provider()
            .build_request(&lookup)
            .expect("should build the refresh request");

        assert_eq!(request.method, http::Method::POST, "should post");
        assert_eq!(
            request.url.as_str(),
            REFRESH_ENDPOINT,
            "should call the endpoint"
        );
        assert_eq!(
            request.body.as_ref(),
            b"example-refresh-token",
            "should post the refresh token only"
        );
        assert_eq!(
            request.context,
            Some(BASE64.encode([7_u8; 32])),
            "should keep the response key for decryption"
        );
    }

    #[test]
    fn makes_no_request_without_an_endpoint() {
        let entry = stored_entry();
        let sources = [SourceDomain::try_from("uid2.example.com").expect("should build source")];
        let owner = ProviderId::new("uid2");
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: None,
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &sources,
            current: OwnedEntries::new(Some(&entry), &sources, &owner),
            cookies: RequestCookies::default(),
            hashed_email: None,
        };

        assert!(
            provider(&json!({})).build_request(&lookup).is_err(),
            "should never generate a token"
        );
    }

    fn encrypted_reply(reply: &serde_json::Value) -> String {
        let cipher = Aes256Gcm::new_from_slice(&[7_u8; 32]).expect("should build cipher");
        let iv = [1_u8; IV_BYTES];
        let ciphertext = cipher
            .encrypt(
                &Nonce::try_from(&iv[..]).expect("should build nonce"),
                reply.to_string().as_bytes(),
            )
            .expect("should encrypt");
        BASE64.encode([&iv[..], &ciphertext].concat())
    }

    fn reply<'a>(body: &'a str, key: &'a str, headers: &'a HeaderMap) -> ProviderResponse<'a> {
        ProviderResponse {
            status: http::StatusCode::OK,
            headers,
            body: body.as_bytes(),
            context: Some(key),
        }
    }

    #[test]
    fn decrypts_a_refreshed_identity() {
        let body = encrypted_reply(&json!({
            "status": "success",
            "body": {
                "advertising_token": "AgAAAArefreshedtoken",
                "refresh_token": "next-refresh-token",
                "identity_expires": 4_700_100_000_000_u64,
                "refresh_from": 4_700_090_000_000_u64,
                "refresh_expires": 4_700_200_000_000_u64,
                "refresh_response_key": BASE64.encode([9_u8; 32]),
            },
        }));

        let (key, headers) = (BASE64.encode([7_u8; 32]), HeaderMap::new());

        let outcome = refreshing_provider()
            .parse_response(reply(&body, &key, &headers))
            .expect("should parse the reply");

        let LookupOutcome::Found(eids) = outcome else {
            panic!("should find the refreshed token");
        };
        let uid = &eids[0].uids[0];
        assert_eq!(uid.id, "AgAAAArefreshedtoken", "should store the new token");
        assert!(
            matches!(uid.expiry, ProviderExpiry::At(at) if at.get() == 4_700_100_000),
            "should expire at the new identity_expires"
        );
        let refresh = uid
            .refresh
            .as_ref()
            .expect("should keep the next refresh state");
        let state: RefreshState =
            serde_json::from_str(&refresh.state).expect("should parse the state");
        assert_eq!(
            state.token, "next-refresh-token",
            "should keep the next refresh token"
        );
        assert_eq!(
            state.key,
            BASE64.encode([9_u8; 32]),
            "should keep the next key"
        );
    }

    #[test]
    fn reports_an_opt_out_from_the_refresh() {
        let body = encrypted_reply(&json!({ "status": "optout" }));

        let (key, headers) = (BASE64.encode([7_u8; 32]), HeaderMap::new());

        let outcome = refreshing_provider()
            .parse_response(reply(&body, &key, &headers))
            .expect("should parse the reply");

        assert!(
            matches!(outcome, LookupOutcome::OptedOut),
            "should remove the stored token on opt-out"
        );
    }

    #[test]
    fn rejects_a_reply_under_another_key() {
        let body = encrypted_reply(&json!({ "status": "optout" }));

        let (key, headers) = (BASE64.encode([8_u8; 32]), HeaderMap::new());

        assert!(
            refreshing_provider()
                .parse_response(reply(&body, &key, &headers))
                .is_err(),
            "should reject a reply that does not decrypt"
        );
    }
}

//! `LiveRamp` identity provider: reads the `RampID` envelope from `LiveRamp`'s own
//! first-party cookie.
//!
//! `LiveRamp`'s ATS script stores the envelope in the `_lr_env` cookie as
//! base64 (or base64url) of a JSON object whose `envelope` field holds it.
//! Prebid's `identityLink` ID module stores the envelope it receives bare,
//! under its configured storage name (commonly `idl_env`); a value that does
//! not decode to JSON is taken as such a bare envelope.
//! The envelope is stored unchanged with agent type 3 for
//! [`ENVELOPE_TTL`].
//!
//! With an `endpoint` (`LiveRamp`'s ATS envelope API, path
//! `/api/identity/v2/envelope`), a `placement_id` and the hashed-email
//! capability (`[settings.hashed_email]`, spec §8.7), the provider also calls
//! `LiveRamp` server to server (`atype=3`) with the placement ID, consent
//! strings, the publisher `Origin` and the visitor's IP in
//! `X-Forwarded-For`:
//!
//! - A visitor without a live envelope is looked up by the hashed email the
//!   publisher's status endpoint returned (`it=4`, `iv=<hashed email>`, sent
//!   as returned). The source needs a hashed email, so the coordinator skips
//!   it for a visitor without one.
//! - An envelope the provider obtained keeps refresh state due
//!   `refresh_interval_secs` later (default 30 minutes, as `LiveRamp`
//!   recommends); its refresh calls `<endpoint>/refresh` with `it=19` and the
//!   envelope, which needs no hashed email.
//!
//! A `200` reply lists `envelopes`; the `RampID` entry (`type` 19) is
//! stored. A `204` reply from the envelope API reports an opt-out. One from
//! the refresh API means `LiveRamp` no longer accepts the envelope: it is
//! kept, without refresh state, for one more refresh interval, after which a
//! signed-in visitor is looked up by hashed email again. Neither call sends
//! the EC ID. Without an `endpoint` no server call is made.
//!
//! With the Lockr capability enabled (`[settings.lockr]`), the cookies Lockr
//! AIM writes the envelope into (default key `_lr_env`) are read after the
//! own cookies, with the same parser.
//!
//! Parsing is lenient: a value outside the base64 alphabet, a JSON body
//! without a string `envelope`, or a blank or placeholder (`0`) envelope reads
//! as no ID.

use std::borrow::Cow;
use std::time::Duration;

use bytes::Bytes;
use http::header::ORIGIN;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use url::Url;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use error_stack::{Report, ResultExt as _};
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    CookieRead, HashedEmailPath, IdentityProvider, LookupInput, LookupOutcome,
    ProviderCapabilities, ProviderCommon, ProviderConfigError, ProviderEid, ProviderError,
    ProviderExpiry, ProviderKind, ProviderRefresh, ProviderRequest, ProviderResponse,
    RequestCookies, SourceInput,
};

use crate::common::{
    CookieReader, HashedEmailSettings, LockrSettings, ReadsLockr, UsesHashedEmail, bounded_expiry,
    cookie_capabilities, cookie_eid, hashed_email_path, is_routable_ip, is_usable_id,
    percent_decoded,
};

/// `it` value of a hashed-email lookup.
const HASHED_EMAIL_TYPE: &str = "4";
/// `it` value of a `RampID` envelope refresh.
const ENVELOPE_REFRESH_TYPE: &str = "19";
/// Prefix of a refresh request's context, followed by the envelope being
/// refreshed; never sent, and handed back with the response.
const REFRESH_CONTEXT: &str = "refresh:";
/// Refresh state kept with an envelope the provider obtained: the envelope
/// itself is the refresh input, so the state only marks it refreshable.
const REFRESH_STATE: &str = "envelope";
/// Default `refresh_interval_secs`: `LiveRamp` recommends refreshing every
/// 30 minutes.
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 30 * 60;
/// Shortest accepted `refresh_interval_secs`.
const MIN_REFRESH_INTERVAL_SECS: u64 = 60;
/// `atype` value of a server-to-server integration (`2` is the ATS API).
const SERVER_TO_SERVER: &str = "3";
/// `type` of the `RampID` envelope in a v2 reply's `envelopes`.
const RAMPID_ENVELOPE_TYPE: u32 = 19;
/// `ct` value of a TCF v2 consent string.
const TCF_CONSENT: &str = "4";
/// `ct` value of a US Privacy consent string.
const US_PRIVACY_CONSENT: &str = "3";

/// How long a stored envelope is kept.
pub const ENVELOPE_TTL: Duration = Duration::from_secs(15 * 24 * 60 * 60);

/// The `LiveRamp` provider kind.
pub struct LiveRamp;

/// Settings for a `liveramp` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRampSettings {
    /// Cookies read in order; the first holding an envelope wins.
    #[serde(default = "LiveRampSettings::default_cookie_names")]
    pub cookie_names: Vec<String>,
    /// The Lockr capability; see [`LockrSettings`].
    #[serde(default)]
    pub lockr: LockrSettings,
    /// `LiveRamp` Placement ID (`pid`); required with an `endpoint`.
    #[serde(default)]
    pub placement_id: Option<u64>,
    /// The hashed-email capability; see [`HashedEmailSettings`]. Required
    /// with an `endpoint`.
    #[serde(default)]
    pub hashed_email: HashedEmailSettings,
    /// Seconds after an envelope is obtained before it is refreshed.
    #[serde(default = "LiveRampSettings::default_refresh_interval_secs")]
    pub refresh_interval_secs: u64,
}

impl LiveRampSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["_lr_env".to_owned(), "idl_env".to_owned()]
    }

    const fn default_refresh_interval_secs() -> u64 {
        DEFAULT_REFRESH_INTERVAL_SECS
    }
}

/// A built `LiveRamp` provider.
#[derive(Debug)]
pub struct LiveRampProvider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    cookies: CookieReader,
    /// The envelope lookup; `None` disables server calls.
    lookup: Option<EnvelopeLookup>,
}

/// A configured server-side envelope lookup and refresh.
#[derive(Debug)]
struct EnvelopeLookup {
    endpoint: Url,
    refresh_endpoint: Url,
    placement_id: u64,
    hashed_email_path: HashedEmailPath,
    refresh_interval: Duration,
}

impl ReadsLockr for LiveRamp {
    const LOCKR_COOKIE_NAMES: &'static [&'static str] = &["_lr_env"];
}

impl UsesHashedEmail for LiveRamp {}

impl ProviderKind for LiveRamp {
    const KIND: &'static str = "liveramp";
    type Settings = LiveRampSettings;
    type Provider = LiveRampProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: LiveRampSettings,
    ) -> Result<LiveRampProvider, Report<ProviderConfigError>> {
        let cookies =
            CookieReader::with_lockr::<Self>(common, settings.cookie_names, settings.lockr)?;
        if cookies.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("a liveramp provider needs at least one cookie name"));
        }
        let (mut capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        let hashed_email_path = hashed_email_path::<Self>(settings.hashed_email)?;
        let refresh_interval = refresh_interval(settings.refresh_interval_secs)?;
        let lookup = match (common.endpoint, settings.placement_id, hashed_email_path) {
            (None, _, None) => None,
            (Some(endpoint), Some(placement_id), Some(hashed_email_path)) => Some(EnvelopeLookup {
                endpoint: endpoint.clone(),
                refresh_endpoint: refresh_endpoint(endpoint)?,
                placement_id,
                hashed_email_path,
                refresh_interval,
            }),
            (None, _, Some(_)) => {
                return Err(Report::new(ProviderConfigError::Invalid)
                    .attach("liveramp hashed_email needs an endpoint and placement_id"));
            }
            (Some(_), _, _) => {
                return Err(Report::new(ProviderConfigError::Invalid)
                    .attach("a liveramp endpoint needs placement_id and hashed_email enabled"));
            }
        };
        // A lookup needs a hashed email; a refresh does not, and the
        // coordinator keeps a refresh-due source due regardless.
        if lookup.is_some() {
            for capability in &mut capabilities.sources {
                capability.input = SourceInput::HashedEmail;
            }
        }
        Ok(LiveRampProvider {
            capabilities,
            source,
            cookies,
            lookup,
        })
    }
}

/// The refresh API: `refresh` appended to the envelope API path.
fn refresh_endpoint(endpoint: &Url) -> Result<Url, Report<ProviderConfigError>> {
    let mut url = endpoint.clone();
    url.path_segments_mut()
        .map_err(|()| {
            Report::new(ProviderConfigError::Invalid).attach("endpoint cannot hold a path")
        })?
        .pop_if_empty()
        .push("refresh");
    Ok(url)
}

/// Checks `refresh_interval_secs`: at least a minute and shorter than the
/// envelope's lifetime.
fn refresh_interval(seconds: u64) -> Result<Duration, Report<ProviderConfigError>> {
    let interval = Duration::from_secs(seconds);
    if seconds < MIN_REFRESH_INTERVAL_SECS || interval >= ENVELOPE_TTL {
        return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
            "liveramp refresh_interval_secs must be at least {MIN_REFRESH_INTERVAL_SECS} and below {}",
            ENVELOPE_TTL.as_secs()
        )));
    }
    Ok(interval)
}

/// The stored envelope the refresh API no longer accepts, rewritten without
/// refresh state to expire after one more refresh interval.
///
/// Keeping its refresh state would make the source refresh-due on every
/// navigation until the envelope expires; once this one lapses, the source
/// is due again and a signed-in visitor is looked up by hashed email.
fn rejected_envelope<'body>(
    source: &SourceDomain,
    envelope: &'body str,
    refresh_interval: Duration,
) -> ProviderEid<'body> {
    cookie_eid(
        source,
        Cow::Borrowed(envelope),
        ProviderExpiry::After(refresh_interval),
    )
}

#[derive(Deserialize)]
struct EnvelopeCookie {
    envelope: String,
}

/// A v2 envelope API reply: `{"envelopes":[{"type":19,"value":…}]}`.
#[derive(Deserialize)]
struct EnvelopeReply {
    #[serde(default)]
    envelopes: Vec<ReplyEnvelope>,
}

/// One entry of [`EnvelopeReply::envelopes`].
#[derive(Deserialize)]
struct ReplyEnvelope {
    #[serde(rename = "type", default)]
    kind: Option<u32>,
    #[serde(default)]
    value: Option<String>,
}

/// Decodes one cookie value into its envelope.
///
/// A value whose base64 decodes to JSON must carry a usable `envelope`
/// (the ATS `_lr_env` format); any other base64-alphabet value is a bare
/// envelope (the Prebid `idl_env` format).
fn envelope(value: &str) -> Option<String> {
    let value = percent_decoded(value);
    let value = value.trim();
    let is_base64_alphabet = value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
    });
    if !is_base64_alphabet {
        return None;
    }
    let json = [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .filter_map(|engine| engine.decode(value).ok())
        .find_map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let envelope = match json {
        Some(body) => {
            serde_json::from_value::<EnvelopeCookie>(body)
                .ok()?
                .envelope
        }
        None => value.to_owned(),
    };
    is_usable_id(&envelope).then_some(envelope)
}

impl IdentityProvider for LiveRampProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// No request carries the EC ID, so the provider may run on the
    /// navigation that mints the EC.
    fn sends_ec_id(&self) -> bool {
        false
    }

    fn hashed_email_path(&self) -> Option<&HashedEmailPath> {
        self.lookup
            .as_ref()
            .map(|envelope_lookup| &envelope_lookup.hashed_email_path)
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        let Some(envelope_lookup) = &self.lookup else {
            return Err(Report::new(ProviderError::Request)
                .attach("liveramp looks up only with an endpoint"));
        };
        // LiveRamp requires the visitor's IP on server-to-server calls.
        let ip = lookup
            .client
            .ip
            .filter(|ip| is_routable_ip(*ip))
            .ok_or_else(|| {
                Report::new(ProviderError::Request).attach("liveramp needs a routable client IP")
            })?;
        // A live envelope whose refresh is due is refreshed; otherwise the
        // visitor is looked up by hashed email.
        let due_envelope = lookup
            .current
            .iter()
            .flat_map(|(_, eid)| eid.uids())
            .find(|uid| uid.refresh_due(lookup.now));
        let (endpoint, identifier_type, identifier, context) =
            match (due_envelope, lookup.hashed_email) {
                (Some(uid), _) => (
                    &envelope_lookup.refresh_endpoint,
                    ENVELOPE_REFRESH_TYPE,
                    uid.id().as_str(),
                    Some(format!("{REFRESH_CONTEXT}{}", uid.id().as_str())),
                ),
                (None, Some(hashed_email)) => (
                    &envelope_lookup.endpoint,
                    HASHED_EMAIL_TYPE,
                    hashed_email.as_str(),
                    None,
                ),
                (None, None) => {
                    return Err(Report::new(ProviderError::Request)
                        .attach("liveramp needs a hashed email or an envelope due for refresh"));
                }
            };
        let consent = lookup.consent;
        let mut url = endpoint.clone();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("pid", &envelope_lookup.placement_id.to_string())
                .append_pair("it", identifier_type)
                .append_pair("iv", identifier)
                .append_pair("atype", SERVER_TO_SERVER);
            if let Some(tc_string) = &consent.raw_tc_string {
                query
                    .append_pair("ct", TCF_CONSENT)
                    .append_pair("cv", tc_string);
            } else if let Some(us_privacy) = &consent.raw_us_privacy {
                query
                    .append_pair("ct", US_PRIVACY_CONSENT)
                    .append_pair("cv", us_privacy);
            }
            if let Some(gpp) = &consent.raw_gpp_string {
                query.append_pair("gpp", gpp);
            }
            // `gpp_sid` is a single section ID; Prebid also sends only the
            // first applicable section.
            if let Some(section_id) = consent.gpp_section_ids.as_deref().and_then(<[u16]>::first) {
                query.append_pair("gpp_sid", &section_id.to_string());
            }
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(&format!("https://{}", lookup.publisher_domain))
                .change_context(ProviderError::Request)?,
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_str(&ip.to_string()).change_context(ProviderError::Request)?,
        );
        Ok(ProviderRequest {
            method: Method::GET,
            url,
            headers,
            body: Bytes::new(),
            context,
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        let refreshed = response
            .context
            .and_then(|context| context.strip_prefix(REFRESH_CONTEXT));
        // The envelope API reports an opt-out with `204`; the refresh API
        // reports an envelope it no longer accepts.
        if response.status == StatusCode::NO_CONTENT {
            return Ok(match (refreshed, &self.lookup) {
                (Some(envelope), Some(envelope_lookup)) if is_usable_id(envelope) => {
                    LookupOutcome::Found(vec![rejected_envelope(
                        &self.source,
                        envelope,
                        envelope_lookup.refresh_interval,
                    )])
                }
                (Some(_), _) => LookupOutcome::NoMatch,
                (None, _) => LookupOutcome::OptedOut,
            });
        }
        if !response.status.is_success() {
            return Err(Report::new(ProviderError::InvalidResponse)
                .attach(format!("liveramp returned status {}", response.status)));
        }
        let reply: EnvelopeReply =
            serde_json::from_slice(response.body).change_context(ProviderError::InvalidResponse)?;
        let Some(envelope) = reply
            .envelopes
            .into_iter()
            .find(|envelope| envelope.kind == Some(RAMPID_ENVELOPE_TYPE))
            .and_then(|envelope| envelope.value)
            .filter(|envelope| is_usable_id(envelope))
        else {
            return Ok(LookupOutcome::NoMatch);
        };
        let mut eid = cookie_eid(
            &self.source,
            Cow::Owned(envelope),
            bounded_expiry(Some(ENVELOPE_TTL)),
        );
        if let Some(envelope_lookup) = &self.lookup {
            for uid in &mut eid.uids {
                uid.refresh = Some(ProviderRefresh {
                    state: Cow::Borrowed(REFRESH_STATE),
                    from: ProviderExpiry::After(envelope_lookup.refresh_interval),
                });
            }
        }
        Ok(LookupOutcome::Found(vec![eid]))
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> CookieRead<'cookie> {
        let Some(envelope) = self.cookies.read(cookies, envelope) else {
            return CookieRead::Absent;
        };
        CookieRead::Found(vec![cookie_eid(
            &self.source,
            Cow::Owned(envelope),
            bounded_expiry(Some(ENVELOPE_TTL)),
        )])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::consent::types::ConsentContext;
    use trusted_server_core::ec::eid_types::{
        AgentType, EidFields, EidValidationError, Expiry, ProviderId, UidValue, UnixSeconds,
    };
    use trusted_server_core::ec::kv_types::{
        KvEid, KvEntry, KvPartnerId, KvRefresh, KvUid, MAX_STORED_UID_LENGTH,
    };
    use trusted_server_core::ec::provider::{
        ClientSignals, HashedEmail, OwnedEntries, ProviderExpiry, build_kind,
    };
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider(settings: &serde_json::Value) -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "liveramp",
            "kind": "liveramp",
            "sources": ["liveramp.example.com"],
            "settings": settings,
        }))
        .expect("should parse provider config");
        build_kind::<LiveRamp>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn encoded(body: &serde_json::Value, engine: &base64::engine::GeneralPurpose) -> String {
        engine.encode(body.to_string())
    }

    fn read(provider: &dyn IdentityProvider, header: &str) -> Option<(String, ProviderExpiry)> {
        let eids = provider
            .read_request_cookies(RequestCookies::new(header))
            .into_found()?;
        assert_eq!(eids.len(), 1, "should return one EID");
        assert_eq!(
            eids[0].source, "liveramp.example.com",
            "should use the source"
        );
        let uid = &eids[0].uids[0];
        assert_eq!(uid.atype, Some(3), "should be person-based");
        Some((uid.id.to_string(), uid.expiry))
    }

    #[test]
    fn reads_the_envelope_in_base64_and_base64url() {
        let provider = provider(&json!({}));
        let body = json!({ "envelope": "AjfowMv4ZHZQJFM8TpiUnYEyA81Vdgg", "ts": 1 });

        for engine in [STANDARD, URL_SAFE_NO_PAD] {
            let (id, expiry) = read(&*provider, &format!("_lr_env={}", encoded(&body, &engine)))
                .expect("should read the envelope");

            assert_eq!(
                id, "AjfowMv4ZHZQJFM8TpiUnYEyA81Vdgg",
                "should keep the envelope"
            );
            assert!(
                matches!(expiry, ProviderExpiry::After(after) if after == ENVELOPE_TTL),
                "should keep for 15 days"
            );
        }
    }

    #[test]
    fn collects_envelopes_up_to_the_uid_length_limit_and_drops_longer_ones() {
        let provider = provider(&json!({}));

        for len in [703, 1000, MAX_STORED_UID_LENGTH] {
            let raw = "A".repeat(len);
            let ats = encoded(&json!({ "envelope": raw }), &STANDARD);
            for header in [format!("idl_env={raw}"), format!("_lr_env={ats}")] {
                let (id, _) = read(&*provider, &header).expect("should read the envelope");

                assert_eq!(id.len(), len, "should keep the whole envelope");
                assert!(
                    UidValue::try_from(id).is_ok(),
                    "should pass validation at {len} bytes"
                );
            }
        }

        let (id, _) = read(
            &*provider,
            &format!("idl_env={}", "A".repeat(MAX_STORED_UID_LENGTH + 1)),
        )
        .expect("should read the oversized envelope");
        assert!(
            matches!(UidValue::try_from(id), Err(EidValidationError::UidTooLong)),
            "should drop an envelope over the UID length limit in validation"
        );
    }

    #[test]
    fn reads_a_bare_prebid_idl_env_envelope() {
        let provider = provider(&json!({}));

        let (id, _) = read(&*provider, "idl_env=AjfowMv4ZHZQJFM8TpiUnYEyA81Vdgg")
            .expect("should read the bare envelope");

        assert_eq!(
            id, "AjfowMv4ZHZQJFM8TpiUnYEyA81Vdgg",
            "should keep the bare envelope"
        );
    }

    #[test]
    fn prefers_lr_env_over_idl_env() {
        let provider = provider(&json!({}));
        let value = encoded(&json!({ "envelope": "env-ats" }), &STANDARD);

        let read = read(
            &*provider,
            &format!("idl_env=envPrebidExample; _lr_env={value}"),
        );

        assert_eq!(
            read.map(|(id, _)| id).as_deref(),
            Some("env-ats"),
            "should prefer the ATS cookie"
        );
    }

    #[test]
    fn reads_a_configured_cookie_name() {
        let provider = provider(&json!({ "cookie_names": ["example_lockr_env"] }));
        let value = encoded(&json!({ "envelope": "env-example" }), &STANDARD);

        let read = read(
            &*provider,
            &format!("_lr_env=ignored; example_lockr_env={value}"),
        );

        assert_eq!(
            read.map(|(id, _)| id).as_deref(),
            Some("env-example"),
            "should read it"
        );
    }

    #[test]
    fn ignores_lockr_cookies_while_lockr_is_disabled() {
        let provider = provider(&json!({
            "lockr": { "cookie_names": ["example_lockr_env"] },
        }));
        let value = encoded(&json!({ "envelope": "env-example" }), &STANDARD);

        let read = read(&*provider, &format!("example_lockr_env={value}"));

        assert!(
            read.is_none(),
            "should not read a Lockr cookie when disabled"
        );
    }

    #[test]
    fn reads_a_custom_lockr_cookie_after_the_own_cookie() {
        let provider = provider(&json!({
            "lockr": { "enabled": true, "cookie_names": ["example_lockr_env"] },
        }));
        let own = encoded(&json!({ "envelope": "env-own" }), &STANDARD);
        let lockr = encoded(&json!({ "envelope": "env-lockr" }), &STANDARD);

        let both = read(
            &*provider,
            &format!("example_lockr_env={lockr}; _lr_env={own}"),
        );
        let lockr_only = read(&*provider, &format!("example_lockr_env={lockr}"));

        assert_eq!(
            both.map(|(id, _)| id).as_deref(),
            Some("env-own"),
            "should prefer the own cookie"
        );
        assert_eq!(
            lockr_only.map(|(id, _)| id).as_deref(),
            Some("env-lockr"),
            "should fall back to the Lockr cookie"
        );
    }

    #[test]
    fn ignores_missing_malformed_and_opt_out_cookies() {
        let provider = provider(&json!({}));
        for header in [
            String::new(),
            "_lr_env=not-base64!".to_owned(),
            "idl_env=0".to_owned(),
            "idl_env=".to_owned(),
            format!("_lr_env={}", encoded(&json!({ "other": "x" }), &STANDARD)),
            format!("_lr_env={}", encoded(&json!({ "envelope": 5 }), &STANDARD)),
            format!("_lr_env={}", encoded(&json!({ "envelope": "" }), &STANDARD)),
            format!(
                "_lr_env={}",
                encoded(&json!({ "envelope": "0" }), &STANDARD)
            ),
        ] {
            assert!(
                read(&*provider, &header).is_none(),
                "should ignore {header:?}"
            );
        }
    }

    #[test]
    fn requires_exactly_one_source_and_a_cookie_name() {
        for (sources, settings) in [
            (json!([]), json!({})),
            (json!(["a.example.com", "b.example.com"]), json!({})),
            (json!(["a.example.com"]), json!({ "cookie_names": [] })),
        ] {
            let config: IdentityProviderConfig = serde_json::from_value(json!({
                "id": "liveramp",
                "kind": "liveramp",
                "sources": sources,
                "settings": settings,
            }))
            .expect("should parse provider config");

            let error = build_kind::<LiveRamp>(&config, &ProviderSecrets::default())
                .err()
                .expect("should reject config");

            assert!(
                matches!(error.current_context(), ProviderConfigError::Invalid),
                "should report invalid config"
            );
        }
    }

    #[test]
    fn makes_no_lookup_without_an_endpoint() {
        let provider = provider(&json!({}));
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: None,
                user_agent: None,
                referer: None,
                accept_language: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::empty(),
            cookies: RequestCookies::default(),
            hashed_email: None,
            now: trusted_server_core::ec::eid_types::UnixSeconds::new(1_700_000_000),
        };

        assert!(
            provider.build_request(&lookup).is_err(),
            "should build no request"
        );
    }

    const ENDPOINT: &str = "https://envelope.example.com/api/identity/v2/envelope";
    const NOW: u64 = 1_700_000_000;

    fn lookup_config(settings: &serde_json::Value) -> IdentityProviderConfig {
        serde_json::from_value(json!({
            "id": "liveramp",
            "kind": "liveramp",
            "endpoint": ENDPOINT,
            "sources": ["liveramp.example.com"],
            "settings": settings,
        }))
        .expect("should parse provider config")
    }

    fn lookup_provider() -> Box<dyn IdentityProvider> {
        build_kind::<LiveRamp>(
            &lookup_config(&json!({
                "placement_id": 14,
                "hashed_email": { "enabled": true, "path": ".liveramp" },
            })),
            &ProviderSecrets::default(),
        )
        .expect("should build provider")
    }

    fn hashed_email() -> HashedEmail {
        HashedEmail::new("b4c9a289323b21a01c3e940f150eb9b8c542587f1abfd8f0e1cc1ffc5e475514")
            .expect("should accept the hashed email")
    }

    fn input<'a>(
        consent: &'a ConsentContext,
        ip: Option<&str>,
        hashed_email: Option<&'a HashedEmail>,
    ) -> LookupInput<'a> {
        LookupInput {
            ec_id: "abc123.suffix",
            consent,
            client: ClientSignals {
                ip: ip.map(|ip| ip.parse().expect("should parse ip")),
                user_agent: None,
                referer: None,
                accept_language: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::empty(),
            cookies: RequestCookies::default(),
            hashed_email,
            now: UnixSeconds::new(NOW),
        }
    }

    /// A row holding `envelope` for the `LiveRamp` source, written by the
    /// provider, with refresh state due at `refresh_from`.
    fn stored(envelope: &str, expires: u64, refresh_from: Option<u64>) -> KvEntry {
        let mut uid = KvUid::new(
            UidValue::try_from(envelope.to_owned()).expect("should build uid"),
            AgentType::try_from(3).expect("should build atype"),
            None,
            Some(ProviderId::new("liveramp")),
            Expiry::At(UnixSeconds::new(expires)),
        );
        if let Some(from) = refresh_from {
            uid = uid.with_refresh(KvRefresh {
                state: REFRESH_STATE.to_owned(),
                from: UnixSeconds::new(from),
            });
        }
        // The record is built while the UID was still live.
        let record = KvPartnerId::from_eids(
            vec![KvEid::new(EidFields::default(), vec![uid])],
            UnixSeconds::new(expires - 2),
        )
        .expect("should keep the uid");
        let mut entry = KvEntry::tombstone(1);
        entry.consent.ok = true;
        entry.ids.insert("liveramp.example.com".to_owned(), record);
        entry
    }

    fn sources() -> [SourceDomain; 1] {
        [SourceDomain::try_from("liveramp.example.com").expect("should build source")]
    }

    fn query(request: &ProviderRequest) -> Vec<(String, String)> {
        request
            .url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn looks_up_an_envelope_by_hashed_email() {
        // Arrange
        let provider = lookup_provider();
        let consent = ConsentContext {
            raw_tc_string: Some("CP_example".to_owned()),
            raw_gpp_string: Some("DBA_example".to_owned()),
            gpp_section_ids: Some(vec![2, 7]),
            ..ConsentContext::default()
        };
        let hashed_email = hashed_email();

        // Act
        let request = provider
            .build_request(&input(&consent, Some("11.22.33.44"), Some(&hashed_email)))
            .expect("should build request");

        // Assert
        assert_eq!(request.method, Method::GET, "should get");
        assert_eq!(
            request.url.path(),
            "/api/identity/v2/envelope",
            "should call the configured endpoint"
        );
        assert_eq!(
            query(&request),
            pairs(&[
                ("pid", "14"),
                ("it", "4"),
                ("iv", hashed_email.as_str()),
                ("atype", "3"),
                ("ct", "4"),
                ("cv", "CP_example"),
                ("gpp", "DBA_example"),
                ("gpp_sid", "2"),
            ]),
            "should send the hashed email as given, with consent"
        );
        assert!(
            !request.url.as_str().contains("abc123"),
            "should never send the EC ID"
        );
        assert_eq!(request.context, None, "should mark no refresh");
        assert_eq!(
            request.headers.get(ORIGIN).map(HeaderValue::as_bytes),
            Some(&b"https://www.example.com"[..]),
            "should send the publisher origin"
        );
        assert_eq!(
            request
                .headers
                .get("x-forwarded-for")
                .map(HeaderValue::as_bytes),
            Some(&b"11.22.33.44"[..]),
            "should forward the visitor IP"
        );
    }

    #[test]
    fn refreshes_a_due_envelope_without_a_hashed_email() {
        // Arrange
        let provider = lookup_provider();
        let consent = ConsentContext::default();
        let entry = stored("AhStoredEnvelope", NOW + 3600, Some(NOW - 1));
        let sources = sources();
        let owner = ProviderId::new("liveramp");
        let hashed_email = hashed_email();

        for hashed_email in [None, Some(&hashed_email)] {
            let lookup = LookupInput {
                due_sources: &sources,
                current: OwnedEntries::new(Some(&entry), &sources, &owner),
                ..input(&consent, Some("11.22.33.44"), hashed_email)
            };

            // Act
            let request = provider
                .build_request(&lookup)
                .expect("should build request");

            // Assert
            assert_eq!(
                request.url.path(),
                "/api/identity/v2/envelope/refresh",
                "should call the refresh API"
            );
            assert_eq!(
                query(&request)[..4],
                pairs(&[
                    ("pid", "14"),
                    ("it", "19"),
                    ("iv", "AhStoredEnvelope"),
                    ("atype", "3"),
                ])[..],
                "should refresh the stored envelope, preferring it over a hashed email"
            );
            assert_eq!(
                request.context.as_deref(),
                Some("refresh:AhStoredEnvelope"),
                "should carry the refreshed envelope as context"
            );
        }
    }

    #[test]
    fn looks_up_by_hashed_email_when_the_stored_envelope_is_not_refreshable() {
        let provider = lookup_provider();
        let consent = ConsentContext::default();
        let sources = sources();
        let owner = ProviderId::new("liveramp");
        let hashed_email = hashed_email();

        // An expired envelope and one read from a cookie (no refresh state).
        for entry in [
            stored("AhExpired", NOW - 1, Some(NOW - 10)),
            stored("AhFromCookie", NOW + 3600, None),
        ] {
            let lookup = LookupInput {
                due_sources: &sources,
                current: OwnedEntries::new(Some(&entry), &sources, &owner),
                ..input(&consent, Some("11.22.33.44"), Some(&hashed_email))
            };

            let request = provider
                .build_request(&lookup)
                .expect("should build request");

            assert_eq!(
                request.url.path(),
                "/api/identity/v2/envelope",
                "should look up by hashed email"
            );
        }
    }

    #[test]
    fn builds_no_request_without_a_hashed_email_or_a_due_envelope() {
        let consent = ConsentContext::default();

        assert!(
            lookup_provider()
                .build_request(&input(&consent, Some("11.22.33.44"), None))
                .is_err(),
            "should not look up without a hashed email"
        );
    }

    #[test]
    fn sends_us_privacy_when_there_is_no_tcf_string() {
        let provider = lookup_provider();
        let consent = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            ..ConsentContext::default()
        };
        let hashed_email = hashed_email();

        let request = provider
            .build_request(&input(&consent, Some("11.22.33.44"), Some(&hashed_email)))
            .expect("should build request");

        let query = query(&request);
        assert!(
            query.contains(&("ct".to_owned(), "3".to_owned()))
                && query.contains(&("cv".to_owned(), "1YNN".to_owned())),
            "should send the US Privacy string"
        );
    }

    #[test]
    fn needs_a_routable_client_ip() {
        let provider = lookup_provider();
        let consent = ConsentContext::default();
        let hashed_email = hashed_email();

        for ip in [None, Some("10.0.0.1")] {
            assert!(
                provider
                    .build_request(&input(&consent, ip, Some(&hashed_email)))
                    .is_err(),
                "should not look up without a routable IP"
            );
        }
    }

    #[test]
    fn reads_its_hashed_email_path_and_sends_no_ec_id() {
        let lookup = lookup_provider();
        let status = json!({ "liveramp": "hash-example" });

        assert_eq!(
            lookup
                .hashed_email_path()
                .and_then(|path| path.read(&status)),
            Some("hash-example"),
            "should expose the configured path"
        );
        assert!(!lookup.sends_ec_id(), "should never send the EC ID");
        assert_eq!(
            lookup.capabilities().sources[0].input,
            SourceInput::HashedEmail,
            "should need a hashed email with an endpoint"
        );
        let cookie_only = provider(&json!({}));
        assert!(
            cookie_only.hashed_email_path().is_none(),
            "should read no hashed email without an endpoint"
        );
        assert_eq!(
            cookie_only.capabilities().sources[0].input,
            SourceInput::Anonymous,
            "should need no input without an endpoint"
        );
    }

    fn reply_with<'body>(
        status: StatusCode,
        body: &'body [u8],
        context: Option<&'body str>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        static HEADERS: std::sync::LazyLock<HeaderMap> = std::sync::LazyLock::new(HeaderMap::new);
        lookup_provider().parse_response(ProviderResponse {
            status,
            headers: &HEADERS,
            body,
            context,
        })
    }

    fn reply(status: StatusCode, body: &[u8]) -> Result<LookupOutcome<'_>, Report<ProviderError>> {
        reply_with(status, body, None)
    }

    #[test]
    fn stores_the_returned_envelope_with_refresh_state() {
        let body = json!({"envelopes": [
            {"type": 25, "source": "pairIds", "value": "pair-example", "err": null},
            {"type": 19, "source": "envelopeLiveramp", "value": "AhIdEnvelope", "err": null},
        ]})
        .to_string();

        for context in [None, Some("refresh:AhOldEnvelope")] {
            let outcome =
                reply_with(StatusCode::OK, body.as_bytes(), context).expect("should parse reply");

            let LookupOutcome::Found(eids) = outcome else {
                panic!("should find an envelope");
            };
            assert_eq!(
                eids[0].source, "liveramp.example.com",
                "should use the source"
            );
            let uid = &eids[0].uids[0];
            assert_eq!(uid.id, "AhIdEnvelope", "should store the envelope");
            assert_eq!(uid.atype, Some(3), "should be person-based");
            assert!(
                matches!(uid.expiry, ProviderExpiry::After(ttl) if ttl == ENVELOPE_TTL),
                "should keep the envelope for the envelope TTL"
            );
            let refresh = uid.refresh.as_ref().expect("should keep refresh state");
            assert_eq!(refresh.state, REFRESH_STATE, "should mark it refreshable");
            assert!(
                matches!(refresh.from, ProviderExpiry::After(after)
                    if after == Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS)),
                "should refresh after the default interval"
            );
        }
    }

    #[test]
    fn maps_no_content_by_api_and_a_blank_envelope_to_no_match() {
        let blank = json!({"envelopes": [{"type": 19, "value": "", "err": null}]}).to_string();
        let other = json!({"envelopes": [{"type": 25, "value": "pair-example"}]}).to_string();
        let untyped = json!({"envelopes": [
            {"value": "untyped-example"},
            {"type": 19, "value": "rampid-example"},
        ]})
        .to_string();

        assert!(
            matches!(
                reply(StatusCode::OK, untyped.as_bytes()),
                Ok(LookupOutcome::Found(eids)) if eids[0].uids[0].id == "rampid-example"
            ),
            "should skip an untyped entry and keep the RampID envelope"
        );
        assert!(
            matches!(
                reply(StatusCode::OK, other.as_bytes()),
                Ok(LookupOutcome::NoMatch)
            ),
            "should report no match without a RampID envelope"
        );
        assert!(
            matches!(
                reply(StatusCode::NO_CONTENT, b""),
                Ok(LookupOutcome::OptedOut)
            ),
            "should report an opt-out from the envelope API"
        );
        let Ok(LookupOutcome::Found(rejected)) =
            reply_with(StatusCode::NO_CONTENT, b"", Some("refresh:AhOldEnvelope"))
        else {
            panic!("should keep the envelope the refresh API rejects");
        };
        let uid = &rejected[0].uids[0];
        assert_eq!(uid.id, "AhOldEnvelope", "should keep the stored envelope");
        assert!(
            uid.refresh.is_none(),
            "should drop its refresh state so it is not refreshed again"
        );
        assert!(
            matches!(uid.expiry, ProviderExpiry::After(after)
                if after == Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS)),
            "should keep it for one more refresh interval"
        );
        assert!(
            matches!(
                reply_with(StatusCode::NO_CONTENT, b"", Some("refresh:")),
                Ok(LookupOutcome::NoMatch)
            ),
            "should report no match without a usable envelope"
        );
        assert!(
            matches!(
                reply(StatusCode::OK, blank.as_bytes()),
                Ok(LookupOutcome::NoMatch)
            ),
            "should report no match"
        );
    }

    #[test]
    fn rejects_an_error_status_and_a_malformed_body() {
        for (status, body) in [
            (StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, &b"{}"[..]),
            (StatusCode::UNAUTHORIZED, &b""[..]),
            (StatusCode::OK, &b"not json"[..]),
        ] {
            assert!(reply(status, body).is_err(), "should reject {status}");
        }
    }

    #[test]
    fn rejects_incomplete_lookup_settings() {
        let hashed_email = json!({ "enabled": true, "path": ".liveramp" });
        for (endpoint, settings) in [
            (true, json!({ "placement_id": 14 })),
            (true, json!({ "hashed_email": hashed_email })),
            (
                true,
                json!({ "placement_id": 14, "hashed_email": { "enabled": true } }),
            ),
            (
                true,
                json!({ "placement_id": 14, "hashed_email": { "enabled": true, "path": "liveramp" } }),
            ),
            (
                true,
                json!({ "placement_id": 14, "custom_id_namespace": "ana-example", "hashed_email": hashed_email }),
            ),
            (
                true,
                json!({ "placement_id": 14, "hashed_email": hashed_email, "refresh_interval_secs": 59 }),
            ),
            (
                true,
                json!({ "placement_id": 14, "hashed_email": hashed_email, "refresh_interval_secs": ENVELOPE_TTL.as_secs() }),
            ),
            (
                false,
                json!({ "placement_id": 14, "hashed_email": hashed_email }),
            ),
            (false, json!({ "refresh_interval_secs": 0 })),
        ] {
            let mut config = lookup_config(&settings);
            if !endpoint {
                config.endpoint = None;
            }

            assert!(
                build_kind::<LiveRamp>(&config, &ProviderSecrets::default()).is_err(),
                "should reject {settings} (endpoint: {endpoint})"
            );
        }
    }

    #[test]
    fn accepts_a_custom_refresh_interval() {
        let config = lookup_config(&json!({
            "placement_id": 14,
            "hashed_email": { "enabled": true, "path": ".liveramp" },
            "refresh_interval_secs": 3600,
        }));
        let provider = build_kind::<LiveRamp>(&config, &ProviderSecrets::default())
            .expect("should build provider");
        let body = json!({"envelopes": [{"type": 19, "value": "AhIdEnvelope"}]}).to_string();
        static HEADERS: std::sync::LazyLock<HeaderMap> = std::sync::LazyLock::new(HeaderMap::new);

        let outcome = provider
            .parse_response(ProviderResponse {
                status: StatusCode::OK,
                headers: &HEADERS,
                body: body.as_bytes(),
                context: None,
            })
            .expect("should parse reply");

        let LookupOutcome::Found(eids) = outcome else {
            panic!("should find an envelope");
        };
        assert!(
            matches!(
                eids[0].uids[0].refresh.as_ref().map(|refresh| refresh.from),
                Some(ProviderExpiry::After(after)) if after == Duration::from_secs(3600)
            ),
            "should refresh after the configured interval"
        );
    }
}

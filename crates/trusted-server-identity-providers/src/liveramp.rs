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
//! `/api/identity/v2/envelope`), a visitor without an envelope cookie is looked up server side by Custom ID (`it=15`): the
//! stable part of the EC ID, sent as `iv=<custom_id_namespace>:<id>` with
//! the placement ID, consent strings, the publisher `Origin` and the
//! visitor's IP in `X-Forwarded-For`. `LiveRamp` must enable Custom ID for
//! the placement. A `204` reply reports an opt-out. Without an `endpoint`
//! no server call is made.
//!
//! With the Lockr capability enabled (`[settings.lockr]`), the cookies Lockr
//! AIM writes the envelope into (default key `_lr_env`) are read after the
//! own cookies, with the same parser.
//!
//! Parsing is lenient: a value outside the base64 alphabet, a JSON body
//! without a string `envelope`, or a blank or opt-out envelope reads as no ID.

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
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderError, ProviderKind, ProviderRequest, ProviderResponse,
    RequestCookies,
};

use crate::common::{
    CookieReader, LockrSettings, ReadsLockr, bounded_expiry, cookie_capabilities, cookie_eid,
    is_routable_ip, is_usable_id, join_section_ids, percent_decoded,
};

/// `it` value of a Custom ID lookup.
const CUSTOM_ID_TYPE: &str = "15";
/// `atype` value of an ATS API integration.
const ATS_API: &str = "2";
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
    /// `LiveRamp`-assigned namespace of the Custom ID (the `ANA_ID` part of
    /// `iv`); required with an `endpoint`.
    #[serde(default)]
    pub custom_id_namespace: Option<String>,
}

impl LiveRampSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["_lr_env".to_owned(), "idl_env".to_owned()]
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

/// A configured Custom ID envelope lookup.
#[derive(Debug)]
struct EnvelopeLookup {
    endpoint: Url,
    placement_id: u64,
    custom_id_namespace: String,
}

impl ReadsLockr for LiveRamp {
    const LOCKR_COOKIE_NAMES: &'static [&'static str] = &["_lr_env"];
}

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
        let (capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        let lookup = match (
            common.endpoint,
            settings.placement_id,
            settings.custom_id_namespace,
        ) {
            (None, _, _) => None,
            (Some(endpoint), Some(placement_id), Some(custom_id_namespace))
                if !custom_id_namespace.trim().is_empty() =>
            {
                Some(EnvelopeLookup {
                    endpoint: endpoint.clone(),
                    placement_id,
                    custom_id_namespace,
                })
            }
            (Some(_), _, _) => {
                return Err(Report::new(ProviderConfigError::Invalid)
                    .attach("a liveramp endpoint needs placement_id and custom_id_namespace"));
            }
        };
        Ok(LiveRampProvider {
            capabilities,
            source,
            cookies,
            lookup,
        })
    }
}

#[derive(Deserialize)]
struct EnvelopeCookie {
    envelope: String,
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
        let stable_id = lookup.ec_id.split('.').next().unwrap_or(lookup.ec_id);
        let consent = lookup.consent;
        let mut url = envelope_lookup.endpoint.clone();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("pid", &envelope_lookup.placement_id.to_string())
                .append_pair("it", CUSTOM_ID_TYPE)
                .append_pair(
                    "iv",
                    &format!("{}:{stable_id}", envelope_lookup.custom_id_namespace),
                )
                .append_pair("atype", ATS_API);
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
            if let Some(section_ids) = &consent.gpp_section_ids {
                query.append_pair("gpp_sid", &join_section_ids(section_ids));
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
            context: None,
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        if response.status == StatusCode::NO_CONTENT {
            return Ok(LookupOutcome::OptedOut);
        }
        if !response.status.is_success() {
            return Err(Report::new(ProviderError::InvalidResponse)
                .attach(format!("liveramp returned status {}", response.status)));
        }
        let reply: EnvelopeCookie =
            serde_json::from_slice(response.body).change_context(ProviderError::InvalidResponse)?;
        if !is_usable_id(&reply.envelope) {
            return Ok(LookupOutcome::NoMatch);
        }
        Ok(LookupOutcome::Found(vec![cookie_eid(
            &self.source,
            Cow::Owned(reply.envelope),
            bounded_expiry(Some(ENVELOPE_TTL)),
        )]))
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
    use trusted_server_core::ec::eid_types::{EidValidationError, UidValue};
    use trusted_server_core::ec::kv_types::MAX_UID_LENGTH;
    use trusted_server_core::ec::provider::{
        ClientSignals, OwnedEntries, ProviderExpiry, build_kind,
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

        for len in [703, 1000, MAX_UID_LENGTH] {
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
            &format!("idl_env={}", "A".repeat(MAX_UID_LENGTH + 1)),
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
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::new(None, &[]),
            cookies: RequestCookies::default(),
            hashed_email: None,
        };

        assert!(
            provider.build_request(&lookup).is_err(),
            "should build no request"
        );
    }

    fn lookup_provider() -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "liveramp",
            "kind": "liveramp",
            "endpoint": "https://envelope.example.com/api/identity/v2/envelope",
            "sources": ["liveramp.example.com"],
            "settings": {"placement_id": 14, "custom_id_namespace": "ana-example"},
        }))
        .expect("should parse provider config");
        build_kind::<LiveRamp>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn input<'a>(consent: &'a ConsentContext, ip: Option<&str>) -> LookupInput<'a> {
        LookupInput {
            ec_id: "abc123.suffix",
            consent,
            client: ClientSignals {
                ip: ip.map(|ip| ip.parse().expect("should parse ip")),
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::new(None, &[]),
            cookies: RequestCookies::default(),
            hashed_email: None,
        }
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
    fn looks_up_an_envelope_by_custom_id() {
        let provider = lookup_provider();
        let consent = ConsentContext {
            raw_tc_string: Some("CP_example".to_owned()),
            raw_gpp_string: Some("DBA_example".to_owned()),
            gpp_section_ids: Some(vec![2, 7]),
            ..ConsentContext::default()
        };

        let request = provider
            .build_request(&input(&consent, Some("11.22.33.44")))
            .expect("should build request");

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
                ("it", "15"),
                ("iv", "ana-example:abc123"),
                ("atype", "2"),
                ("ct", "4"),
                ("cv", "CP_example"),
                ("gpp", "DBA_example"),
                ("gpp_sid", "2,7"),
            ]),
            "should send the stable EC ID as the Custom ID with consent"
        );
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
    fn sends_us_privacy_when_there_is_no_tcf_string() {
        let provider = lookup_provider();
        let consent = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            ..ConsentContext::default()
        };

        let request = provider
            .build_request(&input(&consent, Some("11.22.33.44")))
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

        for ip in [None, Some("10.0.0.1")] {
            assert!(
                provider.build_request(&input(&consent, ip)).is_err(),
                "should not look up without a routable IP"
            );
        }
    }

    fn reply(status: StatusCode, body: &[u8]) -> Result<LookupOutcome<'_>, Report<ProviderError>> {
        static HEADERS: std::sync::LazyLock<HeaderMap> = std::sync::LazyLock::new(HeaderMap::new);
        lookup_provider().parse_response(ProviderResponse {
            status,
            headers: &HEADERS,
            body,
            context: None,
        })
    }

    #[test]
    fn stores_the_returned_envelope() {
        let body = json!({"envelope": "AhIdEnvelope"}).to_string();

        let outcome = reply(StatusCode::OK, body.as_bytes()).expect("should parse reply");

        let LookupOutcome::Found(eids) = outcome else {
            panic!("should find an envelope");
        };
        assert_eq!(
            eids[0].source, "liveramp.example.com",
            "should use the source"
        );
        assert_eq!(
            eids[0].uids[0].id, "AhIdEnvelope",
            "should store the envelope"
        );
        assert_eq!(eids[0].uids[0].atype, Some(3), "should be person-based");
        assert!(
            matches!(eids[0].uids[0].expiry, ProviderExpiry::After(ttl) if ttl == ENVELOPE_TTL),
            "should keep the envelope for the envelope TTL"
        );
    }

    #[test]
    fn maps_no_content_to_an_opt_out_and_a_blank_envelope_to_no_match() {
        let blank = json!({"envelope": ""}).to_string();

        assert!(
            matches!(
                reply(StatusCode::NO_CONTENT, b""),
                Ok(LookupOutcome::OptedOut)
            ),
            "should report an opt-out"
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
    fn an_endpoint_needs_a_placement_and_namespace() {
        for settings in [
            json!({"placement_id": 14}),
            json!({"custom_id_namespace": "ana-example"}),
            json!({"placement_id": 14, "custom_id_namespace": " "}),
        ] {
            let config: IdentityProviderConfig = serde_json::from_value(json!({
                "id": "liveramp",
                "kind": "liveramp",
                "endpoint": "https://envelope.example.com/api/identity/v2/envelope",
                "sources": ["liveramp.example.com"],
                "settings": settings,
            }))
            .expect("should parse provider config");

            assert!(
                build_kind::<LiveRamp>(&config, &ProviderSecrets::default()).is_err(),
                "should reject {settings}"
            );
        }
    }
}

//! `LiveRamp` identity provider: reads the `RampID` envelope from `LiveRamp`'s own
//! first-party cookie.
//!
//! `LiveRamp`'s ATS script stores the envelope in the `_lr_env` cookie as
//! base64 (or base64url) of a JSON object whose `envelope` field holds it.
//! Prebid's `identityLink` ID module stores the envelope it receives bare,
//! under its configured storage name (commonly `idl_env`); a value that does
//! not decode to JSON is taken as such a bare envelope.
//! The envelope is stored unchanged with agent type 3 for
//! [`ENVELOPE_TTL`]. No server call is made: the refresh contract is not
//! confirmed, so the browser keeps refreshing the cookie.
//!
//! With the Lockr capability enabled (`[settings.lockr]`), the cookies Lockr
//! AIM writes the envelope into (default key `_lr_env`) are read after the
//! own cookies, with the same parser.
//!
//! Parsing is lenient: a value outside the base64 alphabet, a JSON body
//! without a string `envelope`, or a blank or opt-out envelope reads as no ID.

use std::borrow::Cow;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    CookieRead, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderError, ProviderKind, ProviderRequest, ProviderResponse,
    RequestCookies,
};

use crate::common::{
    CookieReader, LockrSettings, ReadsLockr, bounded_expiry, cookie_capabilities, cookie_eid,
    is_usable_id, percent_decoded,
};

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
        Ok(LiveRampProvider {
            capabilities,
            source,
            cookies,
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
        _lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        Err(Report::new(ProviderError::Request).attach("liveramp makes no lookup"))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Err(Report::new(ProviderError::InvalidResponse).attach("liveramp makes no lookup"))
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
    fn makes_no_lookup() {
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
}

//! `LiveRamp` identity provider: reads the `RampID` envelope from `LiveRamp`'s own
//! first-party cookie.
//!
//! `LiveRamp`'s ATS script stores the envelope in the `_lr_env` cookie as
//! base64 (or base64url) of a JSON object whose `envelope` field holds it.
//! The envelope is stored unchanged with agent type 3 for
//! [`ENVELOPE_TTL`]. No server call is made: the refresh contract is not
//! confirmed, so the browser keeps refreshing the cookie.
//!
//! Lockr AIM can write the same envelope into a cookie whose name comes
//! from its own configuration; list that name in `cookie_names`.
//!
//! Parsing is lenient: an undecodable cookie, a body without a string
//! `envelope`, or a blank or opt-out envelope reads as no ID.

use std::borrow::Cow;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities, ProviderCommon,
    ProviderConfigError, ProviderEid, ProviderError, ProviderKind, ProviderRequest,
    ProviderResponse, RequestCookies,
};

use crate::common::{
    bounded_expiry, cookie_capabilities, cookie_eid, is_usable_id, percent_decoded,
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
}

impl LiveRampSettings {
    fn default_cookie_names() -> Vec<String> {
        vec!["_lr_env".to_owned()]
    }
}

/// A built `LiveRamp` provider.
#[derive(Debug)]
pub struct LiveRampProvider {
    capabilities: ProviderCapabilities,
    source: SourceDomain,
    cookie_names: Box<[String]>,
}

impl ProviderKind for LiveRamp {
    const KIND: &'static str = "liveramp";
    type Settings = LiveRampSettings;
    type Provider = LiveRampProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: LiveRampSettings,
    ) -> Result<LiveRampProvider, Report<ProviderConfigError>> {
        if settings.cookie_names.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("a liveramp provider needs at least one cookie name"));
        }
        let (capabilities, source) = cookie_capabilities(common, Self::KIND)?;
        Ok(LiveRampProvider {
            capabilities,
            source,
            cookie_names: settings.cookie_names.into_boxed_slice(),
        })
    }
}

#[derive(Deserialize)]
struct EnvelopeCookie {
    envelope: String,
}

/// Decodes one `_lr_env` value into its envelope.
fn envelope(value: &str) -> Option<String> {
    let value = percent_decoded(value);
    let value = value.trim();
    let bytes = [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(value).ok())?;
    let cookie: EnvelopeCookie = serde_json::from_slice(&bytes).ok()?;
    is_usable_id(&cookie.envelope).then_some(cookie.envelope)
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
    ) -> Option<Vec<ProviderEid<'cookie>>> {
        let envelope = self
            .cookie_names
            .iter()
            .filter_map(|name| cookies.get(name))
            .find_map(envelope)?;
        Some(vec![cookie_eid(
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
        let eids = provider.read_request_cookies(RequestCookies::new(header))?;
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
    fn ignores_missing_malformed_and_opt_out_cookies() {
        let provider = provider(&json!({}));
        for header in [
            String::new(),
            "_lr_env=not-base64!".to_owned(),
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
            hashed_email: None,
        };

        assert!(
            provider.build_request(&lookup).is_err(),
            "should build no request"
        );
    }
}

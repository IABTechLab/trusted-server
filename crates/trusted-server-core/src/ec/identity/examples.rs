//! Fictional providers from spec 4.6, for tests of registration, coordination
//! and capture.

use std::time::Duration;

use error_stack::Report;
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use validator::Validate;

use super::descriptor::{Captures, ConsentPolicy, Input, ProviderDescriptor, Resolves};
use super::exchange::{ProviderRequest, ProviderResponse};
use super::ids::UnixTime;
use super::input::{CaptureInput, ResolveInput};
use super::provider::{IdentityProvider, IdentityProviderConfig};
use super::resolution::{IdentityProviderError, Resolution, ResolvedEid, ResolvedUid};
use crate::settings::IntegrationConfig;

/// Integration ID of [`ExampleIdProvider`].
pub(crate) const EXAMPLEID_INTEGRATION_ID: &str = "exampleid";

/// Integration ID of [`ExampleCaptureProvider`].
pub(crate) const EXAMPLECAPTURE_INTEGRATION_ID: &str = "examplecapture";

/// Config of the fictional `exampleid` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub(crate) struct ExampleIdConfig {
    pub enabled: bool,
    #[validate(url)]
    pub endpoint: String,
    pub partner_id: u32,
    pub gvl_id: u16,
}

impl IntegrationConfig for ExampleIdConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for ExampleIdConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(EXAMPLEID_INTEGRATION_ID.parse()?)
            .with_source("exampleid.example.com".parse()?)
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_wants([Input::PageUrl])
                    .with_private_state(),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(self.gvl_id))
            .build()
    }
}

/// A valid [`ExampleIdConfig`].
pub(crate) fn example_config() -> ExampleIdConfig {
    ExampleIdConfig {
        enabled: true,
        endpoint: "https://api.example.com/fetch".to_owned(),
        partner_id: 7,
        gvl_id: 1,
    }
}

/// A resolving provider that round-trips private state.
pub(crate) struct ExampleIdProvider {
    descriptor: ProviderDescriptor,
    partner_id: u32,
}

/// The private state [`ExampleIdProvider`] keeps between calls.
#[derive(Serialize, Deserialize)]
pub(crate) struct ExampleIdState {
    signature: String,
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    ip: String,
    ua: &'a str,
    page: Option<&'a str>,
    gdpr: bool,
    consent: Option<&'a str>,
    signature: Option<String>,
}

#[derive(Deserialize)]
struct FetchReply {
    uid: Option<String>,
    signature: Option<String>,
    link_type: Option<u8>,
    max_age_sec: Option<u64>,
}

impl ExampleIdProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub(crate) fn new(config: &ExampleIdConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
        })
    }
}

impl IdentityProvider for ExampleIdProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let body = FetchBody {
            partner: self.partner_id,
            ip: input.client_ip()?.to_string(),
            ua: input.user_agent()?,
            page: input.page_url().ok().map(Url::as_str),
            gdpr: consent.gdpr_applies(),
            consent: consent.raw_tc_string(),
            signature: input
                .state::<ExampleIdState>()?
                .map(|state| state.signature),
        };
        ProviderRequest::post_json(input.endpoint()?, &body).map(Some)
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let Some(uid) = reply.uid else {
            return Ok(Resolution::no_match());
        };

        let mut uid = ResolvedUid::new(uid).with_atype(1);
        if let Some(link_type) = reply.link_type {
            uid = uid.with_ext(json!({ "linkType": link_type }));
        }
        let mut eid = ResolvedEid::new(input.primary_source(), uid);
        if let Some(max_age) = reply.max_age_sec {
            eid = eid.with_refresh_in(Duration::from_secs(max_age));
        }

        let resolution = Resolution::found([eid]);
        Ok(match reply.signature {
            Some(signature) => resolution.replacing_state(&ExampleIdState { signature }),
            None => resolution,
        })
    }
}

/// A capture-only provider that reads tokens from the API it proxies.
pub(crate) struct ExampleCaptureProvider {
    descriptor: ProviderDescriptor,
}

#[derive(Deserialize)]
struct TokenReply {
    tokens: Vec<Token>,
}

#[derive(Deserialize)]
struct Token {
    source: String,
    value: String,
    expires: u64,
}

impl ExampleCaptureProvider {
    /// Builds the provider with the capture path of spec 4.6.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the descriptor is invalid.
    pub(crate) fn new() -> Result<Self, Report<IdentityProviderError>> {
        Self::with_capture_path("/integrations/examplecapture/api/token")
    }

    /// Builds the provider capturing from `path` instead.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the descriptor is invalid.
    pub(crate) fn with_capture_path(path: &str) -> Result<Self, Report<IdentityProviderError>> {
        let descriptor = ProviderDescriptor::builder(EXAMPLECAPTURE_INTEGRATION_ID.parse()?)
            .with_source("examplecapture.example.com".parse()?)
            .with_captures(Captures::new().with_path(Method::POST, path))
            .build()?;
        Ok(Self { descriptor })
    }
}

impl IdentityProvider for ExampleCaptureProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        Ok(Resolution::found(reply.tokens.into_iter().filter_map(
            |token| {
                let source = input.source_named(&token.source)?;
                Some(
                    ResolvedEid::new(source, ResolvedUid::new(token.value))
                        .with_expires_at(UnixTime::from_secs(token.expires)),
                )
            },
        )))
    }
}

//! `UID2` identity provider: generates and refreshes `UID2` tokens server
//! side.
//!
//! A first token is generated from the hashed email in the `ts-hem` cookie,
//! sent in an AES-256-GCM envelope keyed by the operator's client secret.
//! The refresh token and the key that decrypts its response are kept as
//! private state, so later visits refresh without the hashed email.

use aes_gcm::aead::{Aead as _, KeyInit as _};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::Report;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolveKind,
    ResolvedEid, ResolvedUid, Resolves, UnixTime,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::redacted::Redacted;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `UID2` provider.
pub(crate) const UID2_INTEGRATION_ID: &str = "uid2";

/// The envelope version byte.
const ENVELOPE_VERSION: u8 = 1;
const IV_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const TIMESTAMP_BYTES: usize = 8;
const NONCE_BYTES: usize = 8;

/// Config of the `uid2` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct Uid2Config {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The operator base URL, for example `https://prod.uidapi.com`.
    #[serde(default = "default_base_url")]
    #[validate(url)]
    pub base_url: String,
    /// The API key. A secret-store key name at rest, the resolved value at
    /// runtime.
    pub api_key: Redacted<String>,
    /// The base64 client secret that keys request envelopes. A secret-store
    /// key name at rest, the resolved value at runtime.
    pub client_secret: Redacted<String>,
    /// The IAB Global Vendor List ID whose consent `UID2` calls require where
    /// TCF applies, if any.
    pub tcf_vendor_id: Option<u16>,
}

fn default_base_url() -> String {
    "https://prod.uidapi.com".to_owned()
}

impl IntegrationConfig for Uid2Config {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for Uid2Config {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        // UID2 has no GVL ID, so it keeps Purpose 4 (personalised ads) as
        // its declared purpose. An operator whose UID2 operator is a TCF
        // vendor names its ID; otherwise purpose consent alone applies.
        let consent = ConsentPolicy::strict().with_tcf_purposes([4]);
        let consent = match self.tcf_vendor_id {
            Some(vendor) => consent.with_tcf_vendor(vendor),
            None => consent.without_tcf_vendor(),
        };
        ProviderDescriptor::builder(UID2_INTEGRATION_ID.parse()?)
            .with_source("uidapi.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.base_url)
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail])
                    .with_private_state(),
            )
            .with_consent_policy(consent)
            .build()
    }
}

/// The `UID2` provider.
pub struct Uid2Provider {
    descriptor: ProviderDescriptor,
    api_key: Redacted<String>,
    client_key: [u8; 32],
}

impl Uid2Provider {
    /// Builds the provider from its config with resolved secrets.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the client secret is
    /// not 32 base64 bytes, or the config cannot describe a valid provider.
    pub fn new(config: &Uid2Config) -> Result<Self, Report<IdentityProviderError>> {
        let client_key = STANDARD
            .decode(config.client_secret.expose().trim())
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or_else(|| {
                IdentityProviderError::config("`client_secret` is not 32 base64-encoded bytes")
            })?;
        Ok(Self {
            descriptor: config.build_descriptor()?,
            api_key: config.api_key.clone(),
            client_key,
        })
    }
}

/// The refresh anchor kept between calls.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Uid2State {
    refresh_token: String,
    refresh_response_key: String,
    /// Milliseconds since the epoch.
    refresh_expires: u64,
}

/// The nonce a generate call sent, which its response must echo.
#[derive(Serialize, Deserialize)]
struct GenerateCall {
    request_nonce: [u8; NONCE_BYTES],
}

#[derive(Deserialize)]
struct Uid2Reply {
    status: String,
    body: Option<Uid2Identity>,
}

#[derive(Deserialize)]
struct Uid2Identity {
    advertising_token: String,
    refresh_token: String,
    refresh_response_key: String,
    /// Milliseconds since the epoch, like the two below.
    identity_expires: u64,
    refresh_from: u64,
    refresh_expires: u64,
}

/// `path` under `base`, keeping a `base` path that has no trailing slash.
fn join(base: &Url, path: &str) -> Result<Url, Report<IdentityProviderError>> {
    let mut base = base.clone();
    if !base.path().ends_with('/') {
        let directory = format!("{}/", base.path());
        base.set_path(&directory);
    }
    base.join(path)
        .map_err(|_| IdentityProviderError::config("`base_url` cannot take a path"))
}

/// AES-256-GCM of `plaintext` under `key`, as `ciphertext || tag`.
fn seal(
    key: &[u8; 32],
    iv: &[u8; IV_BYTES],
    plaintext: &[u8],
) -> Result<Vec<u8>, Report<IdentityProviderError>> {
    Aes256Gcm::new(key.into())
        .encrypt(Nonce::from_slice(iv), plaintext)
        .map_err(|_| IdentityProviderError::config("envelope encryption failed"))
}

/// Opens base64 `IV || ciphertext || tag` under `key`.
fn open(key: &[u8], body: &str) -> Result<Vec<u8>, Report<IdentityProviderError>> {
    let bytes = STANDARD
        .decode(body.trim())
        .map_err(|_| IdentityProviderError::malformed())?;
    if bytes.len() < IV_BYTES + TAG_BYTES {
        return Err(IdentityProviderError::malformed());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| IdentityProviderError::malformed())?;
    let (iv, sealed) = bytes.split_at(IV_BYTES);
    cipher
        .decrypt(Nonce::from_slice(iv), sealed)
        .map_err(|_| IdentityProviderError::malformed())
}

/// The token-generate envelope: base64 of the version byte, the IV, and the
/// sealed timestamp, nonce and JSON payload.
fn encrypt_envelope(
    key: &[u8; 32],
    iv: &[u8; IV_BYTES],
    now: UnixTime,
    nonce: &[u8; NONCE_BYTES],
    payload: &serde_json::Value,
) -> Result<String, Report<IdentityProviderError>> {
    let mut plaintext = Vec::with_capacity(TIMESTAMP_BYTES + NONCE_BYTES + 64);
    plaintext.extend_from_slice(&now.as_secs().saturating_mul(1000).to_be_bytes());
    plaintext.extend_from_slice(nonce);
    plaintext.extend_from_slice(payload.to_string().as_bytes());
    let sealed = seal(key, iv, &plaintext)?;
    let mut envelope = Vec::with_capacity(1 + IV_BYTES + sealed.len());
    envelope.push(ENVELOPE_VERSION);
    envelope.extend_from_slice(iv);
    envelope.extend_from_slice(&sealed);
    Ok(STANDARD.encode(envelope))
}

/// Opens a generate response and checks that it echoes `nonce`.
fn decrypt_generate(
    key: &[u8; 32],
    body: &str,
    nonce: &[u8; NONCE_BYTES],
) -> Result<Uid2Reply, Report<IdentityProviderError>> {
    let plaintext = open(key, body)?;
    let payload = plaintext
        .get(TIMESTAMP_BYTES..)
        .and_then(|rest| rest.strip_prefix(nonce.as_slice()))
        .ok_or_else(IdentityProviderError::malformed)?;
    serde_json::from_slice(payload).map_err(|_| IdentityProviderError::malformed())
}

/// Opens a refresh response with the stored base64 response key.
fn decrypt_refresh(key: &str, body: &str) -> Result<Uid2Reply, Report<IdentityProviderError>> {
    let key = STANDARD
        .decode(key)
        .map_err(|_| IdentityProviderError::malformed())?;
    let plaintext = open(&key, body)?;
    serde_json::from_slice(&plaintext).map_err(|_| IdentityProviderError::malformed())
}

impl IdentityProvider for Uid2Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let base = input.endpoint()?;
        if input.is_refresh(input.primary_source()) {
            let Some(state) = input.state::<Uid2State>()? else {
                return Ok(None);
            };
            if UnixTime::from_millis(state.refresh_expires) <= input.now() {
                return Ok(None);
            }
            let url = join(base, "v2/token/refresh")?;
            return Ok(Some(
                ProviderRequest::post_text(&url, &state.refresh_token).with_bearer(&self.api_key),
            ));
        }
        let url = join(base, "v2/token/generate")?;
        let request_nonce = input.random_bytes::<NONCE_BYTES>();
        let envelope = encrypt_envelope(
            &self.client_key,
            &input.random_bytes::<IV_BYTES>(),
            input.now(),
            &request_nonce,
            &json!({ "email_hash": input.hashed_email()?.to_base64(), "optout_check": 1 }),
        )?;
        Ok(Some(
            ProviderRequest::post_text(&url, &envelope)
                .with_bearer(&self.api_key)
                .try_with_call_context(&GenerateCall { request_nonce })?,
        ))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let body = response.success_text()?;
        let reply = match response.call_context::<GenerateCall>()? {
            Some(call) => decrypt_generate(&self.client_key, body, &call.request_nonce)?,
            None => {
                let state = input
                    .state::<Uid2State>()?
                    .ok_or_else(|| IdentityProviderError::config("refresh without state"))?;
                decrypt_refresh(&state.refresh_response_key, body)?
            }
        };
        match reply.status.as_str() {
            "optout" => Ok(Resolution::opted_out().clearing_state()),
            "expired_token" | "invalid_token" => {
                Ok(Resolution::invalidate(input.primary_source()).clearing_state())
            }
            "success" => {
                let identity = reply.body.ok_or_else(IdentityProviderError::malformed)?;
                let uid = ResolvedUid::new(identity.advertising_token).with_atype(3);
                let eid = ResolvedEid::new(input.primary_source(), uid)
                    .with_expires_at(UnixTime::from_millis(identity.identity_expires))
                    .with_refresh_at(UnixTime::from_millis(identity.refresh_from))
                    .with_refresh_until(UnixTime::from_millis(identity.refresh_expires));
                Ok(Resolution::found([eid]).replacing_state(&Uid2State {
                    refresh_token: identity.refresh_token,
                    refresh_response_key: identity.refresh_response_key,
                    refresh_expires: identity.refresh_expires,
                }))
            }
            _ => Err(IdentityProviderError::malformed()),
        }
    }
}

/// Registers the `UID2` provider when `[integrations.uid2]` is enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse, the client secret is
/// invalid, or the config describes an invalid provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<Uid2Config>(UID2_INTEGRATION_ID)? else {
        return Ok(None);
    };
    log::info!("Registering UID2 identity provider");
    Ok(Some(
        IntegrationRegistration::builder(UID2_INTEGRATION_ID)
            .try_with_identity_provider(Uid2Provider::new(&config))?
            .without_js()
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, check_descriptor_ignores_secrets,
    };
    use crate::ec::identity::{HashedEmail, SourceDomain, StateChange, StoredId, When};

    const CLIENT_KEY: [u8; 32] = [7; 32];
    const RESPONSE_KEY: [u8; 32] = [9; 32];

    fn config(api_key: &str, client_secret: &str) -> Uid2Config {
        Uid2Config {
            enabled: true,
            base_url: "https://uid2.example.com".to_owned(),
            api_key: Redacted::new(api_key.to_owned()),
            client_secret: Redacted::new(client_secret.to_owned()),
            tcf_vendor_id: None,
        }
    }

    fn provider() -> Uid2Provider {
        Uid2Provider::new(&config("api-key", &STANDARD.encode(CLIENT_KEY)))
            .expect("should build the provider")
    }

    fn sealed_response(key: &[u8; 32], plaintext: &[u8]) -> String {
        let iv = [3u8; IV_BYTES];
        let mut bytes = iv.to_vec();
        bytes.extend(seal(key, &iv, plaintext).expect("should seal"));
        STANDARD.encode(bytes)
    }

    fn identity() -> serde_json::Value {
        json!({
            "status": "success",
            "body": {
                "advertising_token": "advertising-token",
                "refresh_token": "refresh-token",
                "refresh_response_key": STANDARD.encode(RESPONSE_KEY),
                "identity_expires": 1_760_003_600_000_u64,
                "refresh_from": 1_760_001_800_000_u64,
                "refresh_expires": 1_762_592_000_000_u64
            }
        })
    }

    #[test]
    fn generates_a_token_from_the_hashed_email() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");
        assert_eq!(
            request.url().as_str(),
            "https://uid2.example.com/v2/token/generate",
            "should call token/generate"
        );
        let envelope = STANDARD
            .decode(request.body())
            .expect("should send a base64 envelope");
        assert_eq!(
            envelope[0], ENVELOPE_VERSION,
            "should lead with the version"
        );
        let plaintext = Aes256Gcm::new((&CLIENT_KEY).into())
            .decrypt(
                Nonce::from_slice(&envelope[1..=IV_BYTES]),
                &envelope[1 + IV_BYTES..],
            )
            .expect("should open with the client secret");
        let nonce: [u8; NONCE_BYTES] = plaintext[TIMESTAMP_BYTES..TIMESTAMP_BYTES + NONCE_BYTES]
            .try_into()
            .expect("should hold a nonce");
        let payload: serde_json::Value =
            serde_json::from_slice(&plaintext[TIMESTAMP_BYTES + NONCE_BYTES..])
                .expect("should hold JSON");
        assert_eq!(
            payload["email_hash"],
            STANDARD.encode([0xab; 32]),
            "should send the base64 hash"
        );

        let mut reply = vec![0u8; TIMESTAMP_BYTES];
        reply.extend_from_slice(&nonce);
        reply.extend(identity().to_string().into_bytes());
        let resolution = harness
            .respond_raw(
                StatusCode::OK,
                &[],
                sealed_response(&CLIENT_KEY, &reply).as_bytes(),
            )
            .expect("should decrypt the response");

        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "advertising-token",
            "should store the advertising token"
        );
        let state: Uid2State = harness.state_of(&resolution).expect("should keep state");
        assert_eq!(
            state.refresh_token, "refresh-token",
            "should keep the refresh token"
        );
    }

    #[test]
    fn rejects_a_generate_response_with_another_nonce() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let mut reply = vec![0u8; TIMESTAMP_BYTES + NONCE_BYTES];
        reply.extend(identity().to_string().into_bytes());
        let error = harness
            .respond_raw(
                StatusCode::OK,
                &[],
                sealed_response(&CLIENT_KEY, &reply).as_bytes(),
            )
            .expect_err("should refuse a mismatched nonce");

        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "should be malformed"
        );
    }

    #[test]
    fn refreshes_with_the_stored_state() {
        let provider = provider();
        let source: SourceDomain = "uidapi.com".parse().expect("should parse the source");
        let mut harness = ProviderHarness::new(&provider).with_setup(
            HarnessSetup::default()
                .with_stored(
                    StoredId::for_test(&source, "old-token").expiring_at(UnixTime::from_secs(1)),
                )
                .with_state(&Uid2State {
                    refresh_token: "stored-refresh".to_owned(),
                    refresh_response_key: STANDARD.encode(RESPONSE_KEY),
                    refresh_expires: 1_770_000_000_000,
                }),
        );

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");
        assert_eq!(
            request.body(),
            b"stored-refresh",
            "should send the refresh token"
        );

        let resolution = harness
            .respond_raw(
                StatusCode::OK,
                &[],
                sealed_response(
                    &RESPONSE_KEY,
                    json!({ "status": "optout" }).to_string().as_bytes(),
                )
                .as_bytes(),
            )
            .expect("should decrypt the refresh response");
        assert!(resolution.is_opted_out(), "should report the opt-out");
    }

    fn source() -> SourceDomain {
        "uidapi.com".parse().expect("should parse the source")
    }

    fn state(refresh_expires: u64) -> Uid2State {
        Uid2State {
            refresh_token: "stored-refresh".to_owned(),
            refresh_response_key: STANDARD.encode(RESPONSE_KEY),
            refresh_expires,
        }
    }

    fn refreshing(state: &Uid2State) -> HarnessSetup {
        HarnessSetup::default()
            .with_stored(
                StoredId::for_test(&source(), "old-token").expiring_at(UnixTime::from_secs(1)),
            )
            .with_state(state)
    }

    fn refresh_reply(reply: &serde_json::Value) -> String {
        sealed_response(&RESPONSE_KEY, reply.to_string().as_bytes())
    }

    fn generate_reply(request: &ProviderRequest, reply: &serde_json::Value) -> String {
        let envelope = STANDARD.decode(request.body()).expect("should be base64");
        let plaintext = Aes256Gcm::new((&CLIENT_KEY).into())
            .decrypt(
                Nonce::from_slice(&envelope[1..=IV_BYTES]),
                &envelope[1 + IV_BYTES..],
            )
            .expect("should open");
        let mut sealed = vec![0u8; TIMESTAMP_BYTES];
        sealed.extend_from_slice(&plaintext[TIMESTAMP_BYTES..TIMESTAMP_BYTES + NONCE_BYTES]);
        sealed.extend(reply.to_string().into_bytes());
        sealed_response(&CLIENT_KEY, &sealed)
    }

    fn with_email(provider: &Uid2Provider) -> ProviderHarness<'_> {
        ProviderHarness::new(provider).with_hashed_email(HashedEmail::for_test([0xab; 32]))
    }

    fn requested(harness: &mut ProviderHarness<'_>) -> ProviderRequest {
        harness
            .request()
            .expect("should build a request")
            .expect("should not skip")
    }

    #[test]
    fn skips_a_first_token_without_a_hashed_email() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not call UID2 without a HEM"
        );
    }

    #[test]
    fn generate_sends_the_api_key_as_a_bearer_and_the_time_in_millis() {
        let provider = provider();
        let mut harness = with_email(&provider).with_now(UnixTime::from_secs(1_760_000_000));

        let request = requested(&mut harness);

        assert_eq!(
            request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer api-key"),
            "should authenticate with the API key"
        );
        let envelope = STANDARD.decode(request.body()).expect("should be base64");
        let plaintext = Aes256Gcm::new((&CLIENT_KEY).into())
            .decrypt(
                Nonce::from_slice(&envelope[1..=IV_BYTES]),
                &envelope[1 + IV_BYTES..],
            )
            .expect("should open");
        let millis = u64::from_be_bytes(
            plaintext[..TIMESTAMP_BYTES]
                .try_into()
                .expect("should hold a timestamp"),
        );
        assert_eq!(
            millis, 1_760_000_000_000,
            "should stamp the request in millis"
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&plaintext[TIMESTAMP_BYTES + NONCE_BYTES..]).expect("JSON");
        assert_eq!(
            payload["optout_check"], 1,
            "should ask UID2 to check opt-outs"
        );
    }

    #[test]
    fn each_generate_uses_a_fresh_iv_and_nonce() {
        let provider = provider();
        let mut first = with_email(&provider).with_random_seed(1);
        let mut second = with_email(&provider).with_random_seed(2);

        let a = STANDARD
            .decode(requested(&mut first).body())
            .expect("base64");
        let b = STANDARD
            .decode(requested(&mut second).body())
            .expect("base64");

        assert_ne!(a[1..=IV_BYTES], b[1..=IV_BYTES], "should not reuse an IV");
        assert_ne!(a, b, "should not repeat an envelope");
    }

    #[test]
    fn generate_success_sets_expiry_refresh_and_limit_from_the_identity() {
        let provider = provider();
        let mut harness = with_email(&provider);
        let request = requested(&mut harness);

        let resolution = harness
            .respond_raw(
                StatusCode::OK,
                &[],
                generate_reply(&request, &identity()).as_bytes(),
            )
            .expect("should decrypt");

        let eid = &resolution.eids()[0];
        assert_eq!(eid.source(), &source(), "should write uidapi.com");
        assert_eq!(eid.uids()[0].atype(), Some(3), "should be atype 3");
        assert_eq!(
            eid.expiry(),
            Some(When::At(UnixTime::from_millis(1_760_003_600_000))),
            "should expire at identity_expires"
        );
        assert_eq!(
            eid.refresh(),
            Some(When::At(UnixTime::from_millis(1_760_001_800_000))),
            "should refresh from refresh_from"
        );
        assert_eq!(
            eid.refresh_until(),
            Some(UnixTime::from_millis(1_762_592_000_000)),
            "should stop refreshing at refresh_expires"
        );
    }

    #[test]
    fn generate_opt_out_reports_the_opt_out_and_clears_state() {
        let provider = provider();
        let mut harness = with_email(&provider);
        let request = requested(&mut harness);

        let resolution = harness
            .respond_raw(
                StatusCode::OK,
                &[],
                generate_reply(&request, &json!({ "status": "optout" })).as_bytes(),
            )
            .expect("should decrypt");

        assert!(resolution.is_opted_out(), "should be opted out");
        assert!(
            matches!(resolution.state_change(), StateChange::Clear),
            "should clear state"
        );
        assert!(resolution.eids().is_empty(), "should return no token");
    }

    #[test]
    fn refresh_success_replaces_the_state_with_the_new_refresh_token() {
        let provider = provider();
        let stored = state(1_770_000_000_000);
        let mut harness = ProviderHarness::new(&provider).with_setup(refreshing(&stored));
        let request = requested(&mut harness);
        assert_eq!(
            request.url().as_str(),
            "https://uid2.example.com/v2/token/refresh",
            "should call token/refresh"
        );
        assert!(
            request.headers().get(http::header::AUTHORIZATION).is_some(),
            "should authenticate the refresh"
        );

        let mut reply = identity();
        reply["body"]["refresh_token"] = json!("next-refresh");
        let resolution = harness
            .respond_raw(StatusCode::OK, &[], refresh_reply(&reply).as_bytes())
            .expect("should decrypt with the response key");

        let next: Uid2State = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(
            next.refresh_token, "next-refresh",
            "should rotate the refresh token"
        );
        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "advertising-token",
            "should keep the token"
        );
    }

    #[test]
    fn a_refused_refresh_token_invalidates_and_clears_state() {
        for status in ["expired_token", "invalid_token"] {
            let provider = provider();
            let stored = state(1_770_000_000_000);
            let mut harness = ProviderHarness::new(&provider).with_setup(refreshing(&stored));
            requested(&mut harness);

            let resolution = harness
                .respond_raw(
                    StatusCode::OK,
                    &[],
                    refresh_reply(&json!({ "status": status })).as_bytes(),
                )
                .expect("should decrypt");

            assert_eq!(
                resolution.invalidated(),
                [source()],
                "should drop the token for {status}"
            );
            assert!(
                matches!(resolution.state_change(), StateChange::Clear),
                "should clear state"
            );
        }
    }

    #[test]
    fn skips_a_refresh_whose_refresh_token_has_expired() {
        let provider = provider();
        let stored = state(1_000);
        let mut harness = ProviderHarness::new(&provider)
            .with_setup(refreshing(&stored).with_now(UnixTime::from_secs(1_760_000_000)));

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not send a refresh token UID2 would refuse"
        );
    }

    #[test]
    fn skips_a_refresh_without_state() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_setup(HarnessSetup::default().with_stored(
                StoredId::for_test(&source(), "old-token").expiring_at(UnixTime::from_secs(1)),
            ));

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not refresh without a refresh token"
        );
    }

    #[test]
    fn an_unknown_status_or_a_success_without_body_is_malformed() {
        for reply in [
            json!({ "status": "client_error" }),
            json!({ "status": "success" }),
        ] {
            let provider = provider();
            let stored = state(1_770_000_000_000);
            let mut harness = ProviderHarness::new(&provider).with_setup(refreshing(&stored));
            requested(&mut harness);

            let error = harness
                .respond_raw(StatusCode::OK, &[], refresh_reply(&reply).as_bytes())
                .expect_err("should fail");

            assert!(
                matches!(error.current_context(), IdentityProviderError::Malformed),
                "should be malformed for {reply}: {error:?}"
            );
        }
    }

    #[test]
    fn a_tampered_short_or_non_base64_body_is_malformed() {
        let mut tampered = STANDARD
            .decode(refresh_reply(&identity()))
            .expect("should be base64");
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        let bodies = [
            STANDARD.encode(tampered),
            STANDARD.encode([0u8; IV_BYTES + TAG_BYTES - 1]),
            "%%%".to_owned(),
            sealed_response(&CLIENT_KEY, identity().to_string().as_bytes()),
        ];
        for body in bodies {
            let provider = provider();
            let stored = state(1_770_000_000_000);
            let mut harness = ProviderHarness::new(&provider).with_setup(refreshing(&stored));
            requested(&mut harness);

            let error = harness
                .respond_raw(StatusCode::OK, &[], body.as_bytes())
                .expect_err("should refuse the body");

            assert!(
                matches!(error.current_context(), IdentityProviderError::Malformed),
                "should be malformed: {error:?}"
            );
        }
    }

    #[test]
    fn a_non_success_status_is_rejected_before_decryption() {
        let provider = provider();
        let mut harness = with_email(&provider);
        requested(&mut harness);

        let error = harness
            .respond(
                StatusCode::UNAUTHORIZED,
                &json!({ "status": "unauthorized" }),
            )
            .expect_err("should reject");

        assert!(
            matches!(error.current_context(), IdentityProviderError::Rejected { status } if *status == StatusCode::UNAUTHORIZED),
            "should be rejected: {error:?}"
        );
    }

    #[test]
    fn rejects_a_client_secret_of_the_wrong_length_and_accepts_surrounding_whitespace() {
        assert!(
            Uid2Provider::new(&config("api-key", &STANDARD.encode([1u8; 16]))).is_err(),
            "should refuse a 16-byte key"
        );
        assert!(
            Uid2Provider::new(&config(
                "api-key",
                &format!(" {}\n", STANDARD.encode(CLIENT_KEY))
            ))
            .is_ok(),
            "should trim the secret"
        );
    }

    #[test]
    fn the_config_debug_hides_both_secrets() {
        let config = config("secret-api-key", "secret-client-secret");

        let debug = format!("{config:?}");

        assert!(
            !debug.contains("secret-api-key"),
            "should hide the API key: {debug}"
        );
        assert!(
            !debug.contains("secret-client-secret"),
            "should hide the secret: {debug}"
        );
    }

    #[test]
    fn a_tcf_vendor_id_adds_vendor_consent_only_when_set() {
        let without = config("k", &STANDARD.encode(CLIENT_KEY))
            .build_descriptor()
            .expect("should build");
        assert_eq!(
            without.consent_policy().tcf_vendor(),
            None,
            "should name no vendor by default"
        );

        let with = Uid2Config {
            tcf_vendor_id: Some(21),
            ..config("k", &STANDARD.encode(CLIENT_KEY))
        }
        .build_descriptor()
        .expect("should build");
        assert_eq!(
            with.consent_policy().tcf_vendor(),
            Some(21),
            "should name the vendor"
        );
        assert!(with.is_expiring(&source()), "tokens should expire");
    }

    #[test]
    fn conforms() {
        let stored = state(1_770_000_000_000);
        ProviderHarness::new(&provider()).check_conformance_with_state::<Uid2State>(
            &ConformanceCases::default().with_resolve_case_raw(
                refreshing(&stored),
                StatusCode::OK,
                &[],
                refresh_reply(&identity()).as_bytes(),
            ),
        );
    }

    #[test]
    fn keeps_a_base_url_path_with_or_without_a_trailing_slash() {
        for (base, expected) in [
            (
                "https://uid2.example.com",
                "https://uid2.example.com/v2/token/generate",
            ),
            (
                "https://uid2.example.com/",
                "https://uid2.example.com/v2/token/generate",
            ),
            (
                "https://uid2.example.com/operator",
                "https://uid2.example.com/operator/v2/token/generate",
            ),
            (
                "https://uid2.example.com/operator/",
                "https://uid2.example.com/operator/v2/token/generate",
            ),
        ] {
            let provider = Uid2Provider::new(&Uid2Config {
                base_url: base.to_owned(),
                ..config("api-key", &STANDARD.encode(CLIENT_KEY))
            })
            .expect("should build the provider");
            let mut harness = with_email(&provider);

            let request = requested(&mut harness);

            assert_eq!(
                request.url().as_str(),
                expected,
                "should keep the path of {base}"
            );
        }
    }

    #[test]
    fn rejects_a_client_secret_that_is_not_a_key() {
        assert!(
            Uid2Provider::new(&config("api-key", "not-base64-key")).is_err(),
            "should refuse an invalid client secret"
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(
            &config("api-key-name", "client-secret-name"),
            &config("resolved-api-key", &STANDARD.encode(CLIENT_KEY)),
        );
    }
}

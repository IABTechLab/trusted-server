# UID2 Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate and refresh UID2 tokens at the edge with a server-side `IdentityProvider` under `[integrations.uid2]`, writing the advertising token to the `uidapi.com` source.

**Architecture:** `integrations/uid2.rs` holds `Uid2Config`, `Uid2Provider` and `register`. A first token comes from `POST /v2/token/generate` with the hashed email from `ts-hem`, sent in an AES-256-GCM envelope keyed by the operator's client secret. The refresh token and the key that decrypts its response are kept as private provider state, so later visits call `POST /v2/token/refresh` without the hashed email. The API key and client secret are secret fields: secret-store key names at rest, resolved values at runtime, never read by `build_descriptor`.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `aes-gcm` 0.10 (new dependency), `base64`, `serde`/`serde_json`, `url`, `validator`, `error-stack`.

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`, section 12.3 (UID2: secrets, crypto, private state), section 10.2 (secret fields), and sections 4.2 to 4.6 (the provider API). Operator page: `docs/guide/integrations/identity-vendors.md`.

**Prerequisite:** `docs/superpowers/plans/2026-10-28-identity-provider-inputs.md`. This plan needs `Input::HashedEmail` from the `ts-hem` cookie for the first token, and adds its section to the operator page that plan creates.

## The UID2 wire format

- **Generate request envelope:** base64 of `0x01 | IV (12 bytes) | AES-256-GCM(timestamp (8 bytes, big-endian ms) | nonce (8 bytes) | JSON payload)`, where the GCM output is ciphertext followed by its 16-byte tag, and the key is the base64-decoded 32-byte client secret. The payload is `{"email_hash": <base64 sha256>, "optout_check": 1}`.
- **Generate response:** base64 of `IV (12) | AES-256-GCM(timestamp (8) | nonce (8) | JSON)` under the same client key. The nonce must equal the request's nonce, or the response is rejected as malformed. The request nonce travels from `request` to `response` in the call context.
- **Refresh request:** the stored refresh token as the plain-text body.
- **Refresh response:** base64 of `IV (12) | AES-256-GCM(JSON)` under the stored `refresh_response_key`. The plaintext is JSON only, with no timestamp or nonce prefix.
- **Reply JSON:** `{"status": "...", "body": {advertising_token, refresh_token, refresh_response_key, identity_expires, refresh_from, refresh_expires}}`, times in milliseconds. `success` stores the token; `optout` opts the visitor out of UID2 and clears state; `expired_token` / `invalid_token` invalidate the stored token and clear state; any other status is malformed.
- Both calls send `Authorization: Bearer <api key>`.

## Global Constraints

- Follow `AGENTS.md`: `error-stack` with `derive_more::Display`, `expect("should ...")`, no local imports, comments above code, doc comment on every public item, Arrange-Act-Assert tests.
- Backtick `UID2` and other code-like names in doc comments (`clippy::doc_markdown`).
- Tests use fictional data: `uid2.example.com` base URL, fixed test keys (`[7; 32]`, `[9; 32]`), fake tokens. The `uidapi.com` source and the public default base URL may appear.
- Never log tokens, keys, the API key, the hashed email or decrypted payloads. Secrets are `Redacted<String>`.
- `build_descriptor` must not read `api_key` or `client_secret` (deploy validation runs while they still hold secret-store key names). `check_descriptor_ignores_secrets` enforces it.
- Randomness comes only from `input.random_bytes()`, so the harness is deterministic. Time comes only from `input.now()`.
- UIDs use `atype` **3**. No TCF vendor by default; `tcf_vendor_id` adds one when the operator's UID2 agreement names it.
- Quick test: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2`. Gates: `cargo test-fastly`, `cargo clippy-fastly`.

## File Structure

| File                                                  | Change | Responsibility                                               |
| ----------------------------------------------------- | ------ | ------------------------------------------------------------ |
| `Cargo.toml`, `crates/trusted-server-core/Cargo.toml` | Modify | `aes-gcm = "0.10"` (workspace) and its use in core           |
| `crates/trusted-server-core/src/integrations/uid2.rs` | Create | Config, crypto, provider, registration, tests                |
| `crates/trusted-server-core/src/integrations/mod.rs`  | Modify | `pub mod uid2;` and its `IntegrationBuilder`                 |
| `crates/trusted-server-core/src/config.rs`            | Modify | Secret fields, runtime and deploy validation, deploy ID list |
| `docs/guide/integrations/identity-vendors.md`         | Modify | Table row and `## UID2` section                              |
| `CHANGELOG.md`                                        | Modify | Vendor bullet                                                |

---

### Task 1: Config, secrets and descriptor

**Files:**

- Modify: `Cargo.toml` (`[workspace.dependencies]`), `crates/trusted-server-core/Cargo.toml` (`[dependencies]`)
- Create: `crates/trusted-server-core/src/integrations/uid2.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (add `pub mod uid2;` in alphabetical order)
- Modify: `crates/trusted-server-core/src/config.rs` (`AppConfigMeta::secret_fields`, test `secret_metadata_lists_all_secret_paths_and_optionality`)

**Interfaces:**

- Produces: `pub(crate) const UID2_INTEGRATION_ID: &str = "uid2"`, `pub struct Uid2Config { enabled, base_url: String, api_key: Redacted<String>, client_secret: Redacted<String>, tcf_vendor_id: Option<u16> }`, `pub struct Uid2Provider { descriptor, api_key: Redacted<String>, client_key: [u8; 32] }`, `pub fn Uid2Provider::new(&Uid2Config) -> Result<Self, Report<IdentityProviderError>>`.

- [ ] **Step 1: Add the dependency**

In the root `Cargo.toml` `[workspace.dependencies]`, in alphabetical order:

```toml
aes-gcm = "0.10"
```

In `crates/trusted-server-core/Cargo.toml` `[dependencies]`:

```toml
aes-gcm = { workspace = true }
```

`aes-gcm` is the RustCrypto AEAD implementation; it is pure Rust and builds for `wasm32-wasip1` and `wasm32-unknown-unknown`.

- [ ] **Step 2: Write the failing tests**

Create `uid2.rs` with the tests module:

```rust
#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;
    use crate::ec::identity::test_support::{
        HarnessSetup, ProviderHarness, check_descriptor_ignores_secrets,
    };
    use crate::ec::identity::{HashedEmail, SourceDomain, StoredId};

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
```

In `config.rs`, in `secret_metadata_lists_all_secret_paths_and_optionality`, add after the `integrations.datadome.server_side_key_secret_name` entry:

```rust
                ("integrations.uid2.api_key".to_owned(), true),
                ("integrations.uid2.client_secret".to_owned(), true),
```

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2`
Expected: FAIL to compile (`Uid2Config` not found).

- [ ] **Step 4: Implement the config, descriptor and constructor**

Prepend to `uid2.rs`. The `use` block is the module's final one; items used only by later tasks warn as unused until then.

```rust
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
        let mut consent = ConsentPolicy::strict();
        if let Some(vendor) = self.tcf_vendor_id {
            consent = consent.with_tcf_vendor(vendor);
        }
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

impl IdentityProvider for Uid2Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }
}
```

The hashed email is needed only for `Initial`; a refresh uses private state. Decoding the client secret happens in `new`, at registration, where secrets are resolved, so a wrong secret fails startup instead of every call.

- [ ] **Step 5: Declare the secret fields**

In `config.rs`, in `TrustedServerAppConfig`'s `secret_fields()` list, after the DataDome `server_side_key_secret_name` field:

```rust
            field(
                vec![
                    optional_object("integrations"),
                    optional_object("uid2"),
                    object("api_key"),
                ],
                true,
            ),
            field(
                vec![
                    optional_object("integrations"),
                    optional_object("uid2"),
                    object("client_secret"),
                ],
                true,
            ),
```

`ts config push` then resolves both from the secret store, and the serialized app config keeps only their key names.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2`
Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config::tests::secret_metadata_lists_all_secret_paths_and_optionality`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/trusted-server-core/Cargo.toml crates/trusted-server-core/src/integrations/ crates/trusted-server-core/src/config.rs
git commit -S --signoff -m "Add UID2 provider config with secret-store API key and client secret"
```

---

### Task 2: Generate a token

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/uid2.rs`

**Interfaces:**

- Consumes: `ResolveInput::{endpoint, random_bytes, now, hashed_email, primary_source}`, `HashedEmail::to_base64`, `ProviderRequest::{post_text, with_bearer, try_with_call_context}`, `ProviderResponse::{success_text, call_context}`, `UnixTime::from_millis`.
- Produces: `fn seal(&[u8; 32], &[u8; 12], &[u8]) -> Result<Vec<u8>, _>`, `fn open(&[u8], &str) -> Result<Vec<u8>, _>`, `fn encrypt_envelope(..) -> Result<String, _>`, `fn decrypt_generate(&[u8; 32], &str, &[u8; 8]) -> Result<Uid2Reply, _>`, `pub(crate) struct Uid2State { refresh_token, refresh_response_key, refresh_expires: u64 }`, `struct GenerateCall { request_nonce: [u8; 8] }`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
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
```

The first test decrypts the request with the AEAD directly, independent of the module's helpers, so it checks the wire format rather than round-tripping the provider's own code. The second seals an all-zero nonce, which differs from the nonce the seeded harness randomness gives the request.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2`
Expected: FAIL to compile (`seal`, `Uid2State` not found).

- [ ] **Step 3: Add state, call context and reply types**

Append after `impl Uid2Provider`:

```rust
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
```

The state (refresh token, response key and expiry) fits within the framework's 1,024-byte private-state cap.

- [ ] **Step 4: Add the crypto helpers**

Append:

```rust
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
```

A short body, a bad tag, or a nonce mismatch are all `Malformed`, which the coordinator backs off like any unparseable response. `join` with a relative path keeps a base URL's own path prefix only when it ends in `/`; the default has none.

- [ ] **Step 5: Implement the generate request and response**

Add to `impl IdentityProvider for Uid2Provider`:

```rust
    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let base = input.endpoint()?;
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
        let call = response
            .call_context::<GenerateCall>()?
            .ok_or_else(|| IdentityProviderError::config("missing call context"))?;
        let reply = decrypt_generate(&self.client_key, body, &call.request_nonce)?;
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
```

The token's own times drive the framework: `identity_expires` is the expiry, `refresh_from` the earliest refresh, and `refresh_expires` the last moment a refresh can succeed. Task 3 adds the refresh call and replaces the call-context line in `response`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2`
Expected: PASS (4 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/integrations/uid2.rs
git commit -S --signoff -m "Generate UID2 tokens through an encrypted envelope"
```

---

### Task 3: Refresh with the stored state

A refresh is chosen when the framework schedules a `Refresh` for `uidapi.com`. It needs the stored state; without state, or once `refresh_expires` has passed, the provider skips the call (returns `None`), and the next initial lookup with a hashed email starts over. A refresh call carries no call context, which is how `response` tells the two apart.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/uid2.rs`

**Interfaces:**

- Consumes: `ResolveInput::{is_refresh, state}`, `Uid2State`, `open`.
- Produces: `fn decrypt_refresh(key: &str, body: &str) -> Result<Uid2Reply, Report<IdentityProviderError>>`.

- [ ] **Step 1: Write the failing test**

Add to the tests module:

```rust
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
```

The refresh response plaintext is JSON only, unlike the generate response, which is why `sealed_response` here seals bare JSON.

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2::tests::refreshes_with_the_stored_state`
Expected: FAIL. Without a refresh branch the provider takes the generate path, which fails on the missing hashed email.

- [ ] **Step 3: Add the refresh request**

At the top of `request`, after `let base = input.endpoint()?;`, insert:

```rust
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
```

- [ ] **Step 4: Decrypt refresh responses**

Append after `decrypt_generate`:

```rust
/// Opens a refresh response with the stored base64 response key.
fn decrypt_refresh(key: &str, body: &str) -> Result<Uid2Reply, Report<IdentityProviderError>> {
    let key = STANDARD
        .decode(key)
        .map_err(|_| IdentityProviderError::malformed())?;
    let plaintext = open(&key, body)?;
    serde_json::from_slice(&plaintext).map_err(|_| IdentityProviderError::malformed())
}
```

In `response`, replace

```rust
        let call = response
            .call_context::<GenerateCall>()?
            .ok_or_else(|| IdentityProviderError::config("missing call context"))?;
        let reply = decrypt_generate(&self.client_key, body, &call.request_nonce)?;
```

with

```rust
        let reply = match response.call_context::<GenerateCall>()? {
            Some(call) => decrypt_generate(&self.client_key, body, &call.request_nonce)?,
            None => {
                let state = input
                    .state::<Uid2State>()?
                    .ok_or_else(|| IdentityProviderError::config("refresh without state"))?;
                decrypt_refresh(&state.refresh_response_key, body)?
            }
        };
```

A successful refresh goes through the same `success` arm and replaces the state with the new refresh token and response key.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::uid2`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/uid2.rs
git commit -S --signoff -m "Refresh UID2 tokens from the stored refresh state"
```

---

### Task 4: Register, validate and document

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/uid2.rs` (`register`)
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (`builders()`)
- Modify: `crates/trusted-server-core/src/config.rs`
- Modify: `docs/guide/integrations/identity-vendors.md`, `CHANGELOG.md`

**Interfaces:**

- Produces: `pub fn register(settings: &Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>`.

- [ ] **Step 1: Write `register` and add the builder**

Append to `uid2.rs`, before the tests module:

```rust
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
```

In `integrations/mod.rs` `builders()`, add:

```rust
        IntegrationBuilder {
            id: uid2::UID2_INTEGRATION_ID,
            build: uid2::register,
        },
```

- [ ] **Step 2: Run the deploy-coverage test and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config::tests::deploy_validation_covers_registered_integration_builders`
Expected: FAIL with `missing_ids` containing `"uid2"`.

- [ ] **Step 3: Validate at runtime and at deploy**

In `config.rs`, import `uid2::{UID2_INTEGRATION_ID, Uid2Config}`. Add `UID2_INTEGRATION_ID,` to `DEPLOY_VALIDATED_INTEGRATION_IDS`. In `identity_descriptors_for_deploy`:

```rust
    descriptors.extend(validate_identity_integration::<Uid2Config>(
        settings,
        UID2_INTEGRATION_ID,
    )?);
```

`validate_identity_integration` only calls `build_descriptor`, never `Uid2Provider::new`, so deploy validation works while the secrets are still key names. If it still carries the framework's temporary `#[allow(dead_code)]`, remove it and its comment. In `validate_enabled_integrations`:

```rust
    validate_integration::<Uid2Config>(settings, UID2_INTEGRATION_ID)?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config`
Expected: PASS.

- [ ] **Step 5: Document**

In `docs/guide/integrations/identity-vendors.md`, add this table row:

```markdown
| `uid2` | UID2 | Resolve | `uidapi.com` | Hashed email for a first token |
```

and this section:

````markdown
## UID2

```toml
[integrations.uid2]
enabled = true
# base_url = "https://prod.uidapi.com"
api_key = "uid2_api_key"
client_secret = "uid2_client_secret"
# tcf_vendor_id = 21
```

`api_key` and `client_secret` are secret-store key names; `ts config push` resolves them. The client secret is the base64 key that encrypts token requests. A first token needs the hashed email. Trusted Server keeps the refresh token as private state and refreshes before the token expires. A UID2 `optout` opts the visitor out of UID2 until they consent again. UID2 is not meant for the EU (EUID covers it), so set `tcf_vendor_id` only when your UID2 agreement names one.
````

In `CHANGELOG.md` under `### Added`, insert "UID2 (`[integrations.uid2]`)" into the vendor bullet's list of providers, or, if there is no vendor bullet yet, add:

```markdown
- Server-side identity provider for UID2 (`[integrations.uid2]`). Each source it writes needs an `[[ec.partners]]` entry. See [Identity Vendors](docs/guide/integrations/identity-vendors.md).
```

- [ ] **Step 6: Run the gates**

Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly && cargo clippy-cloudflare-wasm && cargo clippy-spin-wasm`
Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Expected: PASS. The two extra wasm clippy runs confirm `aes-gcm` builds for every wasm adapter.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/ docs/guide/integrations/identity-vendors.md CHANGELOG.md
git commit -S --signoff -m "Register the UID2 identity provider and validate its config"
```

## Self-Review Notes

- Generate envelope `0x01 | IV12 | GCM(ts8 | nonce8 | JSON)`, nonce echo check: Task 2.
- Refresh decrypts with `refresh_response_key`, JSON-only plaintext: Task 3.
- Secrets in `secret_fields()`, never read by `build_descriptor`: Tasks 1 and 4.

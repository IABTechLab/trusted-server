# ID5 Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the ID5 ID at the edge with a server-side `IdentityProvider` under `[integrations.id5]`, storing the complete EIDs ID5 returns for `id5-sync.com`, `true-link-id5-sync.com` and `euid.eu`.

**Architecture:** `integrations/id5.rs` holds `Id5Config` (an `IdentityProviderConfig`), `Id5Provider` (an `IdentityProvider`) and `register`. The provider is pure: `request` builds one JSON `POST` to ID5's fetch endpoint from the declared inputs, and `response` turns ID5's reply into a `Resolution`. ID5's `signature` is kept as private provider state and sent back on the next call. The coordinator from the framework handles consent, eligibility, timing, backoff and the KV write.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `serde`/`serde_json`, `base64`, `url`, `validator`, `error-stack` (all already dependencies).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`, section 12.4 (ID5) and sections 4.2 to 4.6 (the provider API). Operator page: `docs/guide/integrations/identity-vendors.md`. Author guide: `docs/guide/identity-providers.md`.

**Prerequisite:** `docs/superpowers/plans/2026-10-28-identity-provider-inputs.md`. This plan uses `Input::HashedEmail` from the `ts-hem` cookie, `VendorEid` (`integrations/vendor_eid.rs`) and `is_routable_ip` (`integrations/vendor_request.rs`), and adds its section to the operator page that plan creates.

## Why the request looks the way it does

These were checked against ID5's live endpoints:

- **Fetch URL `https://id5-sync.com/g/v2/<partner>.json`.** ID5's config endpoint returns this URL for a partner. The newer `gm/v3` endpoint expects a multiplexed body with a `requests` array and answers 400 to a flat body, so the provider uses `g/v2` with a flat JSON body. The configured `endpoint` is a base; `<partner_id>.json` is appended as a path segment.
- **`Origin` header.** ID5 checks the request origin, so the provider sends `Origin: https://<publisher domain>`.
- **Response.** ID5 returns complete EIDs under `ids.<key>.eid`, a `signature`, and `cache_control.max_age_sec`. Older responses carry only `universal_uid` (with `ext.linkType`), so that is a fallback for the primary source. A `universal_uid` of `0` means the visitor opted out at ID5, so the stored ID is invalidated.
- **Hashed email.** ID5 takes it as partner data: `pd` is base64 of `1=<hex sha256>`.
- **Client IP and user agent** are sent as `ip` and `ua`; the IP only when it is routable (see `is_routable_ip`). The operator doc asks operators to confirm those field names with ID5 for each partner account.

## Global Constraints

- Follow `AGENTS.md`: `error-stack` with `derive_more::Display`, `expect("should ...")`, no local imports, comments above code, doc comment on every public item, Arrange-Act-Assert tests.
- Backtick `OpenRTB` and CamelCase vendor names in doc comments (`clippy::doc_markdown`).
- Tests use fictional data: `www.example.com` pages, documentation IPs (`192.0.2.0/24`), fake IDs such as `ID5*example`. The vendor's public hostname `id5-sync.com` and its source domains may appear.
- Never log UIDs, the signature, the hashed email or consent strings.
- ID5's IAB GVL ID is **131**. Sources, in order: `id5-sync.com` (primary), `true-link-id5-sync.com`, `euid.eu`.
- Quick test: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::id5`. Gates: `cargo test-fastly`, `cargo clippy-fastly`.

## File Structure

| File                                                 | Change | Responsibility                                          |
| ---------------------------------------------------- | ------ | ------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/id5.rs` | Create | Config, descriptor, provider, registration, tests       |
| `crates/trusted-server-core/src/integrations/mod.rs` | Modify | `pub mod id5;` and its `IntegrationBuilder`             |
| `crates/trusted-server-core/src/config.rs`           | Modify | Runtime and deploy validation, deploy-validated ID list |
| `docs/guide/integrations/identity-vendors.md`        | Modify | Table row and `## ID5` section                          |
| `CHANGELOG.md`                                       | Modify | Vendor bullet                                           |

---

### Task 1: Config, descriptor and request

**Files:**

- Create: `crates/trusted-server-core/src/integrations/id5.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (add `pub mod id5;` in alphabetical order)

**Interfaces:**

- Consumes: `is_routable_ip(IpAddr) -> bool`, `ResolveInput::{consent, page_url, hashed_email, user_agent, client_ip, state, endpoint, publisher_domain}`, `ProviderRequest::post_json`, `try_with_header`.
- Produces: `pub(crate) const ID5_INTEGRATION_ID: &str = "id5"`, `pub struct Id5Config { enabled, partner_id: u32, endpoint: String }`, `pub struct Id5Provider`, `pub fn Id5Provider::new(&Id5Config) -> Result<Self, Report<IdentityProviderError>>`, `pub(crate) struct Id5State { signature: String }`.

- [ ] **Step 1: Write the failing tests**

Create `id5.rs` with the tests module only:

```rust
#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;
    use crate::ec::identity::HashedEmail;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, TestConsent,
        check_descriptor_ignores_secrets,
    };

    fn config() -> Id5Config {
        Id5Config {
            enabled: true,
            partner_id: 1234,
            endpoint: default_endpoint(),
        }
    }

    fn provider() -> Id5Provider {
        Id5Provider::new(&config()).expect("should build the provider")
    }

    #[test]
    fn sends_consent_page_and_hashed_email() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0")
            .with_page_url("https://www.example.com/article")
            .with_hashed_email(HashedEmail::for_test([0xab; 32]))
            .with_consent(TestConsent::default().with_us_privacy("1YNN"))
            .with_state(&Id5State {
                signature: "prior-signature".to_owned(),
            });

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");
        assert_eq!(
            request.url().as_str(),
            "https://id5-sync.com/g/v2/1234.json",
            "should call the partner's fetch URL"
        );
        let body: serde_json::Value = harness.request_json();

        assert_eq!(body["partner"], 1234, "should send the partner");
        assert_eq!(body["gdpr"], 0, "should send that GDPR does not apply");
        assert_eq!(body["us_privacy"], "1YNN", "should send US privacy");
        assert_eq!(
            body["s"], "prior-signature",
            "should send the stored signature"
        );
        assert_eq!(
            body["u"], "https://www.example.com/article",
            "should send the page"
        );
        assert_eq!(body["ip"], "192.0.2.10", "should send the client IP");
        assert_eq!(
            body["pd"],
            STANDARD.encode(format!("1={}", "ab".repeat(32))),
            "should send the hashed email as partner data"
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(), &config());
    }
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::id5`
Expected: FAIL to compile (`Id5Config`, `Id5Provider`, `Id5State` not found).

- [ ] **Step 3: Write the config and descriptor**

Prepend to `id5.rs`. The `use` block is the module's final one; items used only by Task 2 (`BTreeMap`, `Duration`, `json!`, `ResolvedEid`, `ResolvedUid`, `ProviderResponse`, `Resolution`, `VendorEid`) warn as unused until then.

```rust
//! ID5 identity provider: resolves the ID5 ID server side.
//!
//! Calls ID5's fetch endpoint with the request's consent, page and network
//! signals, and the hashed email when the JS SDK set one. ID5 returns either
//! complete EIDs under `ids` or the legacy `universal_uid`. The signature ID5
//! returns is kept as private state and sent on the next call.
//!
//! The request field names for the client IP and user agent follow ID5's
//! server-to-server integration and should be confirmed with ID5 for each
//! partner account.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::Report;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolvedEid,
    ResolvedUid, Resolves,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::integrations::vendor_eid::VendorEid;
use crate::integrations::vendor_request::is_routable_ip;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the ID5 provider.
pub(crate) const ID5_INTEGRATION_ID: &str = "id5";

/// ID5's IAB Global Vendor List ID.
const ID5_GVL_ID: u16 = 131;

/// The sources ID5 can return. Operators narrow them with
/// `[ec.identity.providers.id5].sources`.
const ID5_SOURCES: &[&str] = &["id5-sync.com", "true-link-id5-sync.com", "euid.eu"];

/// The universal UID ID5 returns for a visitor who opted out at ID5.
const ID5_OPTED_OUT_UID: &str = "0";

/// Config of the `id5` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct Id5Config {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The partner number ID5 assigned to the publisher.
    pub partner_id: u32,
    /// The fetch endpoint; `<partner_id>.json` is appended to its path.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
}

fn default_endpoint() -> String {
    "https://id5-sync.com/g/v2".to_owned()
}

impl IntegrationConfig for Id5Config {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for Id5Config {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut builder = ProviderDescriptor::builder(ID5_INTEGRATION_ID.parse()?);
        for source in ID5_SOURCES {
            builder = builder.with_source(source.parse()?);
        }
        builder
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_wants([
                        Input::ClientIp,
                        Input::UserAgent,
                        Input::PageUrl,
                        Input::HashedEmail,
                    ])
                    .with_private_state(),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(ID5_GVL_ID))
            .build()
    }
}
```

Every input is _wanted_, not _needed_: ID5 matches on whatever it gets, so a missing input never stops the call.

- [ ] **Step 4: Write the provider and its request**

Append (before the tests module):

```rust
/// The ID5 provider.
pub struct Id5Provider {
    descriptor: ProviderDescriptor,
    partner_id: u32,
}

impl Id5Provider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &Id5Config) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
        })
    }
}

/// The signature ID5 returns, sent back on the next call.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Id5State {
    signature: String,
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    v: &'static str,
    o: &'static str,
    #[serde(rename = "nbPage")]
    nb_page: u32,
    gdpr: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    gdpr_consent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    us_privacy: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_string: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    s: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    u: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tml: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ua: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
}

impl IdentityProvider for Id5Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let page = input.page_url().ok().map(Url::as_str);
        let body = FetchBody {
            partner: self.partner_id,
            v: env!("CARGO_PKG_VERSION"),
            o: "trusted-server",
            nb_page: 1,
            gdpr: u8::from(consent.gdpr_applies()),
            gdpr_consent: consent.raw_tc_string(),
            us_privacy: consent.raw_us_privacy(),
            gpp_string: consent.raw_gpp_string(),
            gpp_sid: consent.gpp_section_ids().map(join_ids),
            s: input.state::<Id5State>()?.map(|state| state.signature),
            u: page,
            tml: page,
            pd: input
                .hashed_email()
                .ok()
                .map(|email| STANDARD.encode(format!("1={}", email.to_hex()))),
            ua: input.user_agent().ok(),
            ip: input
                .client_ip()
                .ok()
                .filter(|ip| is_routable_ip(*ip))
                .map(|ip| ip.to_string()),
        };
        let mut url = input.endpoint()?.clone();
        url.path_segments_mut()
            .map_err(|_| IdentityProviderError::config("endpoint cannot hold a path"))?
            .pop_if_empty()
            .push(&format!("{}.json", self.partner_id));
        ProviderRequest::post_json(&url, &body)?
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))
            .map(Some)
    }
}

fn join_ids(ids: &[u16]) -> String {
    ids.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}
```

`input.hashed_email().ok()` and the other `.ok()` calls are safe for wanted inputs: the accessor returns `InputUnavailable` when the input is absent this time, which is a normal skip of that field. `pop_if_empty()` keeps a configured endpoint with a trailing slash from producing `//1234.json`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::id5`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/id5.rs crates/trusted-server-core/src/integrations/mod.rs
git commit -S --signoff -m "Build ID5 fetch requests from consent, page and hashed email"
```

---

### Task 2: Parse ID5's response

ID5's reply can hold complete EIDs for several sources. Each `ids.<key>.eid` whose `source` names a declared source becomes one `ResolvedEid`, with every UID and EID-level field kept (`VendorEid::to_resolved`); others are ignored. When no EID came back for the primary source, the legacy `universal_uid` fills it in, with `atype` 1 and the reply's `ext.linkType`. `0` there means an ID5 opt-out and invalidates the primary source. `cache_control.max_age_sec` sets each EID's refresh time. A declared source that has a stored value but was not returned this time is invalidated with `also_invalidate`, so a stale TrueLink or EUID value does not outlive ID5's answer. The new `signature` replaces the stored state.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/id5.rs`

**Interfaces:**

- Consumes: `VendorEid::to_resolved(&SourceDomain) -> Option<ResolvedEid>`, `ProviderDescriptor::source_named`, `ResolveInput::{primary_source, sources, stored}`, `Resolution::{found, no_match, invalidate, also_invalidate, replacing_state}`.
- Produces: `Id5Provider::response`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn resolves_complete_eids_and_keeps_the_signature() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({
                    "signature": "next-signature",
                    "ids": {
                        "id5": { "eid": {
                            "source": "id5-sync.com",
                            "uids": [{ "id": "ID5*example", "atype": 1, "ext": { "linkType": 2 } }]
                        }},
                        "trueLinkId": { "eid": {
                            "source": "true-link-id5-sync.com",
                            "uids": [{ "id": "true-link-example", "atype": 1 }]
                        }},
                        "other": { "eid": {
                            "source": "other.example.com",
                            "uids": [{ "id": "ignored" }]
                        }}
                    },
                    "cache_control": { "max_age_sec": 7200 }
                }),
            )
            .expect("should parse");

        let sources: Vec<&str> = resolution
            .eids()
            .iter()
            .map(|eid| eid.source().as_str())
            .collect();
        assert_eq!(
            sources,
            ["id5-sync.com", "true-link-id5-sync.com"],
            "should keep only declared sources"
        );
        let state: Id5State = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(
            state.signature, "next-signature",
            "should keep the signature"
        );
    }

    #[test]
    fn falls_back_to_the_universal_uid() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "universal_uid": "ID5*legacy", "ext": { "linkType": 1 } }),
            )
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(
            eid.source().as_str(),
            "id5-sync.com",
            "should use the primary source"
        );
        assert_eq!(eid.uids()[0].id(), "ID5*legacy", "should keep the UID");
        assert_eq!(
            eid.uids()[0].ext(),
            Some(&json!({ "linkType": 1 })),
            "should keep the link type"
        );
    }

    #[test]
    fn an_opted_out_visitor_invalidates_the_id() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "universal_uid": "0" }))
            .expect("should parse");

        assert_eq!(
            resolution.invalidated()[0].as_str(),
            "id5-sync.com",
            "should invalidate the ID5 ID"
        );
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance_with_state::<Id5State>(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_client_ip("192.0.2.10"),
                StatusCode::OK,
                &json!({ "universal_uid": "ID5*example", "signature": "s" }),
            ),
        );
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::id5`
Expected: the four new tests FAIL with `NotImplemented("response")` from the trait's default `response`.

- [ ] **Step 3: Add the reply types**

Append after `FetchBody`:

```rust
#[derive(Deserialize)]
struct FetchReply {
    universal_uid: Option<String>,
    signature: Option<String>,
    #[serde(default)]
    ext: Option<serde_json::Value>,
    #[serde(default)]
    ids: BTreeMap<String, Id5Id>,
    cache_control: Option<CacheControl>,
}

#[derive(Deserialize)]
struct Id5Id {
    eid: Option<VendorEid>,
}

#[derive(Deserialize)]
struct CacheControl {
    max_age_sec: Option<u64>,
}
```

None of them denies unknown fields: ID5 adds fields over time and a new one must not fail the call.

- [ ] **Step 4: Implement `response`**

Add to `impl IdentityProvider for Id5Provider`, after `request`:

```rust
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let primary = input.primary_source();

        let mut eids: Vec<ResolvedEid> = reply
            .ids
            .values()
            .filter_map(|id| {
                let eid = id.eid.as_ref()?;
                let source = self.descriptor.source_named(&eid.source)?;
                eid.to_resolved(source)
            })
            .collect();
        if !eids.iter().any(|eid| eid.source() == primary) {
            match reply.universal_uid.as_deref() {
                Some(ID5_OPTED_OUT_UID) => return Ok(Resolution::invalidate(primary)),
                Some(uid) if !uid.is_empty() => {
                    let mut resolved = ResolvedUid::new(uid).with_atype(1);
                    if let Some(link_type) = reply.ext.as_ref().and_then(|ext| ext.get("linkType"))
                    {
                        resolved = resolved.with_ext(json!({ "linkType": link_type }));
                    }
                    eids.push(ResolvedEid::new(primary, resolved));
                }
                _ => {}
            }
        }
        if let Some(max_age) = reply.cache_control.and_then(|cache| cache.max_age_sec) {
            eids = eids
                .into_iter()
                .map(|eid| eid.with_refresh_in(Duration::from_secs(max_age)))
                .collect();
        }

        let mut resolution = if eids.is_empty() {
            Resolution::no_match()
        } else {
            let returned: Vec<_> = eids.iter().map(|eid| eid.source().clone()).collect();
            let mut found = Resolution::found(eids);
            for (source, _) in input.sources() {
                if !returned.contains(source) && input.stored(source).is_some() {
                    found = found.also_invalidate(source);
                }
            }
            found
        };
        if let Some(signature) = reply.signature {
            resolution = resolution.replacing_state(&Id5State { signature });
        }
        Ok(resolution)
    }
```

`success_json` maps a non-2xx status to `Rejected` and an unparseable body to `Malformed`, so the coordinator applies the framework's backoff for those.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::id5`
Expected: PASS (6 tests). `check_conformance_with_state` exercises declared-input discipline, state round-trips, oversized and malformed bodies, and error statuses.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/id5.rs
git commit -S --signoff -m "Resolve complete ID5 EIDs and keep the ID5 signature"
```

---

### Task 3: Register, validate and document

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/id5.rs` (`register`)
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (`builders()`)
- Modify: `crates/trusted-server-core/src/config.rs` (imports, `DEPLOY_VALIDATED_INTEGRATION_IDS`, `identity_descriptors_for_deploy`, `validate_enabled_integrations`)
- Modify: `docs/guide/integrations/identity-vendors.md`, `CHANGELOG.md`

**Interfaces:**

- Consumes: `IntegrationRegistration::builder(..).try_with_identity_provider(..).without_js()`, `validate_identity_integration::<T>(settings, id)`, `validate_integration::<T>(settings, id)`.
- Produces: `pub fn register(settings: &Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>`.

- [ ] **Step 1: Write `register` and add the builder**

Append to `id5.rs`, before the tests module:

```rust
/// Registers the ID5 provider when `[integrations.id5]` is enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<Id5Config>(ID5_INTEGRATION_ID)? else {
        return Ok(None);
    };
    log::info!(
        "Registering ID5 identity provider (partner={})",
        config.partner_id
    );
    Ok(Some(
        IntegrationRegistration::builder(ID5_INTEGRATION_ID)
            .try_with_identity_provider(Id5Provider::new(&config))?
            .without_js()
            .build(),
    ))
}
```

`without_js()` because the provider ships no browser module. In `integrations/mod.rs` `builders()`, add:

```rust
        IntegrationBuilder {
            id: id5::ID5_INTEGRATION_ID,
            build: id5::register,
        },
```

- [ ] **Step 2: Run the deploy-coverage test and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config::tests::deploy_validation_covers_registered_integration_builders`
Expected: FAIL with `missing_ids` containing `"id5"`. This test exists so no registered integration escapes `ts config validate`.

- [ ] **Step 3: Validate at runtime and at deploy**

In `config.rs`, import `id5::{ID5_INTEGRATION_ID, Id5Config}` in the `crate::integrations` use list. Add `ID5_INTEGRATION_ID,` to `DEPLOY_VALIDATED_INTEGRATION_IDS`. In `identity_descriptors_for_deploy`, after the generic pull descriptors:

```rust
    descriptors.extend(validate_identity_integration::<Id5Config>(
        settings,
        ID5_INTEGRATION_ID,
    )?);
```

Deploy validation then cross-checks the ID5 descriptor's sources against `[[ec.partners]]` and `[ec.identity.providers.id5]`, as for every provider. If `validate_identity_integration` still carries `#[allow(dead_code)]` from the framework ("the first production identity integration adds the first caller"), remove that attribute and its comment now. In `validate_enabled_integrations`, add:

```rust
    validate_integration::<Id5Config>(settings, ID5_INTEGRATION_ID)?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config`
Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::id5`
Expected: PASS.

- [ ] **Step 5: Document**

In `docs/guide/integrations/identity-vendors.md`, add this row to the vendor table:

```markdown
| `id5` | ID5 | Resolve | `id5-sync.com`, `true-link-id5-sync.com`, `euid.eu` | Nothing (hashed email improves match) |
```

and this section after "Hashed email" (keep vendor sections in table order):

````markdown
## ID5

```toml
[integrations.id5]
enabled = true
partner_id = 1234
# endpoint = "https://id5-sync.com/g/v2"
```

ID5 needs vendor consent for GVL ID 131 where TCF applies. Trusted Server keeps the signature ID5 returns as private state and sends it on the next call. ID5's answer `0` (the visitor opted out at ID5) removes the stored ID. Confirm with ID5 the server-to-server field names for the client IP (`ip`) and user agent (`ua`) on your partner account.
````

In `CHANGELOG.md` under `### Added`, add ID5 to the vendor bullet. If no vendor bullet exists yet, add:

```markdown
- Server-side identity provider for ID5 (`[integrations.id5]`). Each source it writes needs an `[[ec.partners]]` entry. See [Identity Vendors](docs/guide/integrations/identity-vendors.md).
```

When other vendor plans have already added the bullet, insert "ID5 (`[integrations.id5]`)" into its list of providers instead.

- [ ] **Step 6: Run the gates**

Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly`
Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Expected: PASS, no unused-import warnings left.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/ docs/guide/integrations/identity-vendors.md CHANGELOG.md
git commit -S --signoff -m "Register the ID5 identity provider and validate its config"
```

## Self-Review Notes

- Fetch URL, flat body and `Origin`: Task 1, with the `gm/v3` reason above.
- Complete EIDs, legacy fallback, `0` opt-out, `max_age_sec` refresh, `also_invalidate`, signature state: Task 2.
- Registration, runtime and deploy validation, operator doc: Task 3.

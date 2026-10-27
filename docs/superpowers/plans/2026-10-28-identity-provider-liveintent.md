# LiveIntent Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve LiveIntent's IDs at the edge with a server-side `IdentityProvider` under `[integrations.liveintent]`, one `OpenRTB` source per attribute the operator configures.

**Architecture:** `integrations/liveintent.rs` holds `LiveIntentConfig`, `LiveIntentProvider` and `register`. The provider sends one `GET` to LiveIntent's identity exchange with the visitor's first-party `_lc2_fpi` cookie, consent, page origin and client IP, and one `resolve` parameter per configured attribute. A fixed table maps each attribute to the source it carries; only the configured attributes' sources are declared, so only those need `[[ec.partners]]` entries. The reply is a flat JSON object keyed by attribute name.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `serde_json`, `base64`, `chrono`, `http`, `validator`, `error-stack` (all already dependencies).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`, sections 4.2 to 4.6 (the provider API) and section 12 (vendor examples; LiveIntent follows the same shape). Operator page: `docs/guide/integrations/identity-vendors.md`.

**Prerequisite:** `docs/superpowers/plans/2026-10-28-identity-provider-inputs.md`. This plan uses the declared first-party cookie input (`Resolves::with_cookie`, `ResolveInput::cookie`, harness `with_cookie`) and `is_routable_ip` (`integrations/vendor_request.rs`).

## Why the request looks the way it does

These were checked against LiveIntent's live exchange:

- **`GET https://idx.liadm.com/idex/<distributorId|prebid>/<publisherId|any>`.** The path names the distributor (or `prebid` when there is none) and the publisher (or `any`).
- **`Origin` header is required.** Without it LiveIntent answers 400 "Request origin could not be determined". The provider sends `Origin: https://<publisher domain>`.
- **`did` is the distributor ID, and only when configured.** Any other `did` value gets 400 "Malformed did", so the provider never invents one.
- **`duid` is the first-party `_lc2_fpi` cookie** on the publisher's domain, which is why this provider needs the declared-cookie input. It is optional; LiveIntent still matches on other signals.
- **`pip` / `pip6` must be routable.** A loopback or private `pip` makes LiveIntent answer 204, as a local dev server would send. `is_routable_ip` drops such an address. The IP is sent base64url without padding.
- **`pu` is the page origin**, not the full URL.
- **`Expires` response header gives the lifetime** of the returned IDs. A 204 means no match.

## Global Constraints

- Follow `AGENTS.md`: `error-stack` with `derive_more::Display`, `expect("should ...")`, no local imports, comments above code, doc comment on every public item, Arrange-Act-Assert tests.
- Backtick `LiveIntent`, `OpenRTB` and other CamelCase names in doc comments (`clippy::doc_markdown`).
- Tests use fictional data: `www.example.com`, documentation IPs (`192.0.2.0/24`), fake IDs (`li-example`, `fpi-example`). LiveIntent's public hostnames and the `OpenRTB` source domains in the attribute table may appear.
- Never log the `_lc2_fpi` value, UIDs, consent strings, or the request URL with its query.
- LiveIntent's IAB GVL ID is **148**. All UIDs use `atype` **3**.
- Quick test: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`. Gates: `cargo test-fastly`, `cargo clippy-fastly`.

## File Structure

| File                                                        | Change | Responsibility                                          |
| ----------------------------------------------------------- | ------ | ------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/liveintent.rs` | Create | Config, attribute table, provider, registration, tests  |
| `crates/trusted-server-core/src/integrations/mod.rs`        | Modify | `pub mod liveintent;` and its `IntegrationBuilder`      |
| `crates/trusted-server-core/src/config.rs`                  | Modify | Runtime and deploy validation, deploy-validated ID list |
| `docs/guide/integrations/identity-vendors.md`               | Modify | Table row and `## LiveIntent` section                   |
| `CHANGELOG.md`                                              | Modify | Vendor bullet                                           |

---

### Task 1: Config, attribute table and descriptor

The attribute table is the one place that knows which `OpenRTB` source each LiveIntent attribute carries. `LiveIntentConfig::attributes()` resolves the configured names against it, refusing an unknown name, and both the descriptor (sources) and the provider (query and response mapping) use the result.

**Files:**

- Create: `crates/trusted-server-core/src/integrations/liveintent.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (add `pub mod liveintent;` in alphabetical order)

**Interfaces:**

- Produces: `pub(crate) const LIVEINTENT_INTEGRATION_ID: &str = "liveintent"`, `pub struct LiveIntentConfig { enabled, publisher_id: String, distributor_id: Option<String>, endpoint: String, resolve: Vec<String> }`, `fn LiveIntentConfig::attributes(&self) -> Result<Vec<(String, SourceDomain)>, Report<IdentityProviderError>>`, `pub struct LiveIntentProvider`, `pub fn LiveIntentProvider::new(&LiveIntentConfig) -> Result<Self, Report<IdentityProviderError>>`.

- [ ] **Step 1: Write the failing tests**

Create `liveintent.rs` with the tests module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, TestConsent,
        check_descriptor_ignores_secrets,
    };

    fn config(resolve: &[&str]) -> LiveIntentConfig {
        LiveIntentConfig {
            enabled: true,
            publisher_id: "12345".to_owned(),
            distributor_id: None,
            endpoint: default_endpoint(),
            resolve: resolve.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn provider(resolve: &[&str]) -> LiveIntentProvider {
        LiveIntentProvider::new(&config(resolve)).expect("should build the provider")
    }

    #[test]
    fn rejects_an_unknown_attribute() {
        assert!(
            LiveIntentProvider::new(&config(&["unknown"])).is_err(),
            "should refuse an attribute outside the table"
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(&["nonId"]), &config(&["nonId"]));
    }
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`
Expected: FAIL to compile (`LiveIntentConfig` not found).

- [ ] **Step 3: Implement the config and descriptor**

Prepend to `liveintent.rs`. The `use` block is the module's final one; items used only by later tasks warn as unused until then.

```rust
//! `LiveIntent` identity provider: resolves `LiveIntent`'s IDs server side.
//!
//! Calls `LiveIntent`'s identity exchange with the visitor's first-party
//! `LiveIntent` cookie, consent and network signals, and asks for the
//! attributes the operator configured. Each attribute `LiveIntent` returns
//! maps to one `OpenRTB` source.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use error_stack::Report;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolvedEid,
    ResolvedUid, Resolves, SourceDomain,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::integrations::vendor_request::is_routable_ip;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `LiveIntent` provider.
pub(crate) const LIVEINTENT_INTEGRATION_ID: &str = "liveintent";

/// `LiveIntent`'s IAB Global Vendor List ID.
const LIVEINTENT_GVL_ID: u16 = 148;

/// `LiveIntent`'s first-party ID cookie on the publisher's domain.
const FIRST_PARTY_COOKIE: &str = "_lc2_fpi";

/// The attribute that carries `LiveIntent`'s own ID.
const NON_ID_ATTRIBUTE: &str = "nonId";

/// Each attribute `LiveIntent` resolves and the source it carries.
const ATTRIBUTE_SOURCES: &[(&str, &str)] = &[
    (NON_ID_ATTRIBUTE, "liveintent.com"),
    ("bidswitch", "bidswitch.net"),
    ("medianet", "media.net"),
    ("magnite", "rubiconproject.com"),
    ("index", "liveintent.indexexchange.com"),
    ("openx", "openx.net"),
    ("pubmatic", "pubmatic.com"),
    ("sovrn", "liveintent.sovrn.com"),
    ("sharethrough", "sharethrough.com"),
    ("sonobi", "liveintent.sonobi.com"),
    ("triplelift", "liveintent.triplelift.com"),
    ("zetassp", "zeta-ssp.liveintent.com"),
    ("vidazoo", "liveintent.vidazoo.com"),
    ("nexxen", "liveintent.unrulymedia.com"),
    ("uid2", "uidapi.com"),
    ("thetradedesk", "adserver.org"),
];

/// Config of the `liveintent` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct LiveIntentConfig {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The publisher ID `LiveIntent` assigned, or `any`.
    #[serde(default = "default_publisher_id")]
    #[validate(length(min = 1))]
    pub publisher_id: String,
    /// The distributor ID `LiveIntent` assigned to a distributor of the
    /// integration, if any. It takes the place of `prebid` in the path.
    pub distributor_id: Option<String>,
    /// The identity exchange endpoint.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
    /// The attributes to resolve, from the table in this module. Each one's
    /// source needs an `[[ec.partners]]` entry. Default: `nonId`.
    #[serde(default = "default_resolve")]
    #[validate(length(min = 1))]
    pub resolve: Vec<String>,
}

fn default_publisher_id() -> String {
    "any".to_owned()
}

fn default_endpoint() -> String {
    "https://idx.liadm.com/idex".to_owned()
}

fn default_resolve() -> Vec<String> {
    vec![NON_ID_ATTRIBUTE.to_owned()]
}

impl IntegrationConfig for LiveIntentConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl LiveIntentConfig {
    /// The configured attributes and their sources, in config order.
    fn attributes(&self) -> Result<Vec<(String, SourceDomain)>, Report<IdentityProviderError>> {
        self.resolve
            .iter()
            .map(|attribute| {
                let (name, source) = ATTRIBUTE_SOURCES
                    .iter()
                    .find(|(name, _)| name == attribute)
                    .ok_or_else(|| {
                        IdentityProviderError::config(format!(
                            "unknown LiveIntent attribute `{attribute}`"
                        ))
                    })?;
                Ok(((*name).to_owned(), source.parse()?))
            })
            .collect()
    }
}

impl IdentityProviderConfig for LiveIntentConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut builder = ProviderDescriptor::builder(LIVEINTENT_INTEGRATION_ID.parse()?);
        for (_, source) in self.attributes()? {
            builder = builder.with_source(source);
        }
        builder
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_wants([Input::ClientIp, Input::PageUrl])
                    .with_cookie(FIRST_PARTY_COOKIE),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(LIVEINTENT_GVL_ID))
            .build()
    }
}

/// The `LiveIntent` provider.
pub struct LiveIntentProvider {
    descriptor: ProviderDescriptor,
    path: String,
    distributor_id: Option<String>,
    attributes: Vec<(String, SourceDomain)>,
}

impl LiveIntentProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config names an
    /// unknown attribute or cannot describe a valid provider.
    pub fn new(config: &LiveIntentConfig) -> Result<Self, Report<IdentityProviderError>> {
        let distributor = config.distributor_id.as_deref().unwrap_or("prebid");
        Ok(Self {
            descriptor: config.build_descriptor()?,
            path: format!("{distributor}/{}", config.publisher_id),
            distributor_id: config.distributor_id.clone(),
            attributes: config.attributes()?,
        })
    }
}

impl IdentityProvider for LiveIntentProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }
}
```

The first configured attribute's source is the descriptor's primary source. Because an unknown attribute fails `build_descriptor`, `ts config validate` refuses it at deploy time too.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/integrations/liveintent.rs crates/trusted-server-core/src/integrations/mod.rs
git commit -S --signoff -m "Describe the LiveIntent provider from its configured attributes"
```

---

### Task 2: Build the identity exchange request

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/liveintent.rs`

**Interfaces:**

- Consumes: `ResolveInput::{endpoint, publisher_domain, cookie, consent, page_url, client_ip}`, `ProviderRequest::{get, try_with_header, with_query}`, `is_routable_ip`.
- Produces: `LiveIntentProvider::request`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn calls_the_exchange_with_cookie_consent_and_attributes() {
        let provider = provider(&["nonId", "thetradedesk"]);
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_page_url("https://www.example.com/article?x=1")
            .with_cookie(FIRST_PARTY_COOKIE, "fpi-example")
            .with_consent(TestConsent::default().with_us_privacy("1YNN"));

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        let url = request.url();
        assert_eq!(
            url.path(),
            "/idex/prebid/12345",
            "should address the publisher"
        );
        assert_eq!(
            request
                .headers()
                .get("origin")
                .map(http::HeaderValue::as_bytes),
            Some(b"https://www.example.com".as_slice()),
            "should send the publisher origin, which LiveIntent requires"
        );
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let value = |name: &str| {
            query
                .iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            value("duid"),
            ["fpi-example"],
            "should send the first-party ID"
        );
        assert_eq!(value("us_privacy"), ["1YNN"], "should send US privacy");
        assert_eq!(
            value("pu"),
            ["https://www.example.com"],
            "should send the page origin"
        );
        assert_eq!(
            value("pip"),
            [URL_SAFE_NO_PAD.encode("192.0.2.10")],
            "should send the encoded IP"
        );
        assert_eq!(
            value("resolve"),
            ["nonId", "thetradedesk"],
            "should ask for each configured attribute"
        );
    }

    #[test]
    fn leaves_out_a_loopback_client_ip() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider).with_client_ip("127.0.0.1");

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert!(
            !request
                .url()
                .query_pairs()
                .any(|(key, _)| key == "pip" || key == "pip6"),
            "should not send an address LiveIntent cannot match"
        );
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`
Expected: the two new tests FAIL with `NotImplemented("request")`.

- [ ] **Step 3: Implement `request`**

Add to `impl IdentityProvider for LiveIntentProvider`:

```rust
    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let mut url = input.endpoint()?.clone();
        url.path_segments_mut()
            .map_err(|_| IdentityProviderError::config("endpoint cannot hold a path"))?
            .pop_if_empty()
            .extend(self.path.split('/'));
        let mut request = ProviderRequest::get(&url)
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))?;
        if let Some(distributor) = &self.distributor_id {
            request = request.with_query("did", distributor);
        }
        if let Some(fpid) = input.cookie(FIRST_PARTY_COOKIE)? {
            request = request.with_query("duid", fpid);
        }
        let consent = input.consent();
        request = request.with_query("gdpr", if consent.gdpr_applies() { "1" } else { "0" });
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("gdpr_consent", tc_string);
        }
        if let Some(us_privacy) = consent.raw_us_privacy() {
            request = request.with_query("us_privacy", us_privacy);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp_s", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_as", &ids.join(","));
        }
        if let Ok(page) = input.page_url() {
            request = request.with_query("pu", page.origin().ascii_serialization().as_str());
        }
        if let Ok(ip) = input.client_ip()
            && is_routable_ip(ip)
        {
            let name = if ip.is_ipv4() { "pip" } else { "pip6" };
            request = request.with_query(name, &URL_SAFE_NO_PAD.encode(ip.to_string()));
        }
        for (attribute, _) in &self.attributes {
            request = request.with_query("resolve", attribute);
        }
        Ok(Some(request))
    }
```

`input.cookie(..)?` propagates only `UndeclaredInput`, which cannot happen because the descriptor declares `_lc2_fpi`; an absent cookie is `Ok(None)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/integrations/liveintent.rs
git commit -S --signoff -m "Call the LiveIntent identity exchange with origin, first-party ID and consent"
```

---

### Task 3: Map the reply to sources

The reply is a flat object, for example `{"nonId": "...", "segments": [...], "thetradedesk": "..."}`. For each configured attribute whose source this call resolves, a non-empty string value becomes one EID with `atype` 3. `nonId` carries LiveIntent's `segments` as EID-level `ext`. Every other attribute names LiveIntent as `provider` in its UID `ext`; the Trade Desk ID also carries `rtiPartner: "TDID"` and `matcher: "liveintent.com"`, the shape bidders expect for an `adserver.org` ID from LiveIntent. A future `Expires` header sets each EID's expiry. A 204 is no match.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/liveintent.rs`

**Interfaces:**

- Consumes: `ProviderResponse::{status, success_json, header}`, `ResolveInput::{sources, now}`, `ResolvedEid::{with_ext, with_matcher, with_expires_in}`.
- Produces: `LiveIntentProvider::response`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn maps_attributes_to_sources() {
        let provider = provider(&["nonId", "thetradedesk", "pubmatic"]);
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({
                    "nonId": "li-example",
                    "segments": ["s1"],
                    "thetradedesk": "tdid-example",
                    "bidswitch": "not-configured"
                }),
            )
            .expect("should parse");

        let eids = resolution.eids();
        assert_eq!(
            eids.len(),
            2,
            "should map only configured attributes it returned"
        );
        assert_eq!(
            eids[0].source().as_str(),
            "liveintent.com",
            "should map nonId"
        );
        assert_eq!(
            eids[0].ext(),
            Some(&json!({ "segments": ["s1"] })),
            "should keep segments"
        );
        assert_eq!(
            eids[1].source().as_str(),
            "adserver.org",
            "should map the TDID"
        );
        assert_eq!(
            eids[1].matcher(),
            Some("liveintent.com"),
            "should name the matcher"
        );
    }

    #[test]
    fn no_content_is_no_match() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond_raw(StatusCode::NO_CONTENT, &[], b"")
            .expect("should accept 204");

        assert!(resolution.is_no_match(), "should be no match");
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider(&["nonId"])).check_conformance(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_cookie(FIRST_PARTY_COOKIE, "fpi-example"),
                StatusCode::OK,
                &json!({ "nonId": "li-example" }),
            ),
        );
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`
Expected: the three new tests FAIL with `NotImplemented("response")`.

- [ ] **Step 3: Implement `response` and its helpers**

Add to `impl IdentityProvider for LiveIntentProvider`, after `request`:

```rust
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(Resolution::no_match());
        }
        let reply: Map<String, Value> = response.success_json()?;
        let segments = reply.get("segments").cloned();
        let lifetime = expires_in(response, input);

        let eids: Vec<ResolvedEid> = self
            .attributes
            .iter()
            .filter(|(_, source)| input.sources().any(|(wanted, _)| wanted == source))
            .filter_map(|(attribute, source)| {
                let id = reply.get(attribute)?.as_str().filter(|id| !id.is_empty())?;
                let mut eid = ResolvedEid::new(source, resolved_uid(attribute, id));
                if attribute == NON_ID_ATTRIBUTE
                    && let Some(segments) = &segments
                {
                    eid = eid.with_ext(json!({ "segments": segments }));
                }
                if attribute == "thetradedesk" {
                    eid = eid.with_matcher("liveintent.com");
                }
                if let Some(lifetime) = lifetime {
                    eid = eid.with_expires_in(lifetime);
                }
                Some(eid)
            })
            .collect();
        Ok(if eids.is_empty() {
            Resolution::no_match()
        } else {
            Resolution::found(eids)
        })
    }
```

The `input.sources()` filter keeps the result to the sources this call is resolving, so a source the operator narrowed away with `[ec.identity.providers.liveintent].sources` is never returned.

After the `impl` block:

```rust
fn resolved_uid(attribute: &str, id: &str) -> ResolvedUid {
    let uid = ResolvedUid::new(id).with_atype(3);
    match attribute {
        NON_ID_ATTRIBUTE => uid,
        "thetradedesk" => {
            uid.with_ext(json!({ "rtiPartner": "TDID", "provider": "liveintent.com" }))
        }
        _ => uid.with_ext(json!({ "provider": "liveintent.com" })),
    }
}

/// The lifetime `LiveIntent` gives in its `Expires` header, when it is a
/// valid HTTP date in the future.
fn expires_in(response: &ProviderResponse, input: &ResolveInput<'_>) -> Option<Duration> {
    let expires = response.header("expires")?;
    let at = chrono::DateTime::parse_from_rfc2822(expires)
        .ok()?
        .timestamp();
    let seconds = u64::try_from(at).ok()?.checked_sub(input.now().as_secs())?;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}
```

`input.now()` (not the system clock) keeps the provider pure and deterministic in the harness.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveintent`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/integrations/liveintent.rs
git commit -S --signoff -m "Map LiveIntent attributes to their OpenRTB sources"
```

---

### Task 4: Register, validate and document

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/liveintent.rs` (`register`)
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (`builders()`)
- Modify: `crates/trusted-server-core/src/config.rs`
- Modify: `docs/guide/integrations/identity-vendors.md`, `CHANGELOG.md`

**Interfaces:**

- Produces: `pub fn register(settings: &Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>`.

- [ ] **Step 1: Write `register` and add the builder**

Append to `liveintent.rs`, before the tests module:

```rust
/// Registers the `LiveIntent` provider when `[integrations.liveintent]` is
/// enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<LiveIntentConfig>(LIVEINTENT_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    log::info!(
        "Registering LiveIntent identity provider (attributes={})",
        config.resolve.join(",")
    );
    Ok(Some(
        IntegrationRegistration::builder(LIVEINTENT_INTEGRATION_ID)
            .try_with_identity_provider(LiveIntentProvider::new(&config))?
            .without_js()
            .build(),
    ))
}
```

In `integrations/mod.rs` `builders()`, add:

```rust
        IntegrationBuilder {
            id: liveintent::LIVEINTENT_INTEGRATION_ID,
            build: liveintent::register,
        },
```

- [ ] **Step 2: Run the deploy-coverage test and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config::tests::deploy_validation_covers_registered_integration_builders`
Expected: FAIL with `missing_ids` containing `"liveintent"`.

- [ ] **Step 3: Validate at runtime and at deploy**

In `config.rs`, import `liveintent::{LIVEINTENT_INTEGRATION_ID, LiveIntentConfig}`. Add `LIVEINTENT_INTEGRATION_ID,` to `DEPLOY_VALIDATED_INTEGRATION_IDS`. In `identity_descriptors_for_deploy`:

```rust
    descriptors.extend(validate_identity_integration::<LiveIntentConfig>(
        settings,
        LIVEINTENT_INTEGRATION_ID,
    )?);
```

If `validate_identity_integration` still carries the framework's temporary `#[allow(dead_code)]`, remove it and its comment. In `validate_enabled_integrations`:

```rust
    validate_integration::<LiveIntentConfig>(settings, LIVEINTENT_INTEGRATION_ID)?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config`
Expected: PASS.

- [ ] **Step 5: Document**

In `docs/guide/integrations/identity-vendors.md`, add this table row:

```markdown
| `liveintent` | LiveIntent | Resolve | One per configured attribute (see below) | Nothing (`_lc2_fpi` improves match) |
```

and this section:

````markdown
## LiveIntent

```toml
[integrations.liveintent]
enabled = true
publisher_id = "12345"
# distributor_id = "did-0000"
resolve = ["nonId", "thetradedesk"]
```

`resolve` lists the attributes to ask for. Each attribute maps to one source:

| Attribute      | Source                         |
| -------------- | ------------------------------ |
| `nonId`        | `liveintent.com`               |
| `bidswitch`    | `bidswitch.net`                |
| `medianet`     | `media.net`                    |
| `magnite`      | `rubiconproject.com`           |
| `index`        | `liveintent.indexexchange.com` |
| `openx`        | `openx.net`                    |
| `pubmatic`     | `pubmatic.com`                 |
| `sovrn`        | `liveintent.sovrn.com`         |
| `sharethrough` | `sharethrough.com`             |
| `sonobi`       | `liveintent.sonobi.com`        |
| `triplelift`   | `liveintent.triplelift.com`    |
| `zetassp`      | `zeta-ssp.liveintent.com`      |
| `vidazoo`      | `liveintent.vidazoo.com`       |
| `nexxen`       | `liveintent.unrulymedia.com`   |
| `uid2`         | `uidapi.com`                   |
| `thetradedesk` | `adserver.org`                 |

Only the configured attributes are declared, so only their sources need partner entries. LiveIntent needs vendor consent for GVL ID 148. When LiveIntent's first-party `_lc2_fpi` cookie is on the request, it is sent as `duid`.
````

In `CHANGELOG.md` under `### Added`, insert "LiveIntent (`[integrations.liveintent]`)" into the vendor bullet's list of providers, or, if there is no vendor bullet yet, add:

```markdown
- Server-side identity provider for LiveIntent (`[integrations.liveintent]`). Each source it writes needs an `[[ec.partners]]` entry. See [Identity Vendors](docs/guide/integrations/identity-vendors.md).
```

- [ ] **Step 6: Run the gates**

Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly`
Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/ docs/guide/integrations/identity-vendors.md CHANGELOG.md
git commit -S --signoff -m "Register the LiveIntent identity provider and validate its config"
```

## Self-Review Notes

- Path shape, required `Origin`, `did` only when configured, `duid` from `_lc2_fpi`, routable `pip`: Task 2.
- `Expires` lifetime and 204 no match: Task 3.
- Unknown attribute refused at build and deploy: Task 1.

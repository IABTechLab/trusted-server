# LiveRamp ATS Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve and refresh LiveRamp ATS envelopes at the edge with a server-side `IdentityProvider` under `[integrations.liveramp]`, writing them to the `liveramp.com` source.

**Architecture:** `integrations/liveramp.rs` holds `LiveRampConfig`, `LiveRampProvider` and `register`. An initial lookup sends the visitor's hashed email from `ts-hem`; a refresh sends the stored envelope. The provider marks its call as initial or refresh in the request's call context, because LiveRamp's 204 means "no match" for one and "envelope no longer accepted" for the other. The descriptor declares expiring IDs, and the response sets the envelope's expiry from the visitor's region: 15 days in California, 30 days elsewhere.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `serde`, `http`, `validator`, `error-stack` (all already dependencies).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`, section 12.1 (LiveRamp ATS) and sections 4.2 to 4.6 (the provider API). Section 12.1 marks the response shape and consent parameter names as assumed; this plan keeps them and the operator doc asks operators to confirm them with LiveRamp. Operator page: `docs/guide/integrations/identity-vendors.md`.

**Prerequisite:** `docs/superpowers/plans/2026-10-28-identity-provider-inputs.md`. This plan needs `Input::HashedEmail` from the `ts-hem` cookie for the initial lookup, and adds its section to the operator page that plan creates.

## Design notes

- **Initial versus refresh.** `input.is_refresh(source)` with a stored envelope picks a refresh (`it=19`, `iv=<envelope>`); anything else is an initial lookup (`it=4`, `iv=<hex sha256>`). The framework attempts a refresh for an expired envelope too, and LiveRamp answers 204 for one it no longer accepts, so a refresh 204 invalidates the stored envelope while an initial 204 is just no match.
- **Hashed email is needed only for `Initial`** (`with_needs_for(ResolveKind::Initial, [Input::HashedEmail])`). A visitor without `ts-hem` is never sent an initial lookup, but a stored envelope still refreshes on later visits.
- **Client IP and region are needed.** The IP is forwarded in `X-Forwarded-For`; the region decides the envelope's lifetime.
- **Lifetime.** Envelopes expire after 15 days for visitors in California (`US`/`CA`) and 30 days elsewhere. The provider sets no refresh hint, so the operator's `refresh_after_sec` paces refreshes.
- **Product selection.** The reply lists envelope products; the provider keeps the regular ATS envelope (`type` 19, `source` `envelopeLiveramp`) with a non-empty value.

## Global Constraints

- Follow `AGENTS.md`: `error-stack` with `derive_more::Display`, `expect("should ...")`, no local imports, comments above code, doc comment on every public item, Arrange-Act-Assert tests.
- Backtick `LiveRamp` and other CamelCase names in doc comments (`clippy::doc_markdown`).
- Tests use fictional data: `ats.example.com` endpoint, documentation IPs, fake envelopes (`envelope-example`). LiveRamp's public default endpoint host and the `liveramp.com` source may appear.
- Never log envelopes, the hashed email, the client IP or consent strings. The placement ID is an operator identifier, not a secret.
- LiveRamp's IAB GVL ID is **97**. UIDs use `atype` **3**. Expiry: **15** days in `US`/`CA`, **30** days elsewhere.
- Quick test: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveramp`. Gates: `cargo test-fastly`, `cargo clippy-fastly`.

## File Structure

| File                                                      | Change | Responsibility                                          |
| --------------------------------------------------------- | ------ | ------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/liveramp.rs` | Create | Config, descriptor, provider, registration, tests       |
| `crates/trusted-server-core/src/integrations/mod.rs`      | Modify | `pub mod liveramp;` and its `IntegrationBuilder`        |
| `crates/trusted-server-core/src/config.rs`                | Modify | Runtime and deploy validation, deploy-validated ID list |
| `docs/guide/integrations/identity-vendors.md`             | Modify | Table row and `## LiveRamp ATS` section                 |
| `CHANGELOG.md`                                            | Modify | Vendor bullet                                           |

---

### Task 1: Config, descriptor and request

**Files:**

- Create: `crates/trusted-server-core/src/integrations/liveramp.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (add `pub mod liveramp;` in alphabetical order)

**Interfaces:**

- Consumes: `ResolveInput::{primary_source, stored, is_refresh, hashed_email, consent, endpoint, publisher_domain, client_ip}`, `ProviderRequest::{get, with_query, try_with_call_context, try_with_header}`, `HashedEmail::to_hex`.
- Produces: `pub(crate) const LIVERAMP_INTEGRATION_ID: &str = "liveramp"`, `pub struct LiveRampConfig { enabled, endpoint: String, placement_id: String }`, `pub struct LiveRampProvider`, `pub fn LiveRampProvider::new(&LiveRampConfig) -> Result<Self, Report<IdentityProviderError>>`, `enum LiveRampCall { Initial, Refresh }` (call context).

- [ ] **Step 1: Write the failing tests**

Create `liveramp.rs` with the tests module only:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, check_descriptor_ignores_secrets,
    };
    use crate::ec::identity::{HashedEmail, SourceDomain, StoredId};

    fn config() -> LiveRampConfig {
        LiveRampConfig {
            enabled: true,
            endpoint: "https://ats.example.com/api/identity/v2/envelope".to_owned(),
            placement_id: "placement-1".to_owned(),
        }
    }

    fn provider() -> LiveRampProvider {
        LiveRampProvider::new(&config()).expect("should build the provider")
    }

    fn source() -> SourceDomain {
        "liveramp.com".parse().expect("should parse the source")
    }

    fn query(request: &ProviderRequest, name: &str) -> Option<String> {
        request
            .url()
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    #[test]
    fn initial_lookup_sends_the_hashed_email() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("NY"))
            .with_hashed_email(HashedEmail::for_test([0xab; 32]));

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert_eq!(
            query(&request, "it").as_deref(),
            Some("4"),
            "should be a HEM lookup"
        );
        assert_eq!(
            query(&request, "iv"),
            Some("ab".repeat(32)),
            "should send the HEM hex"
        );
        assert_eq!(
            request
                .headers()
                .get("x-forwarded-for")
                .map(http::HeaderValue::as_bytes),
            Some(b"192.0.2.10".as_slice()),
            "should forward the client IP"
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(), &config());
    }
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveramp`
Expected: FAIL to compile (`LiveRampConfig` not found).

- [ ] **Step 3: Implement the config and descriptor**

Prepend to `liveramp.rs`. The `use` block is the module's final one; items used only by Task 2 warn as unused until then.

```rust
//! `LiveRamp` ATS identity provider: resolves and refreshes ATS envelopes
//! server side.
//!
//! An initial lookup sends the hashed email from the `ts-hem` cookie. A
//! refresh sends the stored envelope. Envelopes expire after 15 days in
//! California and 30 days elsewhere. The response shape and the consent
//! parameter names are assumed (spec 12.1) and should be confirmed with
//! `LiveRamp`.

use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolveKind,
    ResolvedEid, ResolvedUid, Resolves,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::integrations::vendor_request::is_routable_ip;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `LiveRamp` provider.
pub(crate) const LIVERAMP_INTEGRATION_ID: &str = "liveramp";

/// `LiveRamp`'s IAB Global Vendor List ID.
const LIVERAMP_GVL_ID: u16 = 97;

/// The regular ATS envelope product.
const ATS_ENVELOPE_TYPE: u32 = 19;
const ATS_ENVELOPE_SOURCE: &str = "envelopeLiveramp";

/// Identifier types: 4 is a hashed email, 19 an envelope refresh.
const IT_HASHED_EMAIL: &str = "4";
const IT_ENVELOPE: &str = "19";

/// Consent type 4: an IAB TCF v2 string.
const CT_TCF: &str = "4";

const SECONDS_PER_DAY: u64 = 86_400;

/// Config of the `liveramp` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct LiveRampConfig {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The ATS envelope endpoint.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
    /// `LiveRamp` placement ID. An operator identifier, not a secret.
    #[validate(length(min = 1))]
    pub placement_id: String,
}

fn default_endpoint() -> String {
    "https://api.rlcdn.com/api/identity/v2/envelope".to_owned()
}

impl IntegrationConfig for LiveRampConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for LiveRampConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(LIVERAMP_INTEGRATION_ID.parse()?)
            .with_source("liveramp.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::Region])
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(LIVERAMP_GVL_ID))
            .build()
    }
}
```

- [ ] **Step 4: Implement the provider and its request**

Append, before the tests module:

```rust
/// The `LiveRamp` ATS provider.
pub struct LiveRampProvider {
    descriptor: ProviderDescriptor,
    placement_id: String,
}

impl LiveRampProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &LiveRampConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            placement_id: config.placement_id.clone(),
        })
    }
}

/// Whether the call was an initial lookup or a refresh, carried from
/// `request` to `response`: a 204 means different things for each.
#[derive(Debug, Serialize, Deserialize)]
enum LiveRampCall {
    Initial,
    Refresh,
}

impl IdentityProvider for LiveRampProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let source = input.primary_source();
        let (call, identifier_type, identifier_value) = match input.stored(source) {
            Some(stored) if input.is_refresh(source) => {
                (LiveRampCall::Refresh, IT_ENVELOPE, stored.uid().to_owned())
            }
            _ => (
                LiveRampCall::Initial,
                IT_HASHED_EMAIL,
                input.hashed_email()?.to_hex(),
            ),
        };
        let consent = input.consent();
        let mut request = ProviderRequest::get(input.endpoint()?)
            .with_query("pid", &self.placement_id)
            .with_query("it", identifier_type)
            .with_query("iv", &identifier_value)
            .with_query("atype", "3")
            .try_with_call_context(&call)?
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))?;
        // A loopback or private address identifies nobody to LiveRamp.
        let client_ip = input.client_ip()?;
        if is_routable_ip(client_ip) {
            request = request.try_with_header("x-forwarded-for", &client_ip.to_string())?;
        }
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("ct", CT_TCF).with_query("cv", tc_string);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_sid", &ids.join(","));
        }
        Ok(Some(request))
    }
}
```

`input.hashed_email()?` in the initial arm is safe: eligibility only schedules an `Initial` when the hashed email is available, because the descriptor needs it for that kind.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveramp`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/liveramp.rs crates/trusted-server-core/src/integrations/mod.rs
git commit -S --signoff -m "Build LiveRamp ATS lookup and refresh requests"
```

---

### Task 2: Parse envelopes, set the lifetime, invalidate a refused refresh

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/liveramp.rs`

**Interfaces:**

- Consumes: `ProviderResponse::{call_context, status, success_json}`, `ResolveInput::region` (`Region::is(country, subdivision)`), `ResolvedEid::with_expires_in`, `Resolution::{found, no_match, invalidate}`.
- Produces: `LiveRampProvider::response`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn california_envelopes_expire_after_15_days() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("CA"))
            .with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "envelopes": [
                    { "type": 19, "source": "envelopeLiveramp", "value": "envelope-example" }
                ]}),
            )
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(
            eid.uids()[0].id(),
            "envelope-example",
            "should keep the envelope"
        );
        assert_eq!(
            eid.expiry().map(|when| harness.resolve(when)),
            Some(crate::ec::identity::UnixTime::from_secs(
                1_760_000_000 + 15 * SECONDS_PER_DAY
            )),
            "should expire after 15 days in California"
        );
    }

    #[test]
    fn a_refused_refresh_invalidates_the_envelope() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("NY"))
            .with_stored(
                StoredId::for_test(&source(), "old-envelope")
                    .expiring_at(crate::ec::identity::UnixTime::from_secs(1)),
            );

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");
        assert_eq!(
            query(&request, "it").as_deref(),
            Some("19"),
            "should be a refresh"
        );

        let resolution = harness
            .respond_raw(StatusCode::NO_CONTENT, &[], b"")
            .expect("should accept 204");
        assert_eq!(
            resolution.invalidated(),
            [source()],
            "should invalidate the envelope"
        );
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default()
                    .with_client_ip("192.0.2.10")
                    .with_region("US", Some("NY"))
                    .with_hashed_email(HashedEmail::for_test([0xab; 32])),
                StatusCode::OK,
                &json!({ "envelopes": [
                    { "type": 19, "source": "envelopeLiveramp", "value": "envelope-example" }
                ]}),
            ),
        );
    }
```

`1_760_000_000` is the harness's fixed "now". The refresh test stores an envelope that expired at second 1: the framework still schedules a `Refresh` for an expired record of an expiring-ID provider, and the refresh request works without a hashed email.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveramp`
Expected: the three new tests FAIL with `NotImplemented("response")`.

- [ ] **Step 3: Add the reply types**

Append after `LiveRampCall`:

```rust
#[derive(Deserialize)]
struct EnvelopeReply {
    #[serde(default)]
    envelopes: Vec<EnvelopeProduct>,
}

#[derive(Deserialize)]
struct EnvelopeProduct {
    #[serde(rename = "type")]
    product_type: u32,
    source: String,
    value: String,
}
```

- [ ] **Step 4: Implement `response`**

Add to `impl IdentityProvider for LiveRampProvider`, after `request`:

```rust
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let source = input.primary_source();
        let call: LiveRampCall = response
            .call_context()?
            .ok_or_else(|| IdentityProviderError::config("missing call context"))?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(match call {
                LiveRampCall::Refresh => Resolution::invalidate(source),
                LiveRampCall::Initial => Resolution::no_match(),
            });
        }
        let reply: EnvelopeReply = response.success_json()?;
        let Some(envelope) = reply.envelopes.into_iter().find(|product| {
            product.product_type == ATS_ENVELOPE_TYPE
                && product.source == ATS_ENVELOPE_SOURCE
                && !product.value.is_empty()
        }) else {
            return Ok(Resolution::no_match());
        };
        let lifetime_days = if input.region()?.is("US", "CA") {
            15
        } else {
            30
        };
        let eid = ResolvedEid::new(source, ResolvedUid::new(envelope.value).with_atype(3))
            .with_expires_in(Duration::from_secs(lifetime_days * SECONDS_PER_DAY));
        Ok(Resolution::found([eid]))
    }
```

A missing call context is a provider bug (the request always sets one), so it is a `Config` error. A non-2xx other than 204 goes through `success_json` as `Rejected` and backs off this visitor only.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::liveramp`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/liveramp.rs
git commit -S --signoff -m "Store LiveRamp envelopes with region lifetimes and invalidate refused refreshes"
```

---

### Task 3: Register, validate and document

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/liveramp.rs` (`register`)
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (`builders()`)
- Modify: `crates/trusted-server-core/src/config.rs`
- Modify: `docs/guide/integrations/identity-vendors.md`, `CHANGELOG.md`

**Interfaces:**

- Produces: `pub fn register(settings: &Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>`.

- [ ] **Step 1: Write `register` and add the builder**

Append to `liveramp.rs`, before the tests module:

```rust
/// Registers the `LiveRamp` provider when `[integrations.liveramp]` is
/// enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<LiveRampConfig>(LIVERAMP_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    log::info!(
        "Registering LiveRamp ATS identity provider (placement={})",
        config.placement_id
    );
    Ok(Some(
        IntegrationRegistration::builder(LIVERAMP_INTEGRATION_ID)
            .try_with_identity_provider(LiveRampProvider::new(&config))?
            .without_js()
            .build(),
    ))
}
```

In `integrations/mod.rs` `builders()`, add:

```rust
        IntegrationBuilder {
            id: liveramp::LIVERAMP_INTEGRATION_ID,
            build: liveramp::register,
        },
```

- [ ] **Step 2: Run the deploy-coverage test and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config::tests::deploy_validation_covers_registered_integration_builders`
Expected: FAIL with `missing_ids` containing `"liveramp"`.

- [ ] **Step 3: Validate at runtime and at deploy**

In `config.rs`, import `liveramp::{LIVERAMP_INTEGRATION_ID, LiveRampConfig}`. Add `LIVERAMP_INTEGRATION_ID,` to `DEPLOY_VALIDATED_INTEGRATION_IDS`. In `identity_descriptors_for_deploy`:

```rust
    descriptors.extend(validate_identity_integration::<LiveRampConfig>(
        settings,
        LIVERAMP_INTEGRATION_ID,
    )?);
```

If `validate_identity_integration` still carries the framework's temporary `#[allow(dead_code)]`, remove it and its comment. In `validate_enabled_integrations`:

```rust
    validate_integration::<LiveRampConfig>(settings, LIVERAMP_INTEGRATION_ID)?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config`
Expected: PASS.

- [ ] **Step 5: Document**

In `docs/guide/integrations/identity-vendors.md`, add this table row:

```markdown
| `liveramp` | LiveRamp ATS | Resolve | `liveramp.com` | Hashed email for a first envelope |
```

and this section:

````markdown
## LiveRamp ATS

```toml
[integrations.liveramp]
enabled = true
placement_id = "placement-1"
# endpoint = "https://api.rlcdn.com/api/identity/v2/envelope"
```

A first envelope needs the hashed email. A refresh sends the stored envelope instead, so it works on later visits without one. Envelopes expire after 15 days in California and 30 days elsewhere. Calls need vendor consent for GVL ID 97. The response shape and consent parameter names follow the design spec's assumptions; confirm them with LiveRamp.
````

In `CHANGELOG.md` under `### Added`, insert "LiveRamp ATS (`[integrations.liveramp]`)" into the vendor bullet's list of providers, or, if there is no vendor bullet yet, add:

```markdown
- Server-side identity provider for LiveRamp ATS (`[integrations.liveramp]`). Each source it writes needs an `[[ec.partners]]` entry. See [Identity Vendors](docs/guide/integrations/identity-vendors.md).
```

- [ ] **Step 6: Run the gates**

Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly`
Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/ docs/guide/integrations/identity-vendors.md CHANGELOG.md
git commit -S --signoff -m "Register the LiveRamp ATS identity provider and validate its config"
```

## Self-Review Notes

- Initial (`it=4`, HEM) versus refresh (`it=19`, envelope) and the call context: Task 1.
- 204 meaning per call kind, product selection, 15-day California / 30-day lifetime: Task 2.

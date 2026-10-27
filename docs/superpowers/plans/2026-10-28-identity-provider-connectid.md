# Yahoo ConnectID Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve Yahoo ConnectID at the edge with a server-side `IdentityProvider` under `[integrations.connectid]`, writing it to the `yahoo.com` source.

**Architecture:** `integrations/connectid.rs` holds `ConnectIdConfig`, `ConnectIdProvider` and `register`. The provider sends one `GET` to Yahoo's user-profile sync endpoint (`<endpoint>/<pixel_id>/fed`) with the hashed email from `ts-hem`, the page URL and the consent strings. Yahoo returns the ConnectID and a lifetime in hours, which becomes the ID's refresh time.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `serde`, `http`, `validator`, `error-stack` (all already dependencies).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`, sections 4.2 to 4.6 (the provider API) and section 12 (vendor examples). Operator page: `docs/guide/integrations/identity-vendors.md`.

**Prerequisite:** `docs/superpowers/plans/2026-10-28-identity-provider-inputs.md`. ConnectID needs `Input::HashedEmail` from the `ts-hem` cookie, so it is called only for visitors whose page set one.

## Design notes

- **The hashed email is needed** for every call (`with_needs([Input::HashedEmail])`); Yahoo has nothing to match on without it. It is sent as `he`, lowercase hex.
- **Two spellings of the ID.** Yahoo returns both `connectId` and `connectid` in one reply, often together. Two serde aliases on one field would fail such a reply as a duplicate field, so the reply type has two fields and the provider takes `connectId` first, then `connectid`.
- **`ttl` is in hours.** A positive `ttl` sets the refresh time; zero or absent leaves pacing to `refresh_after_sec`.
- A 204, an empty reply or an empty ID is no match.

## Global Constraints

- Follow `AGENTS.md`: `error-stack` with `derive_more::Display`, `expect("should ...")`, no local imports, comments above code, doc comment on every public item, Arrange-Act-Assert tests.
- Backtick `ConnectID` and other CamelCase names in doc comments (`clippy::doc_markdown`).
- Tests use fictional data: `ups.example.com` endpoint, `www.example.com` pages, fake IDs (`cid-example`). The public default endpoint host and the `yahoo.com` source may appear.
- Never log the hashed email, the ConnectID or consent strings.
- Yahoo's IAB GVL ID is **25**. UIDs use `atype` **3**.
- Quick test: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::connectid`. Gates: `cargo test-fastly`, `cargo clippy-fastly`.

## File Structure

| File                                                       | Change | Responsibility                                          |
| ---------------------------------------------------------- | ------ | ------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/connectid.rs` | Create | Config, descriptor, provider, registration, tests       |
| `crates/trusted-server-core/src/integrations/mod.rs`       | Modify | `pub mod connectid;` and its `IntegrationBuilder`       |
| `crates/trusted-server-core/src/config.rs`                 | Modify | Runtime and deploy validation, deploy-validated ID list |
| `docs/guide/integrations/identity-vendors.md`              | Modify | Table row and `## Yahoo ConnectID` section              |
| `CHANGELOG.md`                                             | Modify | Vendor bullet                                           |

---

### Task 1: Config, descriptor and request

**Files:**

- Create: `crates/trusted-server-core/src/integrations/connectid.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (add `pub mod connectid;` in alphabetical order)

**Interfaces:**

- Consumes: `ResolveInput::{endpoint, consent, hashed_email, page_url}`, `HashedEmail::to_hex`, `ProviderRequest::{get, with_query}`.
- Produces: `pub(crate) const CONNECTID_INTEGRATION_ID: &str = "connectid"`, `pub struct ConnectIdConfig { enabled, pixel_id: u64, endpoint: String }`, `pub struct ConnectIdProvider`, `pub fn ConnectIdProvider::new(&ConnectIdConfig) -> Result<Self, Report<IdentityProviderError>>`.

- [ ] **Step 1: Write the failing tests**

Create `connectid.rs` with the tests module only:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ec::identity::HashedEmail;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, check_descriptor_ignores_secrets,
    };

    fn config() -> ConnectIdConfig {
        ConnectIdConfig {
            enabled: true,
            pixel_id: 58_776,
            endpoint: "https://ups.example.com/ups".to_owned(),
        }
    }

    fn provider() -> ConnectIdProvider {
        ConnectIdProvider::new(&config()).expect("should build the provider")
    }

    #[test]
    fn sends_the_hashed_email_to_the_pixel() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_hashed_email(HashedEmail::for_test([0xab; 32]))
            .with_page_url("https://www.example.com/article");

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert_eq!(
            request.url().path(),
            "/ups/58776/fed",
            "should address the pixel"
        );
        let he = request
            .url()
            .query_pairs()
            .find(|(key, _)| key == "he")
            .map(|(_, value)| value.into_owned());
        assert_eq!(he, Some("ab".repeat(32)), "should send the HEM hex");
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(), &config());
    }
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::connectid`
Expected: FAIL to compile (`ConnectIdConfig` not found).

- [ ] **Step 3: Implement the config and descriptor**

Prepend to `connectid.rs`. The `use` block is the module's final one; items used only by Task 2 warn as unused until then.

```rust
//! Yahoo `ConnectID` identity provider: resolves `ConnectID` server side.
//!
//! Calls Yahoo's user-profile sync endpoint with the hashed email from the
//! `ts-hem` cookie, the page and the consent strings. Yahoo returns the
//! `ConnectID` and a lifetime in hours.

use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use serde::Deserialize;
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolvedEid,
    ResolvedUid, Resolves,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `ConnectID` provider.
pub(crate) const CONNECTID_INTEGRATION_ID: &str = "connectid";

/// Yahoo's IAB Global Vendor List ID.
const YAHOO_GVL_ID: u16 = 25;

const SECONDS_PER_HOUR: u64 = 3_600;

/// Config of the `connectid` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct ConnectIdConfig {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The pixel ID Yahoo assigned to the publisher.
    pub pixel_id: u64,
    /// The sync endpoint; the pixel ID and `fed` are appended to its path.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
}

fn default_endpoint() -> String {
    "https://ups.analytics.yahoo.com/ups".to_owned()
}

impl IntegrationConfig for ConnectIdConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for ConnectIdConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(CONNECTID_INTEGRATION_ID.parse()?)
            .with_source("yahoo.com".parse()?)
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::HashedEmail])
                    .with_wants([Input::PageUrl]),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(YAHOO_GVL_ID))
            .build()
    }
}
```

- [ ] **Step 4: Implement the provider and its request**

Append, before the tests module:

```rust
/// The Yahoo `ConnectID` provider.
pub struct ConnectIdProvider {
    descriptor: ProviderDescriptor,
    pixel_id: String,
}

impl ConnectIdProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &ConnectIdConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            pixel_id: config.pixel_id.to_string(),
        })
    }
}

impl IdentityProvider for ConnectIdProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let mut url = input.endpoint()?.clone();
        url.path_segments_mut()
            .map_err(|_| IdentityProviderError::config("endpoint cannot hold a path"))?
            .pop_if_empty()
            .push(&self.pixel_id)
            .push("fed");
        let consent = input.consent();
        let mut request = ProviderRequest::get(&url)
            .with_query("v", "1")
            .with_query("1p", "0")
            .with_query("pixelId", &self.pixel_id)
            .with_query("he", &input.hashed_email()?.to_hex())
            .with_query("gdpr", if consent.gdpr_applies() { "1" } else { "0" });
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("gdpr_consent", tc_string);
        }
        if let Some(us_privacy) = consent.raw_us_privacy() {
            request = request.with_query("us_privacy", us_privacy);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_sid", &ids.join(","));
        }
        if let Ok(page) = input.page_url() {
            request = request.with_query("url", page.as_str());
        }
        Ok(Some(request))
    }
}
```

The page URL the framework hands over has no query or fragment, so `url` never carries page parameters.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::connectid`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/connectid.rs crates/trusted-server-core/src/integrations/mod.rs
git commit -S --signoff -m "Build Yahoo ConnectID sync requests from the hashed email"
```

---

### Task 2: Parse the ConnectID and its lifetime

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/connectid.rs`

**Interfaces:**

- Consumes: `ProviderResponse::{status, success_json}`, `ResolvedEid::with_refresh_in`.
- Produces: `ConnectIdProvider::response`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn resolves_the_connect_id_with_its_ttl() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "connectId": "cid-example", "connectid": "cid-example", "ttl": 24 }),
            )
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(eid.source().as_str(), "yahoo.com", "should use yahoo.com");
        assert_eq!(eid.uids()[0].id(), "cid-example", "should keep the ID");
        assert!(eid.refresh().is_some(), "should set a refresh from the ttl");
    }

    #[test]
    fn an_empty_reply_is_no_match() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(StatusCode::OK, &json!({}))
            .expect("should parse");

        assert!(resolution.is_no_match(), "should be no match");
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_hashed_email(HashedEmail::for_test([0xab; 32])),
                StatusCode::OK,
                &json!({ "connectId": "cid-example" }),
            ),
        );
    }
```

The first test sends both spellings in one reply, as Yahoo does, which is the case a two-alias field would fail.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::connectid`
Expected: the three new tests FAIL with `NotImplemented("response")`.

- [ ] **Step 3: Add the reply type**

Append after `impl ConnectIdProvider`:

```rust
#[derive(Deserialize)]
struct SyncReply {
    #[serde(rename = "connectId")]
    connect_id: Option<String>,
    /// The lowercase spelling Yahoo also sends, often next to `connectId`.
    connectid: Option<String>,
    /// Hours.
    ttl: Option<u64>,
}
```

- [ ] **Step 4: Implement `response`**

Add to `impl IdentityProvider for ConnectIdProvider`, after `request`:

```rust
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(Resolution::no_match());
        }
        let reply: SyncReply = response.success_json()?;
        // An empty `connectId` must not hide a usable `connectid`.
        let Some(connect_id) = [reply.connect_id, reply.connectid]
            .into_iter()
            .flatten()
            .find(|id| !id.is_empty())
        else {
            return Ok(Resolution::no_match());
        };
        let mut eid = ResolvedEid::new(
            input.primary_source(),
            ResolvedUid::new(connect_id).with_atype(3),
        );
        if let Some(hours) = reply.ttl.filter(|hours| *hours > 0) {
            eid = eid.with_refresh_in(Duration::from_secs(hours.saturating_mul(SECONDS_PER_HOUR)));
        }
        Ok(Resolution::found([eid]))
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::connectid`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/connectid.rs
git commit -S --signoff -m "Resolve the Yahoo ConnectID with its hour-based lifetime"
```

---

### Task 3: Register, validate and document

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/connectid.rs` (`register`)
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (`builders()`)
- Modify: `crates/trusted-server-core/src/config.rs`
- Modify: `docs/guide/integrations/identity-vendors.md`, `CHANGELOG.md`

**Interfaces:**

- Produces: `pub fn register(settings: &Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>`.

- [ ] **Step 1: Write `register` and add the builder**

Append to `connectid.rs`, before the tests module:

```rust
/// Registers the `ConnectID` provider when `[integrations.connectid]` is
/// enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<ConnectIdConfig>(CONNECTID_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    log::info!(
        "Registering Yahoo ConnectID identity provider (pixel={})",
        config.pixel_id
    );
    Ok(Some(
        IntegrationRegistration::builder(CONNECTID_INTEGRATION_ID)
            .try_with_identity_provider(ConnectIdProvider::new(&config))?
            .without_js()
            .build(),
    ))
}
```

In `integrations/mod.rs` `builders()`, add:

```rust
        IntegrationBuilder {
            id: connectid::CONNECTID_INTEGRATION_ID,
            build: connectid::register,
        },
```

- [ ] **Step 2: Run the deploy-coverage test and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config::tests::deploy_validation_covers_registered_integration_builders`
Expected: FAIL with `missing_ids` containing `"connectid"`.

- [ ] **Step 3: Validate at runtime and at deploy**

In `config.rs`, import `connectid::{CONNECTID_INTEGRATION_ID, ConnectIdConfig}`. Add `CONNECTID_INTEGRATION_ID,` to `DEPLOY_VALIDATED_INTEGRATION_IDS`. In `identity_descriptors_for_deploy`:

```rust
    descriptors.extend(validate_identity_integration::<ConnectIdConfig>(
        settings,
        CONNECTID_INTEGRATION_ID,
    )?);
```

If `validate_identity_integration` still carries the framework's temporary `#[allow(dead_code)]`, remove it and its comment. In `validate_enabled_integrations`:

```rust
    validate_integration::<ConnectIdConfig>(settings, CONNECTID_INTEGRATION_ID)?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config`
Expected: PASS.

- [ ] **Step 5: Document**

In `docs/guide/integrations/identity-vendors.md`, add this table row:

```markdown
| `connectid` | Yahoo ConnectID | Resolve | `yahoo.com` | Hashed email |
```

and this section:

````markdown
## Yahoo ConnectID

```toml
[integrations.connectid]
enabled = true
pixel_id = 12345
# endpoint = "https://ups.analytics.yahoo.com/ups"
```

ConnectID needs the hashed email and vendor consent for GVL ID 25. Yahoo's `ttl`, in hours, sets when the ID is refreshed.
````

In `CHANGELOG.md` under `### Added`, insert "Yahoo ConnectID (`[integrations.connectid]`)" into the vendor bullet's list of providers, or, if there is no vendor bullet yet, add:

```markdown
- Server-side identity provider for Yahoo ConnectID (`[integrations.connectid]`). Each source it writes needs an `[[ec.partners]]` entry. See [Identity Vendors](docs/guide/integrations/identity-vendors.md).
```

- [ ] **Step 6: Run the gates**

Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly`
Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/ docs/guide/integrations/identity-vendors.md CHANGELOG.md
git commit -S --signoff -m "Register the Yahoo ConnectID identity provider and validate its config"
```

## Self-Review Notes

- Hashed email needed, sent as hex `he`: Task 1.
- `connectId` and `connectid` as two fields, `ttl` in hours, 204 and empty no match: Task 2.

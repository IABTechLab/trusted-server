# Lockr Capture Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the existing Lockr integration register, behind `[integrations.lockr].capture_identity`, as a capture-only identity provider that removes Lockr-issued IDs from KV when Lockr's API reports a consent revocation.

**Architecture:** Trusted Server already proxies Lockr's API under `/integrations/lockr/api/...`. A new private module, `integrations/lockr_identity.rs`, implements `IdentityProviderConfig` for the existing `LockrConfig` and an `IdentityProvider` whose descriptor declares capture rules for five exact Lockr API paths and the sources Lockr's tokens carry. The framework offers each proxied 2xx response on those paths to `capture`. Capture is revoke-only: a `revoke-consent` call returns an opt-out, which the framework writes at once. Token responses are parsed into EIDs, but the framework never stores a positive capture; a positive result only clears an earlier Lockr opt-out once Lockr issues again. Lockr's IDs themselves keep reaching KV through browser ingest.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `serde`, `http`, `error-stack` (all already dependencies).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`, section 12.2 (Lockr, passive capture), section 4.7 (capture is revoke-only; removals bypass the consent gate; only a real browser's capture writes), section 8 (opt-out clearing) and section 12.5 (`Captures::with_path`, `with_provenance`, `SourceDomain::matches`). Operator page: `docs/guide/integrations/identity-vendors.md`; existing Lockr page: `docs/guide/integrations/lockr.md`.

**Prerequisite:** `docs/superpowers/plans/2026-10-28-identity-provider-inputs.md`. This plan uses `VendorEid` (`integrations/vendor_eid.rs`) to read the complete EIDs in Lockr's token responses, and adds its section to the operator page that plan creates.

## Design notes

- **Revoke-only.** The coordinator's `run_capture` writes a real browser's removals, opt-outs and clears with the removal write, whatever the consent, and counts and discards any positive ID. So the only effect that reaches KV from this provider is the opt-out on `revoke-consent` and, later, its clear.
- **Why parse tokens at all.** For a capture-only provider without a GVL ID, a positive capture is the one signal that the visitor is back: Lockr only issues tokens again after re-consent. The coordinator clears the opt-out when a positive capture passes the consent steps. Parsing also lets `check_conformance` verify the provider's EIDs.
- **Exact paths.** `Captures::with_path(Method::POST, path)` matches each Lockr call exactly, rather than a prefix that would also capture unrelated Lockr calls.
- **Known key mappings only.** Lockr's `ids` map is keyed by token name. Only confirmed names map to sources: `id5id` to `id5-sync.com`, `_lr_env` to `liveramp.com`, `__uid2_advertising_token` to `uidapi.com`. An entry whose EID `source` does not match the mapped source (normalized with `SourceDomain::matches`) is ignored. An expired token (`aimTokens[].identity_expires`, milliseconds) is skipped.
- **Provenance.** ID5 IDs passed through Lockr carry `inserter: "Lockr-For-Publishers"` and `mm: 2`, declared once with `with_provenance` and applied on output.
- **Sources must be listed.** A capture-only provider must list `[ec.identity.providers.lockr].sources`; framework validation enforces it.

## Global Constraints

- Follow `AGENTS.md`: `error-stack` with `derive_more::Display`, `expect("should ...")`, no local imports, comments above code, doc comment on every public item, Arrange-Act-Assert tests.
- Tests use fictional data: `app-1` app ID, fake IDs (`ID5*example`), `inserter.example.com`. Public source domains (`id5-sync.com`, `liveramp.com`, `uidapi.com`) may appear.
- Never log tokens, UIDs or the proxied request and response bodies.
- `capture_identity` defaults to `false`; with it off, nothing about the Lockr integration changes.
- Quick test: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr` (this filter covers both `lockr` and `lockr_identity`). Gates: `cargo test-fastly`, `cargo clippy-fastly`.

## File Structure

| File                                                            | Change | Responsibility                                                  |
| --------------------------------------------------------------- | ------ | --------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/lockr.rs`          | Modify | `capture_identity` flag, `pub(crate)` ID, register the provider |
| `crates/trusted-server-core/src/integrations/lockr_identity.rs` | Create | Descriptor for `LockrConfig`, `LockrIdentityProvider`, tests    |
| `crates/trusted-server-core/src/integrations/mod.rs`            | Modify | `mod lockr_identity;`                                           |
| `crates/trusted-server-core/src/config.rs`                      | Modify | Deploy-time descriptor when `capture_identity` is on            |
| `docs/guide/integrations/identity-vendors.md`                   | Modify | Table row and `## Lockr capture` section                        |
| `CHANGELOG.md`                                                  | Modify | Vendor bullet                                                   |

---

### Task 1: The flag, the descriptor and revocation capture

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/lockr.rs` (`LOCKR_INTEGRATION_ID` visibility, `LockrConfig`, the test config literal)
- Create: `crates/trusted-server-core/src/integrations/lockr_identity.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs` (add `mod lockr_identity;` after `pub mod lockr;`)

**Interfaces:**

- Consumes: `Captures::{new, with_path}`, `ProviderDescriptorBuilder::{with_captures, with_source, with_provenance}`, `EidProvenance::{new, with_inserter, with_mm}`, `CaptureInput::path`, `Resolution::opted_out`.
- Produces: `pub(crate) const LOCKR_INTEGRATION_ID: &str = "lockr"`, `pub capture_identity: bool` on `LockrConfig`, `impl IdentityProviderConfig for LockrConfig`, `pub struct LockrIdentityProvider`, `pub fn LockrIdentityProvider::new(&LockrConfig) -> Result<Self, Report<IdentityProviderError>>`.

- [ ] **Step 1: Add the flag**

In `lockr.rs`, make the ID visible to the crate:

```rust
pub(crate) const LOCKR_INTEGRATION_ID: &str = "lockr";
```

Add the last field to `LockrConfig`:

```rust
    /// Registers Lockr as a capture-only identity provider that revokes IDs
    /// when Lockr's API reports a consent revocation. Needs
    /// `[ec.identity.providers.lockr].sources`. Default: off.
    #[serde(default)]
    pub capture_identity: bool,
```

and `capture_identity: false,` to the `LockrConfig { .. }` literal in `lockr.rs`'s tests.

- [ ] **Step 2: Write the failing tests**

Create `lockr_identity.rs` with the tests module only, and declare `mod lockr_identity;` in `integrations/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::ec::identity::test_support::{CaptureFixture, ConformanceCases, ProviderHarness};

    fn config() -> LockrConfig {
        serde_json::from_value(json!({ "app_id": "app-1", "capture_identity": true }))
            .expect("should parse the Lockr config")
    }

    fn provider() -> LockrIdentityProvider {
        LockrIdentityProvider::new(&config()).expect("should build the provider")
    }

    fn path(endpoint: &str) -> String {
        format!("{LOCKR_API}/{endpoint}")
    }

    #[test]
    fn a_revocation_opts_out() {
        let provider = provider();

        let resolution = ProviderHarness::new(&provider)
            .capture(&CaptureFixture::new(Method::POST, &path("revoke-consent")))
            .expect("should capture");

        assert!(resolution.is_opted_out(), "should opt the visitor out");
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance(
            &ConformanceCases::default()
                .with_capture_case(CaptureFixture::new(Method::POST, &path("revoke-consent"))),
        );
    }
}
```

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr`
Expected: FAIL to compile (`LockrIdentityProvider`, `LOCKR_API` not found).

- [ ] **Step 4: Implement the descriptor and revocation capture**

Prepend to `lockr_identity.rs`. The `use` block is the module's final one; items used only by Task 2 (`BTreeMap`, `Deserialize`, `ResolvedEid`, `UnixTime`, `VendorEid`) warn as unused until then.

```rust
//! Lockr as a capture-only identity provider (spec 12.2).
//!
//! Reads the Lockr API responses Trusted Server already proxies. Capture is
//! revoke-only: a `revoke-consent` call opts the visitor out of Lockr's
//! sources, and a later token issue clears that opt-out. The tokens it
//! reads are not stored; browser ingest carries Lockr's IDs to KV.

use std::collections::BTreeMap;

use error_stack::Report;
use http::Method;
use serde::Deserialize;

use crate::ec::identity::{
    CaptureInput, Captures, EidProvenance, IdentityProvider, IdentityProviderConfig,
    IdentityProviderError, ProviderDescriptor, Resolution, ResolvedEid, UnixTime,
};
use crate::integrations::lockr::{LOCKR_INTEGRATION_ID, LockrConfig};
use crate::integrations::vendor_eid::VendorEid;

/// The proxied Lockr API prefix.
const LOCKR_API: &str = "/integrations/lockr/api/publisher/app/v2/identityLockr";

/// The Lockr API calls captured, by their last path segment.
const CAPTURED_ENDPOINTS: &[&str] = &[
    "page-view",
    "generate-tokens",
    "refresh-tokens",
    "sync-no-hem-ids",
    "revoke-consent",
];

/// The endpoint that reports a consent revocation.
const REVOKE_ENDPOINT: &str = "revoke-consent";

/// Lockr's token key names and the sources they carry. Only confirmed
/// mappings are listed.
const KEY_SOURCES: &[(&str, &str)] = &[
    ("id5id", "id5-sync.com"),
    ("_lr_env", "liveramp.com"),
    ("__uid2_advertising_token", "uidapi.com"),
];

impl IdentityProviderConfig for LockrConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut captures = Captures::new();
        for endpoint in CAPTURED_ENDPOINTS {
            captures = captures.with_path(Method::POST, &format!("{LOCKR_API}/{endpoint}"));
        }
        let mut builder =
            ProviderDescriptor::builder(LOCKR_INTEGRATION_ID.parse()?).with_captures(captures);
        for (_, domain) in KEY_SOURCES {
            builder = builder.with_source(domain.parse()?);
        }
        builder
            .with_provenance(
                &"id5-sync.com".parse()?,
                EidProvenance::new()
                    .with_inserter("Lockr-For-Publishers")
                    .with_mm(2),
            )
            .build()
    }
}

/// The Lockr capture provider.
pub struct LockrIdentityProvider {
    descriptor: ProviderDescriptor,
}

impl LockrIdentityProvider {
    /// Builds the provider from Lockr's validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the descriptor is
    /// invalid.
    pub fn new(config: &LockrConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
        })
    }
}

impl IdentityProvider for LockrIdentityProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if input
            .path()
            .rsplit('/')
            .next()
            .is_some_and(|endpoint| endpoint == REVOKE_ENDPOINT)
        {
            return Ok(Resolution::opted_out());
        }
        Ok(Resolution::no_match())
    }
}
```

`LockrConfig` already implements `IntegrationConfig`, so the `IdentityProviderConfig` impl is all the framework needs. The descriptor declares no `resolves`, which makes Lockr capture-only. Task 2 replaces the trailing `Ok(Resolution::no_match())` with token parsing.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr`
Expected: PASS, including the existing Lockr tests.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/
git commit -S --signoff -m "Describe Lockr as a capture-only provider that opts out on revocation"
```

---

### Task 2: Read issued tokens

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/lockr_identity.rs`

**Interfaces:**

- Consumes: `CaptureInput::{response, source_named, now}`, `SourceDomain::matches`, `VendorEid::to_resolved`, `ResolvedEid::with_expires_at`, `UnixTime::from_millis`.
- Produces: the positive branch of `LockrIdentityProvider::capture`.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn reads_complete_eids_for_known_keys() {
        let provider = provider();
        let fixture = CaptureFixture::new(Method::POST, &path("generate-tokens")).with_response(
            StatusCode::OK,
            &json!({
                "aimTokens": [{ "key_name": "id5id", "identity_expires": 1_770_000_000_000_u64 }],
                "ids": {
                    "id5id": { "eid": {
                        "source": "id5-sync.com",
                        "uids": [{ "id": "ID5*example", "atype": 1 }],
                        "inserter": "inserter.example.com"
                    }},
                    "unknown": { "eid": {
                        "source": "other.example.com",
                        "uids": [{ "id": "ignored" }]
                    }}
                }
            }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        let eids = resolution.eids();
        assert_eq!(eids.len(), 1, "should keep only mapped keys");
        assert_eq!(
            eids[0].source().as_str(),
            "id5-sync.com",
            "should map id5id"
        );
        assert_eq!(
            eids[0].inserter(),
            Some("inserter.example.com"),
            "should keep the EID fields"
        );
    }

    #[test]
    fn skips_an_expired_token() {
        let provider = provider();
        let fixture = CaptureFixture::new(Method::POST, &path("refresh-tokens")).with_response(
            StatusCode::OK,
            &json!({
                "aimTokens": [{ "key_name": "id5id", "identity_expires": 1_000_u64 }],
                "ids": { "id5id": { "eid": {
                    "source": "id5-sync.com",
                    "uids": [{ "id": "ID5*expired" }]
                }}}
            }),
        );

        let resolution = ProviderHarness::new(&provider)
            .capture(&fixture)
            .expect("should capture");

        assert!(resolution.is_no_match(), "should skip the expired token");
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr_identity`
Expected: `reads_complete_eids_for_known_keys` FAILS (the capture returns no match); `skips_an_expired_token` passes already and stays as a guard.

- [ ] **Step 3: Add the reply types**

Append after `impl LockrIdentityProvider`:

```rust
#[derive(Deserialize)]
struct TokenReply {
    #[serde(rename = "aimTokens", default)]
    aim_tokens: Vec<AimToken>,
    #[serde(default)]
    ids: BTreeMap<String, LockrId>,
}

#[derive(Deserialize)]
struct AimToken {
    key_name: String,
    /// Milliseconds since the epoch.
    identity_expires: Option<u64>,
}

#[derive(Deserialize)]
struct LockrId {
    eid: VendorEid,
}
```

- [ ] **Step 4: Parse tokens in `capture`**

In `capture`, replace the trailing `Ok(Resolution::no_match())` with:

```rust
        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        let eids: Vec<ResolvedEid> = reply
            .ids
            .iter()
            .filter_map(|(key, entry)| {
                let (_, domain) = KEY_SOURCES.iter().find(|(name, _)| name == key)?;
                let source = input.source_named(domain)?;
                if !source.matches(&entry.eid.source) {
                    return None;
                }
                let expires = reply
                    .aim_tokens
                    .iter()
                    .find(|token| token.key_name == *key)
                    .and_then(|token| token.identity_expires)
                    .map(UnixTime::from_millis);
                if expires.is_some_and(|at| at <= input.now()) {
                    return None;
                }
                let eid = entry.eid.to_resolved(source)?;
                Some(match expires {
                    Some(at) => eid.with_expires_at(at),
                    None => eid,
                })
            })
            .collect();
        Ok(if eids.is_empty() {
            Resolution::no_match()
        } else {
            Resolution::found(eids)
        })
```

A body that is not a token reply (for example `page-view`'s) is no match, never an error: capture errors are reserved for provider bugs. `input.source_named` returns only sources the operator kept in `[ec.identity.providers.lockr].sources`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/lockr_identity.rs
git commit -S --signoff -m "Read Lockr-issued tokens so a new issue can clear a Lockr opt-out"
```

---

### Task 3: Register behind the flag, validate and document

`lockr` is already in `builders()` and `DEPLOY_VALIDATED_INTEGRATION_IDS`. This task only adds the identity provider to its registration when the flag is on, and the descriptor to deploy validation. Lockr cannot use `validate_identity_integration`, which describes a provider whenever the integration is enabled: here the integration can be enabled without the provider.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/lockr.rs` (`register`, tests)
- Modify: `crates/trusted-server-core/src/config.rs` (imports, `identity_descriptors_for_deploy`)
- Modify: `docs/guide/integrations/identity-vendors.md`, `CHANGELOG.md`

**Interfaces:**

- Consumes: `IntegrationRegistrationBuilder::try_with_identity_provider`, `IntegrationRegistration::identity_providers`.

- [ ] **Step 1: Write the failing test**

Add to `lockr.rs`'s tests module:

```rust
    #[test]
    fn capture_identity_registers_an_identity_provider() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                LOCKR_INTEGRATION_ID,
                &json!({ "enabled": true, "app_id": "app-1", "capture_identity": true }),
            )
            .expect("should insert the Lockr config");

        let registration = register(&settings)
            .expect("should register")
            .expect("should be enabled");

        assert_eq!(
            registration.identity_providers.len(),
            1,
            "should register the capture provider"
        );
    }
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr::tests::capture_identity_registers_an_identity_provider`
Expected: FAIL, `identity_providers.len()` is 0.

- [ ] **Step 3: Register the provider when the flag is on**

In `lockr.rs`, import the provider:

```rust
use crate::integrations::lockr_identity::LockrIdentityProvider;
```

and replace the end of `register`:

```rust
    let mut builder = IntegrationRegistration::builder(LOCKR_INTEGRATION_ID)
        .with_proxy(integration.clone())
        .with_attribute_rewriter(integration.clone());
    if integration.config.capture_identity {
        builder =
            builder.try_with_identity_provider(LockrIdentityProvider::new(&integration.config))?;
    }
    Ok(Some(builder.build()))
```

- [ ] **Step 4: Describe it at deploy time**

In `config.rs`, change the Lockr import to `lockr::{LOCKR_INTEGRATION_ID, LockrConfig}`, and in `identity_descriptors_for_deploy`, after the other identity integrations:

```rust
    if let Some(lockr) = settings.integration_config::<LockrConfig>(LOCKR_INTEGRATION_ID)?
        && lockr.capture_identity
    {
        descriptors.push(lockr.build_descriptor().change_context(
            TrustedServerError::Configuration {
                message: format!(
                    "integrations.{LOCKR_INTEGRATION_ID}: the identity provider cannot be described"
                ),
            },
        )?);
    }
```

Deploy validation then requires `[ec.identity.providers.lockr].sources` and an `[[ec.partners]]` entry for each listed source, as for any capture-only provider.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::lockr`
Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib config`
Expected: PASS.

- [ ] **Step 6: Document**

In `docs/guide/integrations/identity-vendors.md`, add this table row:

```markdown
| `lockr` | Lockr (capture) | Capture | `id5-sync.com`, `liveramp.com`, `uidapi.com` | Lockr's proxied API |
```

and this section:

````markdown
## Lockr capture

```toml
[integrations.lockr]
enabled = true
app_id = "app-1"
capture_identity = true

[ec.identity.providers.lockr]
sources = ["id5-sync.com"]
```

With `capture_identity`, Lockr becomes a capture-only provider on the Lockr API calls Trusted Server already proxies. Capture is revoke-only: a `revoke-consent` call removes the IDs of the listed sources and blocks the browser's values for them until Lockr issues again. Lockr's IDs themselves reach KV through browser ingest. A capture-only provider must list its `sources`.
````

In `CHANGELOG.md` under `### Added`, add "revoke-only Lockr capture (`[integrations.lockr].capture_identity`)" to the vendor bullet, or, if there is no vendor bullet yet, add:

```markdown
- Revoke-only Lockr capture (`[integrations.lockr].capture_identity`). Each source it lists needs an `[[ec.partners]]` entry. See [Identity Vendors](docs/guide/integrations/identity-vendors.md).
```

- [ ] **Step 7: Run the gates**

Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly`
Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/ docs/guide/integrations/identity-vendors.md CHANGELOG.md
git commit -S --signoff -m "Register Lockr capture behind capture_identity and validate it at deploy"
```

## Self-Review Notes

- Revoke-only, captured tokens never stored (the coordinator discards positive captures): Design notes, Task 2.
- Opt-out on `revoke-consent`: Task 1. Opt-out clear on a later issue: Task 2.
- Flag off changes nothing: `#[serde(default)]` and the `if` in Task 3.

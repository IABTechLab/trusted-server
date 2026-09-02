# Issue #883 — Remove Legacy Consent Store Implementation Plan

## Metadata

- **Issue:** [#883 — Complete removal of the legacy consent KV persistence path](https://github.com/IABTechLab/trusted-server/issues/883)
- **Stack base:** Draft PR #902, branch `perf/group-batch-sync-by-ec-id`
- **Implementation branch:** `refactor/remove-legacy-consent-store`
- **Status:** Implemented and verified
- **Date:** 2026-07-13

## Goal

Completely remove the inactive legacy consent KV persistence path without wiring
new persistence onto the request hot path. Live consent remains request-local;
the EC identity graph remains the only KV-backed EC lifecycle store.

## Approved Behavioral and Migration Contract

1. Live consent is interpreted on each request from cookies, headers,
   geolocation, and publisher policy defaults.
2. `ec.ec_store` remains authoritative for EC identity graph state, minimal
   consent metadata, source-domain partner IDs, and withdrawal tombstones. It
   does not become a store for the deleted legacy consent payload schema.
3. `consent_store` is removed from `ConsentConfig`. Because the configuration
   schema is strict, legacy TOML and JSON/app-config containing that field fail
   during deserialization. There is no ignored field, alias, warning-only
   compatibility path, or silent migration.
4. Operators must remove `consent_store` before upgrading. Legacy records are
   not read or copied into `ec.ec_store`; an old binding/store may be retained
   for a short rollback window and then unlinked and deleted.
5. Auction, page-bids, and publisher routes no longer open an otherwise-unused
   consent KV store. They continue using the request's existing
   `RuntimeServices` and `EcContext`.
6. Existing consent gating, consent forwarding, EC creation/withdrawal,
   tombstone behavior, authentication, routing, and wire contracts remain
   unchanged.
7. Generic `PlatformKvStore`, the generic `RuntimeServices` KV slot, adapter KV
   implementations, and EC-specific KV primitives remain in place; they are
   platform infrastructure outside this issue.

## Current State

- `EcContext` always calls `build_consent_context` with `ec_id: None` and
  `kv_store: None`, so the fallback/write path and all consent storage helpers
  are unreachable in production.
- Fastly's EdgeZero path still opens `settings.consent.consent_store` for
  auction, page-bids, and publisher routes and returns `503` if it cannot be
  opened, even though the returned store is never consumed by consent logic.
- `ConsentConfig` still accepts the misleading field, and local Fastly config
  still provisions a placeholder store.
- The dead storage module, `ConsentSource::KvStore`, adapter comments, and tests
  continue to advertise persisted consent continuity that runtime requests do
  not have.

## File Map

### Modify

- `crates/trusted-server-core/src/settings.rs`
  - Add full-settings TOML and runtime JSON rejection coverage for the removed field.
- `crates/trusted-server-core/src/consent_config.rs`
  - Remove the field and default.
- `crates/trusted-server-core/src/consent/mod.rs`
  - Remove KV pipeline inputs, branches, re-export, tests, and stale docs.
- `crates/trusted-server-core/src/consent/types.rs`
  - Remove `ConsentSource::KvStore` and update source documentation.
- `crates/trusted-server-core/src/ec/mod.rs`
  - Remove obsolete `None` pipeline fields.
- `crates/trusted-server-core/src/integrations/prebid.rs`
  - Preserve non-cookie consent forwarding coverage using `PolicyDefault`.
- `crates/trusted-server-core/src/lib.rs`
  - Remove the deleted storage module export.
- `crates/trusted-server-core/src/migration_guards.rs`
  - Remove deleted storage files from the source include list.
- `crates/trusted-server-adapter-fastly/src/app.rs`
  - Remove the consent-route service resolver and obsolete tests; pass existing
    services directly.
- `crates/trusted-server-adapter-fastly/src/platform.rs`
  - Remove the now-unused named generic KV opener and imports.
- `crates/trusted-server-adapter-spin/src/platform.rs`
  - Remove stale consent-specific claims from generic TTL documentation.
- `crates/trusted-server-adapter-axum/src/platform.rs`
  - Remove stale consent-specific claims from generic unavailable-KV messaging.
- `fastly.toml`
  - Remove the local `consent_store` fixture.
- `docs/guide/configuration.md`
  - Document strict migration and authoritative sources.
- `docs/guide/fastly.md`
  - Document field/binding removal and rollback cleanup.

### Delete

- `crates/trusted-server-core/src/storage/kv_store.rs`
  - Dead consent persistence implementation and tests.
- `crates/trusted-server-core/src/storage/mod.rs`
  - Empty after deleting the only child module.

### Add

- `docs/superpowers/plans/2026-07-13-issue-883-remove-legacy-consent-store.md`
  - Record the reviewed removal and verification contract.

No dependency, EC KV schema, request/response schema, or adapter routing change
is expected.

## Implementation Tasks

### Task 1 — Establish fail-fast migration tests

- [x] Add `settings_rejects_removed_consent_store_toml` using
      `crate_test_settings_str()` plus `[consent] consent_store = ...`.
- [x] Assert `Settings::from_toml` fails during deserialization and its diagnostic
      identifies `consent_store`.
- [x] Add `settings_rejects_removed_consent_store_json` by serializing valid
      settings, injecting the legacy nested field, and calling
      `Settings::from_json_value`; assert the same field-specific diagnostic on
      the runtime app-config deserialization path.
- [x] Run both tests before removing the field and confirm they fail because the
      legacy field is still accepted.
- [x] Preserve `#[serde(deny_unknown_fields)]`; do not add compatibility aliases.

### Task 2 — Remove active configuration and request-pipeline plumbing

- [x] Remove `ConsentConfig::consent_store`, its documentation, serde attributes,
      and default initialization.
- [x] Remove the consent storage re-export from `consent`.
- [x] Remove `ec_id` and `kv_store` from `ConsentPipelineInput` and every struct
      literal.
- [x] Remove the empty-signal KV fallback and write-on-change branch.
- [x] Update module/entry-point documentation to describe request-local consent
      as the only live source.
- [x] Delete the consent-only in-memory KV test double and three persistence/
      fallback tests while retaining all request-local gating tests.

### Task 3 — Delete dead storage and source metadata

- [x] Delete `storage/kv_store.rs` and `storage/mod.rs`.
- [x] Remove `pub mod storage` and the two migration-guard `include_str!` entries.
- [x] Remove `ConsentSource::KvStore` and update type-level documentation.
- [x] Adapt the Prebid cookies-only regression to `PolicyDefault`, preserving the
      Cookie-versus-non-Cookie forwarding distinction.
- [x] Confirm no production EC identity graph or generic platform KV code changed.

### Task 4 — Remove Fastly's no-value route dependency

- [x] Delete `runtime_services_for_consent_route` and its `open_kv_store` call.
- [x] Pass the existing request `RuntimeServices` directly to auction, page-bids,
      publisher handling, and publisher response buffering.
- [x] Remove comments claiming those routes require separate consent KV.
- [x] Delete `settings_with_missing_consent_store` and the two obsolete route
      failure tests.
- [x] Remove test/production imports made unused by the deletion.
- [x] Delete the now-unused Fastly `open_kv_store` wrapper and its imports.
- [x] Keep `AppState::default_kv_store` and generic runtime KV infrastructure.

### Task 5 — Remove fixtures and stale active commentary

- [x] Remove `[[local_server.kv_stores.consent_store]]` from `fastly.toml`.
- [x] Rewrite Spin's generic TTL documentation without claims about consent
      persistence/fallback.
- [x] Rewrite Axum's unavailable generic-KV documentation/log without claims
      about consent routes.
- [x] Confirm no active example or manifest provisions the deleted setting.

### Task 6 — Document migration and authoritative sources

- [x] State that stale TOML and JSON/app-config fail fast and the field must be
      removed before upgrade.
- [x] Tell Fastly operators to remove the resource binding/local fixture and to
      retain legacy data only for rollback before deletion.
- [x] State that legacy consent records are not read or migrated and must not be
      copied into `ec.ec_store`.
- [x] Reaffirm request-local live consent and the identity/tombstone role of
      `ec.ec_store`.
- [x] Leave explicitly historical/superseded plans and specs unchanged.

### Task 7 — Residual audit, review, and full verification

- [x] Search active source/manifests for `consent_store`,
      `ConsentSource::KvStore`, `storage::kv_store`,
      `runtime_services_for_consent_route`, and `open_kv_store`.
- [x] Allow `consent_store` only in strict rejection tests, migration guidance,
      the new plan, and explicitly historical records.
- [x] Run independent scope/correctness and migration/test-quality reviews.
- [x] Apply only issue-scoped fixes.
- [x] Mark this plan implemented only after every verification check passes.

## Acceptance Mapping

| Issue requirement                           | Planned evidence                                                 |
| ------------------------------------------- | ---------------------------------------------------------------- |
| Remove active `consent_store` configuration | Field/default deletion plus strict TOML and JSON tests           |
| Remove pipeline KV fields and branches      | Simplified input/callers and no persistence code                 |
| Remove dead storage implementation          | Deleted storage files, export, and guard entries                 |
| Remove unused consent source                | Deleted enum variant; policy-source Prebid regression retained   |
| Remove Fastly route dependency              | Resolver/opener deletion and direct service passing              |
| Remove manifests/examples                   | Fastly local fixture deletion and residual audit                 |
| Preserve consent/tombstone behavior         | Existing consent, EC, finalization, and adapter suites           |
| Document authoritative sources              | Updated configuration and Fastly guides                          |
| Document migration behavior                 | Explicit fail-fast, rollback, and no-data-migration instructions |

## Migration Details

Before deploying this version, operators with the old setting must:

1. Remove `consent_store` from `[consent]` in TOML or from the equivalent JSON/
   app-config object. Startup/config loading intentionally fails while it is
   present.
2. Remove the legacy Fastly resource link and local Viceroy fixture when no
   rollback depends on them.
3. Do not copy old consent payloads into `ec.ec_store`; the schemas and
   authority differ.
4. Optionally retain the old store unchanged during a defined rollback window,
   then delete it and its records. This release neither reads nor transforms
   those records.

No browser cookie migration is required. Request-local signals continue to be
interpreted normally, and existing EC identity/tombstone entries are unchanged.

## Focused Verification

```bash
cargo test-fastly removed_consent_store
cargo test-fastly settings_rejects_removed_consent_store_json
cargo test-fastly missing_geo_keeps_unknown_jurisdiction
cargo test-fastly to_openrtb_includes_policy_default
cargo test-fastly dispatch
cargo fmt --all -- --check

rg -n 'ConsentSource::KvStore|storage::kv_store|runtime_services_for_consent_route|open_kv_store' crates fastly.toml trusted-server.example.toml
rg -n 'consent_store' crates fastly.toml trusted-server.example.toml docs/guide
```

Expected residual `consent_store` matches are limited to rejection tests and
migration documentation.

## Full Verification Contract

```bash
cargo fmt --all -- --check

cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh

cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm

cd crates/trusted-server-js/lib
npx vitest run
npm run format
cd ../../..

cd docs
npm run format
cd ..

cargo doc --package trusted-server-core --no-deps --all-features --target wasm32-wasip1
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
git diff --check
```

## Risks and Mitigations

- **Intentional breaking config change:** field-specific TOML and JSON tests plus
  prominent migration guidance prevent silent drift.
- **Fastly service-threading regression:** pass the exact existing `services`
  reference to both publisher handling and response buffering; run all route and
  adapter tests.
- **Consent forwarding regression:** retain the non-cookie Prebid test with
  `PolicyDefault` and all request-local consent tests.
- **Accidental EC KV removal:** do not alter `EcKvStore`, `KvIdentityGraph`,
  `ec.ec_store`, or withdrawal code.
- **Overbroad generic KV cleanup:** keep `PlatformKvStore`, RuntimeServices' KV
  slot, and adapter implementations.
- **Misleading residual history:** active code/docs must be clean; explicitly
  superseded historical records remain as history and are allowlisted in audits.
- **Out-of-repository API users:** the deleted module/variant are public, but the
  core crate is private and `publish = false`; repository callers are audited.

## Non-Goals

- No persisted live-consent continuity or new consent data model.
- No migration of legacy consent payloads into the EC identity graph.
- No change to consent interpretation, expiry, conflict resolution, gating, or
  forwarding policy.
- No change to EC identity creation, partner IDs, withdrawal, or tombstone TTL.
- No removal of generic platform KV infrastructure.
- No authentication, routing, response, or adapter platform redesign.

## Review and Verification Record

- Independent plan review: approved (no blockers; runtime JSON-path and doc-command corrections applied)
- Test-first strict-schema failures: confirmed before field removal
- Focused tests: passed (strict schema, consent, Prebid source, and Fastly dispatch)
- Independent code review: approved (no findings)
- Residual audit: passed; only rejection tests, migration docs, plan, and explicit history remain
- Full cross-adapter verification: passed, including host CLI tests and all-features core docs
- Release WASM build: passed

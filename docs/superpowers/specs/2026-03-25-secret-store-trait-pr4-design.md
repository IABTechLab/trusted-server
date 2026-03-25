# PR 4: Secret Store Trait (Read-Only) — Design

**Issue:** #485
**Part of:** #480 (EdgeZero migration)
**Blocked by:** PR 3 (#484)
**Date:** 2026-03-25

---

## Context

PR 4 is Phase 1, step 4 of the EdgeZero migration. The goal of Phase 1 is to
extract platform behaviors behind traits in `trusted-server-core`, with Fastly
SDK implementations living in `trusted-server-adapter-fastly`. This makes the
core crate platform-neutral.

PR 3 (#484) split `fastly_storage.rs` and wired `PlatformConfigStore`.
PR 4 wires `PlatformSecretStore` — the read path only.

---

## Scope (Approach A — thin PR)

This PR is intentionally narrow. It covers only the Fastly adapter
implementation of `PlatformSecretStore`. It does **not** migrate
`request_signing/signing.rs` or any other call sites in core — that work is
blocked on PR 12.5 (#515), which threads `&RuntimeServices` into integration
and provider entry points.

---

## What Is Already Done

All implementation code exists in
`crates/trusted-server-adapter-fastly/src/platform.rs`:

- `FastlyPlatformSecretStore::get_bytes()` — delegates to
  `get_secret_bytes::<SecretStore, _, _>()` helper, which calls
  `fastly::SecretStore::open()`, then `.try_get()` and `.try_plaintext()`.
  Error paths map to `PlatformError::SecretStore` with attached context strings.
- `FastlyPlatformSecretStore::create()` — returns
  `Err(Report::new(PlatformError::NotImplemented))`.
- `FastlyPlatformSecretStore::delete()` — returns
  `Err(Report::new(PlatformError::NotImplemented))`.
- `FastlyPlatformSecretStore` wired into `build_runtime_services()`.
- One existing test: `get_secret_bytes_returns_error_when_decrypt_fails`.

The `PlatformSecretStore` trait and `RuntimeServices` field are already defined
in `crates/trusted-server-core/src/platform/`.

---

## What This PR Adds

Three tests in `crates/trusted-server-adapter-fastly/src/platform.rs`, in the
existing `#[cfg(test)]` block, following established patterns:

### 1. `get_secret_bytes_returns_error_when_open_fails`

Verifies the **store-open failure** path surfaces as `PlatformError::SecretStore`.
The `open_store` closure passed to `get_secret_bytes` returns `Err("open failed")`,
simulating a failed `SecretStore::open()` call. No changes to `StubSecretStore`
are needed — the closure fails before the stub is ever constructed.

Note: `get_secret_bytes` has four reachable error/success branches:
open failure, lookup error, key not found (`Ok(None)`), and decrypt failure.
The decrypt-failure branch is already tested. The lookup-error and key-not-found
branches are deferred — they are not required by the issue's "Done when" criteria
and the coverage gap does not affect production correctness for this PR's scope.

### 2. `fastly_platform_secret_store_create_returns_not_implemented`

Verifies `FastlyPlatformSecretStore::create()` returns
`PlatformError::NotImplemented`. Follows the pattern of
`fastly_platform_http_client_reports_not_implemented`.

### 3. `fastly_platform_secret_store_delete_returns_not_implemented`

Verifies `FastlyPlatformSecretStore::delete()` returns
`PlatformError::NotImplemented`. Same pattern as above.

---

## Files Changed

| File | Change |
|---|---|
| `crates/trusted-server-adapter-fastly/src/platform.rs` | Add three tests |

No other files are modified.

---

## Done When

- `FastlyPlatformSecretStore::get_bytes()` is backed by `fastly::SecretStore`
- `create()` and `delete()` return `PlatformError::NotImplemented`
- The three tests above exist and pass
- `cargo test --workspace`, `cargo clippy`, `cargo fmt --check` all pass

---

## Explicitly Out of Scope

- Migrating `request_signing/signing.rs` to use `services.secret_store()` —
  deferred, blocked by PR 12.5 (#515) threading `&RuntimeServices` into
  `AuctionContext` (which `prebid.rs` needs before `from_config()` can be
  removed)
- `AuctionContext` changes — PR 12.5
- Any changes to `trusted-server-core` — trait and `RuntimeServices` already
  defined
- `PlatformSecretStore::get_string` default method — it delegates to `get_bytes`
  and performs UTF-8 conversion; it has no direct tests in this PR since it is a
  provided trait method whose correctness depends entirely on `get_bytes`, which
  is already covered

---

## Future Follow-Up (Not PR 4)

EdgeZero PR #230 (`stackpop/edgezero`) adds
`edgezero_core::secret_store::SecretStore` — a provider-neutral type across
Fastly, Cloudflare, and Axum. Once it merges, `PlatformSecretStore` can be
re-exported from EdgeZero directly (same pattern as `PlatformKvStore`) and
`FastlyPlatformSecretStore` replaced by the EdgeZero adapter. This swap is not
part of PR 4.

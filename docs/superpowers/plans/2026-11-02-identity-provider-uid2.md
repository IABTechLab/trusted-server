# UID2 Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store the UID2 advertising token the browser already sends in first-party cookies, as an `[[ec.identity_providers]]` entry with `kind = "uid2"`. No login, no server call and no server-side refresh.

**Architecture:** `crates/trusted-server-identity-providers/src/uid2.rs` holds `Uid2` (a `ProviderKind`), `Uid2Settings` and `Uid2Provider`. The provider implements only `IdentityProvider::read_request_cookies`, using the cookie helpers in `common.rs`; `build_request` always fails, so no lookup is sent.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookies

- `identity_cookie_name` (default `__uid_2`): percent-encoded JSON from the UID2 SDK. Only `advertising_token` and `identity_expires` (milliseconds) are deserialized. `refresh_token`, `refresh_from`, `refresh_expires` and `refresh_response_key` are never read, so they are never stored.
- `cookie_names` (default `["__uid2_advertising_token"]`): cookies holding a raw token, such as Prebid's, read in order only when the identity cookie yields nothing.
- A missing token or expiry, a blank or opt-out token, or malformed JSON reads as no ID.

## Stored ID

- One EID for the single configured source, one UID with `atype = 3`.
- From the identity cookie: `ProviderExpiry::At(identity_expires)`; the coordinator's validation drops an expired token.
- From the raw token cookie: `bounded_expiry(None)`, the shared default TTL.
- No refresh: the browser SDK refreshes the identity and the next navigation with a due source picks up the new token.

## Lockr

`uid2` declares the Lockr capability (see `2026-11-02-identity-provider-lockr-capability.md`). With `[settings.lockr] enabled = true`, the Lockr cookies (default `["__uid2_advertising_token"]`) are read last, after the identity cookie and `cookie_names`, with the raw-token parser. Lockr names already in `cookie_names` are not read twice.

## Not ported from the reference provider

- The reference generates a first token from a hashed email and refreshes it with the refresh token it kept from that call. Generation needs login, and refreshing with the SDK cookie's refresh token would spend the browser's single-use refresh token without a way to write the new `__uid_2` cookie back, so neither is ported.

## Tasks

- [ ] Add `uid2.rs` and register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: token and expiry from `__uid_2` with refresh fields not stored, raw token fallback, configured cookie names, missing, malformed and opt-out cookies ignored.
- [ ] Add a commented `kind = "uid2"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

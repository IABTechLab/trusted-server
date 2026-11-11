# UID2 Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store the UID2 advertising token from first-party cookies, as an `[[ec.identity_providers]]` entry with `kind = "uid2"`, and, when `endpoint` is set, refresh it server side. No login; tokens are never generated.

**Architecture:** `crates/trusted-server-identity-providers/src/uid2.rs` holds `Uid2` (a `ProviderKind`), `Uid2Settings` and `Uid2Provider`. `read_request_cookies` reads the token, using the cookie helpers in `common.rs`; with an `endpoint`, `build_request` posts the stored refresh token to `POST /v2/token/refresh` and `parse_response` decrypts the AES-256-GCM reply (12-byte IV first) using the response key passed in `ProviderRequest::context`. Without an `endpoint`, `build_request` fails.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookies

- `identity_cookie_name` (default `__uid_2`): percent-encoded JSON from the UID2 SDK. `advertising_token` and `identity_expires` (milliseconds) are read. With an `endpoint`, `refresh_token`, `refresh_response_key` and `refresh_from` are kept as private refresh state (never served, mirrored or forwarded; the admin view shows it as `[redacted]`); without it they are not stored. `refresh_expires` is not read.
- `cookie_names` (default `["__uid2_advertising_token"]`): cookies holding a raw token, such as Prebid's, read in order only when the identity cookie yields nothing.
- A missing token or expiry, a blank or opt-out token, or malformed JSON reads as no ID.

## Stored ID

- One EID for the single configured source, one UID with `atype = 3`.
- From the identity cookie: `ProviderExpiry::At(identity_expires)`; the coordinator's validation drops an expired token.
- From the raw token cookie: `bounded_expiry(None)`, the shared default TTL.
- Refresh state `from = refresh_from`; the source becomes due then, and is refreshed once due, also while the cookie repeats the stored token (a cookie repeating the held token is not rewritten, so a failed refresh keeps the held refresh state). A refresh is due only while the token is still live, so a failing refresh stops at its expiry; `rate_limit_per_hour` also bounds it. An `optout` reply removes the stored token.

## Lockr

`uid2` declares the Lockr capability (see `2026-11-02-identity-provider-lockr-capability.md`). With `[settings.lockr] enabled = true`, the Lockr cookies (default `["__uid2_advertising_token"]`) are read last, after the identity cookie and `cookie_names`, with the raw-token parser. Lockr names already in `cookie_names` are not read twice.

## Not ported from the reference provider

- Token generation (needs a hashed email; out of scope).

Risk: a server refresh does not rewrite `__uid_2`. Refreshing does not invalidate the previous token or refresh token, so the server refresh and the browser SDK's own refresh both succeed and fork into two token chains. A later navigation whose cookie still carries the older token then replaces the refreshed row token; `rate_limit_per_hour` bounds the resulting refreshes.

## Tasks

- [ ] Add `uid2.rs` and register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: token and expiry from `__uid_2` with refresh fields not stored, raw token fallback, configured cookie names, missing, malformed and opt-out cookies ignored.
- [ ] Refresh: `endpoint`, AES-256-GCM decrypt with the response key from `ProviderRequest::context`, `optout` reply as `LookupOutcome::OptedOut`, and `refresh_state` tests.
- [ ] Add a commented `kind = "uid2"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

# LiveRamp Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store the LiveRamp RampID envelope the browser already sends in LiveRamp's own first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "liveramp"`. No login and no server call.

**Architecture:** `crates/trusted-server-identity-providers/src/liveramp.rs` holds `LiveRamp` (a `ProviderKind`), `LiveRampSettings` and `LiveRampProvider`. The provider implements only `IdentityProvider::read_request_cookies`; `build_request` always fails, so the coordinator never sends a lookup. The coordinator reads cookies only for due sources and applies consent, validation, expiry and persistence exactly as for lookup results.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Default cookie `_lr_env`; `cookie_names` overrides the list, read in order.
- The value is base64 or base64url (padded or not, optionally percent-encoded) of a JSON object. Only its string `envelope` field is used, unchanged.
- A blank or opt-out envelope (`0`, all zeros) and any undecodable value read as no ID.

## Stored ID

- One EID for the single configured source, one UID with `atype = 3`.
- Expiry `ProviderExpiry::After(15 days)` through `bounded_expiry`.
- No refresh call: LiveRamp's server-side refresh contract is not confirmed, so the browser keeps refreshing the cookie. Revisit when it is.

## Lockr

`liveramp` declares the Lockr capability (see `2026-11-02-identity-provider-lockr-capability.md`). With `[settings.lockr] enabled = true`, the Lockr cookies (default `["_lr_env"]`) are read after `cookie_names` with the same envelope parser. `cookie_names` may be empty only when Lockr is enabled.

## Tasks

- [ ] Add `cookie_capabilities`, `cookie_eid`, `is_usable_id` and `percent_decoded` to `common.rs`.
- [ ] Add `liveramp.rs` and register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: base64 and base64url envelopes, configured cookie name, missing, malformed and opt-out cookies ignored, one-source and cookie-name config checks, no lookup built.
- [ ] Add a commented `kind = "liveramp"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

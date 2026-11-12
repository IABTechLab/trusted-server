# LiveRamp Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store the LiveRamp RampID envelope the browser already sends in LiveRamp's own first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "liveramp"`. No login. With an `endpoint`, a visitor without an envelope cookie is also looked up server to server.

**Architecture:** `crates/trusted-server-identity-providers/src/liveramp.rs` holds `LiveRamp` (a `ProviderKind`), `LiveRampSettings` and `LiveRampProvider`. The provider reads the envelope cookies in `IdentityProvider::read_request_cookies`; without an `endpoint`, `build_request` fails, so no lookup is sent. The coordinator reads cookies on every navigation (a cookie repeating the held UID is not rewritten) and applies consent, validation, expiry and persistence exactly as for lookup results.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Default cookies `_lr_env` then `idl_env`; `cookie_names` overrides the list, read in order.
- `_lr_env` (written by ATS) is base64 or base64url (padded or not, optionally percent-encoded) of a JSON object. Only its string `envelope` field is used, unchanged.
- `idl_env` is where Prebid's `identityLink` ID module stores the envelope it fetched, bare. A value in the base64 alphabet that does not decode to JSON is taken as such a bare envelope, unchanged.
- A blank or opt-out envelope (`0`, all zeros), a JSON body without a string `envelope`, and any value outside the base64 alphabet read as no ID.

## Stored ID

- One EID for the single configured source, one UID with `atype = 3`.
- Expiry `ProviderExpiry::After(15 days)` through `bounded_expiry`.
- Optional server lookup (with `endpoint`): `GET /api/identity/v2/envelope` with `pid` (`placement_id`), `it=15`, `iv=<custom_id_namespace>:<stable EC ID>`, `atype=3` (server to server; `2` is the ATS API), consent as `ct`/`cv` (TCF `4`, else US Privacy `3`), `gpp` and `gpp_sid` (the first section ID only); headers `Origin` (publisher) and `X-Forwarded-For` (routable visitor IP, else no lookup). `200 {"envelopes":[{"type":19,"value":…}]}`: the `RampID` entry (`type` 19) is stored like the cookie, and a reply without one is no match; `204` is an opt-out; a blank envelope is no match; other statuses fail. Without `endpoint`, no server call. LiveRamp must enable Custom ID for the placement and approve the origin. LiveRamp resolves a Custom ID only through a mapping file the publisher sends it, which ties each Custom ID to a hashed email; without that file, or for an EC ID absent from it, the lookup finds nothing.

## Lockr

`liveramp` declares the Lockr capability (see `2026-11-02-identity-provider-lockr-capability.md`). With `[settings.lockr] enabled = true`, the Lockr cookies (default `["_lr_env"]`) are read after `cookie_names` with the same envelope parser. `cookie_names` may be empty only when Lockr is enabled.

## Not ported from the reference provider

- Hashed-email and hashed-phone lookups (`it=4`, `it=11`): out of scope (no logged-in experience).

## Tasks

- [x] Custom ID envelope lookup with `endpoint`, `placement_id` and `custom_id_namespace`, with tests.

- [ ] Add `cookie_capabilities`, `cookie_eid`, `is_usable_id` and `percent_decoded` to `common.rs`.
- [ ] Add `liveramp.rs` and register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: base64 and base64url envelopes, configured cookie name, missing, malformed and opt-out cookies ignored, one-source and cookie-name config checks, no lookup built.
- [ ] Add a commented `kind = "liveramp"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

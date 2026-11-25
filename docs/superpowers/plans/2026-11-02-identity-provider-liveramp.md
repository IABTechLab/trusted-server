# LiveRamp Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store the LiveRamp RampID envelope the browser already sends in LiveRamp's own first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "liveramp"`. With an `endpoint` and the hashed-email capability (`2026-11-25-identity-provider-hashed-email-capability.md`), a signed-in visitor without an envelope is looked up server to server by hashed email, and envelopes TS obtained are refreshed server side.

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
- Optional server lookup (with `endpoint`, `placement_id` and `[settings.hashed_email]` enabled; each requires the others). Shared query: `pid` (`placement_id`), `atype=3` (server to server; `2` is the ATS API), consent as `ct`/`cv` (TCF `4`, else US Privacy `3`), `gpp` and `gpp_sid` (the first section ID only); headers `Origin` (publisher) and `X-Forwarded-For` (routable visitor IP, else no lookup). The EC ID is never sent, so the provider may run on the navigation that mints the EC.
  - Envelope lookup, for a source without a live envelope when the provider got a hashed email (spec §8.7): `GET <endpoint>` with `it=4` and `iv=<hashed email>`, passed as the publisher returned it. The source's input is `SourceInput::HashedEmail`, so without a hashed email the coordinator skips it.
  - Refresh, for a live stored envelope whose refresh state is due: `GET <endpoint>/refresh` with `it=19` and `iv=<stored envelope>`. Needs no hashed email.
  - `200 {"envelopes":[{"type":19,"value":…}]}`: the `RampID` entry (`type` 19) is stored for 15 days with refresh state due `refresh_interval_secs` later (default 1800, LiveRamp's recommended 30 minutes); a reply without one is no match. `204` from the envelope API is an opt-out; `204` from the refresh API means LiveRamp no longer accepts the envelope: the stored envelope (carried in the request context, never sent) is rewritten without refresh state to expire one refresh interval later, after which the visitor is looked up by hashed email again. A blank envelope is no match; other statuses fail.
  - Envelopes read from cookies carry no refresh state: ATS refreshes those in the browser.
  - LiveRamp must approve the publisher origin for the placement.
  - The Custom ID lookup (`it=15`, `custom_id_namespace`) is removed: LiveRamp resolves a Custom ID only through a mapping file that ties it to a hashed email.

## Lockr

`liveramp` declares the Lockr capability (see `2026-11-02-identity-provider-lockr-capability.md`). With `[settings.lockr] enabled = true`, the Lockr cookies (default `["_lr_env"]`) are read after `cookie_names` with the same envelope parser. `cookie_names` may be empty only when Lockr is enabled.

## Not ported from the reference provider

- Hashed-phone lookups (`it=11`) and sending SHA-1 or MD5 hashes alongside SHA-256.

## Tasks

- [x] Custom ID envelope lookup with `endpoint`, `placement_id` and `custom_id_namespace`, with tests.
- [x] Replace the Custom ID lookup with the hashed-email envelope lookup (`it=4`) and the envelope refresh (`it=19`), with tests.

- [ ] Add `cookie_capabilities`, `cookie_eid`, `is_usable_id` and `percent_decoded` to `common.rs`.
- [ ] Add `liveramp.rs` and register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: base64 and base64url envelopes, configured cookie name, missing, malformed and opt-out cookies ignored, one-source and cookie-name config checks, no lookup built.
- [ ] Add a commented `kind = "liveramp"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

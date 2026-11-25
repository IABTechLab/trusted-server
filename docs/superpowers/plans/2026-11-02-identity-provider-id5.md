# ID5 Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Resolve the ID5 ID at the edge as an anonymous `[[ec.identity_providers]]` entry with `kind = "id5"`, storing the complete EIDs ID5 returns.

**Architecture:** `crates/trusted-server-identity-providers/src/id5.rs` holds `Id5` (a `ProviderKind`), `Id5Settings` and `Id5Provider` (an `IdentityProvider`), plus one `build` arm in `lib.rs`. The provider is pure: `build_request` builds one JSON `POST`, and `parse_response` borrows EIDs from the body. The coordinator from PR A (`ec::coordinator`) applies consent, the source gate, validation, expiry and persistence.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

**Changes from the earlier ID5 plan (built on the old descriptor framework):**

- Every source is `SourceInput::Anonymous`. With the hashed-email capability enabled (`2026-11-25-identity-provider-hashed-email-capability.md`) and a hashed email for the request, the body also carries `pd`: padded URL-safe base64 of `1=<percent-encoded hashed email>` (partner-data key 1 is the SHA-256 email hash), as ID5's server-side fetch documentation requires.
- A reply's non-empty `signature` is kept as private refresh state, due 8 hours after the reply, and sent back as `s`, as ID5's server-side fetch documentation asks.
- Endpoint, sources and the TCF vendor ID come from the common provider config, not hard-coded defaults.
- The legacy `universal_uid` fallback goes to the first configured source. An opted-out `universal_uid` of `0` is reported as `LookupOutcome::OptedOut`, detected on the reply as sent (not percent-decoded).

## Request

- `POST <endpoint>/<partner_id>.json`, with `Content-Type: application/json; charset=UTF-8` and `Origin: https://<publisher_domain>`.
- Body, following ID5's server-side fetch documentation: `partner`, `provider` (`provider_name`, default the publisher domain), `o = "server-side-fetch-api"`, `u` (`https://<publisher_domain>`), `top = 1`, `v` (the crate version), `ts` (RFC 3339), `gdpr`, and when present `gdpr_consent`, `us_privacy`, `gpp_string`, `gpp_sid` (comma-joined), `ua`, `accept_language` (the visitor's `Accept-Language`), `ref` (the origin of the visitor's `Referer`), and `ipv4` or `ipv6` (only when routable, see `common::is_routable_ip`). `ClientSignals` carries the `Referer` and `Accept-Language` headers for this.
- `pd` only with a hashed email (see above); a fetch without one is unchanged.

## Response

- `204` is no match; another non-2xx status or an unparseable body is `ProviderError::InvalidResponse`.
- `ids.<key>.eid` entries whose source is configured become `ProviderEid`s; every field, including `ext`, is borrowed from the body.
- Every UID gets `ProviderExpiry::After(30 days)`, restarted by each reply; `cache_control` is ignored.
- With no EID for the first source, a non-empty, non-`0` `universal_uid` becomes an EID with `atype = 1` and, when the reply has `ext.linkType`, UID `ext` `{"linkType": ...}`, as in the reference provider and Prebid.

## First-party cookie

ID5's source is provider-owned, so the browser `ts-eids` copy is dropped. Prebid's ID5 module stores the ID in a first-party cookie instead, which `read_request_cookies` reads before any lookup.

- Setting `cookie_names` (default `["id5id"]`), tried in order.
- `id5` declares the Lockr capability (see `2026-11-02-identity-provider-lockr-capability.md`): with `[settings.lockr] enabled = true`, the Lockr cookies (default `["id5id"]`) are read after `cookie_names` and before the lookup, with the same parser.
- A value that is JSON after percent-decoding is parsed with the same code as a lookup response (lenient entries, opt-out `0` dropped, normalized sources, 30-day expiry). When the value was percent-encoded, entries, `ext` objects included, are copied out of the decoded text.
- Any other value is a bare ID: one UID on the first source with `atype = 1` and the default TTL.
- `signature` is never stored as a UID, only as refresh state. A cookie that yields no usable UID is ignored, and the lookup runs as usual; a kept UID skips the lookup.

## Tasks

- [ ] Add `common.rs` with `is_routable_ip`, `join_section_ids` and the borrowed `VendorEid`.
- [ ] Add `id5.rs` and the `build` arm.
- [ ] Unit tests: request shape, private IP dropped, complete EIDs parsed and unconfigured sources dropped, legacy fallback, opted-out and empty replies are no match, error status and malformed body rejected.
- [ ] Read `cookie_names` in `read_request_cookies`; tests for plain and percent-encoded reply JSON, a bare ID from a configured name, and opted-out or malformed cookies reading as no ID.
- [ ] Add a commented `kind = "id5"` example, with `cookie_names`, to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

# ID5 Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the ID5 ID at the edge as an anonymous `[[ec.identity_providers]]` entry with `kind = "id5"`, storing the complete EIDs ID5 returns.

**Architecture:** `crates/trusted-server-identity-providers/src/id5.rs` holds `Id5` (a `ProviderKind`), `Id5Settings` and `Id5Provider` (an `IdentityProvider`), plus one `build` arm in `lib.rs`. The provider is pure: `build_request` builds one JSON `POST`, and `parse_response` borrows EIDs from the body. The coordinator from PR A (`ec::coordinator`) applies consent, the source gate, validation, expiry and persistence.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

**Changes from the earlier ID5 plan (built on the old descriptor framework):**

- No hashed email and no `pd` field: every source is `SourceInput::Anonymous`.
- No private provider state, so ID5's `signature` is not sent back.
- No page URL in `LookupInput`, so `tml` is the publisher origin.
- Endpoint, sources and the TCF vendor ID come from the common provider config, not hard-coded defaults.
- The legacy `universal_uid` fallback goes to the first configured source. An opted-out `universal_uid` of `0` is reported as `LookupOutcome::NoMatch`; there is no separate invalidation outcome.

## Request

- `POST <endpoint>/<partner_id>.json`, with `Content-Type: application/json` and `Origin: https://<publisher_domain>`.
- Body: `partner`, `v`, `o = "trusted-server"`, `nbPage = 1`, `gdpr`, and when present `gdpr_consent`, `us_privacy`, `gpp_string`, `gpp_sid` (comma-joined), `ua`, and `ip` (only when routable, see `common::is_routable_ip`).
- Confirm the `ip` and `ua` field names with ID5 for each partner account.

## Response

- A non-2xx status or an unparseable body is `ProviderError::InvalidResponse`.
- `ids.<key>.eid` entries whose source is configured become `ProviderEid`s; every field, including `ext`, is borrowed from the body.
- `cache_control.max_age_sec` becomes `ProviderExpiry::After` for every UID.
- With no EID for the first source, a non-empty, non-`0` `universal_uid` becomes an EID with `atype = 1`.

## First-party cookie

ID5's source is provider-owned, so the browser `ts-eids` copy is dropped. Prebid's ID5 module and Lockr AIM store the ID in a first-party cookie instead, which `read_request_cookies` reads before any lookup.

- Setting `cookie_names` (default `["id5id"]`), tried in order; Lockr's key name is configurable, so operators list it.
- A value that is JSON after percent-decoding is parsed with the same code as a lookup response (lenient entries, opt-out `0` dropped, normalized sources, `cache_control.max_age_sec` bounded). `ext` objects are dropped when the value was percent-encoded, since they borrow from the decoded copy.
- Any other value is a bare ID: one UID on the first source with `atype = 1` and the default TTL.
- `signature` is never stored as a UID. A cookie that yields no usable UID is ignored, and the lookup runs as usual; a kept UID skips the lookup.

## Tasks

- [ ] Add `common.rs` with `is_routable_ip`, `join_section_ids` and the borrowed `VendorEid`.
- [ ] Add `id5.rs` and the `build` arm.
- [ ] Unit tests: request shape, private IP dropped, complete EIDs parsed and unconfigured sources dropped, legacy fallback, opted-out and empty replies are no match, error status and malformed body rejected.
- [ ] Read `cookie_names` in `read_request_cookies`; tests for plain and percent-encoded reply JSON, a bare ID from a configured name, and opted-out or malformed cookies reading as no ID.
- [ ] Add a commented `kind = "id5"` example, with `cookie_names`, to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

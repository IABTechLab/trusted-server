# Unified ID Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store The Trade Desk ID (TDID) from Prebid's Unified ID first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "unifiedid"`. No server call.

**Architecture:** `crates/trusted-server-identity-providers/src/unifiedid.rs` holds `UnifiedId` (a `ProviderKind`), `UnifiedIdSettings` and `UnifiedIdProvider`. The provider implements only `IdentityProvider::read_request_cookies` and keeps EID-level fields (`EidLevelFields::Preserved`).

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Prebid's `unifiedIdSystem` stores percent-encoded JSON in `pbjs-unifiedid`: `TDID`, `TDID_LOOKUP`, `TDID_CREATED_AT`. Only `TDID` is read.
- Default `cookie_names = ["pbjs-unifiedid"]`, read in order.
- Malformed JSON, a missing or non-string `TDID`, or a blank or opt-out ID reads as no ID.

No Lockr capability.

## Stored ID

- One EID shaped as Prebid sends it: `atype = 1`, UID `ext` `{"rtiPartner":"TDID"}`, `inserter` and `matcher` `adserver.org`, `mm = 4`.
- Default bounded expiry; a cookie repeating the held ID is not rewritten, so the expiry renews only when the source is due.
- The source must not also be claimed by a liveintent `thetradedesk` attribute; pick one owner.

## Not ported

- The Trade Desk has no public publisher S2S ID API; only the Prebid cookie is read.

## Tasks

- [ ] Add `unifiedid.rs`; register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: TDID with Prebid's EID fields, missing, malformed and blank IDs ignored.
- [ ] Add a commented `kind = "unifiedid"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

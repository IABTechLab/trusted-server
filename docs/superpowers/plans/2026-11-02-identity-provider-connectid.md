# Yahoo ConnectID Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store Yahoo ConnectID from Yahoo's own first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "connectid"`. No login and no server call.

**Architecture:** `crates/trusted-server-identity-providers/src/connectid.rs` holds `ConnectId` (a `ProviderKind`), `ConnectIdSettings` and `ConnectIdProvider`. The provider implements only `IdentityProvider::read_request_cookies`, using the cookie helpers in `common.rs`; `build_request` always fails, so no lookup is sent.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Default cookie `connectId`; `cookie_names` overrides the list, read in order.
- The value is JSON, optionally percent-encoded: `connectId`, `he`, `puid`, `lastSynced`, `lastUsed`, `ttl` (timestamps and TTL in milliseconds).
- Only `connectId`, `lastSynced` and `ttl` are read. `he` and `puid` are never deserialized, so they are never stored.
- A missing `connectId` or `lastSynced`, a non-string ID, a blank or opt-out ID, or malformed JSON reads as no ID.

No Lockr capability: Lockr AIM is not documented to deliver ConnectID, so a `[settings.lockr]` table is rejected.

## Stored ID

- One EID for the single configured source, one UID with `atype = 3`.
- Expiry `ProviderExpiry::At(lastSynced + min(ttl, 14 days))`; a missing `ttl` uses the shared default TTL. The provider reads no clock: the coordinator's validation drops the ID once that time has passed.

## Not ported from the reference provider

- The reference calls Yahoo's sync endpoint, which needs a hashed email on every call. The logged-in experience is out of scope, so nothing is ported; this provider reads the `connectId` cookie only.

## Tasks

- [ ] Add `connectid.rs` and register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: ID and expiry from `lastSynced + ttl`, `he`/`puid` not stored, TTL capped and defaulted, missing, malformed and opt-out cookies ignored.
- [ ] Add a commented `kind = "connectid"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

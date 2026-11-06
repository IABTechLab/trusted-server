# Prebid SharedID Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store the Prebid SharedID (publisher common ID) from its first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "sharedid"`. No server call. When no cookie ID and no live stored ID exist, TS generates a random UUID v4 into KV only (no cookie set); a later Prebid cookie replaces it.

**Architecture:** `crates/trusted-server-identity-providers/src/sharedid.rs` holds `SharedId` (a `ProviderKind`), `SharedIdSettings` and `SharedIdProvider`. The provider implements `IdentityProvider::read_request_cookies` and `generate`; `build_request` always fails, so no lookup is sent. `common::device_eid` builds the agent type 1 EID.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Prebid's `sharedIdSystem` writes a bare UUID to the publisher-configured cookie, usually `_sharedid`; older pages use `_pubcid`.
- Default `cookie_names = ["_sharedid", "_pubcid"]`, read in order.
- The `_pubcid_optout` cookie suppresses the ID, as in Prebid.
- A blank or opt-out (all-zero) value reads as no ID.

No Lockr capability.

## Stored ID

- One EID for the single configured source, one UID with `atype = 1`, as Prebid sends `pubcid.org`.
- Cookie IDs: default bounded expiry; a cookie repeating the held ID is not rewritten, so the expiry renews only when the source is due.
- Generated IDs expire after 30 days (`MAX_TTL`), not the default 24 hours.

## Tasks

- [ ] Add `sharedid.rs` and `common::device_eid`; register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: `_sharedid` before `_pubcid`, opt-out cookie honoured, blank and opt-out IDs ignored.
- [ ] `generate` (UUID v4, 30-day expiry) + test.
- [ ] Add a commented `kind = "sharedid"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

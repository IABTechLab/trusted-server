# Prebid SharedID Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store the Prebid SharedID (publisher common ID) from its first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "sharedid"`. No server call.

**Architecture:** `crates/trusted-server-identity-providers/src/sharedid.rs` holds `SharedId` (a `ProviderKind`), `SharedIdSettings` and `SharedIdProvider`. The provider implements only `IdentityProvider::read_request_cookies`; `build_request` always fails, so no lookup is sent. `common::device_eid` builds the agent type 1 EID.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Prebid's `sharedIdSystem` writes a bare UUID to the publisher-configured cookie, usually `_sharedid`; older pages use `_pubcid`.
- Default `cookie_names = ["_sharedid", "_pubcid"]`, read in order.
- The `_pubcid_optout` cookie suppresses the ID, as in Prebid.
- A blank or opt-out (all-zero) value reads as no ID.

No Lockr capability.

## Stored ID

- One EID for the single configured source, one UID with `atype = 1`, as Prebid sends `pubcid.org`.
- Default bounded expiry; each request carrying the cookie refreshes it.

## Tasks

- [ ] Add `sharedid.rs` and `common::device_eid`; register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: `_sharedid` before `_pubcid`, opt-out cookie honoured, blank and opt-out IDs ignored.
- [ ] Add a commented `kind = "sharedid"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

# Criteo Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented; checkboxes are not updated.

**Goal:** Store the Criteo ID from Criteo's first-party cookie, as an `[[ec.identity_providers]]` entry with `kind = "criteo"`. No server call.

**Architecture:** `crates/trusted-server-identity-providers/src/criteo.rs` holds `Criteo` (a `ProviderKind`), `CriteoSettings` and `CriteoProvider`. The provider implements only `IdentityProvider::read_request_cookies`; `build_request` always fails.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

## Cookie

- Prebid's `criteoIdSystem` keeps Criteo's bid ID in the `cto_bidid` cookie (and local storage) and sends that value unchanged, still percent-encoded, as the `criteo.com` EID. `cto_bundle` is Criteo's own sync state and is not an ID.
- Default `cookie_names = ["cto_bidid"]`, read in order. The value is stored verbatim, not decoded.
- A blank or opt-out value reads as no ID. Criteo does not set `cto_bidid` on every page view, so the ID arrives on a later request.

No Lockr capability.

## Stored ID

- One EID for the single configured source, one UID with `atype = 1`.
- Default bounded expiry; a cookie repeating the held ID is not rewritten, so the expiry renews only when the source is due.

## Not ported

- Criteo has no public publisher S2S ID API; Criteo's `gum.criteo.com` sync call needs Criteo's third-party cookie, so it is not made; the first-party cookie is enough.

## Tasks

- [ ] Add `criteo.rs`; register the kind in `is_registered_kind` and `build`.
- [ ] Unit tests: value stored verbatim, missing and blank cookies ignored.
- [ ] Add a commented `kind = "criteo"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

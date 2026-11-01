# LiveIntent Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve `LiveIntent`'s IDs at the edge as an anonymous `[[ec.identity_providers]]` entry with `kind = "liveintent"`, storing one EID per configured attribute.

**Architecture:** `crates/trusted-server-identity-providers/src/liveintent.rs` holds `LiveIntent` (a `ProviderKind`), `LiveIntentSettings` and `LiveIntentProvider` (an `IdentityProvider`), plus one `build` arm in `lib.rs`. The provider is pure: `build_request` builds one `GET` to the identity exchange, and `parse_response` maps returned attributes to sources. The coordinator from PR A applies consent, the source gate, validation, expiry and persistence.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

**Changes from the earlier LiveIntent plan (built on the old descriptor framework):**

- Anonymous only: no hashed email, and no first-party `LiveIntent` cookie (`duid`), because `LookupInput` carries no cookies.
- The attribute-to-source table is operator configuration (`settings.attributes`), not a hard-coded list; every mapped source must be one of the provider's `sources`.
- No page URL in `LookupInput`, so `pu` is the publisher origin.
- EID-level fields are ignored and no `ext` is synthesized (`segments`, `provider`, `rtiPartner`), because `ProviderUid::ext` borrows from the response body.
- The `Expires` header becomes `ProviderExpiry::At`; the provider never reads the clock.

## Settings

| Key              | Default       | Meaning                                           |
| ---------------- | ------------- | ------------------------------------------------- |
| `publisher_id`   | `any`         | Path segment after the distributor.               |
| `distributor_id` | none          | Sent as `did`; the path uses `prebid` when unset. |
| `attributes`     | required, ≥ 1 | `LiveIntent` attribute name → stored source.      |

## Request

- `GET <endpoint>/<distributor or prebid>/<publisher_id>` with `Origin: https://<publisher_domain>`.
- Query: `did`, `gdpr`, `gdpr_consent`, `us_privacy`, `gpp_s`, `gpp_as`, `pu`, `pip` or `pip6` (base64url of a routable client IP), and one `resolve` per attribute whose source is due.

## Response

- `204 No Content` is `LookupOutcome::NoMatch`; another non-2xx status or a non-object body is `ProviderError::InvalidResponse`.
- Each configured attribute with a non-empty string value becomes one EID with `atype = 3`, borrowed from the body.

## Tasks

- [ ] Add `liveintent.rs` and the `build` arm.
- [ ] Unit tests: unconfigured mapped source rejected, request shape with only due attributes, attributes mapped with the `Expires` expiry, no-content and empty IDs are no match, error status and malformed body rejected.
- [ ] Add a commented `kind = "liveintent"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

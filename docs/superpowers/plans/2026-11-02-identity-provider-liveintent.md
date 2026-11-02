# LiveIntent Identity Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve `LiveIntent`'s IDs at the edge as a no-login `[[ec.identity_providers]]` entry with `kind = "liveintent"`, storing one EID per resolved attribute.

**Architecture:** `crates/trusted-server-identity-providers/src/liveintent.rs` holds `LiveIntent` (a `ProviderKind`), `LiveIntentSettings` and `LiveIntentProvider` (an `IdentityProvider`), plus one `build` arm in `lib.rs`. The provider is pure: `build_request` builds one `GET` to the identity exchange, and `parse_response` maps returned attributes to sources. The coordinator from PR A applies consent, the source gate, validation, expiry and persistence.

**Spec:** `docs/superpowers/specs/2026-10-30-s2s-eid-collection-design.md`, section 7.

**Changes from the earlier LiveIntent plan (built on the old descriptor framework):**

- No login: no hashed email. The first-party `LiveIntent` cookie (`_lc2_fpi` by default) is sent as `duid`, read from `LookupInput::cookies`.
- The attribute-to-source table is fixed, as in the reference provider; `settings.resolve` picks attributes from it, and every picked attribute's source must be one of the provider's `sources`.
- No page URL in `LookupInput`, so `pu` is the publisher origin.
- EID-level fields are preserved. As in Prebid, every attribute other than `nonId` is a partner ID whose UID carries `ext` `{"provider":"liveintent.com"}`; the Trade Desk ID's UID also carries `rtiPartner: "TDID"` and its EID `matcher = "liveintent.com"`. The reply's `segments` become the `nonId` EID's `ext` (`{"segments": ...}`), which needs the PR A change letting a provider return an owned EID `ext`.
- The `Expires` header becomes `ProviderExpiry::At`; the provider never reads the clock.

## Settings

| Key              | Default        | Meaning                                                                                                                                                                                                                                                      |
| ---------------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `publisher_id`   | `any`          | Path segment after the distributor.                                                                                                                                                                                                                          |
| `distributor_id` | none           | A `did-...` ID; sent as `did` and used as the path. LiveIntent rejects any other form, so build rejects it. The path uses `prebid` when unset.                                                                                                               |
| `resolve`        | `["nonId"]`    | Attributes to ask for, from the fixed table: `nonId` liveintent.com, `bidswitch`, `medianet`, `magnite`, `index`, `openx`, `pubmatic`, `sovrn`, `sharethrough`, `sonobi`, `triplelift`, `zetassp`, `vidazoo`, `nexxen`, `uid2`, `thetradedesk` adserver.org. |
| `cookie_names`   | `["_lc2_fpi"]` | First-party cookies; the first present one is sent as `duid`.                                                                                                                                                                                                |

No Lockr capability: Lockr AIM is not documented to deliver LiveIntent IDs, and the lookup uses LiveIntent's own signals, so a `[settings.lockr]` table is rejected.

## Request

- `GET <endpoint>/<distributor or prebid>/<publisher_id>` with `Origin: https://<publisher_domain>`.
- Query: `did`, `duid`, `gdpr`, `gdpr_consent`, `us_privacy`, `gpp_s`, `gpp_as`, `pu`, `pip` or `pip6` (base64url of a routable client IP), and one `resolve` per attribute whose source is due.

## Response

- `204 No Content` is `LookupOutcome::NoMatch`. LiveIntent answers `204` to anonymous `gdpr=1` lookups and returns only `nonId` without its own third-party cookie; another non-2xx status or a non-object body is `ProviderError::InvalidResponse`.
- Each resolved attribute with a non-empty string value becomes one EID with `atype = 3`, borrowed from the body; `segments`, the TDID `matcher` and the UID `ext` values are added as above.

## Tasks

- [ ] Add `liveintent.rs` and the `build` arm.
- [ ] Unit tests: unknown attribute and unconfigured source rejected, `duid` from the first present cookie, segments/matcher/ext mapping, request shape with only due attributes, attributes mapped with the `Expires` expiry, no-content and empty IDs are no match, error status and malformed body rejected.
- [ ] Add a commented `kind = "liveintent"` example to `trusted-server.example.toml`.
- [ ] Run `cargo test-fastly` and `cargo clippy-fastly`.

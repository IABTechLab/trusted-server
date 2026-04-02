# Technical Specification: Edge Cookie (EC)

**Status:** Draft
**Author:** Engineering
**PRD reference:** `docs/internal/ssc-prd.md`
**Last updated:** 2026-03-18

---

## Table of Contents

1. [Overview](#1-overview)
2. [Architecture Overview](#2-architecture-overview)
3. [Module Structure](#3-module-structure)
4. [EC Identity Generation](#4-ec-identity-generation)
5. [Cookie and Header Handling](#5-cookie-and-header-handling)
6. [Consent Enforcement](#6-consent-enforcement)
7. [KV Store Identity Graph](#7-kv-store-identity-graph)
8. [Pixel Sync Endpoint (`GET /sync`)](#8-pixel-sync-endpoint-get-sync)
9. [S2S Batch Sync API (`POST /_ts/api/v1/sync`)](#9-s2s-batch-sync-api-post-apiv1sync)
10. [S2S Pull Sync (TS-Initiated)](#10-s2s-pull-sync-ts-initiated)
11. [Identity Resolution Endpoint (`GET /identify`)](#11-identity-resolution-endpoint-get-identify)
12. [Bidstream Decoration (`/auction` Mode B)](#12-bidstream-decoration-auction-mode-b)
13. [Partner Registry and Admin Endpoint](#13-partner-registry-and-admin-endpoint)
14. [Configuration](#14-configuration)
15. [Constants and Header Names](#15-constants-and-header-names)
16. [Error Handling](#16-error-handling)
17. [Request Routing](#17-request-routing)
18. [Testing Strategy](#18-testing-strategy)
19. [Implementation Order](#19-implementation-order)

---

## 1. Overview

Edge Cookie (EC) replaces SyntheticID as the primary user identity mechanism in Trusted Server. It uses a simpler, more stable signal (IP address + publisher passphrase), adds consent enforcement, and backs identity with a server-side KV graph that accumulates partner IDs over time.

EC is the full replacement for SyntheticID. The PRD explicitly states backward compatibility is a non-goal. There is no coexistence, no fallback, no transitional period.

**Prerequisites (must be merged before this epic begins):**

- **SyntheticID removal** — [PR #479](https://github.com/IABTechLab/trusted-server/pull/479) removes SyntheticID from all active code paths: `get_or_generate_synthetic_id()`, `COOKIE_SYNTHETIC_ID`, `X-Synthetic-*` headers, `synthetic.rs` module, `settings.synthetic` config, and all SyntheticID generation/cookie code from `publisher.rs`, `endpoints.rs`, and `registry.rs`. It also renames `ConsentPipelineInput.synthetic_id` to `identity_key`, updates consent KV helper parameters/docs, and handles consent-store key migration (old SyntheticID keys orphaned, TTL expiry cleans them up). **This PR must be merged before implementation of this spec begins.** The spec assumes a codebase where SyntheticID no longer exists. Verify before starting:
  - `grep -r 'synthetic_id' crates/` returns no hits outside test fixtures
  - `grep -r 'X-Synthetic' crates/` returns no hits
  - `trusted-server.toml` has no `[synthetic]` section
  - `ConsentPipelineInput` uses `identity_key`, not `synthetic_id`
- **Consent implementation** — The consent pipeline (`build_consent_context()`, `ConsentContext`, `allows_ec_creation()`, TCF/GPP/US-Privacy decoding) is implemented and available as a stable interface before this epic. PR `#380` merged to `main`. EC calls `allows_ec_creation()` directly — no new gating functions are introduced. Note: EC changes the _phase order_ relative to the old SyntheticID flow — consent is evaluated before EC generation, so first-visit consent KV persistence is deferred to the second request (see §6.1.1 for full analysis).

**Deferred from this spec (not in scope):**

- TS Lite deployment mode (PRD Section 5)
- JOSE-signed KV entries / buyer attestation, and the associated `/.well-known/trusted-server.json` attestation object + `Cache-Control: max-age=3600` response (PRD Section 8.7). The existing discovery endpoint and its tests (`endpoints.rs:579–594`) assert only `version` and `jwks` fields — this spec does not modify that endpoint. Any addition of the PRD-required `attestation` field is deferred to when JOSE signing ships.
- Data deletion framework JWT endpoint (PRD Section 7.4) — the formal IAB-compliant deletion endpoint is deferred. The PRD explicitly acknowledges that manual KV deletion is the interim process until the formal endpoint ships, and states that regulated onboarding requires the formal endpoint to be in place first. This spec implements the manual-deletion-only interim; the JWT endpoint is a prerequisite for regulated onboarding and must be tracked separately.
- Winner notification EC headers on publisher ad server delivery — the current `/auction` path returns JSON inline to the JS caller; there is no server-to-server delivery step. A future delivery architecture is deferred. Note: §12.5 (auction response headers for the existing inline path) IS in scope; only the not-yet-built server-to-server delivery is deferred.

---

## 2. Architecture Overview

```
Browser Request
      │
      ▼
┌─────────────────────────────────────────────────┐
│                 main.rs (router)                │
│  extract GeoInfo → enforce auth → route_request │
└──────────┬──────────────────────────────────────┘
           │
Two-phase model (matches existing codebase pattern):

Phase 1 — pre-routing (like `GeoInfo::from_request()`):
    ┌─────────────────────────────────────────┐
    │  EcContext::read_from_request()          │
    │  - read ts-ec cookie / X-ts-ec header   │
    │  - build_consent_context() → ConsentContext  │
    │  - allows_ec_creation(consent)               │
    │  No generation. No cookie writes.       │
    └──────┬──────────────────────────────────┘
           │
Phase 2 — inside organic handlers only:
   ┌───────┼──────────────────────────────────────────────────┐
   │       │                                                   │
   ▼       ▼                                                   ▼
handle_publisher_request()     integration_registry.handle_proxy()
calls ec_context.generate_if_needed()   calls ec_context.generate_if_needed()

EC route handlers (GET /sync, GET /identify, POST /auction,
POST /_ts/api/v1/sync, POST /_ts/admin/*) NEVER call generate_if_needed().
`/identify`, `/auction`, `POST /_ts/api/v1/sync`, and `POST /_ts/admin/*`
use `EcContext` in read-only form. `GET /sync` is the one exception:
it never bootstraps an EC, but it may replace `ec_context.consent`
with a locally-decoded fallback consent context for that request only
when the optional `consent` query param is the sole available signal.
/auction reads EC identity but never bootstraps it — the publisher
page-load path generates the EC before any auction request arrives.

ec_finalize_response() — after every handler:
    - consent withdrawn + cookie present? → clear_ec_on_response() + tombstone
    - returning-user mismatch? → set_ec_on_response() [reconcile cookie to header EC]
    - ec_generated == true? → set_ec_on_response() [new cookie only]
```

EC state flows through an `EcContext` struct created once per request and passed through handlers.

---

## 3. Module Structure

New files in `crates/common/src/`:

```
crates/common/src/
  ec/
    mod.rs          — EcContext, pub re-exports
    identity.rs     — EC generation (HMAC-SHA256, IP normalization)
    cookie.rs       — create_ec_cookie(), delete_ec_cookie(), set_ec_on_response()
    finalize.rs     — ec_finalize_response() (cookie write/delete, last_seen, tombstone)
    kv.rs           — KvIdentityGraph, read/write/delete identity entries
    partner.rs      — PartnerRecord, PartnerStore, load_partner()
    sync_pixel.rs   — handle_sync() handler
    sync_batch.rs   — handle_batch_sync() handler
    pull_sync.rs    — PullSyncDispatcher, dispatch_background()
    identify.rs     — handle_identify() handler
    admin.rs        — handle_register_partner() handler
```

Existing files modified:

| File                             | Change                                                |
| -------------------------------- | ----------------------------------------------------- |
| `crates/common/src/settings.rs`  | Add `EdgeCookie` settings struct                      |
| `crates/common/src/constants.rs` | Add EC header/cookie name constants                   |
| `crates/common/src/error.rs`     | Add `EdgeCookie` error variant                        |
| `crates/common/src/auction/`     | Inject EC into `user.id`, `user.eids`, `user.consent` |
| `crates/fastly/src/main.rs`      | Register new routes, run EC middleware                |

---

## 4. EC Identity Generation

### 4.1 Module: `ec/identity.rs`

The EC generation mirrors the SyntheticID approach (`synthetic.rs`) but strips volatile inputs.

```rust
/// Generates a fresh EC value from IP address and publisher passphrase.
///
/// Output format: `{64-char hex HMAC-SHA256}.{6-char random alphanumeric}`
///
/// # Errors
///
/// Returns `EdgeCookie` error if HMAC computation fails.
pub fn generate_ec(passphrase: &str, ip: IpAddr) -> Result<String, Report<TrustedServerError>>;

/// Normalizes an IP address for use as an HMAC input.
///
/// - IPv4: returned as-is (`"203.0.113.1"`)
/// - IPv6: truncated to /64 prefix — first 4 hextets joined by `:`, lower-cased
///   (`"2001:db8:85a3:0"`)
/// - On dual-stack, the caller must supply the IPv6 address; this function does
///   not choose between them.
pub fn normalize_ip(ip: IpAddr) -> String;

/// Extracts the stable 64-character hex prefix from a full EC value.
///
/// The prefix is used as the KV store key. The `.suffix` is discarded.
///
/// Returns `None` if the value is not in `{64-hex}.{6-alnum}` format.
pub fn ec_hash(ec_value: &str) -> Option<&str>;
```

**HMAC inputs (fixed — no template):**

| Input     | Value                     |
| --------- | ------------------------- |
| Message   | `normalize_ip(client_ip)` |
| Key       | `settings.ec.passphrase`  |
| Algorithm | HMAC-SHA256               |

**Output format:** `{64-char lowercase hex}.{6-char random alphanumeric}`

The random suffix is generated with `fastly::rand` (same approach as SyntheticID). Once set in a cookie the full value is preserved; only the hash prefix is used as the KV key.

**IPv6 /64 prefix:** Split on `:`, take first 4 groups, join with `:`. Example:
`2001:db8:85a3:0000:0000:8a2e:0370:7334` → `2001:db8:85a3:0`.

**IP source:** Use `req.get_client_ip_addr()` — Fastly's trusted API that returns the verified client IP without relying on any request header. This is the same source used by the existing `synthetic.rs` IP handling. Do not fall back to `X-Forwarded-For` or any other header — those are forgeable by clients. If the API returns `None`, `EcContext.client_ip` is `None` and `generate_if_needed()` logs `warn` and skips EC generation — the page loads without an EC. This is best-effort; a missing client IP never produces a 500.

On dual-stack: prefer IPv6 if the returned address is IPv6; otherwise use IPv4.

### 4.2 EC Retrieval Priority

Pre-routing, EC state is read (not generated) from the inbound request:

1. `X-ts-ec` request header (forwarded by publisher infrastructure)
2. `ts-ec` cookie
3. Neither present → `ec_value = None`, `ec_was_present = false`

When both header and cookie are present, the **header wins** as `ec_value` (used by handlers for identity reads, KV lookups, and response headers). `cookie_was_present` is still set to `true`.

**Mismatch handling:** If the header and cookie carry different EC values, `EcContext` tracks both:

- `ec_value` = header value (authoritative for handler reads)
- `cookie_ec_value` = cookie value (tracked separately for withdrawal)

On consent **withdrawal** (`!allows_ec_creation && cookie_was_present`):

- Delete the browser cookie (always, based on `cookie_was_present`)
- Tombstone the **cookie-derived** hash: `kv.write_withdrawal_tombstone(ec_hash(cookie_ec_value))`
- If the header-derived hash differs, also tombstone it: `kv.write_withdrawal_tombstone(ec_hash(ec_value))`
- This matches the existing SyntheticID behavior where revocation targets the cookie value (`publisher.rs:515`), not the header value.

On **non-withdrawal** paths (last_seen, handler reads): use `ec_value` (header-derived) as the active identity. When `cookie_ec_value` is set (mismatch), `ec_finalize_response()` overwrites the browser cookie with the header-derived `ec_value` via `set_ec_on_response()`. This reconciles the browser identity to match the publisher-forwarded identity and prevents persistent oscillation between two ECs on subsequent requests.

**Validation:** Both the header and cookie values are validated independently via `ec_hash()` (`{64-hex}.{6-alnum}` format). If the header is present but malformed, it is discarded and the cookie value is used instead (if valid). A malformed header must not suppress a valid cookie — bad forwarding infrastructure should not break returning-user identity. `cookie_was_present` is set based on the raw cookie existing, regardless of validity — an invalid cookie value is still a cookie that needs to be cleared on withdrawal.

Generation (step 3 above becoming a new EC) happens only inside organic handlers — see §5.4. This logic lives in `EcContext::read_from_request()` (phase 1) and `EcContext::generate_if_needed()` (phase 2).

### 4.3 `EcContext`

```rust
/// Per-request Edge Cookie state. Constructed pre-routing once per request.
/// Organic handlers call `generate_if_needed()` to mint new ECs. `/sync` is the
/// one EC route that may replace `consent` with a locally-decoded fallback for
/// the remainder of that request only.
pub struct EcContext {
    /// Full EC value (`hash.suffix`), if present on request or generated this request.
    pub ec_value: Option<String>,
    /// Whether the `ts-ec` **cookie** was present on the inbound request.
    /// This is the only field that gates consent-withdrawal cookie deletion —
    /// the PRD's delete branch is conditioned on the cookie, not on X-ts-ec header.
    pub cookie_was_present: bool,
    /// The cookie's EC value, if different from `ec_value` (header won priority).
    /// Used only for withdrawal: tombstone targets the cookie-derived hash to match
    /// existing SyntheticID revocation behavior (`publisher.rs:515`).
    /// `None` when cookie absent or cookie == header value.
    pub cookie_ec_value: Option<String>,
    /// Whether any EC value was available (cookie OR X-ts-ec header).
    pub ec_was_present: bool,
    /// Set to true by `generate_if_needed()` when a new EC is minted this request.
    /// `ec_finalize_response()` uses this to decide whether to write a Set-Cookie header.
    pub ec_generated: bool,
    /// Full consent context from the prerequisite consent pipeline.
    /// Use `allows_ec_creation(&self.consent)` to derive a grant/deny decision.
    /// Raw TCF/GPP strings (for KV writes and `user.consent`) are on `consent.raw_tc_string`
    /// and `consent.raw_gpp_string`.
    pub consent: ConsentContext,
    /// Client IP extracted from `req` during `read_from_request()`.
    /// Stored here so pull sync can use it after `req` has been consumed by routing.
    /// `None` only if Fastly's `get_client_ip_addr()` returns `None`.
    pub client_ip: Option<IpAddr>,
}

impl EcContext {
    /// Phase 1: reads cookie/header and builds consent context. Does not generate.
    /// Does not write to the **EC identity KV store**. Called pre-routing, like
    /// `GeoInfo::from_request()` in the current `main.rs`.
    ///
    /// Calls `build_consent_context()` with the EC hash (when present) passed
    /// via `ConsentPipelineInput.identity_key` (renamed from `synthetic_id`
    /// in PR #479).
    ///
    /// When an EC hash is available (returning user), this enables the consent
    /// pipeline's KV fallback (read) and KV persistence (write to the
    /// **consent** KV store). On a first visit (no EC cookie), `ec_hash` is
    /// `None` and no consent KV interaction occurs; consent is evaluated purely
    /// from request cookies/headers. This means consent is not persisted to
    /// consent KV until the user's second request. See §6.1.1.
    pub fn read_from_request(
        req: &Request,
        settings: &Settings,
        geo: Option<&GeoInfo>,
    ) -> Result<Self, Report<TrustedServerError>>;

    /// Phase 2: generates a new EC if none is present and consent is granted.
    /// Called only inside organic handlers (`handle_publisher_request`,
    /// `integration_registry.handle_proxy`). Never called by EC route handlers
    /// or the auction handler — those consume EC identity but never bootstrap it.
    /// Sets `ec_generated = true` when a new EC is minted, and writes the initial
    /// KV entry via `kv.create_or_revive()` (best-effort — logs warn on failure,
    /// does not block). Using `create_or_revive` (not `create`) ensures that a user
    /// who re-consents within the 24-hour tombstone window recovers immediately.
    ///
    /// **Best-effort / never 500s organic traffic.** If EC generation fails
    /// (e.g., `get_client_ip_addr()` returns `None`), the function logs `warn`
    /// and returns without setting `ec_generated`. The organic handler proceeds
    /// normally without an EC — the page still loads. Callers must NOT propagate
    /// this error with `?`.
    pub fn generate_if_needed(
        &mut self,
        settings: &Settings,
        kv: &KvIdentityGraph,
    );

    /// Returns the stable 64-char hex prefix, or `None` if no EC.
    pub fn ec_hash(&self) -> Option<&str>;
}
```

**`ec_finalize_response()` behavior** (signature: `ec_finalize_response(settings, geo, ec_context, kv, response)`):

1. If `!allows_ec_creation(&consent) && cookie_was_present`: call `clear_ec_on_response()` (deletes cookie **and** strips any handler-built `X-ts-ec`, `X-ts-eids`, `X-ts-ec-consent`, `x-ts-eids-truncated`, and `X-ts-<partner_id>` response headers) and write withdrawal tombstones for each valid known EC hash (cookie-derived and, when different, header-derived). This runs on **every route** — consent withdrawal is always real-time enforced. Keyed on `cookie_was_present`, not `ec_was_present`, because only a cookie-held EC can be deleted by the browser. When the cookie is malformed and there is no valid header-derived hash, no tombstone is written.
2. If `ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)`: call `kv.update_last_seen()` (debounced). If `cookie_ec_value.is_some()`, also call `set_ec_on_response()` to reconcile the browser cookie to the authoritative header-derived EC.
3. If `ec_generated == true`: call `set_ec_on_response()` — sets `Set-Cookie` and `X-ts-ec`. KV create already happened inside `generate_if_needed()`; `ec_finalize_response()` does NOT write KV beyond tombstones and `last_seen`.
4. Handler-built response headers (`X-ts-ec`, `X-ts-eids` set directly by `/identify`) are preserved on non-withdrawal paths only.

**Note on `kv_degraded`:** Not on `EcContext` — `read_from_request()` does not read KV. Handlers track degraded state locally. `/identify` returns `degraded: true` in the JSON body on KV read failure; the auction handler treats a failed read as `eids: []`.

````

---

## 5. Cookie and Header Handling

### 5.1 Cookie attributes

| Attribute | Value |
|-----------|-------|
| Name | `ts-ec` |
| Domain | `.{publisher.domain}` — derived by prepending `.` to `settings.publisher.domain`, **not** `settings.publisher.cookie_domain` |
| Path | `/` |
| Secure | Yes |
| SameSite | `Lax` |
| Max-Age | `31536000` (1 year) |
| HttpOnly | No |

### 5.2 Module: `ec/cookie.rs`

The `cookie_domain` parameter passed to all functions below is computed as
`format!(".{}", settings.publisher.domain)`. Do **not** use
`settings.publisher.cookie_domain` — that field is used by other cookie helpers
and does not carry the EC ownership guarantee. No startup validation change is
needed for `publisher.cookie_domain` — it continues to serve its existing
purpose for non-EC cookies. EC simply does not read it.

```rust
/// Builds the `Set-Cookie` header value for a newly generated EC.
pub fn create_ec_cookie(ec_value: &str, cookie_domain: &str) -> String;

/// Builds the `Set-Cookie` header value that expires (deletes) the EC cookie.
pub fn delete_ec_cookie(cookie_domain: &str) -> String;
// Sets Max-Age=0 with same Domain/Path/Secure/SameSite attributes.

/// Sets the EC cookie and `X-ts-ec` response header on a response.
pub fn set_ec_on_response(response: &mut Response, ec_value: &str, cookie_domain: &str);

/// Removes the EC cookie and strips all EC-related response headers:
/// `X-ts-ec`, `X-ts-eids`, `X-ts-ec-consent`, `x-ts-eids-truncated`,
/// and any `X-ts-<partner_id>` headers. Called on consent withdrawal to
/// prevent leaking EC identity in handler-built headers.
pub fn clear_ec_on_response(response: &mut Response, cookie_domain: &str);
````

### 5.3 Response header

`X-ts-ec: {ec_hash.suffix}` is set by `set_ec_on_response()` when an EC is available. In current behavior, `ec_finalize_response()` calls `set_ec_on_response()` for returning users (`ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)`) and for newly generated ECs (`ec_generated == true`). `/identify` and `/auction` also set EC-related headers on their response paths.

This header is added to `INTERNAL_HEADERS` in `constants.rs` so it is stripped before proxying to downstream backends, consistent with existing `X-ts-*` handling.

### 5.4 Per-request EC lifecycle

**Phase 1 — pre-routing** (always runs, all routes):

```
EcContext::read_from_request()
  Read ts-ec cookie value and X-ts-ec header value independently
  Validate each via ec_hash() — returns None if not {64-hex}.{6-alnum}
  If both valid: header wins as ec_value; cookie stored as cookie_ec_value (if differs)
  If only header valid: ec_value = header, cookie_ec_value = None
  If only cookie valid: ec_value = cookie, cookie_ec_value = None
  If neither valid: ec_value = None
  ec_was_present = ec_value.is_some()
  cookie_was_present = ts-ec cookie raw key exists (regardless of validity)
  ec_hash = ec_value.as_deref().and_then(ec_hash)   // None on first visit or malformed
  build_consent_context(jar, req, config, geo, ec_hash) → consent: ConsentContext
  // ec_hash is the identity key for consent KV (renamed from synthetic_id in PR #479).
  // When ec_hash is Some: consent KV fallback read + consent KV write (to consent store, not EC store).
  // When ec_hash is None (first visit): no consent KV interaction — cookies/headers only.
  ec_generated = false
```

**Phase 2 — inside organic handlers only** (`handle_publisher_request`, `handle_proxy`):

```
ec_context.generate_if_needed(settings, &kv)    // best-effort — never 500s
  └── allows_ec_creation(&consent) && ec_value == None?
          → client_ip from ec_context.client_ip (captured in phase 1)
          → client_ip is None? log warn, return (no EC generation possible)
          → generate_ec(passphrase, ip)
          → ec_value = Some(new_ec)
          → ec_generated = true
          → kv.create_or_revive(ec_hash, &entry)   (best-effort, log warn if fails)
            // create_or_revive overwrites a tombstone (ok=false) on re-consent
            // no-ops if a live entry (ok=true) already exists
```

**`ec_finalize_response(settings, geo, ec_context, &kv, response)` — always runs, all routes:**

```
  ├── !allows_ec_creation(&consent) && cookie_was_present?
  │       → clear_ec_on_response()             (delete cookie + strip ALL EC headers from response)
  │       → // Tombstone all known valid EC hashes. May be 0, 1, or 2 hashes.
  │         if let Some(cookie_hash) = cookie_ec_value.and_then(|v| ec_hash(&v)):
  │           kv.write_withdrawal_tombstone(cookie_hash)       // cookie-derived hash
  │         if let Some(header_hash) = ec_value.and_then(|v| ec_hash(&v)):
  │           if Some(header_hash) != cookie_hash:
  │             kv.write_withdrawal_tombstone(header_hash)     // header-derived hash (if different)
  │         // When cookie is malformed and no valid header exists: no tombstone written.
  │         // Cookie deletion is still the authoritative enforcement mechanism.
  │         // Tombstone fails? log error, do NOT block — no retry possible on browser path.
  │
  ├── ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)?
  │       → kv.update_last_seen(ec_hash, now())   (returning user — debounced at 300s)
  │       → set_ec_on_response()   (Set-Cookie + X-ts-ec refresh on returning user)
  │
  └── ec_generated == true?
          → set_ec_on_response()        (Set-Cookie + X-ts-ec on response)
```

EC route handlers (`GET /sync`, `GET /identify`, `POST /_ts/api/v1/sync`, `POST /_ts/admin/*`) never call `generate_if_needed()`. `ec_finalize_response()` will still delete the cookie on those routes if consent is withdrawn — that is intentional.

**Cookie write rule:** `Set-Cookie` is written when `set_ec_on_response()` is called. In current behavior this includes returning-user requests (consent allowed + EC present) and first-time generation (`ec_generated == true`), so `Max-Age` is refreshed on ordinary returning requests.

---

## 6. Consent Enforcement

### 6.1 Prerequisite contracts

Consent decoding shipped in `#380` (already merged). This spec treats the following as stable, pre-existing contracts — it does not implement them:

- **`build_consent_context(input: &ConsentPipelineInput) -> ConsentContext`** — the main entry point. Extracts, decodes, and normalizes signals from cookies and headers.
- **`ConsentContext`** — carries: `raw_tc_string`, `raw_gpp_string`, `raw_us_privacy`, `gdpr_applies: bool`, `tcf: Option<TcfConsent>`, `gpp: Option<GppConsent>`, `us_privacy: Option<UsPrivacy>`, `expired: bool`, `gpc: bool`, `jurisdiction: Jurisdiction`, `source: ConsentSource`
- **`TcfConsent.has_storage_consent()`** — true when TCF Purpose 1 (store/access on device) is granted
- **`Jurisdiction { Gdpr, UsState(String), NonRegulated, Unknown }`** — detected privacy regime (from geo + config)
- **`UsPrivacy.opt_out_sale: PrivacyFlag`** — CCPA opt-out (`Yes`/`No`/`NotApplicable`)

### 6.1.1 EC consent gating

EC reuses the existing `allows_ec_creation(&ConsentContext) -> bool` function
from the consent module (`consent/mod.rs`). No parallel gating function is
introduced — EC calls `allows_ec_creation()` directly for all consent decisions
(EC generation, withdrawal detection, sync gating).

There is no EC-specific consent gate and no behavior change to
`allows_ec_creation()` in this spec. Shared consent-policy semantics stay in
the consent module; EC only consumes that existing decision.

**Consent pipeline integration:**

`EcContext::read_from_request()` calls `build_consent_context()` with the EC hash as the identity key, passed via `ConsentPipelineInput.identity_key` (renamed from `synthetic_id` in PR #479). The consent pipeline's KV persistence and fallback behavior works with EC hashes:

- **Returning user** (EC cookie present → `ec_hash` is `Some`): consent KV fallback read is available when consent cookies are absent; consent KV write persists cookie-sourced consent for future requests. Note: `build_consent_context()` calls `try_kv_write()` internally, so phase 1 writes to the **consent** KV store (not the EC identity store).
- **First visit** (no EC cookie → `ec_hash` is `None`): no consent KV interaction. Consent is evaluated purely from request cookies/headers. The gap: consent is not persisted to consent KV on the first request. This is accepted — in regulated jurisdictions (GDPR, US state), consent cookies/headers must be present for `allows_ec_creation()` to return `true`, so there is always a signal to persist on the next request. In non-regulated jurisdictions, `allows_ec_creation()` returns `true` without consent signals, so there is nothing to persist anyway. Consent KV persistence begins on the second request when the EC cookie is present.

**Consent store keying:** Old consent KV entries under SyntheticID keys become orphaned after PR #479 ships. New entries are keyed by EC hash. Orphaned entries expire via TTL — no explicit migration is performed.

**Rollout impact:** At cutover, returning users who relied on consent KV fallback (consent cookies absent, consent loaded from KV under SyntheticID key) will lose that fallback until a new EC-keyed consent entry is written on a subsequent request where consent cookies are present. This is a one-time window: once the EC cookie is set and a request with consent cookies arrives, the consent KV entry is written under the EC hash and fallback works again. The window duration depends on how quickly users return with consent cookies. This is accepted — consent cookies are the primary signal; KV fallback is a secondary mechanism for when cookies are blocked or absent.

All downstream EC logic calls `allows_ec_creation(&self.consent)`. No consent decoding or gating logic is added in this epic.

### 6.2 Consent withdrawal — KV delete

When `allows_ec_creation(&consent)` returns `false` for a user whose **`ts-ec` cookie** is present (`cookie_was_present == true`). A user identified only by the `X-ts-ec` request header is not subject to cookie deletion — there is no cookie to expire.

1. Issue `Set-Cookie: ts-ec=; Max-Age=0; ...` and strip all EC response headers (synchronous — must not fail silently). This always happens when `cookie_was_present == true`.
2. Write tombstone for each valid EC hash available (`cookie_ec_value` and/or `ec_value`). When neither is valid (malformed cookie, no header), **no tombstone is written** — cookie deletion alone is the enforcement mechanism. When at least one valid hash exists: `kv.write_withdrawal_tombstone(hash)` sets `consent.ok = false`, clears partner IDs, TTL 24h — approximately 25ms per write.

The tombstone write runs in the request path (not async) to ensure real-time enforcement. Using a tombstone rather than a hard delete preserves the `consent_withdrawn` signal for batch sync clients for 24 hours — otherwise batch sync cannot distinguish consent withdrawal from an EC that never existed.

If the tombstone write fails:

- Log at `error` level with EC hash
- Do not block the response — cookie deletion is the primary enforcement mechanism
- **No retry is possible on the browser path.** Once the cookie is deleted, subsequent browser requests carry no EC value (`ec_hash()` returns `None`), so there is no hash to tombstone. A failed tombstone means batch sync clients may see `ec_hash_not_found` (after TTL expiry) rather than `consent_withdrawn` — this is accepted degradation. The cookie deletion remains the authoritative enforcement mechanism.

---

## 7. KV Store Identity Graph

### 7.1 Module: `ec/kv.rs`

Two KV stores are used. Their names are configured in `trusted-server.toml`:

| Store            | TOML key           | Purpose                            |
| ---------------- | ------------------ | ---------------------------------- |
| Identity graph   | `ec.ec_store`      | EC hash → identity JSON            |
| Partner registry | `ec.partner_store` | Partner ID → config + API key hash |

### 7.2 Identity graph schema

**KV key:** 64-character hex hash (the stable prefix from `ec_value`, without `.suffix`).

**KV value (JSON, max ~5KB):**

```json
{
  "v": 1,
  "created": 1741824000,
  "last_seen": 1741910400,
  "consent": {
    "tcf": "CP...",
    "gpp": "DBA...",
    "ok": true,
    "updated": 1741910400
  },
  "geo": {
    "country": "US",
    "region": "CA"
  },
  "ids": {
    "ssp_x": { "uid": "abc123", "synced": 1741824000 },
    "liveramp": { "uid": "LR_xyz", "synced": 1741890000 }
  }
}
```

**KV metadata (max 2048 bytes, readable without streaming body):**

```json
{ "ok": true, "country": "US", "v": 1 }
```

The `ok` field in metadata is a **historical consent record for S2S consumers only** — it is set to `false` by `write_withdrawal_tombstone()` so that batch sync clients (`POST /_ts/api/v1/sync`) can return `consent_withdrawn` rather than `ec_hash_not_found` during the 24-hour tombstone TTL.

**`consent.ok` is NOT used to make the withdrawal decision on the main request path.** Consent withdrawal is determined entirely from `allows_ec_creation(&ec_context.consent)` on the current request. When withdrawal is detected, the cookie is deleted and `write_withdrawal_tombstone()` is called in-path (setting `ok = false`, 24h TTL — see §6.2). Engineers must not add a KV read to the consent withdrawal hot path based on this field.

**Rust types:**

```rust
pub struct KvEntry {
    pub v: u8,
    pub created: u64,
    pub last_seen: u64,
    pub consent: KvConsent,
    pub geo: KvGeo,
    pub ids: HashMap<String, KvPartnerId>,
}

pub struct KvConsent {
    pub tcf: Option<String>,
    pub gpp: Option<String>,
    pub ok: bool,
    pub updated: u64,
}

pub struct KvGeo {
    pub country: String,
    pub region: Option<String>,
}

pub struct KvPartnerId {
    pub uid: String,
    pub synced: u64,
}

pub struct KvMetadata {
    pub ok: bool,
    pub country: String,
    pub v: u8,
}
```

### 7.3 TTL

All KV writes use `time_to_live_sec = 31536000` (1 year), matching the cookie `Max-Age`.

### 7.4 Conflict resolution — atomic read-modify-write

Concurrent writes from different partners must not overwrite each other. Each partner's ID is namespaced under `ids[partner_id]` — a write for `ssp_x` must not clobber an existing `liveramp` entry.

Implementation uses Fastly KV Store's **generation markers** (optimistic concurrency):

```rust
pub struct KvIdentityGraph {
    store_name: String,
}

impl KvIdentityGraph {
    pub fn new(store_name: impl Into<String>) -> Self;

    /// Reads the full entry, returning the generation marker for CAS writes.
    pub fn get(
        &self,
        ec_hash: &str,
    ) -> Result<Option<(KvEntry, u64)>, Report<TrustedServerError>>;

    /// Reads only the metadata fields (consent flag, country).
    pub fn get_metadata(
        &self,
        ec_hash: &str,
    ) -> Result<Option<KvMetadata>, Report<TrustedServerError>>;

    /// Creates a new entry. Returns `Ok(())` if successful, `Err` if the key
    /// already exists (concurrent create) or on KV error.
    pub fn create(
        &self,
        ec_hash: &str,
        entry: &KvEntry,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Creates a new entry, OR overwrites an existing tombstone (`consent.ok = false`)
    /// with a fresh entry when the user re-consents within the tombstone TTL.
    ///
    /// Behavior:
    /// - No existing key → behaves identically to `create()`.
    /// - Existing key with `consent.ok = false` (tombstone) → overwrites with
    ///   the new entry via CAS. Retries up to `MAX_CAS_RETRIES` on conflict.
    /// - Existing key with `consent.ok = true` (live entry) → no-op, returns `Ok(())`.
    ///
    /// Called by `generate_if_needed()` instead of `create()`. This ensures that
    /// re-consent recovery is immediate — a user who withdraws and then re-consents
    /// within the 24-hour tombstone window gets a fresh identity entry without delay.
    pub fn create_or_revive(
        &self,
        ec_hash: &str,
        entry: &KvEntry,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Atomically merges `ids[partner_id]` into the existing entry using a
    /// generation marker. Retries up to `MAX_CAS_RETRIES` (3) times on
    /// generation conflict before returning `Err`.
    ///
    /// If the key does not exist, creates a minimal live entry first:
    /// `consent.ok = true`, `consent.tcf = None`, `consent.gpp = None`,
    /// `created = synced`, `last_seen = synced`, `geo.country = "ZZ"`,
    /// `geo.region = None`, and `ids = { partner_id: ... }`.
    ///
    /// This recovery path is intentional: it materializes the graph later when
    /// the initial best-effort `create_or_revive()` on EC generation failed.
    /// Batch sync still performs its explicit existence/tombstone check before
    /// calling this method, so `POST /_ts/api/v1/sync` retains its `ec_hash_not_found`
    /// contract.
    pub fn upsert_partner_id(
        &self,
        ec_hash: &str,
        partner_id: &str,
        uid: &str,
        synced: u64,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Updates `last_seen` timestamp, but only if the stored value is more than
    /// 300 seconds older than `timestamp`. This debounce prevents KV write
    /// thrashing under bursty traffic — Fastly KV enforces a 1 write/sec limit
    /// per key. Callers should log `warn` on failure and continue.
    pub fn update_last_seen(
        &self,
        ec_hash: &str,
        timestamp: u64,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Writes a withdrawal tombstone for consent enforcement.
    ///
    /// Instead of hard-deleting the KV entry, this overwrites it with
    /// `consent.ok = false`, clears all partner IDs, and sets a 24-hour TTL.
    /// The tombstone allows batch sync clients (`POST /_ts/api/v1/sync`) to return
    /// `consent_withdrawn` rather than `ec_hash_not_found` for the tombstone TTL.
    ///
    /// After the 24-hour TTL expires, the entry is gone. Any subsequent `get()`
    /// returns `None` (`ec_hash_not_found`) — the distinction is time-bounded.
    ///
    /// Caller must handle `Err` by logging at `error` level; the cookie deletion
    /// in `ec_finalize_response()` is the primary enforcement mechanism.
    pub fn write_withdrawal_tombstone(
        &self,
        ec_hash: &str,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Hard-deletes the entry. Used only for data deletion requests (IAB deletion
    /// framework — deferred). For consent withdrawal, use `write_withdrawal_tombstone()`.
    pub fn delete(&self, ec_hash: &str) -> Result<(), Report<TrustedServerError>>;
}
```

`MAX_CAS_RETRIES = 3`. If all retries fail on a generation conflict, return `Err` — callers handle per-endpoint policy (§8.3 step 7 for pixel sync, §9.4 for batch sync).

### 7.5 KV degraded behavior

| Operation                          | KV unavailable | Action                                                                                         |
| ---------------------------------- | -------------- | ---------------------------------------------------------------------------------------------- |
| EC cookie creation                 | KV error       | Set cookie. Skip KV create. Log `warn`.                                                        |
| `/sync` KV write                   | KV error       | Redirect with `ts_synced=0&ts_reason=write_failed`.                                            |
| `/identify` KV read                | KV error       | Return `200` with `ec` set, `degraded: true`, empty `uids`/`eids`.                             |
| `POST /_ts/api/v1/sync`            | KV error       | Return `207` with all mappings rejected, `reason: "kv_unavailable"`.                           |
| Pull sync KV write                 | KV error       | Discard uid. Log `warn`. Retry on next qualifying request.                                     |
| Consent withdrawal tombstone write | KV error       | Delete cookie (primary enforcement). Log `error`. Next request: no cookie → no EC regenerated. |

---

## 8. Pixel Sync Endpoint (`GET /sync`)

### 8.1 Module: `ec/sync_pixel.rs`

```rust
pub async fn handle_sync(
    settings: &Settings,
    kv: &KvIdentityGraph,
    partner_store: &PartnerStore,
    req: &Request,
    ec_context: &mut EcContext,
) -> Result<Response, Report<TrustedServerError>>;
```

### 8.2 Query parameters

| Parameter | Required | Description                                                                  |
| --------- | -------- | ---------------------------------------------------------------------------- |
| `partner` | Yes      | Partner ID — must exist in `partner_store`                                   |
| `uid`     | Yes      | Partner's user ID for this user                                              |
| `return`  | Yes      | Redirect-back URL (must match partner's `allowed_return_domains`)            |
| `consent` | No       | Fallback TCF/GPP string if `ec_context.consent.is_empty()` after pre-routing |

### 8.3 Flow

```
1. Parse query params. Missing required params → 400.

2. Require a valid cookie-held EC.
   If `cookie_was_present == false` OR `ec_context.ec_hash().is_none()`
   (cookie missing or malformed) → redirect to
   {return}?ts_synced=0&ts_reason=no_ec

3. Look up partner record in partner_store.
   Not found → 400.

4. Validate return URL host against partner.allowed_return_domains.
   - Exact hostname match only — no suffix or wildcard.
   - Mismatch → 400.

5. Evaluate consent. Use `ec_context.consent` (built pre-routing via
   `build_consent_context()`). The optional `consent` query param is a **fallback
   only** — used solely when `ec_context.consent.is_empty()` returns `true`.
   This is the actual contract from the consent module. It is broader than
   “no cookies or headers on the wire”: if consent KV fallback, decoded objects,
   GPP section IDs, AC string, raw US privacy, or GPC already populated the
   context, `is_empty()` is `false` and the query param is ignored entirely.

   When the fallback applies: decode the query param into a **locally-built**
   `ConsentContext` (same TCF/GPP/USP decoders, same jurisdiction inputs), then
   assign that value into `ec_context.consent` for the remainder of this request.
   This makes the sync write decision and `ec_finalize_response()` use the same
   effective consent view, avoiding a same-request “write partner ID, then
   withdraw EC” conflict. Do NOT re-call `build_consent_context()` — that would
   trigger `try_kv_write()` and persist the query-param consent to the consent KV
   store, which is not intended. The decoded fallback applies only to this `/sync`
   request; it is not written to the consent KV store and does not change any
   future request unless the client sends real consent cookies/headers again.

   `!allows_ec_creation(...)` → redirect to {return}?ts_synced=0&ts_reason=no_consent

6. Check anti-stuffing rate limit (sync_rate_limit per EC hash per partner per hour).
   Exceeded → `429 Too Many Requests` (no redirect — the `return` URL is never called).

7. kv.upsert_partner_id(ec_hash, partner_id, uid, now())
   If the root KV entry is missing (e.g. initial `create_or_revive()` failed on
   the organic page load), `upsert_partner_id()` creates a minimal live entry and
   then writes `ids[partner_id]`. This is the recovery path for best-effort EC
   creation misses.
   KV write failure → redirect to {return}?ts_synced=0&ts_reason=write_failed

8. Success → redirect to {return}?ts_synced=1
```

`ts_synced` values:

| Value                                | Meaning                       |
| ------------------------------------ | ----------------------------- |
| `ts_synced=1`                        | KV write succeeded            |
| `ts_synced=0&ts_reason=no_ec`        | No valid EC cookie present    |
| `ts_synced=0&ts_reason=no_consent`   | Consent absent or denied      |
| `ts_synced=0&ts_reason=write_failed` | KV write failed after retries |

Rate limit exceeded returns `429 Too Many Requests` directly — the partner's `return` URL is not called in this case.

### 8.4 Return URL construction

Append `ts_synced` (and optional `ts_reason`) to the `return` URL:

- If the URL already has a query string, append `&ts_synced=...`
- If not, append `?ts_synced=...`

Do not modify any other query parameters on the `return` URL.

### 8.5 Security

- `return` URL validated by exact hostname match against `partner.allowed_return_domains`. No subdomain wildcard matching.
- No HMAC signature required on inbound sync request.
- Rate limit: `partner.sync_rate_limit` writes per EC hash per partner per hour. Default: 100. Configurable per partner in `partner_store`.

---

## 9. S2S Batch Sync API (`POST /_ts/api/v1/sync`)

### 9.1 Module: `ec/sync_batch.rs`

```rust
pub async fn handle_batch_sync(
    settings: &Settings,
    kv: &KvIdentityGraph,
    partner_store: &PartnerStore,
    req: Request,
) -> Result<Response, Report<TrustedServerError>>;
```

### 9.2 Authentication

`Authorization: Bearer <api_key>` header required. Auth flow:

1. Compute `sha256_hex(api_key)`.
2. Look up `partner_store.find_by_api_key_hash(hash)` — uses the `apikey:{hash}` secondary index (§13.1) for O(1) lookup instead of scanning all partners.
3. If the index returns a partner, verify the partner's stored `api_key_hash` matches the computed hash (constant-time comparison). This guards against stale index entries from key rotation.
4. If no match or verification fails → `401 Unauthorized` with no body processing.
5. If KV lookup fails (store unavailable) → `503 Service Unavailable`.

Key rotation does not require binary redeployment — partners update via `/_ts/admin/partners/register`, which handles old API-key index cleanup (§13.1).

### 9.2.1 API-key rate limiting

After successful auth, check the API-key level rate limit: `partner.batch_rate_limit` requests per partner per minute (default 60). Uses the same Fastly rate-limiting API as pixel sync (§14.3), with key `batch:{partner_id}`.

Exceeded → `429 Too Many Requests` with body `{ "error": "rate_limit_exceeded" }`. No mappings are processed.

### 9.3 Request format

```
POST /_ts/api/v1/sync
Content-Type: application/json
Authorization: Bearer <api_key>

{
  "mappings": [
    {
      "ec_hash": "<64-character hex hash>",
      "partner_uid": "abc123",
      "timestamp": 1741824000
    }
  ]
}
```

Maximum batch size: 1000 mappings. Requests exceeding this receive `400 Bad Request`.

### 9.4 Processing

The authenticated partner's ID (from the `PartnerRecord` resolved via API key in §9.2) determines the `ids[partner_id]` namespace for all writes in this batch. A partner can only write to their own namespace.

For each mapping:

1. Validate `ec_hash` format (must be exactly 64 lowercase hex characters). Invalid format → reject with `reason: "invalid_ec_hash"`.
2. Read KV metadata for `ec_hash`. If not found → reject with `reason: "ec_hash_not_found"`. If `consent.ok = false` → reject with `reason: "consent_withdrawn"`.
3. `kv.upsert_partner_id(ec_hash, partner_id, partner_uid, timestamp)`. The upsert internally skips the write if the existing `ids[partner_id].synced ≥ timestamp` (idempotent — counted as accepted, no error). On KV failure → reject all remaining mappings with `reason: "kv_unavailable"`, return `207`.

### 9.5 Response format

```json
{
  "accepted": 998,
  "rejected": 2,
  "errors": [
    { "index": 45, "reason": "ec_hash_not_found" },
    { "index": 72, "reason": "consent_withdrawn" }
  ]
}
```

HTTP status rules:

| Condition                              | Status                                                 |
| -------------------------------------- | ------------------------------------------------------ |
| All mappings accepted                  | `200 OK`                                               |
| Some accepted, some rejected           | `207 Multi-Status`                                     |
| All rejected (auth valid, batch valid) | `207 Multi-Status` with `accepted: 0`                  |
| Auth invalid                           | `401 Unauthorized`                                     |
| Auth KV lookup failed (store down)     | `503 Service Unavailable`                              |
| Malformed JSON or > 1000 mappings      | `400 Bad Request`                                      |
| KV entirely unavailable                | `207 Multi-Status`, all rejected with `kv_unavailable` |

```rust
pub struct BatchSyncResponse {
    pub accepted: usize,
    pub rejected: usize,
    pub errors: Vec<BatchSyncError>,
}

pub struct BatchSyncError {
    pub index: usize,
    pub reason: BatchSyncRejection,
}

#[derive(Debug, derive_more::Display)]
pub enum BatchSyncRejection {
    #[display("invalid_ec_hash")]
    InvalidEcHash,
    #[display("ec_hash_not_found")]
    EcHashNotFound,
    #[display("consent_withdrawn")]
    ConsentWithdrawn,
    #[display("kv_unavailable")]
    KvUnavailable,
}
```

---

## 10. S2S Pull Sync (TS-Initiated)

### 10.1 Module: `ec/pull_sync.rs`

Pull sync inverts the batch model: TS calls the partner's resolution endpoint server-to-server and writes the returned UID into the KV graph. No browser redirect is involved.

```rust
pub struct PullSyncDispatcher {
    concurrency_limit: usize,
}

impl PullSyncDispatcher {
    pub fn new(concurrency_limit: usize) -> Self;

    /// Dispatches pull sync calls for all qualifying partners.
    /// Called after `send_to_client()` — fires outbound requests using
    /// `Request::send_async()` which returns `PendingRequest` handles.
    /// Internally: fires up to `concurrency_limit` requests via `send_async()`,
    /// then calls `PendingRequest::wait()` (blocking) on each handle to collect
    /// responses and write results to KV. This is synchronous blocking code
    /// running after the client response is already flushed — no async runtime
    /// needed. The Fastly WASM invocation remains alive until this returns.
    pub fn dispatch_background(
        &self,
        ec_context: &EcContext,
        client_ip: IpAddr,
        partners: &[PartnerRecord],
        kv: &KvIdentityGraph,
    );
}

/// Fires a single partner pull request via `send_async()`, waits for the
/// response via `PendingRequest::wait()`, and writes the result to KV.
fn pull_one_partner(
    ec_hash: &str,
    ip: IpAddr,
    partner: &PartnerRecord,
    kv: &KvIdentityGraph,
);
```

### 10.2 Trigger conditions

A pull sync is dispatched for a partner when all of the following are true on a request:

1. The request was routed to an **organic handler** (`handle_publisher_request` or `integration_registry.handle_proxy`). Pull sync never fires on EC route handlers (`/sync`, `/identify`, `/_ts/api/v1/sync`, `/_ts/admin/*`) or `/auction`. This matches the PRD requirement that pull calls must not happen during the pixel sync flow.
2. A valid EC is present (`ec_context.ec_hash().is_some()`). This includes an EC
   newly generated on the current organic request — pull sync may run immediately
   after first-page EC creation because the response cookie is flushed before the
   background dispatch starts.
3. `allows_ec_creation(&ec_context.consent) == true`
4. `partner.pull_sync_enabled == true`
5. Either: no entry exists for this partner in the KV graph, or the existing `synced` timestamp is older than `partner.pull_sync_ttl_sec` (default 86400 seconds)
6. Rate limit not exceeded: `partner.pull_sync_rate_limit` calls per EC hash per partner per hour (default 10)

### 10.3 Execution model

Pull calls are dispatched using Fastly's background task / `send_async` model after the response is flushed. They do not add latency to the user-facing request.

Maximum concurrent pull calls per request: `settings.ec.pull_sync_concurrency` (default 3).

**Architectural divergence from PRD:** The PRD describes excess partner calls being queued and dispatched on subsequent requests for the same user. A persistent queue is not implementable in the stateless Fastly WASM edge environment — there is no cross-request mutable state. This spec adapts the intent using a stateless rotating offset: sort qualifying partners by ID, then use `(unix_timestamp_secs / 3600) % partner_count` as the starting index (wrapping). This ensures different partners are prioritized across different requests without persisted state. Partners not called on a given request remain eligible on the next qualifying request per their `pull_sync_ttl_sec` condition. The practical outcome (all partners eventually called) matches the PRD intent; the mechanism differs due to the platform constraint.

### 10.4 Outbound request

```
GET {partner.pull_sync_url}?ec_hash={64-char-hex}&ip={ip_address}
Authorization: Bearer {partner.ts_pull_token}
```

Before dispatching, `pull_sync.rs` validates that `pull_sync_url`'s hostname is present in `partner.pull_sync_allowed_domains`. If not, the call is skipped and an `error` is logged — this is a configuration error that should not occur at runtime if admin validation is working correctly (§13.2 step 3).

Only the EC hash and IP are sent. No consent strings, geo data, or other partner IDs are included.

**Expected partner responses:**

```json
{ "uid": "abc123" }   // resolved
{ "uid": null }       // not recognized
```

Or `404 Not Found`. Both null and 404 are no-ops — no KV write, no error logged above `debug`.

Any other non-200 response is treated as a transient failure. No retry. The next qualifying request triggers a new attempt.

### 10.5 KV write on success

On a non-null `uid`: call `kv.upsert_partner_id(ec_hash, partner_id, uid, now())`. If the root entry is missing, the upsert creates a minimal live entry first (same recovery path as `/sync`). On KV failure: log `warn` and discard the result. Retry occurs on the next qualifying request.

The write updates `ids[partner_id].synced` to the current timestamp, resetting the `pull_sync_ttl_sec` window.

---

## 11. Identity Resolution Endpoint (`GET /identify`)

### 11.1 Module: `ec/identify.rs`

```rust
pub async fn handle_identify(
    settings: &Settings,
    kv: &KvIdentityGraph,
    partner_store: &PartnerStore,
    req: &Request,
    ec_context: &EcContext,
) -> Result<Response, Report<TrustedServerError>>;
```

### 11.2 Call patterns

**Browser-direct:** The browser sends the request to `ec.publisher.com/identify`. Cookies and consent cookies are sent automatically (same-site). No special header forwarding required.

**Server-side proxy (for use case 2):** The publisher's origin server must forward:

| Header                                                    | Required                               |
| --------------------------------------------------------- | -------------------------------------- |
| `Cookie: ts-ec=<value>` or `X-ts-ec: <value>`             | Yes                                    |
| `Cookie: euconsent-v2=<value>` or `Cookie: __gpp=<value>` | Yes for EU/UK/US users                 |
| `X-consent-advertising: <value>`                          | Optional — takes precedence if present |

### 11.3 EC and consent handling

`/identify` follows `EcContext` retrieval priority (Section 4.2). It does **not**
generate a new EC, and the handler itself does not write cookies. However,
`ec_finalize_response()` still runs after the handler: on consent withdrawal it
deletes the EC cookie, and on header/cookie mismatch it may reconcile the cookie
to the authoritative header-derived EC.

Consent is evaluated using the same logic as Section 6.

### 11.4 Response

**`200 OK` — EC present, consent granted:**

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": false,
  "uids": {
    "uid2": "A4A...",
    "liveramp": "LR_xyz"
  },
  "eids": [
    { "source": "uidapi.com", "uids": [{ "id": "A4A...", "atype": 3 }] },
    { "source": "liveramp.com", "uids": [{ "id": "LR_xyz", "atype": 3 }] }
  ]
}
```

`uids` contains one key per partner with `bidstream_enabled: true` and a resolved UID in the KV graph. Partners with no resolved UID for this user are omitted.

**`200 OK` — KV unavailable (degraded):**

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": true,
  "uids": {},
  "eids": []
}
```

**`200 OK` — EC present, KV entry missing (no synced partners yet):**

This case occurs by design when `create_or_revive()` fails on EC generation (best-effort) or when the EC was just created and no partners have synced yet. It is not an error — the EC is valid, just has no partner data.

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": false,
  "uids": {},
  "eids": []
}
```

Note: `degraded` is `false` because the KV read succeeded (it returned `None`, meaning no entry exists). `degraded: true` is reserved for KV read errors where the entry might exist but couldn't be retrieved.

**`403 Forbidden` — consent denied (regardless of EC presence):**

```json
{ "consent": "denied" }
```

Consent is evaluated **before** EC presence. If `!allows_ec_creation(&consent)`, return `403` immediately — do not fall through to the `204` branch. This ensures consent denial is always surfaced, even for users with no EC.

**`204 No Content` — no EC present, consent not denied.** No body.

### 11.5 Response headers (supplementary)

Set on `200` responses only:

| Header              | Value                                                                                                                                                                                                                                                                                                                           |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `X-ts-ec`           | `{ec_hash.suffix}`                                                                                                                                                                                                                                                                                                              |
| `X-ts-eids`         | Standard base64 (RFC 4648, with `=` padding) of the JSON array of OpenRTB 2.6 `user.eids` objects. Capped at **4 KB** after encoding. If the encoded value exceeds 4 KB, the array is truncated (fewest partners first — highest `synced` timestamp retained) until it fits, and a `x-ts-eids-truncated: true` header is added. |
| `X-ts-<partner_id>` | Resolved UID per partner (e.g., `X-ts-uid2`). One header per partner with a resolved UID. **Capped at 20 partners** — partners sorted by most-recently synced; excess partners are omitted silently.                                                                                                                            |
| `X-ts-ec-consent`   | `ok` (always — denied consent returns `403`, not `200`)                                                                                                                                                                                                                                                                         |

These are supplementary — callers should read the JSON body as the primary contract. The 4 KB cap on `X-ts-eids` and the 20-partner cap on `X-ts-<partner_id>` headers reflect typical proxy and browser total-header-budget constraints. Both caps apply independently.

### 11.6 Performance target

`/identify` must respond within 30ms (excluding network latency) when EC is present and KV read succeeds. This requires the KV read to be on the fast path with no retries.

CORS headers must be set to allow browser-direct calls from the publisher's page. The `Access-Control-Allow-Origin` header is dynamically reflected from the `Origin` request header if the origin is an exact match or a subdomain of `settings.publisher.domain`:

```
// e.g. publisher.domain = "example.com"
// Allowed: https://example.com, https://www.example.com, https://news.example.com
// Rejected: https://evil.com, https://notexample.com

Access-Control-Allow-Origin: <reflected Origin>
Access-Control-Allow-Credentials: true
Access-Control-Allow-Methods: GET, OPTIONS
Access-Control-Allow-Headers: Cookie, X-ts-ec, X-consent-advertising
Access-Control-Expose-Headers: X-ts-ec, X-ts-eids, X-ts-ec-consent, X-ts-eids-truncated, <X-ts-{partner_id} for each partner with a resolved UID in the response>
Vary: Origin
```

**`Access-Control-Expose-Headers` note:** The dynamic `X-ts-<partner_id>` headers must be enumerated per-response, not as a static constant. The handler builds the expose list by iterating the partner IDs that have resolved UIDs in the response. `x-ts-eids-truncated` is always included in the expose list (browser JS should be able to detect truncation even when it occurs).

**Origin validation logic:** CORS headers are only relevant when the `Origin` request header is present (browser requests always send it; server-side proxy calls typically do not).

- **No `Origin` header present:** Process normally. No CORS headers added. No `403`. This is the server-side proxy path from §11.2 — origin-server calls forwarding `Cookie` and consent headers.
- **`Origin` header present, hostname matches `publisher.domain` or ends with `.{publisher.domain}` and scheme is `https`:** Reflect origin in `Access-Control-Allow-Origin`. Add `Vary: Origin`.
- **`Origin` header present but does not match:** Return `403`. No body.

Browser `fetch()` with `credentials: "include"` sends an `OPTIONS` preflight. The router handles `OPTIONS /identify` identically — returns `200 OK` with the CORS headers above and no body.

---

## 12. Bidstream Decoration (`/auction` Mode B)

### 12.1 Changes to existing auction path

The auction handler (`crates/common/src/auction/`) is modified to inject EC identity into outbound OpenRTB requests. This is **not** a builder tweak — it requires explicit schema additions across multiple files. SyntheticID is fully removed from the auction path — no fallback, no `X-Synthetic-*` headers, no `get_or_generate_synthetic_id()`.

| Concern                               | Behavior                                                                                                    |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `UserInfo.id`                         | Replace with `ec_value` when EC is present. Remove `synthetic_id` field. When no EC → `user.id` is omitted. |
| Outbound OpenRTB `user.id`            | Set to `ec_value` when EC present. Omit when no EC (no fallback).                                           |
| `X-Synthetic-*` response headers      | **Removed.** Replaced by `X-ts-ec`.                                                                         |
| `X-ts-ec` response header             | Set when EC is present.                                                                                     |
| Publisher and integration proxy paths | Only `ec_context.generate_if_needed()` runs. `get_or_generate_synthetic_id()` is removed.                   |
| `convert_tsjs_to_auction_request()`   | Takes `ec_context: &EcContext` (not Optional). SyntheticID parameter removed.                               |

**Schema changes required before handler changes:**

| File           | Change                                                                                                                                                                                                                                |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `types.rs`     | Replace `id: String` in `UserInfo` with `ec_value: Option<String>`. Add `Eid` and `EidUid` OpenRTB 2.6 types. Remove synthetic fields.                                                                                                |
| `openrtb.rs`   | Add `eids: Vec<Eid>` and `consent: Option<String>` to `User` struct. Remove `ext.synthetic_fresh`.                                                                                                                                    |
| `prebid.rs`    | Populate `user.id` from EC value. Add `user.eids`, `user.consent`. Remove synthetic fallback.                                                                                                                                         |
| `formats.rs`   | Accept `ec_context: &EcContext` (not Optional). Remove `synthetic_id` parameter.                                                                                                                                                      |
| `endpoints.rs` | Remove `get_or_generate_synthetic_id()` call. Remove `X-Synthetic-*` headers. Use `ec_context.consent` instead of internal `build_consent_context()`. Pass `ec_context` to `convert_tsjs_to_auction_request()`. Add `X-ts-ec` header. |

These changes affect the OpenRTB wire format — confirm with engineering that no existing SSP integrations break before merging.

### 12.2 `user` object injection

When an `EcContext` is available on the request, the auction handler performs an explicit KV read before building the OpenRTB request:

```rust
// In handle_auction():
let (user_id, eids) = match ec_context.ec_hash() {
    Some(hash) => {
        let kv_entry = kv.get(hash).ok().flatten();
        let eids = match kv_entry {
            Some((entry, _gen)) => build_eids_from_kv(&entry, partner_store),
            None => vec![],  // KV read failed or no entry — degrade gracefully
        };
        (ec_context.ec_value.clone(), eids)
    }
    None => (None, vec![]),  // No EC — user.id omitted, no EIDs. Auction still runs.
};

user.id = user_id;
user.consent = consent_string;  // TCF string from ec_context.consent, else None
user.eids = eids;
```

`build_eids_from_kv` iterates `kv_entry.ids` and includes only partners with `bidstream_enabled: true` and a non-empty `uid`. Partners without a resolved UID are omitted.

### 12.3 OpenRTB `user.eids` structure

```json
{
  "user": {
    "id": "a1b2c3...AbC123",
    "consent": "CP...",
    "eids": [
      {
        "source": "liveramp.com",
        "uids": [{ "id": "LR_xyz", "atype": 3 }]
      },
      {
        "source": "uidapi.com",
        "uids": [{ "id": "A4A...", "atype": 3 }]
      }
    ]
  }
}
```

`atype: 3` for all EC-derived IDs (partner-defined), per OpenRTB 2.6 spec.

### 12.4 SSP-specific adapter `ext.eids`

When calling a specific PBS adapter, include only that SSP's resolved ID in the adapter-level `ext.eids`. The full `user.eids` array contains all configured identity providers.

### 12.5 `/auction` response headers (in-scope)

The current `/auction` path returns a JSON response inline to the JS caller (`endpoints.rs:71`). There is no server-to-server delivery step to a publisher ad server. EC headers are added to this existing response:

| Header                | Value                                                                                                              |
| --------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `X-ts-ec`             | `{ec_hash.suffix}` — when EC is present                                                                            |
| `X-ts-eids`           | Standard base64 (RFC 4648) of OpenRTB 2.6 `user.eids` JSON array. Capped at 4 KB — same truncation rules as §11.5. |
| `X-ts-eids-truncated` | `true` — present only when `X-ts-eids` was truncated                                                               |
| `X-ts-ec-consent`     | `ok` — only present when consent granted; on withdrawal `ec_finalize_response()` strips all EC headers             |

**Deferred:** A future server-to-server winner-notification delivery step to a publisher ad server is not in scope for this iteration. See §1 deferred items.

---

## 13. Partner Registry and Admin Endpoint

### 13.1 Module: `ec/partner.rs`

```rust
pub struct PartnerRecord {
    /// Partner identifier. Must match `^[a-z0-9_-]{1,32}$` (lowercase, no spaces).
    /// Used to build `X-ts-<id>` response headers — header-safety is required.
    /// Reserved names that would collide with existing managed headers are rejected
    /// at registration: `ec`, `eids`, `ec-consent`, `eids-truncated`, `synthetic`, `ts`, `version`, `env`.
    pub id: String,
    pub name: String,
    pub allowed_return_domains: Vec<String>,
    pub api_key_hash: String,               // SHA-256 hex of the partner's API key
    pub bidstream_enabled: bool,
    pub source_domain: String,              // OpenRTB source (e.g., "liveramp.com")
    pub openrtb_atype: u8,                  // typically 3
    pub sync_rate_limit: u32,               // per EC hash per partner per hour
    pub batch_rate_limit: u32,              // API-key level: requests per partner per minute (default 60)
    pub pull_sync_enabled: bool,
    pub pull_sync_url: Option<String>,      // required when pull_sync_enabled; validated at registration
    pub pull_sync_allowed_domains: Vec<String>, // allowlist of domains TS may call for this partner
    pub pull_sync_ttl_sec: u64,             // default 86400
    pub pull_sync_rate_limit: u32,          // default 10
    pub ts_pull_token: Option<String>,      // required when pull_sync_enabled; outbound bearer token
}

pub struct PartnerStore {
    store_name: String,
}

impl PartnerStore {
    pub fn new(store_name: impl Into<String>) -> Self;

    /// Looks up a partner by ID. Returns `None` if not found.
    pub fn get(&self, partner_id: &str) -> Result<Option<PartnerRecord>, Report<TrustedServerError>>;

    /// Verifies an API key against the stored hash for a given partner.
    /// Uses constant-time comparison.
    pub fn verify_api_key(&self, partner_id: &str, api_key: &str) -> bool;

    /// Writes or updates a partner record.
    /// Returns `true` if this was a new partner (create), `false` if an existing
    /// partner was updated. The pre-read needed for index maintenance (old API key
    /// deletion) also determines this.
    pub fn upsert(&self, record: &PartnerRecord) -> Result<bool, Report<TrustedServerError>>;

    /// Looks up the partner owning a given API key hash (for batch sync auth).
    /// Uses the `apikey:{hash}` secondary index for O(1) lookup, then verifies the
    /// stored `api_key_hash` matches (guards against stale index from key rotation).
    pub fn find_by_api_key_hash(&self, hash: &str) -> Result<Option<PartnerRecord>, Report<TrustedServerError>>;

    /// Returns all partner records with `pull_sync_enabled == true`.
    /// Used by the pull sync dispatcher after each organic request. Implementations
    /// must re-check `pull_sync_enabled` on the fetched record before returning it,
    /// because the `_pull_enabled` secondary index is best-effort and may be stale.
    pub fn pull_enabled_partners(&self) -> Result<Vec<PartnerRecord>, Report<TrustedServerError>>;
}
```

**Storage layout:** Partner records are stored as JSON values in `partner_store` KV, keyed by `partner_id`. Two operations require access patterns beyond single-key lookup:

1. **`find_by_api_key_hash(hash)`** — batch sync auth needs to find the partner owning a given API key hash. Implementation: maintain a secondary index entry `apikey:{sha256_hex} → partner_id` in the same KV store. Written on `upsert()`, looked up on batch auth. **On key rotation:** `upsert()` must read the existing record first, and if the `api_key_hash` has changed, delete the old `apikey:{old_hash}` index entry before writing the new one. This prevents old API keys from remaining valid after rotation.

2. **`pull_enabled_partners()`** — pull sync needs all partners with `pull_sync_enabled == true`. Implementation: maintain an index entry `_pull_enabled → [partner_id_1, partner_id_2, ...]` (JSON array of partner IDs) in the same KV store. Updated on `upsert()` when `pull_sync_enabled` changes. The dispatcher reads this list, then does individual `get()` calls for each partner record. This bounds the number of KV reads to `1 + pull_partner_count` per organic request.

**Consistency model:** These index writes are **best-effort, not atomic** — Fastly KV does not support multi-key transactions. `upsert()` writes in order: (1) primary record, (2) old API-key index deletion (if key changed), (3) new API-key index, (4) `_pull_enabled` list. If the process fails mid-sequence, indexes may be stale. All readers handle this defensively:

- `find_by_api_key_hash()`: if the index points to a partner whose stored `api_key_hash` does not match the lookup hash, treat as auth failure (stale index from a rotation).
- `pull_enabled_partners()`: if a listed partner ID returns `None` from `get()`, skip it silently. If the fetched record has `pull_sync_enabled == false`, also skip it silently — that is a stale `_pull_enabled` index entry.
- The `_pull_enabled` list is vulnerable to lost updates under concurrent registrations. This is accepted — partner registration is a low-frequency admin operation (not a hot path). If lost updates become an issue, a CAS-based read-modify-write can be added later.

### 13.2 Admin endpoint (`POST /_ts/admin/partners/register`)

**Module:** `ec/admin.rs`

> **Codebase invariant — requires test update:** `Settings::ADMIN_ENDPOINTS` in `settings.rs` lists routes that must be covered by a `[[handlers]]` Basic Auth entry. The existing test at `settings.rs:1504-1530` scans `main.rs` for **every** `/_ts/admin/` route string and asserts it appears in `ADMIN_ENDPOINTS`. When `/_ts/admin/partners/register` is added to `main.rs`, this test will fail.
>
> **Required changes:**
>
> 1. Do **NOT** add `/_ts/admin/partners/register` to `ADMIN_ENDPOINTS` — it uses bearer-token-in-handler auth.
> 2. Update the admin-route-scan test (`settings.rs:1504-1530`) to maintain an exclusion list of bearer-token-authed admin routes (e.g., `const BEARER_AUTH_ADMIN_ROUTES: &[&str] = &["/_ts/admin/partners/register"]`) and skip those when asserting `ADMIN_ENDPOINTS` coverage.
> 3. Narrow the `[[handlers]]` pattern in `trusted-server.toml` from `"^/_ts/admin"` to `"^/_ts/admin/keys"` so that `/_ts/admin/partners/register` is not intercepted by `enforce_basic_auth()` before reaching its bearer-token handler.

```rust
pub async fn handle_register_partner(
    settings: &Settings,
    partner_store: &PartnerStore,
    req: Request,
) -> Result<Response, Report<TrustedServerError>>;
```

Authentication: `Authorization: Bearer <token>` header, validated inside the handler against `settings.ec.admin_token_hash` (SHA-256 constant-time comparison). This is a publisher-level admin credential — separate from partner API keys, and enforced in-handler (not via `[[handlers]]` Basic Auth). Returns `401 Unauthorized` with no body if the token is missing or invalid.

**Request:**

```
POST /_ts/admin/partners/register
Authorization: Bearer <admin_token>
Content-Type: application/json

{
  "id": "ssp_x",
  "name": "SSP Example",
  "allowed_return_domains": ["sync.example-ssp.com"],
  "api_key": "raw_key_to_hash_and_store",
  "bidstream_enabled": true,
  "source_domain": "example-ssp.com",
  "openrtb_atype": 3,
  "sync_rate_limit": 100,
  "batch_rate_limit": 60,
  "pull_sync_enabled": false,
  "pull_sync_url": null,
  "pull_sync_allowed_domains": [],
  "pull_sync_ttl_sec": 86400,
  "pull_sync_rate_limit": 10,
  "ts_pull_token": null
}
```

**Processing:**

1. Validate `Authorization: Bearer <token>`: SHA-256 hash the token and compare against `settings.ec.admin_token_hash` using constant-time comparison. `401` if missing or invalid.
2. Validate required fields (`id`, `name`, `allowed_return_domains`, `api_key`, `source_domain`). `400` on failure.
   Validate `id` format: must match `^[a-z0-9_-]{1,32}$`. Must not be a reserved name
   (`ec`, `eids`, `ec-consent`, `eids-truncated`, `synthetic`, `ts`, `version`, `env`). `400` with descriptive message on failure.
3. If `pull_sync_enabled == true`, validate that both `pull_sync_url` and `ts_pull_token` are present and non-empty. `400` with `"pull_sync_url and ts_pull_token are required when pull_sync_enabled is true"` if either is missing.
   If `pull_sync_url` is set, validate that its hostname is present in `pull_sync_allowed_domains`. `400` on failure with `"pull_sync_url domain must be in pull_sync_allowed_domains"`. This prevents TS from being directed to call arbitrary URLs — the allowlist must be declared in the same registration payload.
4. Hash `api_key` with SHA-256 before writing — never store plaintext.
5. `let created = partner_store.upsert(record)?`. `503` on KV failure.
   `upsert()` returns `true` for a new partner, `false` for an update.
6. Return `201 Created` if new partner (`created == true`), or `200 OK` if update
   (`created == false`). Use an explicit response DTO — do NOT serialize the full
   `PartnerRecord` (which contains `api_key_hash` and `ts_pull_token`).

**Response:**

```json
{
  "id": "ssp_x",
  "name": "SSP Example",
  "pull_sync_enabled": false,
  "bidstream_enabled": true,
  "created": true
}
```

The response confirms the registration succeeded and echoes key fields. `api_key_hash`, `ts_pull_token`, and `api_key` are never returned. `PartnerRecord` does not have a `registered_at` field — use the `created` boolean to signal first registration vs. upsert update.

---

## 14. Configuration

### 14.1 New `EdgeCookie` settings struct

Added to `crates/common/src/settings.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct EdgeCookie {
    /// Publisher passphrase used as HMAC key for EC generation.
    /// Must be identical across all of the publisher's owned domains.
    /// Publishers sharing this value with partners form an identity-federated consortium.
    #[validate(custom(function = EdgeCookie::validate_passphrase))]
    pub passphrase: String,

    /// Fastly KV store name for the EC identity graph.
    #[validate(length(min = 1))]
    pub ec_store: String,

    /// Fastly KV store name for the partner registry.
    #[validate(length(min = 1))]
    pub partner_store: String,

    /// SHA-256 hex of the publisher admin token for `POST /_ts/admin/partners/register`.
    /// The plaintext token is provided in the `Authorization: Bearer` header;
    /// it is never stored in plaintext.
    #[validate(custom(function = EdgeCookie::validate_sha256_hex))]
    pub admin_token_hash: String,

    /// Maximum concurrent pull sync calls dispatched per request.
    #[validate(range(min = 1))]
    #[serde(default = "EdgeCookie::default_pull_sync_concurrency")]
    pub pull_sync_concurrency: usize,
}

impl EdgeCookie {
    fn validate_passphrase(passphrase: &str) -> Result<(), ValidationError>;
    // Rejects "passphrase" or empty string as placeholder.

    fn validate_sha256_hex(value: &str) -> Result<(), ValidationError>;
    // Requires exactly 64 lowercase hex characters.

    fn default_pull_sync_concurrency() -> usize { 3 }
}
```

Added to `Settings`:

```rust
pub struct Settings {
    // ... existing fields ...
    #[validate(nested)]
    pub ec: EdgeCookie,  // Required — omitting [ec] is a startup error
}
```

`EdgeCookie` does not derive `Default` — omitting the `[ec]` section from TOML is a deserialization error at startup. This is intentional: `passphrase`, `ec_store`, `partner_store`, and `admin_token_hash` have no safe defaults. The `#[validate(nested)]` attribute ensures `EdgeCookie::validate_passphrase()` runs when `settings.validate()` is called at startup (`settings_data.rs:28`), matching the pattern used by `Publisher` and `Rewrite` in the existing `Settings` struct (`Synthetic` is removed in PR #479).

### 14.2 TOML configuration example

```toml
[ec]
passphrase = "publisher-chosen-secret"
ec_store = "ec_identity_store"
partner_store = "ec_partner_store"
admin_token_hash = "sha256-hex-of-publisher-admin-token"
pull_sync_concurrency = 3
```

### 14.3 Rate Limit Storage

Pixel sync and pull sync rate limits (per EC hash per partner per hour) cannot use in-memory state in a WASM/Fastly Compute environment — there is no shared memory across requests.

**Implementation:** Use Fastly's Edge Rate Limiting API (`fastly::erl::RateCounter`), which provides distributed per-key counting without KV latency and is designed for high-frequency counting without per-key write limits.

| Counter    | Key format                    | Window   |
| ---------- | ----------------------------- | -------- |
| Pixel sync | `{partner_id}:{ec_hash}`      | 1 hour   |
| Pull sync  | `pull:{partner_id}:{ec_hash}` | 1 hour   |
| Batch sync | `batch:{partner_id}`          | 1 minute |

Engineering must confirm `fastly::erl::RateCounter` availability in the target before implementation of Steps 7, 9, and 10 is considered complete. Do NOT silently skip rate limiting in production if ERL is unavailable. Do NOT fall back to KV-based counters — they would hit the same 1 write/sec/key limit that necessitates `update_last_seen()` debouncing, and would thrash under real sync traffic. If ERL is unavailable, the rate-limited routes are blocked on an approved alternative counting mechanism.

### 14.4 Deprecation note

`settings.synthetic` is removed in PR #479. The `[synthetic]` TOML section, `counter_store`, `opid_store`, and `secret_key` fields are no longer present.

---

## 15. Constants and Header Names

New constants in `crates/common/src/constants.rs`:

```rust
// EC cookie name
pub const COOKIE_EC: &str = "ts-ec";

// EC response header
pub const HEADER_X_TS_EC: &str = "x-ts-ec";

// Supplementary identity headers
pub const HEADER_X_TS_EIDS: &str = "x-ts-eids";
pub const HEADER_X_TS_EC_CONSENT: &str = "x-ts-ec-consent";
pub const HEADER_X_TS_EIDS_TRUNCATED: &str = "x-ts-eids-truncated";

// Consent cookies (must match existing constants in constants.rs)
pub const COOKIE_TCF: &str = "euconsent-v2";
pub const COOKIE_GPP: &str = "__gpp";
pub const COOKIE_GPP_SID: &str = "__gpp_sid";
pub const COOKIE_US_PRIVACY: &str = "us_privacy";

// No EC-specific geo/IP header constants — use req.get_client_ip_addr() and GeoInfo::from_request(req).
```

The following EC headers must be added to `INTERNAL_HEADERS` in `constants.rs` to ensure they are stripped before proxying to downstream backends:

- `HEADER_X_TS_EC` (`x-ts-ec`)
- `HEADER_X_TS_EIDS` (`x-ts-eids`)
- `HEADER_X_TS_EC_CONSENT` (`x-ts-ec-consent`)
- `HEADER_X_TS_EIDS_TRUNCATED` (`x-ts-eids-truncated`)
- Dynamic `X-ts-<partner_id>` headers — these cannot be registered statically because partners are added at runtime via `/_ts/admin/partners/register`. The `INTERNAL_HEADERS` filter **must use prefix stripping** (`x-ts-` prefix match) rather than enumerating partner IDs. A startup snapshot would miss partners registered after deployment. The current filter in `http_util.rs` uses explicit header names — extend it to also strip any header matching the `x-ts-` prefix pattern.

---

## 16. Error Handling

New error variants in `crates/common/src/error.rs`:

```rust
pub enum TrustedServerError {
    // ... existing variants ...

    /// Edge Cookie operation failed — used only for EC-specific route handler
    /// errors (e.g., KV read failure in /identify). EC generation failure on
    /// organic routes does NOT produce this error — it is best-effort (log warn,
    /// continue without EC). Missing client IP is logged but never surfaced as 500.
    #[display("Edge Cookie error: {message}")]
    EdgeCookie { message: String },
    // Maps to StatusCode::INTERNAL_SERVER_ERROR (500)
    // Used for: EC-specific handler errors only (not organic-path generation)

    /// Partner not found in partner_store.
    #[display("Partner not found: {partner_id}")]
    PartnerNotFound { partner_id: String },
    // Maps to StatusCode::BAD_REQUEST (400)

    /// Partner API key authentication failed.
    #[display("Invalid API key for partner: {partner_id}")]
    PartnerAuthFailed { partner_id: String },
    // Maps to StatusCode::UNAUTHORIZED (401)
}
```

---

## 17. Request Routing

New routes added to `route_request()` in `crates/fastly/src/main.rs`:

```rust
// EC sync pixel — no auth required (partner validation is internal)
(GET, "/sync") → handle_sync(settings, &kv, &partner_store, &req, &mut ec_context)

// EC identity resolution — no auth required (consent-gated)
(GET, "/identify") → handle_identify(settings, &kv, &partner_store, &req, &ec_context)

// CORS preflight for /identify — must be registered explicitly, current router dispatches by exact method/path
(OPTIONS, "/identify") → cors_preflight_identify(settings, &req)

// S2S batch sync — partner API key auth (internal to handler)
(POST, "/_ts/api/v1/sync") → handle_batch_sync(settings, &kv, &partner_store, req)

// Partner registration — publisher admin auth enforced in-handler (Bearer token)
(POST, "/_ts/admin/partners/register") → handle_register_partner(settings, &partner_store, req)
```

Route ordering: EC routes are inserted before the fallback `handle_publisher_request()`. The `/_ts/admin/partners/register` route uses bearer-token auth in-handler (not `[[handlers]]` Basic Auth). The current `trusted-server.toml` has `path = "^/_ts/admin"` which catches **all** `/_ts/admin/*` paths via `enforce_basic_auth()` before routing — this would block bearer-token requests to `/_ts/admin/partners/register`. **Required change:** narrow the existing `[[handlers]]` pattern from `"^/_ts/admin"` to `"^/_ts/admin/keys"` so it covers only `/_ts/admin/keys/rotate` and `/_ts/admin/keys/deactivate` (the routes in `Settings::ADMIN_ENDPOINTS`). `/_ts/admin/partners/register` then passes through `enforce_basic_auth()` unchallenged and reaches the bearer-token handler.

### 17.1 EC integration in `main.rs`

EC follows the same pre-routing pattern as `GeoInfo::from_request()` (line 70). The pull sync background step requires a **structural refactor of the Fastly entrypoint**:

1. `route_request()` return type changes from `Result<Response, Error>` to `Result<(), Error>`.
2. The response is flushed mid-function via `response.send_to_client()` instead of being returned to `main()`.
3. The `#[fastly::main]` function (`main.rs:32`) currently returns `Result<Response, Error>` — it must change to call `route_request()` and return `Ok(())` (or map the error). The current `fn main(req: Request) -> Result<Response, Error>` signature is incompatible with the `send_to_client()` pattern.
4. After `send_to_client()`, the WASM invocation continues for background pull sync work.

This is a supported Fastly Compute pattern — `Response::send_to_client()` flushes the response to the client immediately and allows the WASM invocation to continue. This is not a small wiring change; it restructures how the application returns responses.

```rust
async fn route_request(...) -> Result<(), Error> {
    let geo_info = GeoInfo::from_request(&req);

    // Pre-routing — read only, no generation (matches GeoInfo pattern).
    // EcContext stores client_ip internally (same req.get_client_ip_addr()
    // already called by GeoInfo::from_request() above).
    let ec_context_result = EcContext::read_from_request(&req, settings, geo_info.as_ref());
    let mut ec_context = match ec_context_result {
        Ok(ctx) => ctx,
        Err(e) => {
            // Pre-routing failure — no route matched yet, but we still need to
            // send an HTTP error response. Construct one and flush immediately.
            log::error!("EcContext initialization failed: {e:?}");
            let mut response = to_error_response(&e);
            response.send_to_client();
            return Ok(());
        }
    };
    let kv = KvIdentityGraph::new(&settings.ec.ec_store);
    let partner_store = PartnerStore::new(&settings.ec.partner_store);
    let pull_sync_dispatcher = PullSyncDispatcher::new(settings.ec.pull_sync_concurrency);

    if let Some(mut response) = enforce_basic_auth(settings, &req) {
        ec_finalize_response(settings, geo_info.as_ref(), &ec_context, &kv, &mut response);
        response.send_to_client();
        return Ok(());
    }

    let path = req.get_path().to_string();
    let method = req.get_method().clone();

    // Route dispatch — req is moved (consumed) inside the matching arm.
    // is_organic tracks whether pull sync should fire (organic routes only — §10.2).
    let mut is_organic = false;
    let result = match (method, path.as_str()) {
        // EC-specific routes — all read-only except /sync which takes &mut.
        // /sync may assign fallback consent into ec_context.consent when the
        // query param is the only signal — see §8.3.
        (GET, "/sync")              => handle_sync(settings, &kv, &partner_store, &req, &mut ec_context).await,
        (GET, "/identify")          => handle_identify(settings, &kv, &partner_store, &req, &ec_context).await,
        (OPTIONS, "/identify")      => cors_preflight_identify(settings, &req),
        (POST, "/_ts/api/v1/sync")      => handle_batch_sync(settings, &kv, &partner_store, req).await,
        (POST, "/_ts/admin/partners/register") => handle_register_partner(settings, &partner_store, req).await,

        // /auction — EC-read-only; never generates EC.
        // NOTE: handle_auction signature changes from (settings, orchestrator, req) to
        // (settings, orchestrator, &kv, req, &ec_context) — this is a call-graph change,
        // not just wiring. See §12 for the full auction integration.
        (POST, "/auction")          => handle_auction(settings, orchestrator, &kv, req, &ec_context).await,

        // Organic routes — generate EC if needed (best-effort, never 500s), then dispatch
        (m, path) if integration_registry.has_route(&m, path) => {
            is_organic = true;
            ec_context.generate_if_needed(settings, &kv);
            integration_registry.handle_proxy(&m, path, settings, req, &ec_context).await
        },
        _ => {
            is_organic = true;
            ec_context.generate_if_needed(settings, &kv);
            handle_publisher_request(settings, integration_registry, req, &ec_context)
        },
    };

    // Unwrap result — errors become error responses (matches existing pattern)
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    // finalize_response runs on every route — enforces cookie write/deletion/last_seen
    ec_finalize_response(settings, geo_info.as_ref(), &ec_context, &kv, &mut response);

    // Flush response to client; WASM continues for background pull sync.
    response.send_to_client();

    // Background pull sync — organic routes only (§10.2). Never fires on /sync,
    // /identify, /auction, /_ts/api/v1/sync, or /_ts/admin/* routes.
    // Fires outbound HTTP calls via send_async(), blocks on PendingRequest::wait().
    if is_organic {
        if let (Some(ip), Ok(pull_partners)) = (ec_context.client_ip, partner_store.pull_enabled_partners()) {
            pull_sync_dispatcher.dispatch_background(&ec_context, ip, &pull_partners, &kv);
        }
    }

    Ok(())
}
```

The existing `finalize_response()` in `main.rs` becomes `ec_finalize_response()` with the extended signature that accepts `ec_context` and `kv`. The `#[fastly::main]` entrypoint changes to call `route_request()` and return `Ok(())` (the response is already sent via `send_to_client()`).

`PullSyncDispatcher::dispatch_background` uses `Request::send_async()` to fire outbound HTTP calls, then calls `PendingRequest::wait()` (blocking) on each handle under `settings.ec.pull_sync_concurrency` concurrency. No async runtime is needed — this is synchronous blocking code running after `send_to_client()` has flushed the response. The Fastly WASM invocation stays alive until `dispatch_background` returns. This does not add latency to the user-facing response.

---

## 18. Testing Strategy

Follow the project's **Arrange-Act-Assert** pattern. Test both happy paths and error conditions. Use `expect()` with `"should ..."` messages.

### 18.1 Unit tests

Each module in `ec/` has a `#[cfg(test)]` module covering:

| Module          | Key test cases                                                                                            |
| --------------- | --------------------------------------------------------------------------------------------------------- |
| `identity.rs`   | IPv4/IPv6 normalization, /64 truncation, HMAC determinism, output format                                  |
| `finalize.rs`   | `ec_finalize_response()`: cookie write on generation, deletion on withdrawal, `update_last_seen` debounce |
| `cookie.rs`     | Cookie string format, Max-Age=0 for deletion, domain derivation                                           |
| `kv.rs`         | Serialization/deserialization roundtrip, CAS merge logic, metadata extraction                             |
| `partner.rs`    | API key hash verification (constant-time), record serialization                                           |
| `sync_pixel.rs` | All `ts_synced` redirect codes, 429 rate limit, return URL construction                                   |
| `sync_batch.rs` | Status code selection (200/207/401/400/429), per-mapping rejection reasons, API-key rate limit            |
| `pull_sync.rs`  | Trigger conditions, null/404 no-op, dispatch limit                                                        |
| `identify.rs`   | All response codes (200/403/204), degraded flag, `uids` filtering                                         |

### 18.2 Integration tests

KV behavior is tested with Viceroy (local Fastly Compute simulator) using real KV store operations. Key scenarios:

- Consent withdrawal: cookie deletion + tombstone write (`write_withdrawal_tombstone()`) + all EC response headers stripped — in same request
- Concurrent writes: CAS retry logic under simulated generation conflicts
- KV degraded: EC cookie still set when KV `create_or_revive()` fails (best-effort)
- Sync-then-identify flow: pixel sync writes partner ID, then `/identify` returns it

**Eventually-consistent caveat:** Fastly KV does not guarantee read-after-write consistency. The sync→identify scenario may not be immediately visible on production — Viceroy may behave differently. Tests for this flow should use retry with backoff (up to 1s) and be documented as Viceroy-only consistency. Do not write assertions that assume immediate visibility after a KV write.

### 18.3 JS tests (if applicable)

If any JS changes are made for EC (e.g., publisher-side `/identify` fetch helper in `crates/js/`), use Vitest with `vi.hoisted()` for mocks.

---

## 19. Implementation Order

Suggested order to minimize risk and allow incremental testing. Each step should pass `cargo test --workspace` before the next begins.

| Step | Scope                                                     | Deliverable                                                                                    |
| ---- | --------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| 1    | `ec/identity.rs` + constants + settings                   | `generate_ec()`, `normalize_ip()`, `EcContext`                                                 |
| 2    | `ec/finalize.rs`                                          | `ec_finalize_response()` (cookie write, deletion, tombstone, last_seen)                        |
| 3    | `ec/cookie.rs`                                            | Cookie creation, deletion, response header                                                     |
| 4    | `ec/kv.rs`                                                | `KvIdentityGraph` CRUD with CAS                                                                |
| 5    | `ec/partner.rs` + `ec/admin.rs`                           | `PartnerStore`, `/_ts/admin/partners/register`                                                 |
| 6    | EC middleware in `main.rs`, `publisher.rs`, `registry.rs` | `EcContext::read_from_request()` pre-routing, `generate_if_needed()`, `ec_finalize_response()` |
| 7    | `ec/sync_pixel.rs`                                        | `GET /sync` handler + route                                                                    |
| 8    | `ec/identify.rs`                                          | `GET /identify` handler + route                                                                |
| 9    | `ec/sync_batch.rs`                                        | `POST /_ts/api/v1/sync` handler + route                                                        |
| 10   | `ec/pull_sync.rs`                                         | Background pull sync dispatch (blocking, after `send_to_client()`)                             |
| 11   | Auction integration                                       | EC injection into `user.id`, `user.eids`, `user.consent`                                       |
| 12   | End-to-end integration tests                              | Viceroy-based flow tests                                                                       |

---

## 20. Epic and Stories

### Epic: Implement Edge Cookie (EC) identity system

Enable the trusted server to generate, persist, and serve a publisher-owned,
privacy-safe Edge Cookie (EC) that can be used for ID sync, identity lookup,
and auction decoration — without relying on third-party cookies.

**Done when:** All 12 stories below are complete, `cargo test --workspace` and
`cargo clippy` pass with no warnings, and the end-to-end Viceroy flow tests
cover the full sync → identify → auction path.

**Spec ref:** This document. PRD: `docs/internal/ssc-prd.md`.

---

### Story 1 — EC generation and request context

Implement the core EC data types, generation logic, and per-request context
struct that all subsequent stories depend on.

**Scope:** `ec/identity.rs`, `ec/mod.rs`, `trusted-server.toml` `[ec]` section,
`Settings` struct update.

**Acceptance criteria:**

- `generate_ec(passphrase, ip)` produces a deterministic 71-char string:
  64-char lowercase hex hash + `.` + 6-char random alphanumeric suffix.
  HMAC inputs are `normalize_ip(ip)` as message and `passphrase` as key.
- `normalize_ip()` truncates IPv6 to /64 (first 4 groups), passes IPv4 unchanged.
- IP is sourced from `req.get_client_ip_addr()` — no header fallback.
- `EcContext::read_from_request(req, settings, geo)` reads the `ts-ec` cookie
  and `X-ts-ec` header. Sets `cookie_was_present`, `ec_was_present`, `ec_value`,
  and `cookie_ec_value` (when header and cookie carry different valid EC values —
  see §4.2 mismatch handling). Validates values via `ec_hash()` — malformed
  values are treated as absent; if header is invalid, falls back to cookie.
  Captures `client_ip` from `req.get_client_ip_addr()` (stored as
  `Option<IpAddr>` for pull sync use after `req` is consumed by routing).
  Calls `build_consent_context()` with the EC hash as identity key and stores
  the result as `consent: ConsentContext` (see §6.1.1). Does not generate.
  Does not write to EC identity KV. (Note: `build_consent_context()` may write
  to the consent KV store when an EC hash is available.)
- `EcContext::generate_if_needed(settings, kv)` generates a new EC when
  `ec_value == None && allows_ec_creation(&consent)`, sets `ec_generated = true`,
  and writes the initial KV entry via `kv.create_or_revive()` (best-effort).
  Using `create_or_revive` (not `create`) ensures re-consent within the 24h
  tombstone window recovers immediately. This function is best-effort: if
  generation fails (e.g., missing client IP), it logs `warn` and returns
  without setting `ec_generated`. It never returns an error — organic traffic
  must not 500 on EC failure.
- `[ec]` settings block parses from TOML: `passphrase`, `ec_store`,
  `partner_store`, `admin_token_hash`, `pull_sync_concurrency`.
- All unit tests in `identity.rs` pass (HMAC determinism, format, IP normalization).

**Spec ref:** §2, §3, §4, §5.4, §14.1

---

### Story 2 — EC finalize response

Implement `ec_finalize_response()` — the post-routing function that enforces
cookie writes, deletions, tombstones, and last-seen updates on every response.

**Scope:** `ec/finalize.rs` (new file)

**Acceptance criteria:**

- `ec_finalize_response(settings, geo, ec_context, kv, response)` runs on every route.
- Consent gating uses the existing `allows_ec_creation()` — no new gating function.
- When `!allows_ec_creation(&consent) && cookie_was_present`: calls
  `clear_ec_on_response()` (deletes cookie and strips all EC response headers)
  and writes tombstone for each valid EC hash available. When the cookie is
  malformed and no valid header exists, no tombstone is written — cookie
  deletion alone enforces withdrawal (see §6.2).
- When `ec_was_present && !ec_generated && allows_ec_creation(&consent)`: calls
  `kv.update_last_seen(ec_hash, now())` (debounced at 300s). If `cookie_ec_value`
  is set (header/cookie mismatch), also calls `set_ec_on_response()` to reconcile
  the browser cookie to the header-derived identity.
- When `ec_generated == true`: calls `set_ec_on_response()`.
- Unit tests cover all four branches: withdrawal (with and without valid hash),
  returning-user last_seen + mismatch reconciliation, and new-EC generation.

**Spec ref:** §5.4, §6.2

---

### Story 3 — EC cookie helpers

Implement the low-level functions that create and delete the `ts-ec` cookie
and set EC response headers. These are called by `ec_finalize_response()` (Story 2).

**Scope:** `ec/cookie.rs`

**Acceptance criteria:**

- `create_ec_cookie()` produces a cookie with `Domain=.{publisher.domain}`,
  `Max-Age=31536000`, `SameSite=Lax; Secure`. `HttpOnly` is NOT set
  (JS on the publisher page must be able to read the cookie).
- `delete_ec_cookie()` produces a cookie with `Max-Age=0`, same attributes.
- `set_ec_on_response()` sets `Set-Cookie` and `X-ts-ec` response headers.
- `clear_ec_on_response()` sets `Set-Cookie` with `Max-Age=0` **and** strips all
  EC-related response headers: `X-ts-ec`, `X-ts-eids`, `X-ts-ec-consent`,
  `x-ts-eids-truncated`, and any `X-ts-<partner_id>` headers. This prevents
  leaking EC identity on consent-withdrawal responses where a handler may have
  already set these headers before `ec_finalize_response()` runs.
- Unit tests cover cookie string format, Max-Age=0 deletion, domain derivation,
  and header stripping (verify headers are removed after `clear_ec_on_response`).

**Spec ref:** §5.1, §5.3, §5.4, §17 (ec_finalize_response)

---

### Story 4 — KV identity graph

Implement the KV read/write/delete layer for EC identity entries, including
CAS-based concurrent write protection and consent withdrawal delete.

**Scope:** `ec/kv.rs`

**Acceptance criteria:**

- `KvIdentityGraph::get(ec_hash)` returns the deserialized entry and generation
  marker as `Option<(KvEntry, u64)>`, or `None` if not found.
- `KvIdentityGraph::get_metadata(ec_hash)` returns `Option<KvMetadata>` for
  cheap consent/country checks without streaming the full body.
- `KvIdentityGraph::create(ec_hash, &entry)` writes a new entry with
  `consent.ok = true`. Returns `Err` if the key already exists (concurrent
  create) or on KV error. No retry — callers handle conflicts.
- `KvIdentityGraph::create_or_revive(ec_hash, &entry)` creates a new entry OR
  overwrites an existing tombstone (`consent.ok = false`) with a fresh entry;
  no-ops if a live entry already exists. Called by `generate_if_needed()`.
- `KvIdentityGraph::update_last_seen(ec_hash, timestamp)` updates `last_seen`
  without overwriting partner IDs (CAS merge), and only writes if the stored
  value is more than 300s older than `timestamp` (debounce to avoid 1 write/sec
  KV limit). Callers pass `now()` as `timestamp`.
- `KvIdentityGraph::write_withdrawal_tombstone(ec_hash)` sets `consent.ok = false`,
  clears partner IDs, and applies a 24-hour TTL (see §6.2). Returns `Result` —
  callers log `error` on failure and continue (cookie deletion is the primary
  enforcement mechanism).
- `KvIdentityGraph::delete(ec_hash)` hard-deletes the entry — used only for IAB
  data deletion requests, not for consent withdrawal (which uses tombstones).
- `kv.upsert_partner_id(ec_hash, partner_id, uid, timestamp)` writes to
  `ids[partner_id]`, creating a minimal live root entry first if the key is
  absent, and skips if existing `synced >= timestamp` (idempotent).
- KV schema matches §7 exactly (JSON roundtrip test).
- Unit tests cover CAS merge logic, tombstone write, tombstone error handling,
  serialization/deserialization roundtrip, metadata extraction.

**Spec ref:** §4, §5.4, §6.2

---

### Story 5 — Partner registry and admin endpoint

Implement `PartnerRecord`, `PartnerStore`, and the admin registration endpoint
that operators use to onboard ID sync partners.

**Scope:** `ec/partner.rs`, `ec/admin.rs`, router update

**Acceptance criteria:**

- `PartnerRecord` contains all fields from §13.1 including
  `pull_sync_allowed_domains` and `batch_rate_limit`.
- `PartnerStore::get()`, `upsert()`, `find_by_api_key_hash()` operate on
  `partner_store` KV.
- `pull_enabled_partners()` re-checks `pull_sync_enabled == true` on fetched
  records so stale `_pull_enabled` index entries do not dispatch disabled partners.
- API key stored as SHA-256 hex; plaintext never written to KV.
- `verify_api_key()` uses constant-time comparison.
- `POST /_ts/admin/partners/register` validates `Authorization: Bearer <token>` inside
  the handler against `settings.ec.admin_token_hash` (constant-time SHA-256 comparison).
  Returns `401` if missing or invalid — before any request body is read.
- Admin endpoint validates: `pull_sync_url` hostname must be in
  `pull_sync_allowed_domains` when set — returns `400` otherwise.
- Returns `201 Created` on new partner or `200 OK` on update, with an explicit
  response DTO (see §13.2 step 6 — do NOT serialize full `PartnerRecord`).
  Returns `400` on validation failure; `503` on KV failure.
- `/_ts/admin/partners/register` is **NOT** added to `Settings::ADMIN_ENDPOINTS` —
  it uses bearer-token-in-handler auth, not `[[handlers]]` Basic Auth.
- The admin-route-scan test (`settings.rs:1504-1530`) must be updated to exclude
  bearer-token-authed routes from its `ADMIN_ENDPOINTS` assertion. Add an exclusion
  list (see §13.2 codebase invariant note).
- The `[[handlers]]` pattern in `trusted-server.toml` must be narrowed from
  `"^/_ts/admin"` to `"^/_ts/admin/keys"` (see §13.2).
- Unit tests cover API key hash verification and record serialization.

**Spec ref:** §13

---

### Story 6 — EC middleware integration

Wire `EcContext` into the request pipeline following the two-phase model
(§5.4 and §17.1). `EcContext::read_from_request()` runs pre-routing like
`GeoInfo`; `generate_if_needed()` runs inside organic handlers only.

**Scope:** `main.rs`, `publisher.rs`, `endpoints.rs`, `registry.rs`

**Acceptance criteria:**

- `EcContext::read_from_request()` is called before the route match on every
  request, passed the existing `geo_info` (no duplicate geo header parsing).
- EC route handlers receive `ec_context` without EC generation. `/identify`,
  `/auction`, `/_ts/api/v1/sync`, and `/_ts/admin/*` use read-only `&EcContext` and
  never mutate it. **Exception:** `/sync` receives `&mut EcContext`; when the
  consent query-param fallback applies (`ec_context.consent.is_empty()`), it
  assigns the locally-decoded consent into `ec_context.consent` so that both
  the sync write decision and `ec_finalize_response()` share the same effective
  consent view. This prevents a same-request "write partner ID, then withdraw
  EC" conflict. See §8.3 for full details.
- `/auction` consumes EC identity but never bootstraps it.
- `handle_publisher_request()` and `integration_registry.handle_proxy()` call
  `ec_context.generate_if_needed(settings, &kv)` before their handler logic (best-effort, never 500s).
- `ec_finalize_response()` receives `ec_context` and `kv` and:
  - Deletes the EC cookie and writes a withdrawal tombstone when `!allows_ec_creation(&consent) && cookie_was_present` (runs on all routes).
  - Calls `kv.update_last_seen(ec_hash, now())` when `ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)` (returning user with valid consent).
  - Calls `set_ec_on_response()` when `ec_context.ec_generated == true`, and also
    on returning-user mismatch reconciliation when `cookie_ec_value.is_some()`.
- `route_request()` return type changes from `Result<Response, Error>` to
  `Result<(), Error>`; response is flushed via `response.send_to_client()` instead
  of being returned. The `#[fastly::main]` entrypoint must also change to match.
  This is a structural refactor of the Fastly entrypoint, not an additive change —
  see §17.1 for the full scope.
- `handle_auction()` signature changes to accept `&KvIdentityGraph` and `&EcContext`
  (see §17.1 pseudocode comment).
- **Handler refactoring:** PR #479 removes `get_or_generate_synthetic_id()`,
  `COOKIE_SYNTHETIC_ID`, and `X-Synthetic-*` headers from all handlers. This
  epic completes the refactoring by replacing the internal `build_consent_context()`
  calls with `ec_context.consent`:
  - `handle_publisher_request()`, `handle_auction()`, and
    `integration_registry.handle_proxy()` no longer call `build_consent_context()`
    internally — they use `ec_context.consent` (built pre-routing).
  - Identity comes from `ec_context.ec_value` (no synthetic fallback).
- `cargo test --workspace` passes with no regressions.

**Spec ref:** §5, §17

---

### Story 7 — Pixel sync (`GET /sync`)

Implement the pixel-based ID sync endpoint that partners use to write their
user ID against an EC hash.

**Scope:** `ec/sync_pixel.rs`, router update

**Acceptance criteria:**

- Missing required query params (`partner`, `uid`, `return`) → `400`.
- No valid `ts-ec` cookie (missing or malformed) → redirect to
  `{return}?ts_synced=0&ts_reason=no_ec`.
- Unknown `partner` ID → `400`.
- `return` URL hostname not in `partner.allowed_return_domains` → `400`.
- Consent uses `ec_context.consent`. The optional `consent` query param is a fallback
  only: it is used exclusively when `ec_context.consent.is_empty()` returns `true`
  — meaning no consent signals of any kind are present (no TCF string, no GPP
  string, no US Privacy string, no AC string, no GPC, no decoded consent objects).
  Use the `ConsentContext::is_empty()` method directly; do not reimplement the
  check from this description. If consent KV fallback or any other pre-routing
  source has already populated `ec_context.consent`, `is_empty()` is `false` and
  the param is ignored.
  When the fallback applies, decode the consent string locally into a
  `ConsentContext` and **assign it into `ec_context.consent`** so that both
  the sync write and `ec_finalize_response()` share the same effective consent
  (prevents a same-request "write partner ID, then withdraw EC" conflict).
  Do NOT re-call `build_consent_context()` (that would trigger consent KV writes).
  Denied or absent → redirect to `{return}?ts_synced=0&ts_reason=no_consent`.
- Rate limit exceeded → `429 Too Many Requests` (no redirect).
- KV write failure → redirect to `{return}?ts_synced=0&ts_reason=write_failed`.
- `kv.upsert_partner_id()` creates a minimal live root entry first when the EC
  exists in the cookie but the identity graph key is still missing because the
  original best-effort `create_or_revive()` failed on generation.
- Success → redirect to `{return}?ts_synced=1`.
- Return URL construction correctly appends `&` or `?` based on existing query string.
- Rate counter key: `{partner_id}:{ec_hash}`, 1-hour window, via `fastly::erl::RateCounter`.
- Unit tests cover all redirect/response codes and return URL construction.

**Spec ref:** §8

---

### Story 8 — Identity lookup (`GET /identify`)

Implement the browser-facing endpoint that publishers call to retrieve the EC
hash and synced partner UIDs for the current user.

**Scope:** `ec/identify.rs`, router update

**Acceptance criteria:**

- `!allows_ec_creation(consent)` (consent denied, regardless of EC presence) → `403 Forbidden`.
  When EC is present but consent is denied, the handler returns `403` and
  `ec_finalize_response()` deletes the cookie and writes a tombstone.
- No EC present (`ec_was_present == false`) and consent not denied → `204 No Content`.
- Valid EC, consent granted, KV read succeeds with entry → `200` with full JSON body
  including `ec`, `consent`, `uids`, `eids`.
- Valid EC, consent granted, KV read succeeds but no entry (never synced or
  `create_or_revive()` failed on generation) → `200` with `degraded: false`,
  empty `uids`/`eids`. This is not an error — see §11.4.
- `uids` filtered to partners where `bidstream_enabled = true` and consent
  granted.
- KV read error (store unavailable) → `200` with `degraded: true` and empty
  `uids`/`eids`.
- No `Origin` header (server-side proxy): process normally, no CORS headers, no `403`.
- `Origin` header present and matches `publisher.domain` or subdomain: reflect in
  `Access-Control-Allow-Origin` + `Vary: Origin`.
- `Origin` header present but does not match: `403`, no body.
- `OPTIONS /identify` preflight → `200` with CORS headers, no body.
- `generate_if_needed()` is never called — no new EC is generated. The handler
  itself does not write cookies, but `ec_finalize_response()` may still delete
  the cookie on withdrawal or reconcile it on header/cookie mismatch.
- Response time target: 30ms p95 (documented, not gate).
- Unit tests cover all response codes, degraded flag, `uids` filtering,
  CORS origin validation.

**Spec ref:** §11

---

### Story 9 — S2S batch sync (`POST /_ts/api/v1/sync`)

Implement the server-to-server batch sync endpoint for partners to bulk-write
their UIDs against a list of EC hashes.

**Scope:** `ec/sync_batch.rs`, router update

**Acceptance criteria:**

- Missing or invalid `Authorization: Bearer` → `401`. Auth uses index-based
  lookup via `find_by_api_key_hash()` (§9.2) with constant-time hash verification.
- Auth KV lookup failure (store unavailable) → `503 Service Unavailable`.
- API-key rate limit exceeded (`batch_rate_limit` per partner per minute) → `429`
  with `{ "error": "rate_limit_exceeded" }`.
- More than 1000 mappings → `400`.
- Per-mapping rejections: `invalid_ec_hash`, `ec_hash_not_found`,
  `consent_withdrawn`, `kv_unavailable`.
- KV write failure aborts remaining mappings with `kv_unavailable`; partial
  results returned as `207`.
- All mappings accepted → `200`. Any rejection → `207`.
- `kv.upsert_partner_id()` is idempotent: duplicate timestamp counted as
  accepted, no error.
- Rate counter key: `batch:{partner_id}`, 1-minute window.
- Unit tests cover status code selection, all rejection reasons, and API-key
  rate limit.

**Spec ref:** §9

---

### Story 10 — Pull sync dispatch

Implement the background pull sync dispatcher that calls partner resolution
endpoints after the response is flushed via `send_to_client()`. Uses
`send_async()` + `PendingRequest::wait()` (synchronous blocking, no async
runtime). Only fires on organic routes (§10.2).

**Scope:** `ec/pull_sync.rs`

**Acceptance criteria:**

- Dispatch only when: EC present (including an EC generated on the current
  organic request), consent granted, `pull_sync_enabled = true`, and either no
  existing partner entry or existing `synced` is older than `pull_sync_ttl_sec`.
- Rate limit: `pull_sync_rate_limit` per EC hash per partner per hour; counter
  key `pull:{partner_id}:{ec_hash}`.
- Maximum concurrent pulls per request: `settings.ec.pull_sync_concurrency`
  (default 3).
- Before calling, validate `pull_sync_url` hostname is in
  `pull_sync_allowed_domains`; skip and log `error` if not.
- Outbound request: `GET {pull_sync_url}?ec_hash={hash}&ip={ip}` with
  `Authorization: Bearer {ts_pull_token}`.
- `{ "uid": null }` and `404` are no-ops — no KV write, no error logged above
  `debug`.
- Any other non-200 → transient failure, no retry, no error above `warn`.
- Dispatch runs after `send_to_client()` — does not add latency to the
  user-facing response. Uses `send_async()` + `PendingRequest::wait()` (blocking).
- Only fires on organic routes (`handle_publisher_request`, `handle_proxy`) —
  never on `/sync`, `/identify`, `/auction`, `/_ts/api/v1/sync`, or `/_ts/admin/*`.
- Unit tests cover trigger conditions, null/404 no-op, domain allowlist check,
  dispatch limit enforcement.

**Spec ref:** §10

---

### Story 11 — Auction bidstream decoration

Inject EC identity data into outbound OpenRTB bid requests for publishers with
`bidstream_enabled = true` partners.

**Scope:** Auction handler (Mode B path in existing auction code)

**Acceptance criteria:**

- `user.id` set to `ec_context.ec_value` when EC present and consent granted.
  No synthetic fallback — when no EC is present, `user.id` is omitted.
- `user.eids` populated with one entry per `bidstream_enabled` partner that
  has a synced UID, using `partner.source_domain` and `partner.openrtb_atype`.
- `user.consent` set to `ec_context.consent.raw_tc_string` when present.
- No EID entry written for partners with no synced UID.
- KV read failure → `user.eids` omitted (empty); `user.id` still set from EC;
  auction proceeds without EID data (no 5xx).
- No EC present → `user.id` omitted; `user.eids` is empty. Auction still runs.
- `X-Synthetic-*` response headers are not present (removed in PR #479). Only `X-ts-ec` is set.
- Unit tests cover EID structure, consent string threading, KV-degraded path,
  and no-EC path (verify no synthetic fallback).

**Spec ref:** §12

---

### Story 12 — End-to-end integration tests

Write Viceroy-based integration tests covering the full identity lifecycle
across multiple handlers in a single simulated environment.

**Scope:** `tests/` (integration test crate or new test module)

**Acceptance criteria:**

- **Full flow:** First-party page load → EC generated → pixel sync writes
  partner UID → `/identify` returns that UID → auction includes EID.
- **Consent withdrawal:** Request with denied consent clears EC cookie and writes
  a KV tombstone (`consent.ok = false`, 24h TTL) in the same request; subsequent
  `/identify` with consent still denied returns `403` (consent denied → §11.4);
  batch sync returns `consent_withdrawn` within the tombstone TTL.
- **KV create failure:** EC cookie is still set when `create_or_revive()` fails
  (best-effort). Subsequent `/identify` returns `200` with `degraded: false` and
  empty `uids`/`eids` (KV read succeeds — entry simply does not exist).
- **KV read failure:** `/identify` returns `200` with `degraded: true` and empty
  `uids`/`eids` (store unavailable, entry might exist but can't be read).
- **Concurrent writes:** Two simultaneous EC creates for the same hash resolve
  without data loss (CAS retry).
- **Rate limits:** Pixel sync returns `429` after `sync_rate_limit` is
  exceeded; batch sync returns `429` after `batch_rate_limit` is exceeded.
- **Pull sync no-op:** Partner returning `{ "uid": null }` produces no KV
  write and no error log.
- All tests pass under `cargo test --workspace` with Viceroy.

**Spec ref:** §18.2

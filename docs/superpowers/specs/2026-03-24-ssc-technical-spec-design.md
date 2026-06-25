# Technical Specification: Edge Cookie (EC)

**Status:** Draft
**Author:** Engineering
**PRD reference:** `docs/internal/ssc-prd.md`
**Last updated:** 2026-04-14

> **Supersession note (issue #666):** Sections in this historical design spec
> that describe a separate `consent_store` or consent KV fallback are obsolete.
> Current runtime behavior interprets live consent from request cookies, headers,
> geolocation, and policy defaults. `ec_identity_store` is the only KV-backed EC
> lifecycle store and holds identity graph state plus withdrawal tombstones.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Architecture Overview](#2-architecture-overview)
3. [Module Structure](#3-module-structure)
4. [EC Identity Generation](#4-ec-identity-generation)
5. [Cookie and Header Handling](#5-cookie-and-header-handling)
6. [Consent Enforcement](#6-consent-enforcement)
7. [KV Store Identity Graph](#7-kv-store-identity-graph)
   7A. [Device Signals and Bot Gate](#7a-device-signals-and-bot-gate)
8. [Prebid EID Cookie Ingestion](#8-prebid-eid-cookie-ingestion)
9. [S2S Batch Sync API (`POST /_ts/api/v1/batch-sync`)](#9-s2s-batch-sync-api-post-apiv1sync)
10. [S2S Pull Sync (TS-Initiated)](#10-s2s-pull-sync-ts-initiated)
11. [Identity Resolution Endpoint (`GET /_ts/api/v1/identify`)](#11-identity-resolution-endpoint-get-identify)
12. [Bidstream Decoration (`/auction` Mode B)](#12-bidstream-decoration-auction-mode-b)
13. [Partner Registry (Config-Based)](#13-partner-registry-config-based)
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

- **SyntheticID removal** — [PR #479](https://github.com/IABTechLab/trusted-server/pull/479) removes SyntheticID from all active code paths: `get_or_generate_synthetic_id()`, `COOKIE_SYNTHETIC_ID`, `X-Synthetic-*` headers, `synthetic.rs` module, `settings.synthetic` config, and all SyntheticID generation/cookie code from `publisher.rs`, `endpoints.rs`, and `registry.rs`. **This PR must be merged before implementation of this spec begins.** The spec assumes a codebase where SyntheticID no longer exists. Verify before starting:
  - `grep -r 'synthetic_id' crates/` returns no hits outside test fixtures
  - `grep -r 'X-Synthetic' crates/` returns no hits
  - `trusted-server.toml` has no `[synthetic]` section
  - `ConsentPipelineInput` uses `identity_key`, not `synthetic_id`
- **Consent implementation** — The consent pipeline (`build_consent_context()`, `ConsentContext`, `allows_ec_creation()`, TCF/GPP/US-Privacy decoding) is implemented and available as a stable interface before this epic. PR `#380` merged to `main`. EC calls `allows_ec_creation()` directly — no new gating functions are introduced. Consent is evaluated from live request cookies, headers, geolocation, and policy defaults before EC generation.

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
Phase 0 — bot gate (pure in-memory, no KV I/O):
    ┌─────────────────────────────────────────────────┐
    │  derive_device_signals(req)                      │
    │  - UA → is_mobile, platform_class                │
    │  - req.get_tls_ja4() → ja4_class (Section 1)    │
    │  - req.get_client_h2_fingerprint() → h2_fp_hash  │
    │  - (ja4_class, h2_fp_hash) → known_browser       │
    │                                                   │
    │  !looks_like_browser()?                             │
    │    → suppress KV graph (None), skip ec_finalize,  │
    │      skip pull sync. Request still proxied to     │
    │      origin — bot receives valid HTML but leaves   │
    │      no trace in the identity graph.              │
    └──────┬────────────────────────────────────────────┘
           │
Phase 1 — pre-routing (like `GeoInfo::from_request()`):
    ┌─────────────────────────────────────────┐
    │  EcContext::read_from_request()          │
    │  - read ts-ec cookie / X-ts-ec header   │
    │  - build_consent_context() → ConsentContext  │
    │  - allows_ec_creation(consent)               │
    │  No generation. No cookie writes.       │
    │                                              │
    │  ec_context.set_device_signals(signals)      │
    │  (passed through to KvEntry on creation)     │
    └──────┬──────────────────────────────────┘
           │
Phase 2 — inside organic handlers only:
   ┌───────┼──────────────────────────────────────────────────┐
   │       │                                                   │
   ▼       ▼                                                   ▼
handle_publisher_request()     integration_registry.handle_proxy()
calls ec_context.generate_if_needed()   calls ec_context.generate_if_needed()

EC route handlers (GET /_ts/api/v1/identify, POST /auction,
POST /_ts/api/v1/batch-sync) NEVER call generate_if_needed().
`/_ts/api/v1/identify`, `/auction`, and `POST /_ts/api/v1/batch-sync`
use `EcContext` in read-only form.
/auction reads EC identity but never bootstraps it — the publisher
page-load path generates the EC before any auction request arrives.

ec_finalize_response() — after every handler:
    - !allows_ec_creation(&consent)? → strip EC response headers
    - explicit withdrawal + cookie present? → also expire the cookie and write tombstones
    - returning user with consent? → set x-ts-ec header only (no cookie/KV TTL refresh)
    - ec_generated == true? → set EC cookie + x-ts-ec header
    - Prebid EID ingestion: reads `ts-eids` cookie, matches source domains
      via PartnerRegistry, writes changed partner UIDs to KV (same UID = no write)
```

EC state flows through an `EcContext` struct created once per request and passed through handlers.

---

## 3. Module Structure

New files in `crates/trusted-server-core/src/`:

```
crates/trusted-server-core/src/
  ec/
    mod.rs          — EcContext, pub re-exports
    generation.rs   — EC generation (HMAC-SHA256, IP normalization)
    cookies.rs      — set_ec_cookie(), expire_ec_cookie()
    consent.rs      — EC consent gating helpers
    device.rs       — DeviceSignals derivation, UA/JA4/H2 parsing, known browser allowlist
    eids.rs         — OpenRTB EID construction helpers
    finalize.rs     — ec_finalize_response() (cookie write/delete, tombstone, EID ingestion)
    kv.rs           — KvIdentityGraph, read/write/delete identity entries, cluster evaluation
    kv_types.rs     — KvEntry, KvGeo, KvConsent, KvPubProperties, KvNetwork, KvDevice, KvMetadata
    partner.rs      — Partner validation helpers (ID format, API key hashing)
    registry.rs     — PartnerRegistry (in-memory, config-based, O(1) indexes)
    rate_limiter.rs — RateLimiter trait and Fastly ERL implementation
    prebid_eids.rs  — ingest_prebid_eids() — ts-eids cookie parsing and KV sync
    batch_sync.rs   — handle_batch_sync() handler
    pull_sync.rs    — PullSyncDispatcher, dispatch_background()
    identify.rs     — handle_identify() handler
```

Existing files modified:

| File                                               | Change                                                |
| -------------------------------------------------- | ----------------------------------------------------- |
| `crates/trusted-server-core/src/settings.rs`       | Add `Ec` and `EcPartner` settings structs             |
| `crates/trusted-server-core/src/constants.rs`      | Add EC header/cookie name constants                   |
| `crates/trusted-server-core/src/error.rs`          | Add `EdgeCookie` error variant                        |
| `crates/trusted-server-core/src/auction/`          | Inject EC into `user.id`, `user.eids`, `user.consent` |
| `crates/trusted-server-adapter-fastly/src/main.rs` | Register new routes, run EC middleware                |

---

## 4. EC Identity Generation

### 4.1 Module: `ec/generation.rs`

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

/// Extracts the stable 64-character hex prefix from a full EC ID.
///
/// This is primarily used for logging and debugging. Both the EC identity
/// EC identity KV operations use the **full EC ID** (including the
/// `.suffix`) as the key, not just this prefix. The suffix provides uniqueness
/// for users behind the same NAT/proxy infrastructure.
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

The random suffix is generated with `fastly::rand` (same approach as SyntheticID). Once set in a cookie, the full value (hash + suffix) is preserved and used as the KV store key for the EC identity graph. The suffix provides uniqueness for users behind the same NAT/proxy who share the same IP-derived hash.

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

On **explicit consent withdrawal** (`has_explicit_ec_withdrawal(&consent) && cookie_was_present`):

- Delete the browser cookie (always, based on `cookie_was_present`)
- Tombstone the **cookie-derived** hash: `kv.write_withdrawal_tombstone(ec_hash(cookie_ec_value))`
- If the header-derived hash differs, also tombstone it: `kv.write_withdrawal_tombstone(ec_hash(ec_value))`
- This matches the existing SyntheticID behavior where revocation targets the cookie value (`publisher.rs:515`), not the header value.

If `allows_ec_creation(&consent)` is `false` but there is **no explicit withdrawal signal** (for example, unknown jurisdiction or missing/undecodable consent in a regulated regime), the response strips EC-related headers only. It does **not** delete the cookie or write tombstones.

On **non-withdrawal** paths (handler reads and response headers): use `ec_value` (header-derived) as the active identity. Returning-user responses set `x-ts-ec` for the active identity but do not refresh or repair the browser cookie. Cookie writes are reserved for newly generated ECs; cookie deletion is reserved for explicit consent withdrawal.

**Validation:** Both the header and cookie values are validated independently via `ec_hash()` (`{64-hex}.{6-alnum}` format). If the header is present but malformed, it is discarded and the cookie value is used instead (if valid). A malformed header must not suppress a valid cookie — bad forwarding infrastructure should not break returning-user identity. `cookie_was_present` is set based on the raw cookie existing, regardless of validity — an invalid cookie value is still a cookie that needs to be cleared on withdrawal.

Generation (step 3 above becoming a new EC) happens only inside organic handlers — see §5.4. This logic lives in `EcContext::read_from_request()` (phase 1) and `EcContext::generate_if_needed()` (phase 2).

### 4.3 `EcContext`

```rust
/// Per-request Edge Cookie state. Constructed pre-routing once per request.
/// Organic handlers call `generate_if_needed()` to mint new ECs.
pub struct EcContext {
    /// Full EC ID (`{64-hex}.{6-alnum}`), if present on request or generated this request.
    pub ec_value: Option<String>,
    /// Whether the `ts-ec` **cookie** was present on the inbound request.
    /// This is the only field that gates consent-withdrawal cookie deletion —
    /// the PRD's delete branch is conditioned on the cookie, not on X-ts-ec header.
    pub cookie_was_present: bool,
    /// The cookie's EC value, if different from `ec_value` (header won priority).
    /// Used only for withdrawal: tombstone targets the cookie-derived EC ID to match
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
    /// Device signals derived from TLS/H2/UA in the adapter layer.
    /// Set via `set_device_signals()` after `read_from_request()` returns.
    /// Converted to `KvDevice` and stored on new entries in `generate_if_needed()`.
    /// `None` when the adapter does not provide signals (e.g., test environments).
    pub device_signals: Option<DeviceSignals>,
}

impl EcContext {
    /// Phase 1: reads cookie/header and builds consent context. Does not generate.
    /// Does not write to the **EC identity KV store**. Called pre-routing, like
    /// `GeoInfo::from_request()` in the current `main.rs`.
    ///
    /// Calls `build_consent_context()` with request-local cookies, headers,
    /// settings, and geo data. There is no separate consent KV fallback; live
    /// consent is interpreted from the current request.
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

    /// Sets device signals derived from the adapter layer (TLS/H2/UA).
    /// Must be called before `generate_if_needed()` so new entries include `KvDevice`.
    pub fn set_device_signals(&mut self, signals: DeviceSignals);

    /// Returns the device signals, if set.
    pub fn device_signals(&self) -> Option<&DeviceSignals>;

    /// Returns the stable 64-char hex prefix, or `None` if no EC.
    ///
    /// Note: This extracts only the prefix for display/logging purposes. All KV
    /// operations use the full EC ID (via `ec_value()`), not just this hash.
    pub fn ec_hash(&self) -> Option<&str>;
}
```

**`ec_finalize_response()` behavior** (signature: `ec_finalize_response(settings, ec_context, kv, registry, eids_cookie, response)`):

1. If `!allows_ec_creation(&consent)`: call `clear_ec_headers_on_response()` to strip any handler-built `X-ts-ec`, `X-ts-eids`, `X-ts-ec-consent`, `x-ts-eids-truncated`, and `X-ts-<partner_id>` response headers. This runs on **every route**, including fail-closed cases where consent cannot be verified.
2. If `has_explicit_ec_withdrawal(&consent) && cookie_was_present`: additionally expire the cookie and write withdrawal tombstones for each valid known EC ID (cookie-derived and, when different, header-derived). Keyed on `cookie_was_present`, not `ec_was_present`, because only a cookie-held EC can be deleted by the browser. When the cookie is malformed and there is no valid header-derived EC ID, no tombstone is written.
3. If `ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)`: ingest Prebid EIDs from the `ts-eids` cookie if present (see section 8) and set the `x-ts-ec` response header only. Ordinary returning-user requests do not refresh the EC cookie and do not write KV solely to extend TTL.
4. If `ec_generated == true`: set `Set-Cookie` and `X-ts-ec`. KV create already happened inside `generate_if_needed()`; `ec_finalize_response()` does NOT write KV beyond explicit-withdrawal tombstones and Prebid EID ingestion. Also ingest Prebid EIDs from the `ts-eids` cookie if present.
5. Handler-built response headers (`X-ts-ec` set directly by `/_ts/api/v1/identify`) are preserved only when consent currently allows EC.

**Note on `kv_degraded`:** Not on `EcContext` — `read_from_request()` does not read KV. Handlers track degraded state locally. `/_ts/api/v1/identify` returns `degraded: true` in the JSON body on KV read failure; the auction handler treats a failed read as `eids: []`.

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

### 5.2 Module: `ec/cookies.rs`

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

/// Sets only the `X-ts-ec` response header on a response.
pub fn set_ec_header_on_response(response: &mut Response, ec_value: &str);

/// Sets the EC cookie and `X-ts-ec` response header on a response.
pub fn set_ec_cookie_and_header_on_response(response: &mut Response, ec_value: &str, cookie_domain: &str);

/// Removes the EC cookie and strips all EC-related response headers:
/// `X-ts-ec`, `X-ts-eids`, `X-ts-ec-consent`, `x-ts-eids-truncated`,
/// and any `X-ts-<partner_id>` headers. Called on explicit consent
/// withdrawal to prevent leaking EC identity in handler-built headers.
pub fn clear_ec_on_response(response: &mut Response, cookie_domain: &str);
````

### 5.3 Response header

`X-ts-ec: {64-hex}.{6-alnum}` is set when an EC is available for the response. In current behavior, returning users (`ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)`) receive the header only; newly generated ECs (`ec_generated == true`) receive both the header and `Set-Cookie`. `/_ts/api/v1/identify` and `/auction` also set EC-related headers on their response paths.

This header is added to `INTERNAL_HEADERS` in `constants.rs` so it is stripped before proxying to downstream backends, consistent with existing `X-ts-*` handling.

### 5.4 Per-request EC lifecycle

**Phase 0 — bot gate** (always runs, all routes — pure in-memory, no KV I/O):

```
derive_device_signals(req)
  ua = req.get_header_str("user-agent")
  ja4 = req.get_tls_ja4()                   // Fastly SDK — full JA4 hash
  h2_fp = req.get_client_h2_fingerprint()    // Fastly SDK — raw H2 SETTINGS string

  DeviceSignals::derive(ua, ja4, h2_fp)
    is_mobile = parse_is_mobile(ua)          // 0=desktop, 1=mobile, 2=unknown
    ja4_class = extract_ja4_section1(ja4)    // split on '_', take [0]
    platform_class = parse_platform_class(ua) // mac/windows/ios/android/linux/None
    h2_fp_hash = sha256(h2_fp)[..6].hex()   // 12 hex chars
    known_browser = evaluate_known_browser(ja4_class, h2_fp_hash) // allowlist match

  is_real_browser = looks_like_browser()   // ja4_class.is_some() && platform_class.is_some()

  if !is_real_browser:
    log::debug("Bot gate: blocking EC operations")
    kv_graph = None                          // suppress all KV operations
    // ec_finalize_response() will be skipped
    // pull sync will be skipped
    // request still proxied to origin normally
```

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
  ec_id = ec_value.as_deref()   // None on first visit or malformed
  build_consent_context(jar, req, config, geo, ec_id) → consent: ConsentContext
  // Consent is interpreted from request-local cookies, headers, settings, and geo.
  // No separate consent KV fallback or persistence runs in the EC lifecycle.
  ec_generated = false

  ec_context.set_device_signals(device_signals) // for KvDevice on creation
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
          → kv.create_or_revive(new_ec, &entry)   (best-effort, log warn if fails)
            // create_or_revive overwrites a tombstone (ok=false) on re-consent
            // no-ops if a live entry (ok=true) already exists
```

**`ec_finalize_response(settings, geo, ec_context, &kv, response)` — runs only when `is_real_browser == true`:**

```
  // Bot gate: when !looks_like_browser(), this entire block is skipped.
  // The response is proxied to origin without any cookie writes or KV operations.

  ├── !allows_ec_creation(&consent)?
  │       → clear_ec_headers_on_response()    (strip ALL EC headers from response)
  │       → has_explicit_ec_withdrawal(&consent) && cookie_was_present?
  │             → expire_ec_cookie()
  │             → // Tombstone all known valid EC IDs. May be 0, 1, or 2 IDs.
  │               if let Some(cookie_ec_id) = cookie_ec_value.filter(|v| is_valid_ec_id(v)):
  │                 kv.write_withdrawal_tombstone(cookie_ec_id)       // cookie-derived EC ID
  │               if let Some(header_ec_id) = ec_value.filter(|v| is_valid_ec_id(v)):
  │                 if Some(header_ec_id) != cookie_ec_id:
  │                   kv.write_withdrawal_tombstone(header_ec_id)     // header-derived EC ID (if different)
  │               // When cookie is malformed and no valid header exists: no tombstone written.
  │               // Cookie deletion is still the authoritative enforcement mechanism.
  │               // Tombstone fails? log error, do NOT block — no retry possible on browser path.
  │       → return
  │
  ├── ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)?
  │       → set_ec_header_on_response()       (returning user — no cookie/KV TTL refresh)
  │
  └── ec_generated == true?
          → set_ec_cookie_and_header_on_response()  (Set-Cookie + X-ts-ec on response)
```

EC route handlers (`GET /_ts/api/v1/identify`, `POST /_ts/api/v1/batch-sync`) never call `generate_if_needed()`. `ec_finalize_response()` will still delete the cookie on those routes if consent is explicitly withdrawn — that is intentional.

**Cookie write rule:** `Set-Cookie` is written for newly generated ECs and consent-withdrawal deletion only. Ordinary returning requests set `x-ts-ec` but do not refresh the cookie `Max-Age`.

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
from the consent module (`consent/mod.rs`) for EC generation, header emission,
and other "may this request use ECs right now?" decisions.

Explicit withdrawal semantics use a separate
`has_explicit_ec_withdrawal(&ConsentContext) -> bool` helper. This narrower
signal distinguishes authoritative opt-outs from fail-closed cases where EC use
must be blocked for the current request but an already-issued EC must not be
revoked (for example, unknown jurisdiction or missing/undecodable consent in a
regulated regime).

There is no new consent source or KV lookup in this spec. Shared
consent-policy semantics stay in the consent module; EC consumes the existing
request-local decision plus the explicit-withdrawal helper.

**Consent pipeline integration:**

`EcContext::read_from_request()` calls `build_consent_context()` with request-local cookies, headers, settings, and geo data. Current runtime behavior does not use a separate consent KV store or consent KV fallback. Consent is interpreted from live request signals on every request; the EC identity store only keeps the minimal `KvEntry.consent` snapshot and withdrawal tombstones for S2S enforcement.

All downstream EC logic uses `allows_ec_creation(&self.consent)` for creation/forwarding decisions and `has_explicit_ec_withdrawal(&self.consent)` for cookie-expiry/tombstone decisions. No consent decoding or KV-backed gating logic is added in this epic.

### 6.2 Consent withdrawal — explicit delete path

When `allows_ec_creation(&consent)` returns `false`, Trusted Server **always**
strips EC-related response headers for that request. This covers both explicit
revocation and fail-closed cases.

Cookie expiry and tombstone writes happen only when
`has_explicit_ec_withdrawal(&consent)` returns `true` **and** the request
carried a **`ts-ec` cookie** (`cookie_was_present == true`). A user identified
only by the `X-ts-ec` request header is not subject to cookie deletion or
`tombstoning` on this path — there is no browser cookie to revoke.

1. Strip all EC response headers (synchronous — must not fail silently) whenever `!allows_ec_creation(&consent)`.
2. If `has_explicit_ec_withdrawal(&consent) && cookie_was_present == true`, issue `Set-Cookie: ts-ec=; Max-Age=0; ...`.
3. In that same explicit-withdrawal + cookie-present case, write a tombstone for each valid EC ID available (`cookie_ec_value` and/or `ec_value`). When neither is valid (malformed cookie, no header), **no tombstone is written** — cookie deletion alone is the browser-side enforcement mechanism. When at least one valid EC ID exists: `kv.write_withdrawal_tombstone(ec_id)` sets `consent.ok = false`, clears partner IDs, TTL 24h — approximately 25ms per write.

The tombstone write runs in the request path (not async) to ensure real-time enforcement for authoritative withdrawals. Using a tombstone rather than a hard delete preserves the `consent_withdrawn` signal for batch sync clients for 24 hours — otherwise batch sync cannot distinguish consent withdrawal from an EC that never existed.

If the tombstone write fails:

- Log at `error` level with EC ID
- Do not block the response — cookie deletion is the primary enforcement mechanism on explicit-withdrawal paths
- **No retry is possible on the browser path.** Once the cookie is deleted, subsequent browser requests carry no EC value (`ec_value` returns `None`), so there is no EC ID to tombstone. A failed tombstone means batch sync clients may see `ec_id_not_found` (after TTL expiry) rather than `consent_withdrawn` — this is accepted degradation.

Fail-closed / unverifiable-consent cases keep the cookie intact and do not write tombstones; they only suppress EC use on that request.

---

## 7. KV Store Identity Graph

### 7.1 Module: `ec/kv.rs`

One KV store is used for the identity graph. Its name is configured in `trusted-server.toml`:

| Store          | TOML key      | Purpose               |
| -------------- | ------------- | --------------------- |
| Identity graph | `ec.ec_store` | EC ID → identity JSON |

Partners are defined in config (`[[ec.partners]]` in TOML) and loaded into an in-memory `PartnerRegistry` at startup. There is no KV-backed partner store.

### 7.2 Identity graph schema

**KV key:** Full EC ID in `{64-char hex}.{6-char alphanumeric}` format. The random suffix is intentionally included to provide uniqueness for users behind the same NAT/proxy infrastructure who would otherwise share identical IP-derived hash prefixes.

**KV value (JSON, max ~5KB):**

```json
{
  "v": 1,
  "created": 1775162556,
  "consent": {
    "tcf": "CP...",
    "gpp": "DBA...",
    "ok": true,
    "updated": 1775162556
  },
  "geo": {
    "country": "US",
    "region": "TN",
    "asn": 7922,
    "dma": 659
  },
  "device": {
    "is_mobile": 0,
    "ja4_class": "t13d1516h2",
    "platform_class": "mac",
    "h2_fp_hash": "a3f9d21c8b04",
    "known_browser": true
  },
  "pub_properties": {
    "origin_domain": "autoblog.com",
    "seen_domains": ["autoblog.com"]
  },
  "network": {
    "cluster_size": 2
  },
  "ids": {
    "id5": { "uid": "ID5*qe8VHv..." },
    "trade_desk": { "uid": "226fb4b3-..." },
    "liveramp_ats": { "uid": "Ag2z1TDA..." }
  }
}
```

**KV metadata (max 2048 bytes, readable without streaming body):**

```json
{
  "ok": true,
  "country": "US",
  "v": 1,
  "cluster_size": 2,
  "is_mobile": 0,
  "known_browser": true
}
```

The `ok` field in metadata is a **historical consent record for S2S consumers only** — it is set to `false` by `write_withdrawal_tombstone()` so that batch sync clients (`POST /_ts/api/v1/batch-sync`) can return `consent_withdrawn` rather than `ec_id_not_found` during the 24-hour tombstone TTL.

**`consent.ok` is NOT used to make the withdrawal decision on the main request path.** Withdrawal enforcement is driven by current request-local consent: `allows_ec_creation(&ec_context.consent)` decides whether EC use and EC response headers are allowed on this request, and `has_explicit_ec_withdrawal(&ec_context.consent)` decides whether to expire the cookie and call `write_withdrawal_tombstone()` in-path (setting `ok = false`, 24h TTL — see §6.2). Engineers must not add a KV read to the consent withdrawal hot path based on this field.

**Rust types:**

```rust
pub struct KvEntry {
    pub v: u8,
    pub created: u64,
    pub consent: KvConsent,
    pub geo: KvGeo,
    /// Creation-time publisher property metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_properties: Option<KvPubProperties>,
    /// Device class signals. Written once on creation — never updated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<KvDevice>,
    /// Network cluster disambiguation. Written only by /identify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<KvNetwork>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Autonomous System Number (e.g. 7922 = Comcast).
    /// Primary signal for distinguishing home ISP vs. corporate VPN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,
    /// DMA/metro code (e.g. 807 = San Francisco).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dma: Option<i64>,
}

pub struct KvPartnerId {
    pub uid: String,
}

/// Publisher property metadata captured when an EC entry is created.
pub struct KvPubProperties {
    /// Apex domain where this EC entry was first created.
    pub origin_domain: String,
    /// Bounded set of publisher apex domains seen for this EC entry.
    /// Capped at 50 entries.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub seen_domains: BTreeSet<String>,
}

/// Coarse device signals derived from TLS handshake and UA.
/// Written once on creation — never updated after.
pub struct KvDevice {
    /// 0 = desktop, 1 = mobile, 2 = unknown (non-standard client).
    pub is_mobile: u8,
    /// JA4 Section 1 only (e.g. "t13d1516h2" = Chrome).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ja4_class: Option<String>,
    /// Coarse OS family: "mac", "windows", "ios", "android", "linux".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_class: Option<String>,
    /// SHA256 prefix (12 hex chars) of H2 SETTINGS fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_fp_hash: Option<String>,
    /// true = known browser, false = known bot, None = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_browser: Option<bool>,
}

/// Network cluster disambiguation data.
/// Written only by /identify — too expensive for organic hot path.
pub struct KvNetwork {
    /// Number of distinct EC suffixes sharing this hash prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
}

pub struct KvMetadata {
    pub ok: bool,
    pub country: String,
    pub v: u8,
    /// Mirrors KvNetwork::cluster_size. None = not yet evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
    /// Mirrors KvDevice::is_mobile. Enables propagation gating without body read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_mobile: Option<u8>,
    /// Mirrors KvDevice::known_browser. Buyer-facing quality signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_browser: Option<bool>,
}
```

All new fields use `Option` types or `serde(default)`, so existing entries
deserialize without error. No schema version bump is needed — v1 has not
shipped yet.

### 7.3 TTL

New live entries use `time_to_live_sec = 31536000` (1 year), matching the initial cookie `Max-Age`. Ordinary returning-user page views do not refresh the EC cookie and do not write the KV entry solely to extend TTL. Real data mutations (for example, a changed partner UID or first cluster-size evaluation) still write the live entry with the live-entry TTL. Withdrawal tombstones use a 24-hour TTL.

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
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn get(
        &self,
        ec_id: &str,
    ) -> Result<Option<(KvEntry, u64)>, Report<TrustedServerError>>;

    /// Reads only the metadata fields (consent flag, country).
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn get_metadata(
        &self,
        ec_id: &str,
    ) -> Result<Option<KvMetadata>, Report<TrustedServerError>>;

    /// Creates a new entry. Returns `Ok(())` if successful, `Err` if the key
    /// already exists (concurrent create) or on KV error.
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn create(
        &self,
        ec_id: &str,
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
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn create_or_revive(
        &self,
        ec_id: &str,
        entry: &KvEntry,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Atomically merges `ids[partner_id]` into the existing entry using a
    /// generation marker. Retries up to `MAX_CAS_RETRIES` (3) times on
    /// generation conflict before returning `Err`.
    ///
    /// If the key does not exist, returns `Err` and intentionally fails
    /// closed. Pull sync and Prebid EID ingestion log the rejection and retry
    /// on a later qualifying request after the organic EC creation path has
    /// materialized the root entry.
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn upsert_partner_id(
        &self,
        ec_id: &str,
        partner_id: &str,
        uid: &str,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Upserts a partner ID only when the KV entry already exists. Used by
    /// S2S batch sync. Returns `Unchanged` when the existing UID matches,
    /// avoiding a KV write. Different UIDs overwrite the stored value; mapping
    /// timestamps are not used for ordering because they are no longer stored
    /// in the EC identity entry.
    pub fn upsert_partner_id_if_exists(
        &self,
        ec_id: &str,
        partner_id: &str,
        uid: &str,
    ) -> Result<UpsertResult, Report<TrustedServerError>>;

    /// Counts the number of KV keys sharing a hash prefix via the list API.
    /// Uses a single-page list with `limit(100)`. Returns the count, or
    /// `None` if the list exceeds 100 keys (clearly a large network).
    pub fn count_hash_prefix_keys(
        &self,
        hash_prefix: &str,
    ) -> Result<Option<u32>, Report<TrustedServerError>>;

    /// Evaluates the network cluster size for an EC entry.
    ///
    /// Returns a stored `cluster_size` without a list call when present on a
    /// live entry. Tombstone entries (`consent.ok = false`) return `None`
    /// without list or write-back so their 24-hour withdrawal TTL is not
    /// extended. If missing on a live entry, calls `count_hash_prefix_keys()`
    /// and writes the result to `entry.network` via CAS. Returns the cluster
    /// size for inclusion in the `/_ts/api/v1/identify` response.
    pub fn evaluate_cluster(
        &self,
        ec_id: &str,
        entry: &KvEntry,
        generation: u64,
    ) -> Result<Option<u32>, Report<TrustedServerError>>;

    /// Writes a withdrawal tombstone for consent enforcement.
    ///
    /// Instead of hard-deleting the KV entry, this overwrites it with
    /// `consent.ok = false`, clears all partner IDs, and sets a 24-hour TTL.
    /// The tombstone allows batch sync clients (`POST /_ts/api/v1/batch-sync`) to return
    /// `consent_withdrawn` rather than `ec_id_not_found` for the tombstone TTL.
    ///
    /// After the 24-hour TTL expires, the entry is gone. Any subsequent `get()`
    /// returns `None` (`ec_id_not_found`) — the distinction is time-bounded.
    ///
    /// Caller must handle `Err` by logging at `error` level; the cookie deletion
    /// in `ec_finalize_response()` is the primary enforcement mechanism.
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn write_withdrawal_tombstone(
        &self,
        ec_id: &str,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Hard-deletes the entry. Used only for data deletion requests (IAB deletion
    /// framework — deferred). For consent withdrawal, use `write_withdrawal_tombstone()`.
    ///
    /// # Arguments
    ///
    /// * `ec_id` — The full EC ID (`{64-hex}.{6-alnum}`) used as the KV key.
    pub fn delete(&self, ec_id: &str) -> Result<(), Report<TrustedServerError>>;
}
```

`MAX_CAS_RETRIES = 5`. If all retries fail on a generation conflict, return `Err` — callers handle per-endpoint policy (§9.4 for batch sync, §8.4 for Prebid EID ingestion).

### 7.5 KV degraded behavior

| Operation                          | KV unavailable | Action                                                                                         |
| ---------------------------------- | -------------- | ---------------------------------------------------------------------------------------------- |
| EC cookie creation                 | KV error       | Set cookie. Skip KV create. Log `warn`.                                                        |
| Prebid EID ingestion KV write      | KV error       | Skip write. Log `warn`. Retry on next qualifying request.                                      |
| `/_ts/api/v1/identify` KV read     | KV error       | Return `200` with `ec` set, `degraded: true`, empty `uid`/`eid`.                               |
| `POST /_ts/api/v1/batch-sync`      | KV error       | Return `207` with all mappings rejected, `reason: "kv_unavailable"`.                           |
| Pull sync KV write                 | KV error       | Discard uid. Log `warn`. Retry on next qualifying request.                                     |
| Consent withdrawal tombstone write | KV error       | Delete cookie (primary enforcement). Log `error`. Next request: no cookie → no EC regenerated. |

---

## 7A. Device Signals and Bot Gate

### 7A.1 Overview

Device signals provide coarse, non-PII browser classification derived from
the TLS handshake and User-Agent header at the Fastly edge. They serve two
purposes:

1. **Bot gate** — block all KV identity operations for unrecognized clients
   (bots, scrapers, non-standard HTTP clients). The request is still proxied
   to the publisher origin normally — the bot receives valid HTML but leaves
   no trace in the identity graph.
2. **Device class record** — store a write-once `KvDevice` on each EC entry
   for future cross-browser propagation decisions and buyer-facing device
   quality scoring.

All signal derivation is pure in-memory computation — no KV I/O. It runs on
every request before EC context creation.

### 7A.2 Signal derivation

No Client Hints are used — JA4 and UA platform parsing provide equivalent or
superior signal for every browser including Safari and Firefox, which do not
send Client Hints.

**`is_mobile`** — derived in priority order:

| Condition                                      | Value                                                                      |
| ---------------------------------------------- | -------------------------------------------------------------------------- |
| UA contains `iPhone`, `iPad`, or `Android`     | `1` — confirmed mobile                                                     |
| UA contains `Macintosh`, `Windows`, or `Linux` | `0` — confirmed desktop                                                    |
| Neither pattern matches                        | `2` — genuinely unknown (rare; typically bots or heavily hardened clients) |

Note: `is_mobile: 2` in practice signals a non-standard client rather than
Safari, since Safari always produces a recognizable UA platform string.

**`platform_class`** — coarse OS family parsed from UA (checked in order):

| UA segment                         | `platform_class` |
| ---------------------------------- | ---------------- |
| `iPhone` or `iPad`                 | `ios`            |
| `Android` (checked before `Linux`) | `android`        |
| `Macintosh`                        | `mac`            |
| `Windows NT`                       | `windows`        |
| `Linux` (non-Android)              | `linux`          |
| No match                           | `None`           |

**`ja4_class`** — Section 1 of the JA4 fingerprint only (e.g. `t13d1516h2`).
Available via `req.get_tls_ja4()` in the Fastly Compute Rust SDK. The full
JA4 format is `section1_section2_section3` separated by underscores; we split
on `_` and take `[0]`. Section 1 identifies browser family (cipher count,
extension count, ALPN) without uniquely fingerprinting a device. The full JA4
is never stored.

**`h2_fp_hash`** — first 12 hex characters of SHA256 of the raw HTTP/2
SETTINGS fingerprint string, available via `req.get_client_h2_fingerprint()`.
Used alongside `ja4_class` to confirm browser family and detect bots.

**`known_browser`** — set `true` when `ja4_class` + `h2_fp_hash` match a
known legitimate browser pattern from the allowlist below. `None` when
unknown. Both signals must be present for a match — if either is `None`,
returns `None`.

### 7A.3 Known browser fingerprint allowlist

Empirically derived from Fastly Compute production responses (2026-04-03):

| Browser                             | `ja4_class`  | `h2_fp` raw string               | `known_browser` |
| ----------------------------------- | ------------ | -------------------------------- | --------------- |
| Chrome/Mac (v146)                   | `t13d1516h2` | `1:65536;2:0;4:6291456;6:262144` | `true`          |
| Safari/Mac (v26) + Safari/iOS (v26) | `t13d2013h2` | `2:0;3:100;4:2097152`            | `true`          |
| Firefox/Mac (v149)                  | `t13d1717h2` | `1:65536;2:0;4:131072;5:16384`   | `true`          |

Safari Mac and Safari iOS share identical TLS/H2 stacks — distinguished only
by `platform_class` (`mac` vs `ios`) and `is_mobile` (`0` vs `1`).

This allowlist will expand as new browser versions are observed in production.
Entries not matching any allowlist row get `known_browser: None` (not `false`)
unless they match a confirmed bot pattern.

The allowlist comparison works by hashing the known raw H2 SETTINGS strings
at evaluation time and comparing against the request's `h2_fp_hash`. The list
is small (3 entries) so the cost is negligible.

### 7A.4 Bot gate behavior

The bot gate checks for **signal presence** rather than matching against a
hardcoded fingerprint allowlist. Real browsers always produce a valid TLS
fingerprint (`ja4_class`) and a recognizable UA platform string
(`platform_class`). Raw HTTP clients (curl, Python requests, Go net/http,
headless scrapers) typically lack one or both.

The gate uses `DeviceSignals::looks_like_browser()`:

```rust
pub fn looks_like_browser(&self) -> bool {
    self.ja4_class.is_some() && self.platform_class.is_some()
}
```

| Condition                                        | EC operations | Example                          |
| ------------------------------------------------ | ------------- | -------------------------------- |
| `ja4_class` present AND `platform_class` present | **Allowed**   | Any real browser on any OS       |
| `ja4_class` missing OR `platform_class` missing  | **Blocked**   | curl, Python requests, Googlebot |

`known_browser` (the fingerprint allowlist match) is still computed and stored
on `KvDevice` for analytics and future buyer-facing quality scoring, but it
does **not** gate identity operations. This avoids blocking legitimate browsers
whose JA4/H2 fingerprints are not yet in the allowlist.

**Implementation in the Fastly adapter:**

1. After `GeoInfo::from_request()`, call `derive_device_signals(req)` which
   reads `User-Agent`, `req.get_tls_ja4()`, and
   `req.get_client_h2_fingerprint()`.
2. If `!looks_like_browser()`:
   - `kv_graph` is set to `None` (suppresses all KV reads and writes)
   - `ec_finalize_response()` is skipped (no cookie set/deleted)
   - Pull sync is skipped
   - The request proceeds through normal routing — organic requests are
     proxied to publisher origin, API endpoints respond normally (but
     without EC identity data)
3. If `looks_like_browser()`: proceed normally. Device signals are set
   on `EcContext` via `set_device_signals()` so they flow through to
   `KvEntry` creation.

**Current bot response:** the request is served normally (proxied to origin)
without any KV operations or cookie writes. The bot receives a valid HTML
response but leaves no trace in the identity graph.

### 7A.5 `DeviceSignals` struct

```rust
/// Device signals derived from a single request.
/// Computed in the Fastly adapter from raw TLS/H2/UA data.
pub struct DeviceSignals {
    pub is_mobile: u8,
    pub ja4_class: Option<String>,
    pub platform_class: Option<String>,
    pub h2_fp_hash: Option<String>,
    pub known_browser: Option<bool>,
}

impl DeviceSignals {
    /// Derives all device signals from raw request data.
    pub fn derive(ua: &str, ja4: Option<&str>, h2_fp: Option<&str>) -> Self;

    /// Returns true when ja4_class and platform_class are both present.
    /// Used by the bot gate — see §7A.4.
    pub fn looks_like_browser(&self) -> bool;

    /// Converts to KvDevice for KV storage.
    pub fn to_kv_device(&self) -> KvDevice;
}
```

### 7A.6 `KvDevice` write policy

`KvDevice` is written to `KvEntry.device` only during `generate_if_needed()`
(new EC creation). It is never updated after creation — device signals are a
first-seen record of how this EC entry was established.

Existing entries (created before device signals were implemented) will have
`device: None`. Downstream consumers must handle `None` as "pre-device-signals
entry" rather than "unknown device."

### 7A.7 Publisher property metadata (`KvPubProperties`)

`KvPubProperties` records the publisher domain where the EC entry was created.
Earlier drafts treated `seen_domains` as mutable domain history, but the current
implementation avoids recurring organic-request KV writes. New entries seed only
the creation domain and runtime requests do not append domains. Legacy
map-shaped records with per-domain visit objects are accepted on read and
reserialized as a domain list on future writes.

```rust
pub struct KvPubProperties {
    pub origin_domain: String,
    pub seen_domains: BTreeSet<String>,
}
```

**Written:** on `KvEntry::new()` / `create_or_revive()` for the creation domain
only. Ordinary returning-user requests do not update this structure.

**Cap:** `seen_domains` sets are capped at 50 entries (`MAX_SEEN_DOMAINS`)
during validation so old or malformed records cannot grow unbounded.

### 7A.8 Network cluster disambiguation (`KvNetwork`)

Tracks how many distinct EC entries share the same hash prefix. A high count
indicates a shared network (corporate VPN, campus); a low count indicates an
individual or household.

```rust
pub struct KvNetwork {
    pub cluster_size: Option<u32>,
}
```

**Written:** only by the `/_ts/api/v1/identify` endpoint, never on the organic proxy path.
The prefix-match list API call required to compute `cluster_size` is too
expensive for the hot path.

**Evaluation:** `evaluate_cluster()` on `KvIdentityGraph`:

- Returns the stored `cluster_size` without a prefix-list call when present
- If `cluster_size` is missing, calls `count_hash_prefix_keys()` with `limit(100)` — a single list-page call
- Writes the computed result to `entry.network` via best-effort CAS
- `cluster_recheck_secs` is retained only as a legacy compatibility setting because no cluster-check timestamp is stored in the EC identity entry

**Threshold guidance:**

| Cluster size | Likely scenario                           |
| ------------ | ----------------------------------------- |
| 1–3          | Individual / household                    |
| 4–10         | Small shared space (family, small office) |
| 11–50        | Medium office, hotel, coworking           |
| 50+          | Corporate VPN, university, campus         |

**Default trust threshold:** entries with `cluster_size <= 10` are treated as
individual users for identity resolution purposes. Configurable per publisher
via `trusted-server.toml`:

```toml
[ec]
cluster_trust_threshold = 10  # default
# cluster_recheck_secs is legacy compatibility; cluster_size is computed once per entry
```

### 7A.9 Geo extensions (`KvGeo`)

`KvGeo` is extended with two non-PII network signals available from Fastly's
`geo_lookup()` on the client IP:

- **`asn: Option<u32>`** — Autonomous System Number (e.g. `7922` = Comcast).
  Primary signal for distinguishing home ISP vs. corporate VPN. Populated from
  `GeoInfo::asn` which reads `fastly::geo::Geo::as_number()`. A value of `0`
  from the Fastly API is mapped to `None`.
- **`dma: Option<i64>`** — DMA/metro code (e.g. `807` = San Francisco).
  Market-level targeting signal; not personal data. Populated from
  `GeoInfo::metro_code` when non-zero.

Both fields are written on initial `KvEntry::new()` from `GeoInfo`. Never
updated after creation — geo is a first-seen signal, not a real-time one.

### 7A.10 IP address storage policy

Raw IP addresses are personal data under GDPR (CJEU _Breyer v. Germany_, 2016)
and must not be stored in KV entries. The EC hash already derives from the IP
without persisting it.

Permitted IP-derived signals (written at creation time):

- `geo.country` — ISO 3166-1 alpha-2
- `geo.region` — ISO 3166-2 subdivision
- `geo.asn` — ASN number (network identifier, not personal data)
- `geo.dma` — DMA/metro code (market identifier, not personal data)

### 7A.11 Privacy rationale

`ja4_class` (Section 1 only) and `platform_class` are category signals, not
unique device identifiers. They are equivalent in precision to `geo.country`
— they identify a class of client, not an individual. The full JA4 fingerprint
(Sections 2 and 3) is never stored, as it approaches unique device
identification and would require explicit consent basis under GDPR Art. 4(1).

---

## 8. Prebid EID Cookie Ingestion

> **Note:** The pixel sync endpoint (`GET /_ts/api/v1/sync`) has been removed. Partner ID sync from the browser is now handled via the Prebid EID cookie, which is written client-side by the TSJS Prebid integration and ingested server-side in `ec_finalize_response()`.

### 8.1 Module: `ec/prebid_eids.rs`

```rust
/// Parses a `ts-eids` cookie value and writes matched partner UIDs to KV.
///
/// Best-effort: all errors are logged and swallowed so the main request
/// path is never affected.
pub fn ingest_prebid_eids(
    cookie_value: &str,
    ec_id: &str,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
);
```

### 8.2 Cookie format

| Attribute  | Value                                                                                        |
| ---------- | -------------------------------------------------------------------------------------------- |
| Name       | `ts-eids`                                                                                    |
| Format     | Base64-encoded (standard RFC 4648) JSON array of OpenRTB-style EIDs (`{source, uids:[...]}`) |
| Max size   | JS writer targets 3 KB; backend parser accepts up to 8 KiB raw cookie length                 |
| Written by | TSJS Prebid integration (client-side JS)                                                     |
| Read by    | `ec_finalize_response()` (server-side, via `ingest_prebid_eids()`)                           |

**Example decoded value:**

```json
[
  {
    "source": "uidapi.com",
    "uids": [{ "id": "A4A...", "atype": 3 }]
  },
  {
    "source": "liveramp.com",
    "uids": [{ "id": "LR_xyz", "atype": 3 }]
  }
]
```

### 8.3 JS side

The TSJS Prebid integration calls `pbjs.getUserIdsAsEids()` in the `bidsBackHandler` callback after each auction. The returned OpenRTB-style EID array is base64-encoded and written to the `ts-eids` cookie. This runs entirely client-side — no server round-trip is needed for the write. Current writers preserve the full `{source, uids:[...]}` shape; the backend remains backward-compatible with the earlier flattened `{source, id, atype}` payload during rollout.

### 8.4 Backend side

`ingest_prebid_eids()` is called from `ec_finalize_response()` on both returning-user and new-EC paths when a `ts-eids` cookie is present and consent is granted. The flow:

1. Base64-decode the cookie value.
2. JSON-parse into OpenRTB-style `Eid` entries; if that parse fails, fall back to the earlier flattened `{source, id, atype}` payload for backward compatibility.
3. For each EID entry:
   a. Look up `registry.find_by_source_domain(&eid.source)`. Skip if no match.
   b. Find the first non-empty UID in `eid.uids`. Skip the source if none is present.
   c. Skip oversized UID values.
   d. Call `kv.upsert_partner_id(ec_id, &partner.id, &uid.id)`. The upsert skips the KV write when the stored UID already matches.
4. All errors are logged and swallowed — EID ingestion never blocks the response.

### 8.5 Source domain matching

Source domains are matched via `PartnerRegistry.find_by_source_domain()`, which performs a case-insensitive lookup against the `source_domain` field configured on each partner in `[[ec.partners]]`. The registry builds a `by_source_domain` HashMap at startup for O(1) lookups.

### 8.6 Write suppression

EC identity entries no longer store per-partner sync timestamps. Instead of a
time-based debounce, `upsert_partner_id()` skips the KV write when the stored UID
already matches the incoming UID. Different UIDs replace the stored value.

---

## 9. S2S Batch Sync API (`POST /_ts/api/v1/batch-sync`)

### 9.1 Module: `ec/batch_sync.rs`

```rust
pub fn handle_batch_sync(
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    rate_limiter: &dyn RateLimiter,
    req: Request,
) -> Result<Response, Report<TrustedServerError>>;
```

### 9.2 Authentication

`Authorization: Bearer <api_token>` header required. Auth flow:

1. Compute `sha256_hex(api_token)`.
2. Look up `registry.find_by_api_key_hash(hash)` — the `PartnerRegistry` maintains a `by_api_key_hash` HashMap built at startup from `[[ec.partners]]` config for O(1) lookup.
3. If no match → `401 Unauthorized` with no body processing.

Key rotation requires updating the `api_token` in `[[ec.partners]]` TOML and redeploying.

### 9.2.1 API-key rate limiting

After successful auth, check the API-key level rate limit: `partner.batch_rate_limit` requests per partner per minute (default 60). Uses Fastly's Edge Rate Limiting API (§14.3), with key `batch:{partner_id}`.

Exceeded → `429 Too Many Requests` with body `{ "error": "rate_limit_exceeded" }`. No mappings are processed.

### 9.3 Request format

```
POST /_ts/api/v1/batch-sync
Content-Type: application/json
Authorization: Bearer <api_key>

{
  "mappings": [
    {
      "ec_id": "<full EC ID: {64-hex}.{6-alnum}>",
      "partner_uid": "abc123",
      "timestamp": 1741824000
    }
  ]
}
```

Maximum batch size: 1000 mappings. Requests exceeding this receive `400 Bad Request`.

### 9.4 Processing

The authenticated partner's ID (from the `PartnerConfig` resolved via API key hash in §9.2) determines the `ids[partner_id]` namespace for all writes in this batch. A partner can only write to their own namespace.

For each mapping:

1. Validate `ec_id` format (must match `{64-hex}.{6-alnum}` pattern). Invalid format → reject with `reason: "invalid_ec_id"`.
2. Read KV metadata for `ec_id`. If not found → reject with `reason: "ec_id_not_found"`. If `consent.ok = false` → reject with `reason: "consent_withdrawn"`.
3. `kv.upsert_partner_id_if_exists(ec_id, partner_id, partner_uid)`. Mapping `timestamp` is retained for API compatibility but is not used for ordering. The upsert skips the write if the existing UID already matches (counted as accepted). A different UID overwrites the stored value. On KV failure → reject all remaining mappings with `reason: "kv_unavailable"`, return `207`.

### 9.5 Response format

```json
{
  "accepted": 998,
  "rejected": 2,
  "errors": [
    { "index": 45, "reason": "ec_id_not_found" },
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
    #[display("invalid_ec_id")]
    InvalidEcId,
    #[display("ec_id_not_found")]
    EcIdNotFound,
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
        partners: &[&PartnerConfig],
        kv: &KvIdentityGraph,
    );
}

/// Fires a single partner pull request via `send_async()`, waits for the
/// response via `PendingRequest::wait()`, and writes the result to KV.
fn pull_one_partner(
    ec_id: &str,
    ip: IpAddr,
    partner: &PartnerConfig,
    kv: &KvIdentityGraph,
);
```

### 10.2 Trigger conditions

A pull sync is dispatched for a partner when all of the following are true on a request:

1. The request was routed to an **organic handler** (`handle_publisher_request` or `integration_registry.handle_proxy`). Pull sync never fires on EC route handlers (`/_ts/api/v1/identify`, `/_ts/api/v1/batch-sync`) or `/auction`.
2. A valid EC is present (`ec_context.ec_hash().is_some()`). This includes an EC
   newly generated on the current organic request — pull sync may run immediately
   after first-page EC creation because the response cookie is flushed before the
   background dispatch starts.
3. `allows_ec_creation(&ec_context.consent) == true`
4. `partner.pull_sync_enabled == true`
5. The partner UID is missing from the KV graph. If `ids[partner_id]` is already present, pull sync is skipped.
6. Rate limit not exceeded: `partner.pull_sync_rate_limit` calls per EC ID per partner per hour (default 10)

`partner.pull_sync_ttl_sec` is retained for configuration compatibility, but is not used by the current fill-missing-only behavior because EC entries no longer store per-partner sync timestamps.

### 10.3 Execution model

Pull calls are dispatched using Fastly's background task / `send_async` model after the response is flushed. They do not add latency to the user-facing request.

Maximum concurrent pull calls per request: `settings.ec.pull_sync_concurrency` (default 3).

**Architectural divergence from PRD:** The PRD describes excess partner calls being queued and dispatched on subsequent requests for the same user. A persistent queue is not implementable in the stateless Fastly WASM edge environment — there is no cross-request mutable state. This spec adapts the intent using a stateless rotating offset: sort qualifying partners by ID, then use `(unix_timestamp_secs / 3600) % partner_count` as the starting index (wrapping). This ensures different missing partners are prioritized across requests without persisted queue state. Once a partner UID is stored, that partner is no longer eligible for pull sync under the current fill-missing-only behavior.

### 10.4 Outbound request

```
GET {partner.pull_sync_url}?ec_id={64-hex}.{6-alnum}
Authorization: Bearer {partner.ts_pull_token}
```

Before dispatching, `pull_sync.rs` validates that `pull_sync_url`'s hostname is present in `partner.pull_sync_allowed_domains`. If not, the call is skipped and an `error` is logged — this is a configuration error that should not occur at runtime if startup validation in `PartnerRegistry::from_config()` is working correctly.

Only the full EC ID is sent. No client IP, consent strings, geo data, or other partner IDs are included.

**Expected partner responses:**

```json
{ "uid": "abc123" }   // resolved
{ "uid": null }       // not recognized
```

Or `404 Not Found`. Both null and 404 are no-ops — no KV write, no error logged above `debug`.

Any other non-200 response is treated as a transient failure. No retry. The next qualifying request triggers a new attempt.

### 10.5 KV write on success

On a non-null `uid`: call `kv.upsert_partner_id(ec_id, partner_id, uid)`. If the root entry is missing, the upsert fails closed; pull sync logs `warn` and discards the result. If the same UID is already stored, the upsert skips the KV write. On KV failure: log `warn` and discard the result. Retry occurs on the next qualifying request while the partner UID remains missing.

---

## 11. Identity Resolution Endpoint (`GET /_ts/api/v1/identify`)

### 11.1 Module: `ec/identify.rs`

```rust
pub fn handle_identify(
    settings: &Settings,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    req: &Request,
    ec_context: &EcContext,
) -> Result<Response, Report<TrustedServerError>>;
```

### 11.2 Authentication

**Bearer token required.** The `Authorization: Bearer <api_token>` header identifies the requesting partner. Auth flow:

1. Parse the Bearer token from the `Authorization` header.
2. Compute `sha256_hex(api_token)`.
3. Look up `registry.find_by_api_key_hash(hash)` — O(1) in-memory lookup.
4. If no match → `401 Unauthorized` with `{ "error": "invalid_token" }`.

The authenticated partner determines which UID is returned — each partner sees only their own synced UID for the given EC, not all partners' UIDs.

### 11.2.1 Call patterns

**Browser-direct:** The browser sends the request to `ec.publisher.com/_ts/api/v1/identify` with the partner's API token in the `Authorization` header. Cookies (including `ts-ec` and consent cookies) are sent automatically (same-site).

**Server-side proxy:** The publisher's origin server must forward:

| Header                                                    | Required                               |
| --------------------------------------------------------- | -------------------------------------- |
| `Authorization: Bearer <api_token>`                       | Yes                                    |
| `Cookie: ts-ec=<value>` or `X-ts-ec: <value>`             | Yes                                    |
| `Cookie: euconsent-v2=<value>` or `Cookie: __gpp=<value>` | Yes for EU/UK/US users                 |
| `X-consent-advertising: <value>`                          | Optional — takes precedence if present |

### 11.3 EC and consent handling

`/_ts/api/v1/identify` follows `EcContext` retrieval priority (Section 4.2). It does **not**
generate a new EC, and the handler itself does not write cookies. After the
handler, `ec_finalize_response()` may still delete the EC cookie on consent
withdrawal. Ordinary returning-user responses set the `x-ts-ec` header only;
they do not refresh or repair the browser cookie.

Consent is evaluated using the same logic as Section 6.

### 11.4 Response

**`401 Unauthorized` — missing or invalid Bearer token:**

```json
{ "error": "invalid_token" }
```

This is checked first, before consent or EC presence.

**`200 OK` — EC present, consent granted, partner UID resolved:**

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": false,
  "partner_id": "liveramp",
  "uid": "LR_xyz",
  "eid": { "source": "liveramp.com", "uids": [{ "id": "LR_xyz", "atype": 3 }] },
  "cluster_size": 2
}
```

The response is scoped to the requesting partner only. `partner_id` identifies which partner was authenticated. `uid` is the partner's resolved UID for this EC. `eid` is the OpenRTB 2.6 EID object for this partner. `cluster_size` is included when the network cluster has been evaluated (see §7A.8); absent when not yet evaluated.

**`200 OK` — EC present, consent granted, no UID for this partner:**

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": false,
  "partner_id": "liveramp",
  "cluster_size": null
}
```

`uid` and `eid` are omitted when the partner has no synced UID for this EC.

**`200 OK` — KV unavailable (degraded):**

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": true,
  "partner_id": "liveramp",
  "cluster_size": null
}
```

**`200 OK` — EC present, KV entry missing (no synced partners yet):**

This case occurs by design when `create_or_revive()` fails on EC generation (best-effort) or when the EC was just created and no partners have synced yet. It is not an error — the EC is valid, just has no partner data.

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": false,
  "partner_id": "liveramp",
  "cluster_size": null
}
```

Note: `degraded` is `false` because the KV read succeeded (it returned `None`, meaning no entry exists). `degraded: true` is reserved for KV read errors where the entry might exist but couldn't be retrieved.

**`403 Forbidden` — consent denied (regardless of EC presence):**

```json
{ "consent": "denied" }
```

Consent is evaluated **after** auth but **before** EC presence. If `!allows_ec_creation(&consent)`, return `403` immediately — do not fall through to the `204` branch. This ensures consent denial is always surfaced, even for users with no EC.

**`204 No Content` — no EC present, consent not denied.** No body.

### 11.5 Response headers (supplementary)

Set on `200` responses only:

| Header    | Value                             |
| --------- | --------------------------------- |
| `X-ts-ec` | `{64-hex}.{6-alnum}` — full EC ID |

The JSON body is the primary contract. The `X-ts-ec` header is supplementary for proxy-layer consumers.

### 11.6 Performance target

`/_ts/api/v1/identify` must respond within 30ms (excluding network latency) when EC is present and KV read succeeds. This requires the KV read to be on the fast path with no retries.

CORS headers must be set to allow browser-direct calls from the publisher's page. The `Access-Control-Allow-Origin` header is dynamically reflected from the `Origin` request header if the origin is an exact match or a subdomain of `settings.publisher.domain`:

```
// e.g. publisher.domain = "example.com"
// Allowed: https://example.com, https://www.example.com, https://news.example.com
// Rejected: https://evil.com, https://notexample.com

Access-Control-Allow-Origin: <reflected Origin>
Access-Control-Allow-Credentials: true
Access-Control-Allow-Methods: GET, OPTIONS
Access-Control-Allow-Headers: Authorization, X-ts-ec
Access-Control-Max-Age: 600
Vary: Origin
```

**Origin validation logic:** CORS headers are only relevant when the `Origin` request header is present (browser requests always send it; server-side proxy calls typically do not).

- **No `Origin` header present:** Process normally. No CORS headers added. No `403`. This is the server-side proxy path from §11.2.1 — origin-server calls forwarding `Cookie`, consent headers, and `Authorization`.
- **`Origin` header present, hostname matches `publisher.domain` or ends with `.{publisher.domain}` and scheme is `https`:** Reflect origin in `Access-Control-Allow-Origin`. Add `Vary: Origin`.
- **`Origin` header present but does not match:** Return `403`. No body.

Browser `fetch()` with `credentials: "include"` sends an `OPTIONS` preflight. The router handles `OPTIONS /_ts/api/v1/identify` identically — returns `200 OK` with the CORS headers above and no body.

---

## 12. Bidstream Decoration (`/auction` Mode B)

### 12.1 Changes to existing auction path

The auction handler (`crates/trusted-server-core/src/auction/`) is modified to inject EC identity into outbound OpenRTB requests. This is **not** a builder tweak — it requires explicit schema additions across multiple files. SyntheticID is fully removed from the auction path — no fallback, no `X-Synthetic-*` headers, no `get_or_generate_synthetic_id()`.

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
            Some((entry, _gen)) => build_eids_from_kv(&entry, &registry),
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
| `X-ts-ec`             | `{64-hex}.{6-alnum}` — full EC ID, when EC is present                                                              |
| `X-ts-eids`           | Standard base64 (RFC 4648) of OpenRTB 2.6 `user.eids` JSON array. Capped at 4 KB — same truncation rules as §11.5. |
| `X-ts-eids-truncated` | `true` — present only when `X-ts-eids` was truncated                                                               |
| `X-ts-ec-consent`     | `ok` — only present when consent granted; on withdrawal `ec_finalize_response()` strips all EC headers             |

**Deferred:** A future server-to-server winner-notification delivery step to a publisher ad server is not in scope for this iteration. See §1 deferred items.

---

## 13. Partner Registry (Config-Based)

### 13.1 Overview

Partners are defined in `[[ec.partners]]` TOML configuration and loaded into an in-memory `PartnerRegistry` at startup. There is no KV-backed partner store and no admin registration endpoint. Partner changes require a config update and redeployment.

### 13.2 Module: `ec/partner.rs`

Contains only validation helpers and API key hashing. The full partner data model and registry live in `ec/registry.rs`.

```rust
/// Validates a partner ID format and checks against reserved names.
///
/// # Errors
///
/// Returns a descriptive error string on validation failure.
pub fn validate_partner_id(id: &str) -> Result<(), String>;
// Must match `^[a-z0-9_-]{1,32}$`. Reserved names rejected:
// `ec`, `eids`, `ec-consent`, `eids-truncated`, `synthetic`, `ts`, `version`, `env`.

/// Computes the SHA-256 hex digest of an API key.
pub fn hash_api_key(api_key: &str) -> String;
```

### 13.3 Module: `ec/registry.rs`

```rust
/// Runtime-ready partner configuration with precomputed API key hash.
#[derive(Debug, Clone)]
pub struct PartnerConfig {
    pub id: String,
    pub name: String,
    pub source_domain: String,
    pub openrtb_atype: u8,
    pub bidstream_enabled: bool,
    pub api_key_hash: String,           // SHA-256 hex, precomputed at startup
    pub batch_rate_limit: u32,          // requests per partner per minute (default 60)
    pub pull_sync_enabled: bool,
    pub pull_sync_url: Option<String>,
    pub pull_sync_allowed_domains: Vec<String>,
    pub pull_sync_ttl_sec: u64,         // default 86400
    pub pull_sync_rate_limit: u32,      // default 10
    pub ts_pull_token: Option<String>,  // outbound bearer token for pull sync
}

/// In-memory partner registry with O(1) lookups by ID, API key hash,
/// and source domain.
///
/// Built once at startup from `[[ec.partners]]` in `trusted-server.toml`.
/// All validation happens during construction.
pub struct PartnerRegistry {
    by_id: HashMap<String, PartnerConfig>,
    by_api_key_hash: HashMap<String, String>,
    by_source_domain: HashMap<String, String>,
}

impl PartnerRegistry {
    /// Builds a registry from the config-defined partner list.
    ///
    /// # Errors
    ///
    /// Returns `TrustedServerError::Configuration` if any partner has an
    /// invalid ID, duplicate ID, duplicate API token hash, duplicate source
    /// domain, or invalid pull sync configuration.
    pub fn from_config(partners: &[EcPartner]) -> Result<Self, Report<TrustedServerError>>;

    /// Returns an empty registry (no partners configured).
    pub fn empty() -> Self;

    /// Looks up a partner by ID.
    pub fn get(&self, partner_id: &str) -> Option<&PartnerConfig>;

    /// Looks up a partner by the SHA-256 hex hash of their API token.
    pub fn find_by_api_key_hash(&self, hash: &str) -> Option<&PartnerConfig>;

    /// Looks up a partner by their `source_domain` (case-insensitive).
    /// Used by Prebid EID ingestion to match EID sources to partners.
    pub fn find_by_source_domain(&self, domain: &str) -> Option<&PartnerConfig>;

    /// Returns all partners with `pull_sync_enabled = true`.
    pub fn pull_enabled_partners(&self) -> Vec<&PartnerConfig>;

    /// Returns an iterator over all configured partners.
    pub fn all(&self) -> impl Iterator<Item = &PartnerConfig>;

    /// Returns the number of configured partners.
    pub fn len(&self) -> usize;

    /// Returns true if no partners are configured.
    pub fn is_empty(&self) -> bool;
}
```

### 13.4 TOML configuration

Partners are defined in `trusted-server.toml` as `[[ec.partners]]` array entries:

```toml
[[ec.partners]]
id = "liveramp"
name = "LiveRamp ATS"
source_domain = "liveramp.com"
openrtb_atype = 3
bidstream_enabled = true
api_token = "partner-api-token-here"
batch_rate_limit = 60
pull_sync_enabled = true
pull_sync_url = "https://api.liveramp.com/resolve"
pull_sync_allowed_domains = ["api.liveramp.com"]
pull_sync_ttl_sec = 86400
pull_sync_rate_limit = 10
ts_pull_token = "outbound-bearer-token"

[[ec.partners]]
id = "uid2"
name = "UID 2.0"
source_domain = "uidapi.com"
openrtb_atype = 3
bidstream_enabled = true
api_token = "uid2-api-token"
batch_rate_limit = 60
```

### 13.5 Startup validation

`PartnerRegistry::from_config()` validates during construction:

1. Each partner ID matches `^[a-z0-9_-]{1,32}$` and is not reserved.
2. No duplicate partner IDs.
3. No duplicate API token hashes (collision detection).
4. No duplicate source domains.
5. Rate limits are within valid bounds.
6. If `pull_sync_enabled`, both `pull_sync_url` and `ts_pull_token` must be present.
7. If `pull_sync_url` is set, its hostname must be in `pull_sync_allowed_domains`.

Any validation failure causes a startup error (`TrustedServerError::Configuration`).

---

## 14. Configuration

### 14.1 `Ec` settings struct

Added to `crates/trusted-server-core/src/settings.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct Ec {
    /// Publisher passphrase used as HMAC key for EC generation.
    /// Must be identical across all of the publisher's owned domains.
    /// Publishers sharing this value with partners form an identity-federated consortium.
    #[validate(custom(function = Ec::validate_passphrase))]
    pub passphrase: Redacted<String>,

    /// Fastly KV store name for the EC identity graph.
    #[serde(default)]
    pub ec_store: Option<String>,

    /// Maximum concurrent pull sync calls dispatched per request.
    #[serde(default = "Ec::default_pull_sync_concurrency")]
    pub pull_sync_concurrency: usize,

    /// Network cluster trust threshold. Entries with `cluster_size <= threshold`
    /// are treated as individual users for identity resolution purposes.
    /// B2B publishers should raise this to 50+ for office-heavy audiences.
    #[serde(default = "Ec::default_cluster_trust_threshold")]
    pub cluster_trust_threshold: u32,

    /// Seconds between cluster size re-evaluations per entry.
    /// Avoids repeated list-prefix API calls on every /identify request.
    #[serde(default = "Ec::default_cluster_recheck_secs")]
    pub cluster_recheck_secs: u64,

    /// Partners (SSPs, DSPs, identity vendors) for EC identity sync.
    #[serde(default)]
    pub partners: Vec<EcPartner>,
}

impl Ec {
    fn validate_passphrase(passphrase: &str) -> Result<(), ValidationError>;
    // Rejects known placeholder values as non-production passphrases.

    fn default_pull_sync_concurrency() -> usize { 3 }
    fn default_cluster_trust_threshold() -> u32 { 10 }
    fn default_cluster_recheck_secs() -> u64 { 3600 }
}
```

The `EcPartner` struct (see §13.4 for TOML format):

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EcPartner {
    pub id: String,
    pub name: String,
    pub source_domain: String,
    #[serde(default = "EcPartner::default_openrtb_atype")]
    pub openrtb_atype: u8,                     // default 3
    #[serde(default)]
    pub bidstream_enabled: bool,
    pub api_token: Redacted<String>,           // hashed at startup
    #[serde(default = "EcPartner::default_batch_rate_limit")]
    pub batch_rate_limit: u32,                 // default 60
    #[serde(default)]
    pub pull_sync_enabled: bool,
    #[serde(default)]
    pub pull_sync_url: Option<String>,
    #[serde(default)]
    pub pull_sync_allowed_domains: Vec<String>,
    #[serde(default = "EcPartner::default_pull_sync_ttl_sec")]
    pub pull_sync_ttl_sec: u64,                // default 86400
    #[serde(default = "EcPartner::default_pull_sync_rate_limit")]
    pub pull_sync_rate_limit: u32,             // default 10
    #[serde(default)]
    pub ts_pull_token: Option<Redacted<String>>,
}
```

Added to `Settings`:

```rust
pub struct Settings {
    // ... existing fields ...
    #[validate(nested)]
    pub ec: Ec,  // Required — omitting [ec] is a startup error
}
```

`Ec` does not derive `Default` — omitting the `[ec]` section from TOML is a deserialization error at startup. This is intentional: `passphrase` has no safe default. The `#[validate(nested)]` attribute ensures `Ec::validate_passphrase()` runs when `settings.validate()` is called at startup, matching the pattern used by `Publisher` and `Rewrite` in the existing `Settings` struct.

### 14.2 TOML configuration example

```toml
[ec]
passphrase = "publisher-chosen-secret"
ec_store = "ec_identity_store"
pull_sync_concurrency = 3
# cluster_trust_threshold = 10  # raise to 50+ for B2B publishers
# cluster_recheck_secs = 3600   # legacy compatibility; cluster_size is computed once per entry

[[ec.partners]]
id = "liveramp"
name = "LiveRamp ATS"
source_domain = "liveramp.com"
api_token = "partner-api-token-here"
bidstream_enabled = true
batch_rate_limit = 60
pull_sync_enabled = true
pull_sync_url = "https://api.liveramp.com/resolve"
pull_sync_allowed_domains = ["api.liveramp.com"]
ts_pull_token = "outbound-bearer-token"

[[ec.partners]]
id = "uid2"
name = "UID 2.0"
source_domain = "uidapi.com"
api_token = "uid2-api-token"
bidstream_enabled = true
```

### 14.3 Rate Limit Storage

Batch sync and pull sync rate limits cannot use in-memory state in a WASM/Fastly Compute environment — there is no shared memory across requests.

**Implementation:** Use Fastly's Edge Rate Limiting API (`fastly::erl::RateCounter`), which provides distributed per-key counting without KV latency and is designed for high-frequency counting without per-key write limits. The `RateLimiter` trait abstracts this for testability.

| Counter    | Key format                    | Window   |
| ---------- | ----------------------------- | -------- |
| Batch sync | `batch:{partner_id}`          | 1 minute |
| Pull sync  | `pull:{partner_id}:{ec_hash}` | 1 hour   |

Engineering must confirm `fastly::erl::RateCounter` availability in the target before implementation is considered complete. Do NOT silently skip rate limiting in production if ERL is unavailable. Do NOT fall back to KV-based counters — they would hit the same 1 write/sec/key limit that motivated removing recurring organic-request KV writes, and would thrash under real sync traffic. If ERL is unavailable, the rate-limited routes are blocked on an approved alternative counting mechanism.

### 14.4 Deprecation note

`settings.synthetic` is removed in PR #479. The `[synthetic]` TOML section, `counter_store`, `opid_store`, and `secret_key` fields are no longer present.

---

## 15. Constants and Header Names

New constants in `crates/trusted-server-core/src/constants.rs`:

```rust
// EC cookie names
pub const COOKIE_TS_EC: &str = "ts-ec";
pub const COOKIE_TS_EIDS: &str = "ts-eids";

// EC response headers
pub const HEADER_X_TS_EC: HeaderName = HeaderName::from_static("x-ts-ec");
pub const HEADER_X_TS_EIDS: HeaderName = HeaderName::from_static("x-ts-eids");
pub const HEADER_X_TS_EC_CONSENT: HeaderName = HeaderName::from_static("x-ts-ec-consent");
pub const HEADER_X_TS_EIDS_TRUNCATED: HeaderName = HeaderName::from_static("x-ts-eids-truncated");
```

The following EC headers are included in `INTERNAL_HEADERS` in `constants.rs` to ensure they are stripped before proxying to downstream backends:

- `x-ts-ec`
- `x-ts-eids`
- `x-ts-ec-consent`
- `x-ts-eids-truncated`

The `INTERNAL_HEADERS` filter uses `x-ts-` prefix stripping in `http_util.rs` to also strip any dynamic `X-ts-<partner_id>` headers without needing to enumerate partner IDs.

---

## 16. Error Handling

New error variants in `crates/trusted-server-core/src/error.rs`:

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

    /// Partner not found in registry.
    #[display("Partner not found: {partner_id}")]
    PartnerNotFound { partner_id: String },
    // Maps to StatusCode::BAD_REQUEST (400)

    /// Partner API key authentication failed.
    #[display("Invalid API key")]
    PartnerAuthFailed,
    // Maps to StatusCode::UNAUTHORIZED (401)
}
```

---

## 17. Request Routing

New routes added to `route_request()` in `crates/trusted-server-adapter-fastly/src/main.rs`:

```rust
// EC identity resolution — Bearer token auth (internal to handler)
(GET, "/_ts/api/v1/identify") → handle_identify(settings, &kv, &registry, &req, &ec_context)

// CORS preflight for /identify — must be registered explicitly, current router dispatches by exact method/path
(OPTIONS, "/_ts/api/v1/identify") → cors_preflight_identify(settings, &req)

// S2S batch sync — partner API key auth (internal to handler)
(POST, "/_ts/api/v1/batch-sync") → handle_batch_sync(&kv, &registry, &limiter, req)
```

Route ordering: EC routes are inserted before the fallback `handle_publisher_request()`.

### 17.1 EC integration in `main.rs`

EC follows the same pre-routing pattern as `GeoInfo::from_request()` (line 70). The pull sync background step requires a **structural refactor of the Fastly entrypoint**:

1. `route_request()` return type changes from `Result<Response, Error>` to `Result<(), Error>`.
2. The response is flushed mid-function via `response.send_to_client()` instead of being returned to `main()`.
3. The `#[fastly::main]` function (`main.rs:32`) currently returns `Result<Response, Error>` — it must change to call `route_request()` and return `Ok(())` (or map the error). The current `fn main(req: Request) -> Result<Response, Error>` signature is incompatible with the `send_to_client()` pattern.
4. After `send_to_client()`, the WASM invocation continues for background pull sync work.

This is a supported Fastly Compute pattern — `Response::send_to_client()` flushes the response to the client immediately and allows the WASM invocation to continue. This is not a small wiring change; it restructures how the application returns responses.

```rust
fn route_request(...) -> Result<(), Error> {
    let geo_info = GeoInfo::from_request(&req);

    // Phase 0 — bot gate (pure in-memory, no KV I/O). See §7A.
    let device_signals = derive_device_signals(&req);
    let is_real_browser = device_signals.looks_like_browser();
    if !is_real_browser {
        log::debug!("Bot gate: blocking EC operations (ja4={:?}, platform={:?})",
            device_signals.ja4_class, device_signals.platform_class);
    }

    // Pre-routing — read only, no generation (matches GeoInfo pattern).
    // EcContext stores client_ip internally (same req.get_client_ip_addr()
    // already called by GeoInfo::from_request() above).
    let ec_context_result = EcContext::read_from_request(&req, settings, geo_info.as_ref());
    let mut ec_context = match ec_context_result {
        Ok(ctx) => ctx,
        Err(e) => {
            log::error!("EcContext initialization failed: {e:?}");
            let mut response = to_error_response(&e);
            response.send_to_client();
            return Ok(());
        }
    };

    // Pass device signals through for KvDevice on creation.
    ec_context.set_device_signals(device_signals);

    // Build partner registry from config at startup.
    let registry = PartnerRegistry::from_config(&settings.ec.partners)?;

    // Extract ts-eids cookie before routing consumes the request.
    let eids_cookie = extract_cookie_value(&req, COOKIE_TS_EIDS);

    // Bot gate: suppress all KV operations for unrecognized clients.
    let kv = if is_real_browser {
        settings.ec.ec_store.as_deref().map(KvIdentityGraph::new)
    } else {
        None
    };
    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);

    if let Some(mut response) = enforce_basic_auth(settings, &req) {
        // Bot gate: skip EC cookie writes for unrecognized clients.
        if is_real_browser {
            ec_finalize_response(settings, &ec_context, kv.as_ref(), &registry, eids_cookie.as_deref(), &mut response);
        }
        response.send_to_client();
        return Ok(());
    }

    let path = req.get_path().to_string();
    let method = req.get_method().clone();

    // Route dispatch — req is moved (consumed) inside the matching arm.
    // is_organic tracks whether pull sync should fire (organic routes only — §10.2).
    let mut is_organic = false;
    let result = match (method, path.as_str()) {
        (GET, "/_ts/api/v1/identify")          => handle_identify(settings, kv.as_ref(), &registry, &req, &ec_context),
        (OPTIONS, "/_ts/api/v1/identify")      => cors_preflight_identify(settings, &req),
        (POST, "/_ts/api/v1/batch-sync")       => handle_batch_sync(kv.as_ref(), &registry, &limiter, req),
        (POST, "/auction")                     => handle_auction(settings, orchestrator, kv.as_ref(), req, &ec_context),

        (m, path) if integration_registry.has_route(&m, path) => {
            is_organic = true;
            ec_context.generate_if_needed(settings, kv.as_ref());
            integration_registry.handle_proxy(&m, path, settings, req, &ec_context)
        },
        _ => {
            is_organic = true;
            ec_context.generate_if_needed(settings, kv.as_ref());
            handle_publisher_request(settings, integration_registry, req, &ec_context)
        },
    };

    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    // Bot gate: skip EC cookie writes and finalize for unrecognized clients.
    if is_real_browser {
        ec_finalize_response(settings, &ec_context, kv.as_ref(), &registry, eids_cookie.as_deref(), &mut response);
    }

    response.send_to_client();

    // Background pull sync — organic routes only, real browsers only (§7A.4, §10.2).
    if is_real_browser && is_organic {
        if let Some(ip) = ec_context.client_ip {
            let pull_partners = registry.pull_enabled_partners();
            pull_sync_dispatcher.dispatch_background(&ec_context, ip, &pull_partners, kv.as_ref());
        }
    }

    Ok(())
}
```

The existing `finalize_response()` in `main.rs` becomes `ec_finalize_response()` with the extended signature that accepts `ec_context`, `kv`, `registry`, and `eids_cookie`. The `#[fastly::main]` entrypoint changes to call `route_request()` and return `Ok(())` (the response is already sent via `send_to_client()`). The `PartnerRegistry` is built once at startup via `PartnerRegistry::from_config(&settings.ec.partners)` and passed by reference throughout the request lifecycle.

`PullSyncDispatcher::dispatch_background` uses `Request::send_async()` to fire outbound HTTP calls, then calls `PendingRequest::wait()` (blocking) on each handle under `settings.ec.pull_sync_concurrency` concurrency. No async runtime is needed — this is synchronous blocking code running after `send_to_client()` has flushed the response. The Fastly WASM invocation stays alive until `dispatch_background` returns. This does not add latency to the user-facing response.

---

## 18. Testing Strategy

Follow the project's **Arrange-Act-Assert** pattern. Test both happy paths and error conditions. Use `expect()` with `"should ..."` messages.

### 18.1 Unit tests

Each module in `ec/` has a `#[cfg(test)]` module covering:

| Module           | Key test cases                                                                                                        |
| ---------------- | --------------------------------------------------------------------------------------------------------------------- |
| `generation.rs`  | IPv4/IPv6 normalization, /64 truncation, HMAC determinism, output format                                              |
| `finalize.rs`    | `ec_finalize_response()`: cookie write on generation, deletion on withdrawal, returning-user EC header, EID ingestion |
| `cookies.rs`     | Cookie string format, Max-Age=0 for deletion, domain derivation                                                       |
| `kv.rs`          | Serialization/deserialization roundtrip, CAS merge logic, metadata extraction                                         |
| `partner.rs`     | Partner ID validation, API key hashing                                                                                |
| `registry.rs`    | `from_config()` validation, duplicate detection, O(1) lookups by ID/hash/domain                                       |
| `prebid_eids.rs` | Base64 decode, JSON parse, source domain matching, debounce                                                           |
| `batch_sync.rs`  | Status code selection (200/207/401/400/429), per-mapping rejection reasons, API-key rate limit                        |
| `pull_sync.rs`   | Trigger conditions, null/404 no-op, dispatch limit                                                                    |
| `identify.rs`    | Bearer auth (200/401/403/204), scoped partner response, degraded flag, CORS                                           |

### 18.2 Integration tests

KV behavior is tested with Viceroy (local Fastly Compute simulator) using real KV store operations. Key scenarios:

- Explicit consent withdrawal: cookie deletion + tombstone write (`write_withdrawal_tombstone()`) + all EC response headers stripped — in same request
- Concurrent writes: CAS retry logic under simulated generation conflicts
- KV degraded: EC cookie still set when KV `create_or_revive()` fails (best-effort)
- Prebid EID ingestion: `ts-eids` cookie parsed, source domain matched, partner UID written to KV
- Batch sync then identify: batch sync writes partner UID, then `/_ts/api/v1/identify` returns it for that partner

**Eventually-consistent caveat:** Fastly KV does not guarantee read-after-write consistency. The sync→identify scenario may not be immediately visible on production — Viceroy may behave differently. Tests for this flow should use retry with backoff (up to 1s) and be documented as Viceroy-only consistency. Do not write assertions that assume immediate visibility after a KV write.

### 18.3 JS tests (if applicable)

If any JS changes are made for EC (e.g., publisher-side `/_ts/api/v1/identify` fetch helper in `crates/trusted-server-js/`), use Vitest with `vi.hoisted()` for mocks.

---

## 19. Implementation Order

Implementation was completed in the following order. Each step passed `cargo test --workspace` before the next began.

| Step | Scope                                                     | Deliverable                                                                                       |
| ---- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| 1    | `ec/generation.rs` + constants + settings                 | `generate_ec()`, `normalize_ip()`, `EcContext`                                                    |
| 2    | `ec/cookies.rs`                                           | Cookie creation, deletion, response header                                                        |
| 3    | `ec/kv.rs` + `ec/kv_types.rs`                             | `KvIdentityGraph` CRUD with CAS                                                                   |
| 4    | `ec/finalize.rs`                                          | `ec_finalize_response()` (cookie write on generation, deletion, tombstone, returning-user header) |
| 5    | `ec/partner.rs` + `ec/registry.rs`                        | `PartnerRegistry` (config-based), partner validation helpers                                      |
| 6    | EC middleware in `main.rs`, `publisher.rs`, `registry.rs` | `EcContext::read_from_request()` pre-routing, `generate_if_needed()`, `ec_finalize_response()`    |
| 7    | `ec/prebid_eids.rs`                                       | Prebid EID cookie ingestion (replaces pixel sync)                                                 |
| 8    | `ec/identify.rs`                                          | `GET /_ts/api/v1/identify` handler + route (Bearer auth, scoped response)                         |
| 9    | `ec/batch_sync.rs` + `ec/rate_limiter.rs`                 | `POST /_ts/api/v1/batch-sync` handler + route                                                     |
| 10   | `ec/pull_sync.rs`                                         | Background pull sync dispatch (blocking, after `send_to_client()`)                                |
| 11   | Auction integration                                       | EC injection into `user.id`, `user.eids`, `user.consent`                                          |
| 12   | End-to-end integration tests                              | Viceroy-based flow tests                                                                          |

---

## 20. Epic and Stories

### Epic: Implement Edge Cookie (EC) identity system

Enable the trusted server to generate, persist, and serve a publisher-owned,
privacy-safe Edge Cookie (EC) that can be used for ID sync, identity lookup,
and auction decoration — without relying on third-party cookies.

**Done when:** All 12 stories below are complete, `cargo test --workspace` and
`cargo clippy` pass with no warnings, and the end-to-end Viceroy flow tests
cover the full EID ingestion → identify → auction path.

**Spec ref:** This document. PRD: `docs/internal/ssc-prd.md`.

---

### Story 1 — EC generation and request context

Implement the core EC data types, generation logic, and per-request context
struct that all subsequent stories depend on.

**Scope:** `ec/generation.rs`, `ec/mod.rs`, `trusted-server.toml` `[ec]` section,
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
  using the request-local consent context.)
- `EcContext::generate_if_needed(settings, kv)` generates a new EC when
  `ec_value == None && allows_ec_creation(&consent)`, sets `ec_generated = true`,
  and writes the initial KV entry via `kv.create_or_revive()` (best-effort).
  Using `create_or_revive` (not `create`) ensures re-consent within the 24h
  tombstone window recovers immediately. This function is best-effort: if
  generation fails (e.g., missing client IP), it logs `warn` and returns
  without setting `ec_generated`. It never returns an error — organic traffic
  must not 500 on EC failure.
- `[ec]` settings block parses from TOML: `passphrase`, `ec_store`,
  `pull_sync_concurrency`, `partners`.
- All unit tests in `generation.rs` pass (HMAC determinism, format, IP normalization).

**Spec ref:** §2, §3, §4, §5.4, §14.1

---

### Story 2 — EC finalize response

Implement `ec_finalize_response()` — the post-routing function that enforces
cookie writes on generation, cookie deletion on withdrawal, tombstones, returning-user `x-ts-ec` headers, and EID ingestion on responses.

**Scope:** `ec/finalize.rs` (new file)

**Acceptance criteria:**

- `ec_finalize_response(settings, geo, ec_context, kv, response)` runs on every route.
- Consent gating uses `allows_ec_creation()` for current-request EC usage and `has_explicit_ec_withdrawal()` for cookie-expiry/tombstone decisions.
- When `!allows_ec_creation(&consent)`: strips all EC response headers.
- When `has_explicit_ec_withdrawal(&consent) && cookie_was_present`: additionally expires the cookie and writes tombstones for each valid EC ID available. When the cookie is malformed and no valid header exists, no tombstone is written — cookie deletion alone enforces withdrawal (see §6.2).
- When `ec_was_present && !ec_generated && allows_ec_creation(&consent)`: sets the `x-ts-ec` response header only. It does not refresh the EC cookie, repair header/cookie mismatches, or write KV solely to extend TTL.
- When `ec_generated == true`: calls `set_ec_cookie_and_header_on_response()`.
- Unit tests cover explicit-withdrawal, fail-closed header stripping, returning-user header behavior, and new-EC generation.

**Spec ref:** §5.4, §6.2

---

### Story 3 — EC cookie helpers

Implement the low-level functions that create and delete the `ts-ec` cookie
and set EC response headers. These are called by `ec_finalize_response()` (Story 2).

**Scope:** `ec/cookies.rs`

**Acceptance criteria:**

- `create_ec_cookie()` produces a cookie with `Domain=.{publisher.domain}`,
  `Max-Age=31536000`, `SameSite=Lax; Secure`. `HttpOnly` is NOT set
  (JS on the publisher page must be able to read the cookie).
- `delete_ec_cookie()` produces a cookie with `Max-Age=0`, same attributes.
- `set_ec_header_on_response()` sets only `X-ts-ec`; `set_ec_cookie_and_header_on_response()` sets both `Set-Cookie` and `X-ts-ec`.
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
- Returning-user page views do not update a last-seen field; EC entries no longer store `last_seen` or mutable publisher-domain visit timestamps.
- `KvIdentityGraph::write_withdrawal_tombstone(ec_hash)` sets `consent.ok = false`,
  clears partner IDs, and applies a 24-hour TTL (see §6.2). Returns `Result` —
  callers log `error` on failure and continue (cookie deletion is the primary
  enforcement mechanism).
- `KvIdentityGraph::delete(ec_hash)` hard-deletes the entry — used only for IAB
  data deletion requests, not for consent withdrawal (which uses tombstones).
- `kv.upsert_partner_id(ec_hash, partner_id, uid)` writes to `ids[partner_id]`, creating a minimal live root entry first if the key is absent, and skips writes when the existing UID already matches (idempotent).
- KV schema matches §7 exactly (JSON roundtrip test).
- Unit tests cover CAS merge logic, tombstone write, tombstone error handling,
  serialization/deserialization roundtrip, metadata extraction.

**Spec ref:** §4, §5.4, §6.2

---

### Story 5 — Partner registry (config-based)

Implement partner ID validation, API key hashing, and the in-memory
`PartnerRegistry` that replaces the KV-backed `PartnerStore`.

**Scope:** `ec/partner.rs`, `ec/registry.rs`

**Acceptance criteria:**

- `validate_partner_id()` enforces `^[a-z0-9_-]{1,32}$` and rejects reserved
  names (`ec`, `eids`, `ec-consent`, `eids-truncated`, `synthetic`, `ts`,
  `version`, `env`).
- `hash_api_key()` computes SHA-256 hex of the plaintext API token.
- `PartnerConfig` contains all fields from §13.3 including
  `pull_sync_allowed_domains` and `batch_rate_limit`.
- `PartnerRegistry::from_config()` builds the registry from `Vec<EcPartner>`
  with O(1) `by_id`, `by_api_key_hash`, and `by_source_domain` indexes.
- Startup validation catches: invalid IDs, duplicate IDs, duplicate API token
  hashes, duplicate source domains, invalid pull sync configuration.
- `get()`, `find_by_api_key_hash()`, `find_by_source_domain()` return
  `Option<&PartnerConfig>`.
- `pull_enabled_partners()` returns only partners with `pull_sync_enabled = true`.
- No admin endpoint — partner changes require config update and redeployment.
- Unit tests cover partner ID validation, hash computation, registry
  construction, and duplicate detection.

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
- EC route handlers receive `ec_context` without EC generation. `/_ts/api/v1/identify`,
  `/auction`, and `/_ts/api/v1/batch-sync` use read-only `&EcContext` and
  never mutate it.
- `/auction` consumes EC identity but never bootstraps it.
- `handle_publisher_request()` and `integration_registry.handle_proxy()` call
  `ec_context.generate_if_needed(settings, &kv)` before their handler logic (best-effort, never 500s).
- `ec_finalize_response()` receives `ec_context` and `kv` and:
  - Strips EC response headers whenever `!allows_ec_creation(&consent)`.
  - Additionally deletes the EC cookie and writes a withdrawal tombstone when `has_explicit_ec_withdrawal(&consent) && cookie_was_present` (runs on all routes).
  - Sets `x-ts-ec` header when `ec_was_present == true && ec_generated == false && allows_ec_creation(&consent)` (returning user with valid consent). Also ingests Prebid EIDs from `ts-eids` cookie.
  - Calls `set_ec_cookie_and_header_on_response()` when `ec_context.ec_generated == true` (newly generated ECs). Returning-user mismatch repair is not performed. Also ingests Prebid EIDs.
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

### Story 7 — Prebid EID cookie ingestion

Implement the server-side ingestion of the `ts-eids` cookie, which replaces
the pixel sync endpoint as the browser-side ID sync mechanism.
**Scope:** `ec/prebid_eids.rs`, `ec/finalize.rs` update

**Acceptance criteria:**

- `ingest_prebid_eids(cookie_value, ec_id, kv, registry)` decodes a base64 JSON
  array of OpenRTB-style `{source, uids:[...]}` objects and syncs matched partners to KV. The backend also accepts the earlier flattened `{source, id, atype}` payload for backward compatibility.
- Source domain matching via `registry.find_by_source_domain()` (case-insensitive).
- Sources with no non-empty UID are skipped.
- Idempotent write suppression: if the stored UID already matches the incoming UID, the write is skipped for that partner.
- KV write via `kv.upsert_partner_id()` — best-effort, errors logged at `warn`.
- Called from `ec_finalize_response()` on both returning-user and new-EC paths
  when a `ts-eids` cookie is present and consent is granted.
- JS writer target size: 3 KB; backend parser raw-cookie limit: 8 KiB.
- All errors are logged and swallowed — never blocks the response.
- Unit tests cover base64 decode, JSON parse, source domain matching, size limits,
  and empty/oversized UID handling.

**Spec ref:** §8

---

### Story 8 — Identity lookup (`GET /_ts/api/v1/identify`)

Implement the partner-facing endpoint that authenticated partners call to
retrieve their own synced UID for the current EC.

**Scope:** `ec/identify.rs`, router update

**Acceptance criteria:**

- **Bearer token required.** Missing or invalid `Authorization: Bearer` → `401`
  with `{ "error": "invalid_token" }`. Auth uses `registry.find_by_api_key_hash()`.
- `!allows_ec_creation(consent)` (consent denied, regardless of EC presence) → `403 Forbidden`.
  When the denial is an explicit withdrawal signal and a `ts-ec` cookie was present, `ec_finalize_response()` also deletes the cookie and writes a tombstone. Fail-closed / unverifiable-consent cases still return `403`, but they strip EC headers only.
- No EC present (`ec_was_present == false`) and consent not denied → `204 No Content`.
- Valid EC, consent granted, KV read succeeds with entry → `200` with scoped JSON body
  including `ec`, `consent`, `partner_id`, `uid` (single partner's UID), `eid`
  (single partner's OpenRTB EID object), `cluster_size`.
- Valid EC, consent granted, KV read succeeds but no entry for this partner →
  `200` with `degraded: false`, `uid` and `eid` absent. Not an error — see §11.4.
- KV read error (store unavailable) → `200` with `degraded: true`, `uid` and
  `eid` absent.
- Response scoped to the authenticated partner only — no multi-partner `uids`/`eids` maps.
- `X-ts-ec` response header set on `200` responses.
- No `Origin` header (server-side proxy): process normally, no CORS headers, no `403`.
- `Origin` header present and matches `publisher.domain` or subdomain: reflect in
  `Access-Control-Allow-Origin` + `Vary: Origin`.
- `Origin` header present but does not match: `403`, no body.
- `Access-Control-Allow-Headers` includes `Authorization, X-ts-ec`.
- `OPTIONS /_ts/api/v1/identify` preflight → `200` with CORS headers, no body.
- `generate_if_needed()` is never called — no new EC is generated.
- Response time target: 30ms p95 (documented, not gate).
- Unit tests cover Bearer auth (200/401/403/204), scoped partner response,
  degraded flag, CORS origin validation.

**Spec ref:** §11

---

### Story 9 — S2S batch sync (`POST /_ts/api/v1/batch-sync`)

Implement the server-to-server batch sync endpoint for partners to bulk-write
their UIDs against a list of EC hashes.

**Scope:** `ec/batch_sync.rs`, `ec/rate_limiter.rs`, router update

**Acceptance criteria:**

- Missing or invalid `Authorization: Bearer` → `401`. Auth uses in-memory
  lookup via `registry.find_by_api_key_hash()` (§9.2).
- API-key rate limit exceeded (`batch_rate_limit` per partner per minute) → `429`
  with `{ "error": "rate_limit_exceeded" }`.
- More than 1000 mappings → `400`.
- Per-mapping rejections: `invalid_ec_id`, `ec_id_not_found`,
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
  existing partner entry; existing partner UIDs are not refreshed by pull sync.
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
  never on `/_ts/api/v1/identify`, `/_ts/api/v1/batch-sync`, or `/auction`.
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

- **Full flow:** First-party page load → EC generated → Prebid EID cookie
  ingestion writes partner UID → `/_ts/api/v1/identify` returns that UID
  (scoped to authenticated partner) → auction includes EID.
- **Consent withdrawal:** Request with denied consent clears EC cookie and writes
  a KV tombstone (`consent.ok = false`, 24h TTL) in the same request; subsequent
  `/_ts/api/v1/identify` with consent still denied returns `403` (consent denied → §11.4);
  batch sync returns `consent_withdrawn` within the tombstone TTL.
- **KV create failure:** EC cookie is still set when `create_or_revive()` fails
  (best-effort). Subsequent `/_ts/api/v1/identify` returns `200` with `degraded: false` and
  empty `uids`/`eids` (KV read succeeds — entry simply does not exist).
- **KV read failure:** `/_ts/api/v1/identify` returns `200` with `degraded: true` and empty
  `uids`/`eids` (store unavailable, entry might exist but can't be read).
- **Concurrent writes:** Two simultaneous EC creates for the same hash resolve
  without data loss (CAS retry).
- **Rate limits:** Batch sync returns `429` after `batch_rate_limit` is exceeded.
- **Pull sync no-op:** Partner returning `{ "uid": null }` produces no KV
  write and no error log.
- All tests pass under `cargo test --workspace` with Viceroy.

**Spec ref:** §18.2

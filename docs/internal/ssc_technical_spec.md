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
9. [S2S Batch Sync API (`POST /api/v1/sync`)](#9-s2s-batch-sync-api-post-apiv1sync)
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

EC is the intended full replacement for SyntheticID. The PRD explicitly states backward compatibility is a non-goal. Coexistence in this spec is a **temporary implementation detail only** — not a product commitment. SyntheticID runs alongside EC solely because the cutover and removal work belongs to a follow-on spec; it is not preserved for compatibility reasons. EC is authoritative where present.

**Prerequisites (completed before this epic begins):**

The following work is handled in separate epics that must ship before this epic starts:

- **SyntheticID → EC rename** — all SyntheticID references in both the product and codebase are renamed to Edge Cookie / EC terminology (e.g. `get_or_generate_synthetic_id` → `get_or_generate_ec`, `ts-synthetic` cookie → `ts-ec`, product-facing naming). This spec assumes the rename is already in place; any SyntheticID naming in existing code shown here reflects the current codebase state at time of writing.
- **Consent implementation** — The consent pipeline (`build_consent_context()`, `ConsentContext`, TCF/GPP/US-Privacy decoding) is implemented and available as a stable interface before this epic. PR `#380` merged to `main`. This spec uses `ConsentContext` as a pre-existing contract and adds only the EC-specific `ec_consent_granted()` gating layer on top.

**Deferred from this spec (not in scope):**

- TS Lite deployment mode (PRD Section 5)
- JOSE-signed KV entries / buyer attestation, and the associated `/.well-known/trusted-server.json` attestation object + `Cache-Control: max-age=3600` response (PRD Section 8.7). The existing discovery endpoint and its tests (`endpoints.rs:579–594`) assert only `version` and `jwks` fields — this spec does not modify that endpoint. Any addition of the PRD-required `attestation` field is deferred to when JOSE signing ships.
- Data deletion framework JWT endpoint (PRD Section 7.4) — the formal IAB-compliant deletion endpoint is deferred. The PRD explicitly acknowledges that manual KV deletion is the interim process until the formal endpoint ships, and states that regulated onboarding requires the formal endpoint to be in place first. This spec implements the manual-deletion-only interim; the JWT endpoint is a prerequisite for regulated onboarding and must be tracked separately.
- Winner notification EC headers on publisher ad server delivery (§12.5) — the current `/auction` path returns JSON inline to the JS caller; there is no server-to-server delivery step. §12.5 is deferred until that delivery architecture exists.
- SyntheticID cutover — removing the old SyntheticID module, `X-Synthetic-*` headers, and related code is a follow-on spec. This spec only adds EC alongside the renamed SyntheticID code.

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
    │  - ec_consent_granted(consent)               │
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
POST /api/v1/sync, POST /admin/*) NEVER call generate_if_needed().
EcContext is available to them in read-only form. /auction reads
EC identity but never bootstraps it — the publisher page-load path
generates the EC before any auction request arrives.

finalize_response() — after every handler:
    - consent withdrawn + cookie present? → delete_ec_cookie() [always]
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
    consent.rs      — ec_consent_granted()  [thin gating layer over prerequisite ConsentContext]
    kv.rs           — KvIdentityGraph, read/write/delete identity entries
    partner.rs      — PartnerRecord, PartnerStore, load_partner()
    sync_pixel.rs   — handle_sync() handler
    sync_batch.rs   — handle_batch_sync() handler
    pull_sync.rs    — dispatch_pull_sync() async
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
pub fn generate_ec(ip: IpAddr, passphrase: &str) -> Result<String, Report<TrustedServerError>>;

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

**IP source:** Use `req.get_client_ip_addr()` — Fastly's trusted API that returns the verified client IP without relying on any request header. This is the same source used by the existing `synthetic.rs` IP handling. Do not fall back to `X-Forwarded-For` or any other header — those are forgeable by clients. Return an error if the API returns `None`; do not create an EC without an IP.

On dual-stack: prefer IPv6 if the returned address is IPv6; otherwise use IPv4.

### 4.2 EC Retrieval Priority

Pre-routing, EC state is read (not generated) from the inbound request:

1. `X-ts-ec` request header (forwarded by publisher infrastructure)
2. `ts-ec` cookie
3. Neither present → `ec_value = None`, `ec_was_present = false`

Generation (step 3 above becoming a new EC) happens only inside organic handlers — see §5.4. This logic lives in `EcContext::read_from_request()` (phase 1) and `EcContext::generate_if_needed()` (phase 2).

### 4.3 `EcContext`

```rust
/// Per-request Edge Cookie state. Constructed pre-routing in read-only form;
/// organic handlers call `generate_if_needed()` to mint new ECs.
pub struct EcContext {
    /// Full EC value (`hash.suffix`), if present on request or generated this request.
    pub ec_value: Option<String>,
    /// Whether the `ts-ec` **cookie** was present on the inbound request.
    /// This is the only field that gates consent-withdrawal cookie deletion —
    /// the PRD's delete branch is conditioned on the cookie, not on X-ts-ec header.
    pub cookie_was_present: bool,
    /// Whether any EC value was available (cookie OR X-ts-ec header).
    pub ec_was_present: bool,
    /// Set to true by `generate_if_needed()` when a new EC is minted this request.
    /// `finalize_response()` uses this to decide whether to write a Set-Cookie header.
    pub ec_generated: bool,
    /// Full consent context from the prerequisite consent pipeline.
    /// Use `ec_consent_granted(&self.consent)` to derive a grant/deny decision.
    /// Raw TCF/GPP strings (for KV writes and `user.consent`) are on `consent.raw_tc_string`
    /// and `consent.raw_gpp_string`.
    pub consent: ConsentContext,
    /// Client IP extracted from `req` during `read_from_request()`.
    /// Stored here so pull sync can use it after `req` has been consumed by routing.
    /// `None` only if Fastly's `get_client_ip_addr()` returns `None`.
    pub client_ip: Option<IpAddr>,
}

impl EcContext {
    /// Phase 1: reads cookie/header and builds consent context. Does not generate. Does not write KV.
    /// Called pre-routing, like `GeoInfo::from_request()` in the current `main.rs`.
    /// Calls `build_consent_context()` with the EC hash (if cookie present) as `synthetic_id`
    /// so KV-persisted consent can be loaded for users without fresh consent cookies.
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
    pub fn generate_if_needed(
        &mut self,
        req: &Request,
        settings: &Settings,
        kv: &KvIdentityGraph,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Returns the stable 64-char hex prefix, or `None` if no EC.
    pub fn ec_hash(&self) -> Option<&str>;
}
```

**`finalize_response()` behavior** (updated signature: `finalize_response(settings, geo, ec_context, kv, response)`):

1. If `!ec_consent_granted(&consent) && cookie_was_present`: call `delete_ec_cookie()` and `kv.write_withdrawal_tombstone(ec_hash)`. This runs on **every route** — consent withdrawal is always real-time enforced. Keyed on `cookie_was_present`, not `ec_was_present`, because only a cookie-held EC can be deleted by the browser.
2. If `ec_generated == true`: call `set_ec_on_response()` — sets `Set-Cookie` and `X-ts-ec`. KV create already happened inside `generate_if_needed()`; `finalize_response()` does NOT write KV beyond the tombstone.
3. Handler-built response headers (`X-ts-ec`, `X-ts-eids` set directly by `/identify`) are not modified.

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
and does not carry the EC ownership guarantee.

```rust
/// Builds the `Set-Cookie` header value for a newly generated EC.
pub fn create_ec_cookie(ec_value: &str, cookie_domain: &str) -> String;

/// Builds the `Set-Cookie` header value that expires (deletes) the EC cookie.
pub fn delete_ec_cookie(cookie_domain: &str) -> String;
// Sets Max-Age=0 with same Domain/Path/Secure/SameSite attributes.

/// Sets the EC cookie and `X-ts-ec` response header on a response.
pub fn set_ec_on_response(response: &mut Response, ec_value: &str, cookie_domain: &str);

/// Removes the EC cookie and clears `X-ts-ec` response header.
pub fn clear_ec_on_response(response: &mut Response, cookie_domain: &str);
````

### 5.3 Response header

`X-ts-ec: {ec_hash.suffix}` is set on every response where an EC is present.

This header is added to `INTERNAL_HEADERS` in `constants.rs` so it is stripped before proxying to downstream backends, consistent with existing `X-ts-*` handling.

### 5.4 Per-request EC lifecycle

**Phase 1 — pre-routing** (always runs, all routes):

```
EcContext::read_from_request()
  Read ts-ec cookie / X-ts-ec header → ec_value, ec_was_present
  build_consent_context(jar, req, config, geo, ec_hash?) → consent: ConsentContext
  ec_generated = false
```

**Phase 2 — inside organic handlers only** (`handle_publisher_request`, `handle_proxy`):

```
ec_context.generate_if_needed(&req, settings, &kv)
  └── ec_consent_granted(&consent) && ec_value == None?
          → generate_ec(passphrase, ip)
          → ec_value = Some(new_ec)
          → ec_generated = true
          → kv.create_or_revive(ec_hash, &entry)   (best-effort, log warn if fails)
            // create_or_revive overwrites a tombstone (ok=false) on re-consent
            // no-ops if a live entry (ok=true) already exists
```

**`finalize_response(settings, geo, ec_context, &kv, response)` — always runs, all routes:**

```
  ├── !ec_consent_granted(&consent) && cookie_was_present?
  │       → delete_ec_cookie()                  (always — real-time withdrawal enforcement)
  │       → kv.write_withdrawal_tombstone(ec_hash)   (synchronous — see §6.2)
  │           Tombstone fails? log error, do NOT block — no retry possible on browser path
  │             cookie deletion is the authoritative enforcement mechanism
  │
  ├── ec_was_present == true && ec_generated == false && ec_consent_granted(&consent)?
  │       → kv.update_last_seen(ec_hash, now())   (returning user — debounced at 300s)
  │
  └── ec_generated == true?
          → set_ec_on_response()        (Set-Cookie + X-ts-ec on response)
```

EC route handlers (`GET /sync`, `GET /identify`, `POST /api/v1/sync`, `POST /admin/*`) never call `generate_if_needed()`. `finalize_response()` will still delete the cookie on those routes if consent is withdrawn — that is intentional.

**One rule:** `Set-Cookie` is written if and only if `ec_generated == true` (first-time generation). There is no cookie refresh or Max-Age reset on returning users. The PRD defers a blanket refresh-on-every-request strategy to a future iteration.

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

The consent pipeline surfaces signals — it does not make grant/deny decisions. EC defines its own gating function in `ec/consent.rs`:

```rust
/// Returns true when consent is sufficient to create or maintain an EC for this request.
/// Uses Jurisdiction (geo-derived) as the primary enforcement signal.
pub fn ec_consent_granted(consent: &ConsentContext) -> bool {
    match &consent.jurisdiction {
        // GDPR: require TCF Purpose 1 (storage) and not expired
        Jurisdiction::Gdpr => {
            let tcf_ok = consent.tcf.as_ref()
                .or_else(|| consent.gpp.as_ref()?.eu_tcf.as_ref())
                .map_or(false, |t| t.has_storage_consent());
            tcf_ok && !consent.expired
        }
        // US state privacy law: require no opt-out
        Jurisdiction::UsState(_) => {
            !consent.gpc
                && consent.us_privacy.as_ref()
                    .map_or(true, |p| p.opt_out_sale != PrivacyFlag::Yes)
        }
        // Non-regulated region: always granted
        Jurisdiction::NonRegulated => true,
        // Unknown geo (Fastly lookup failed): fail-closed — treat as regulated
        Jurisdiction::Unknown => false,
    }
}
```

`EcContext::read_from_request()` calls `build_consent_context()` then stores the result. All downstream logic (EC generation gating, withdrawal detection) calls `ec_consent_granted(&self.consent)`. No consent decoding logic lives in this epic.

### 6.2 Consent withdrawal — KV delete

When `ec_consent_granted(&consent)` returns `false` for a user whose **`ts-ec` cookie** is present (`cookie_was_present == true`). A user identified only by the `X-ts-ec` request header is not subject to cookie deletion — there is no cookie to expire.

1. Issue `Set-Cookie: ts-ec=; Max-Age=0; ...` (synchronous — must not fail silently)
2. Write tombstone: `kv.write_withdrawal_tombstone(ec_hash)` — sets `consent.ok = false`, clears partner IDs, TTL 24h — approximately 25ms

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

The `ok` field in metadata is a **historical consent record for S2S consumers only** — it is set to `false` by `write_withdrawal_tombstone()` so that batch sync clients (`POST /api/v1/sync`) can return `consent_withdrawn` rather than `ec_hash_not_found` during the 24-hour tombstone TTL.

**`consent.ok` is NOT used to make the withdrawal decision on the main request path.** Consent withdrawal is determined entirely from `ec_consent_granted(&ec_context.consent)` on the current request. When withdrawal is detected, the cookie is deleted and `write_withdrawal_tombstone()` is called in-path (setting `ok = false`, 24h TTL — see §6.2). Engineers must not add a KV read to the consent withdrawal hot path based on this field.

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
    /// The tombstone allows batch sync clients (`POST /api/v1/sync`) to return
    /// `consent_withdrawn` rather than `ec_hash_not_found` for the tombstone TTL.
    ///
    /// After the 24-hour TTL expires, the entry is gone. Any subsequent `get()`
    /// returns `None` (`ec_hash_not_found`) — the distinction is time-bounded.
    ///
    /// Caller must handle `Err` by logging at `error` level; the cookie deletion
    /// in `finalize_response()` is the primary enforcement mechanism.
    pub fn write_withdrawal_tombstone(
        &self,
        ec_hash: &str,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Hard-deletes the entry. Used only for data deletion requests (IAB deletion
    /// framework — deferred). For consent withdrawal, use `write_withdrawal_tombstone()`.
    pub fn delete(&self, ec_hash: &str) -> Result<(), Report<TrustedServerError>>;
}
```

`MAX_CAS_RETRIES = 3`. If all retries fail on a generation conflict, return `Err` — callers handle per-endpoint policy (Section 8.4 for pixel sync, Section 10 for batch).

### 7.5 KV degraded behavior

| Operation                          | KV unavailable | Action                                                                |
| ---------------------------------- | -------------- | --------------------------------------------------------------------- |
| EC cookie creation                 | KV error       | Set cookie. Skip KV create. Log `warn`.                               |
| `/sync` KV write                   | KV error       | Redirect with `ts_synced=0&ts_reason=write_failed`.                   |
| `/identify` KV read                | KV error       | Return `200` with `ec` set, `degraded: true`, empty `uids`/`eids`.    |
| `POST /api/v1/sync`                | KV error       | Return `207` with all mappings rejected, `reason: "kv_unavailable"`.  |
| Pull sync KV write                 | KV error       | Discard uid. Log `warn`. Retry on next qualifying request.            |
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
    ec_context: &EcContext,
) -> Result<Response, Report<TrustedServerError>>;
```

### 8.2 Query parameters

| Parameter | Required | Description                                                       |
| --------- | -------- | ----------------------------------------------------------------- |
| `partner` | Yes      | Partner ID — must exist in `partner_store`                        |
| `uid`     | Yes      | Partner's user ID for this user                                   |
| `return`  | Yes      | Redirect-back URL (must match partner's `allowed_return_domains`) |
| `consent` | No       | Fallback TCF/GPP string if no consent cookie on request           |

### 8.3 Flow

```
1. Parse query params. Missing required params → 400.

2. Read ts-ec cookie.
   Absent → redirect to {return}?ts_synced=0&ts_reason=no_ec

3. Look up partner record in partner_store.
   Not found → 400.

4. Validate return URL host against partner.allowed_return_domains.
   - Exact hostname match only — no suffix or wildcard.
   - Mismatch → 400.

5. Evaluate consent. Use `ec_context.consent` (built pre-routing via
   `build_consent_context()`). The optional `consent` query param is a **fallback
   only** — used solely when `ec_context.consent.is_empty()` (no cookies or
   headers carried consent signals). If any signal exists, the query param is
   ignored entirely. When the fallback applies, re-call `build_consent_context()`
   with a synthetic request or cookie jar that includes the consent param value.
   `!ec_consent_granted(...)` → redirect to {return}?ts_synced=0&ts_reason=no_consent

6. Check anti-stuffing rate limit (sync_rate_limit per EC hash per partner per hour).
   Exceeded → `429 Too Many Requests` (no redirect — the `return` URL is never called).

7. kv.upsert_partner_id(ec_hash, partner_id, uid, now())
   KV write failure → redirect to {return}?ts_synced=0&ts_reason=write_failed

8. Success → redirect to {return}?ts_synced=1
```

`ts_synced` values:

| Value                                | Meaning                       |
| ------------------------------------ | ----------------------------- |
| `ts_synced=1`                        | KV write succeeded            |
| `ts_synced=0&ts_reason=no_ec`        | No EC cookie present          |
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

## 9. S2S Batch Sync API (`POST /api/v1/sync`)

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

`Authorization: Bearer <api_key>` header required. The `api_key` is looked up in `partner_store` by a constant-time comparison of its SHA-256 hash against the stored `api_key_hash`. Key rotation does not require binary redeployment — partners update `partner_store` directly via `/admin/partners/register`.

Returns `401 Unauthorized` with no body processing if auth fails.

### 9.2.1 API-key rate limiting

After successful auth, check the API-key level rate limit: `partner.batch_rate_limit` requests per partner per minute (default 60). Uses the same Fastly rate-limiting API as pixel sync (§14.3), with key `batch:{partner_id}`.

Exceeded → `429 Too Many Requests` with body `{ "error": "rate_limit_exceeded" }`. No mappings are processed.

### 9.3 Request format

```
POST /api/v1/sync
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
    /// Called after `send_to_client()` — fires outbound requests using `send_async()`.
    /// Takes `client_ip` directly (extracted before `req` is consumed by routing).
    pub fn dispatch_background(
        &self,
        ec_context: &EcContext,
        client_ip: IpAddr,
        partners: &[PartnerRecord],
        kv: &KvIdentityGraph,
    );
}

/// Calls a single partner's resolution endpoint and writes the result to KV.
async fn pull_one_partner(
    ec_hash: &str,
    ip: IpAddr,
    partner: &PartnerRecord,
    kv: &KvIdentityGraph,
);
```

### 10.2 Trigger conditions

A pull sync is dispatched for a partner when all of the following are true on a request:

1. A valid `ts-ec` cookie is present
2. `ec_consent_granted(&ec_context.consent) == true`
3. `partner.pull_sync_enabled == true`
4. Either: no entry exists for this partner in the KV graph, or the existing `synced` timestamp is older than `partner.pull_sync_ttl_sec` (default 86400 seconds)
5. Rate limit not exceeded: `partner.pull_sync_rate_limit` calls per EC hash per partner per hour (default 10)

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

On a non-null `uid`: call `kv.upsert_partner_id(ec_hash, partner_id, uid, now())`. On KV failure: log `warn` and discard the result. Retry occurs on the next qualifying request.

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

| Header                                                  | Required                               |
| ------------------------------------------------------- | -------------------------------------- |
| `Cookie: ts-ec=<value>` or `X-ts-ec: <value>`           | Yes                                    |
| `Cookie: euconsent-v2=<value>` or `Cookie: gpp=<value>` | Yes for EU/UK/US users                 |
| `X-consent-advertising: <value>`                        | Optional — takes precedence if present |

### 11.3 EC and consent handling

`/identify` follows `EcContext` retrieval priority (Section 4.2). It does **not** generate a new EC. It does **not** set or modify cookies.

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

**`403 Forbidden` — consent denied:**

```json
{ "consent": "denied" }
```

**`204 No Content` — no EC present.** No body.

### 11.5 Response headers (supplementary)

Set on `200` responses only:

| Header              | Value                                                        |
| ------------------- | ------------------------------------------------------------ |
| `X-ts-ec`           | `{ec_hash.suffix}`                                           |
| `X-ts-eids`         | Standard base64 (RFC 4648, with `=` padding) of the JSON array of OpenRTB 2.6 `user.eids` objects. Capped at **4 KB** after encoding. If the encoded value exceeds 4 KB, the array is truncated (fewest partners first — highest `synced` timestamp retained) until it fits, and a `x-ts-eids-truncated: true` header is added. |
| `X-ts-<partner_id>` | Resolved UID per partner (e.g., `X-ts-uid2`). One header per partner with a resolved UID. **Capped at 20 partners** — partners sorted by most-recently synced; excess partners are omitted silently. |
| `X-ts-ec-consent`   | `ok` or `denied`                                             |

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

The auction handler (`crates/common/src/auction/`) is modified to inject EC identity into outbound OpenRTB requests. This is **not** a builder tweak — it requires explicit schema additions across multiple files.

**EC + SyntheticID coexistence (transitional — cutover is out of scope for this spec):**

EC is the authoritative identity signal where present; SyntheticID continues to run alongside it during the transition. Removal of SyntheticID generation, its cookies, and its response headers is a follow-on spec.

| Concern | This-spec behavior |
|---------|-------------------|
| `UserInfo.id` | Add `ec_id: Option<String>` to `UserInfo`. When EC is present, `ec_id = Some(ec_value)`. `id` continues to hold synthetic ID unchanged. |
| Outbound OpenRTB `user.id` | Set to `ec_value` when EC present, `synthetic_id` otherwise. |
| `X-Synthetic-*` response headers | **Kept unchanged** — transitional compatibility. Removal is a follow-on. |
| `X-ts-ec` response header | Added alongside `X-Synthetic-*` when EC is present. |
| Publisher and integration proxy paths | Both `get_or_generate_synthetic_id()` and `ec_context.generate_if_needed()` run. |
| `convert_tsjs_to_auction_request()` | Add `ec_context: Option<&EcContext>` parameter alongside existing synthetic logic. |

**Schema changes required before handler changes:**

| File           | Change                                                                                                                          |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `types.rs`     | Add `ec_id: Option<String>` to `UserInfo`. Add `Eid` and `EidUid` OpenRTB 2.6 types. No removals.                             |
| `openrtb.rs`   | Add `eids: Vec<Eid>` and `consent: Option<String>` to `User` struct. Keep `ext.synthetic_fresh`.                              |
| `prebid.rs`    | Populate `user.id` from EC when present (fall back to synthetic). Add `user.eids`, `user.consent`. Keep existing synthetic fields. |
| `formats.rs`   | Accept `ec_context: Option<&EcContext>`. Keep `get_or_generate_synthetic_id()` calls.                                          |
| `endpoints.rs` | Pass `ec_context` to `convert_tsjs_to_auction_request()`. Add `X-ts-ec` header. Keep `X-Synthetic-*`.                         |

These changes affect the OpenRTB wire format — confirm with engineering that no existing SSP integrations break before merging.

### 12.2 `user` object injection

When an `EcContext` is available on the request, the auction handler performs an explicit KV read before building the OpenRTB request:

```rust
// In handle_auction():
let kv_entry = kv.get(ec_context.ec_hash()?).ok().flatten();

user.id = ec_context.ec_value.clone();  // full hash.suffix
user.consent = consent_string;           // TCF string from ec_context.consent, else None
user.eids = match kv_entry {
    Some((entry, _gen)) => build_eids_from_kv(&entry, partner_store),
    None => vec![],  // KV read failed or no entry — degrade gracefully, omit eids
};
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

| Header              | Value                                                |
| ------------------- | ---------------------------------------------------- |
| `X-ts-ec`           | `{ec_hash.suffix}` — when EC is present              |
| `X-ts-eids`         | Standard base64 (RFC 4648) of OpenRTB 2.6 `user.eids` JSON array. Capped at 4 KB — same truncation rules as §11.5. |
| `X-ts-eids-truncated` | `true` — present only when `X-ts-eids` was truncated |
| `X-ts-ec-consent`   | `ok` or `denied`                                     |
| `X-Synthetic-ID`    | **Transitional** — kept while SyntheticID cutover is pending |
| `X-Synthetic-Fresh` | **Transitional** — kept while SyntheticID cutover is pending |
| `X-Synthetic-Trusted-Server` | **Transitional** — kept while SyntheticID cutover is pending |

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
    pub fn upsert(&self, record: &PartnerRecord) -> Result<(), Report<TrustedServerError>>;

    /// Looks up the partner owning a given API key hash (for batch sync auth).
    /// Iterates all partner records — called once per batch request, not per mapping.
    pub fn find_by_api_key_hash(&self, hash: &str) -> Result<Option<PartnerRecord>, Report<TrustedServerError>>;

    /// Returns all partner records with `pull_sync_enabled == true`.
    /// Used by the pull sync dispatcher after each organic request.
    pub fn pull_enabled_partners(&self) -> Result<Vec<PartnerRecord>, Report<TrustedServerError>>;
}
```

Partner records are stored as JSON values in `partner_store` KV, keyed by `partner_id`.

### 13.2 Admin endpoint (`POST /admin/partners/register`)

**Module:** `ec/admin.rs`

> **Codebase invariant:** `Settings::ADMIN_ENDPOINTS` in `settings.rs` hard-codes the list of admin routes and its tests verify coverage. Adding `/admin/partners/register` requires updating that constant and the associated auth-coverage tests. Failure to do so will break existing tests. See `settings.rs:391,398` and the test at `settings.rs:1363,1395`.

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
POST /admin/partners/register
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
5. `partner_store.upsert(record)`. `503` on KV failure.
6. Return `201 Created` with the stored record (without `api_key_hash` raw value).

**Response:**

```json
{
  "id": "ssp_x",
  "name": "SSP Example",
  "registered_at": 1741824000
}
```

---

## 14. Configuration

### 14.1 New `EdgeCookie` settings struct

Added to `crates/common/src/settings.rs`:

```rust
#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct EdgeCookie {
    /// Publisher passphrase used as HMAC key for EC generation.
    /// Must be identical across all of the publisher's owned domains.
    /// Publishers sharing this value with partners form an identity-federated consortium.
    #[validate(custom(function = EdgeCookie::validate_passphrase))]
    pub passphrase: String,

    /// Fastly KV store name for the EC identity graph.
    pub ec_store: String,

    /// Fastly KV store name for the partner registry.
    pub partner_store: String,

    /// SHA-256 hex of the publisher admin token for `POST /admin/partners/register`.
    /// The plaintext token is provided in the `Authorization: Bearer` header;
    /// it is never stored in plaintext.
    pub admin_token_hash: String,

    /// Maximum concurrent pull sync calls dispatched per request.
    #[serde(default = "EdgeCookie::default_pull_sync_concurrency")]
    pub pull_sync_concurrency: usize,
}

impl EdgeCookie {
    fn validate_passphrase(passphrase: &str) -> Result<(), ValidationError>;
    // Rejects "passphrase" or empty string as placeholder.

    fn default_pull_sync_concurrency() -> usize { 3 }
}
```

Added to `Settings`:

```rust
pub struct Settings {
    // ... existing fields ...
    #[serde(default)]
    pub ec: EdgeCookie,
}
```

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

If the rate-limiting API is unavailable in the WASM target, fall back to a KV-based counter (`ec_store` key `rl:{partner_id}:{ec_hash}`, hourly TTL). Engineering to confirm API availability during Step 7 (pixel sync implementation).

### 14.4 Deprecation note

`settings.synthetic.counter_store` and `settings.synthetic.opid_store` are currently configured but unused. They are not removed in this iteration — a follow-on cleanup ticket will address them.

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

// Consent cookies
pub const COOKIE_TCF: &str = "euconsent-v2";
pub const COOKIE_GPP: &str = "gpp";

// No EC-specific geo/IP header constants — use req.get_client_ip_addr() and GeoInfo::from_request(req).
```

The following EC headers must be added to `INTERNAL_HEADERS` in `constants.rs` to ensure they are stripped before proxying to downstream backends:

- `HEADER_X_TS_EC` (`x-ts-ec`)
- `HEADER_X_TS_EIDS` (`x-ts-eids`)
- `HEADER_X_TS_EC_CONSENT` (`x-ts-ec-consent`)
- `HEADER_X_TS_EIDS_TRUNCATED` (`x-ts-eids-truncated`)
- Dynamic `X-ts-<partner_id>` headers — these cannot be registered statically. The current `INTERNAL_HEADERS` filter uses explicit names, not a wildcard. Engineering must either extend the filter to strip the full `x-ts-` prefix pattern or enumerate all active partner IDs at startup. This must be confirmed before shipping.

---

## 16. Error Handling

New error variants in `crates/common/src/error.rs`:

```rust
pub enum TrustedServerError {
    // ... existing variants ...

    /// Edge Cookie operation failed.
    #[display("Edge Cookie error: {message}")]
    EdgeCookie { message: String },
    // Maps to StatusCode::INTERNAL_SERVER_ERROR (500)
    // Used for: EC generation failure, req.get_client_ip_addr() returning None

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
(GET, "/sync") → handle_sync(settings, &ec_context, kv, partner_store, req)

// EC identity resolution — no auth required (consent-gated)
(GET, "/identify") → handle_identify(settings, &ec_context, kv, partner_store, &req)

// CORS preflight for /identify — must be registered explicitly, current router dispatches by exact method/path
(OPTIONS, "/identify") → cors_preflight_identify(settings, &req)

// S2S batch sync — partner API key auth (internal to handler)
(POST, "/api/v1/sync") → handle_batch_sync(settings, kv, partner_store, req)

// Partner registration — publisher admin auth enforced in-handler (Bearer token)
(POST, "/admin/partners/register") → handle_register_partner(settings, partner_store, req)
```

Route ordering: EC routes are inserted before the fallback `handle_publisher_request()`. The `/admin/partners/register` route is NOT covered by the `[[handlers]]` Basic Auth config — it validates `Authorization: Bearer <token>` against `settings.ec.admin_token_hash` inside `handle_register_partner()`. The `[[handlers]]` block in `trusted-server.toml` must NOT include `/admin/partners/register` in its pattern (or must be narrowed so it does not cover this path).

### 17.1 EC integration in `main.rs`

Follows the same pattern as `GeoInfo::from_request()` which already runs pre-routing (line 70):

```rust
// Pre-routing — read only, no generation (matches GeoInfo pattern).
// EcContext::read_from_request() extracts and stores client_ip internally
// (same req.get_client_ip_addr() call used by GeoInfo::from_request() above).
let mut ec_context = EcContext::read_from_request(&req, settings, geo_info.as_ref())?;
let kv = KvIdentityGraph::new(&settings.ec.ec_store);

// Route dispatch — req is moved (consumed) inside the matching arm
let result = match (method, path.as_str()) {
    // EC-specific routes — receive ec_context read-only
    (GET, "/sync")     => handle_sync(settings, &kv, partner_store, &req, &ec_context),
    (GET, "/identify") => handle_identify(settings, &kv, partner_store, &req, &ec_context),
    (OPTIONS, "/identify") => cors_preflight_identify(settings, &req),
    (POST, "/api/v1/sync") => handle_batch_sync(settings, &kv, partner_store, req),
    (POST, "/admin/partners/register") => handle_register_partner(settings, partner_store, req),

    // /auction — EC-read-only; never generates EC
    (POST, "/auction") => handle_auction(settings, orchestrator, &kv, req, &ec_context).await,

    // Organic routes — generate EC if needed, then dispatch
    (m, path) if integration_registry.has_route(&m, path) => {
        ec_context.generate_if_needed(&req, settings, &kv)?;
        integration_registry.handle_proxy(&m, path, settings, req, &ec_context).await
    },
    _ => {
        ec_context.generate_if_needed(&req, settings, &kv)?;
        handle_publisher_request(settings, integration_registry, req, &ec_context)
    },
};

// finalize_response runs on every route — enforces cookie write/deletion
finalize_response(settings, geo_info.as_ref(), &ec_context, &kv, &mut response);

// Send the response to the client first, then continue for background work.
// In Fastly Compute, calling send_to_client() flushes the response immediately;
// the WASM invocation continues running after the client connection is released.
response.send_to_client();

// Background pull sync — fires outbound HTTP calls using send_async() (non-blocking).
// req is already consumed above; client_ip is read from ec_context (stored at construction).
// pull_enabled_partners() returns only records with pull_sync_enabled == true.
if let (Some(ip), Ok(pull_partners)) = (ec_context.client_ip, partner_store.pull_enabled_partners()) {
    pull_sync_dispatcher.dispatch_background(&ec_context, ip, &pull_partners, &kv);
}
```

`PullSyncDispatcher::dispatch_background` uses `Request::send_async()` for each partner call — it fires the outbound HTTP requests and collects the `PendingRequest` handles, then awaits them with a concurrency cap of `settings.ec.pull_sync_concurrency`. This does not add latency to the user-facing response because `send_to_client()` has already been called.

---

## 18. Testing Strategy

Follow the project's **Arrange-Act-Assert** pattern. Test both happy paths and error conditions. Use `expect()` with `"should ..."` messages.

### 18.1 Unit tests

Each module in `ec/` has a `#[cfg(test)]` module covering:

| Module          | Key test cases                                                                                 |
| --------------- | ---------------------------------------------------------------------------------------------- |
| `identity.rs`   | IPv4/IPv6 normalization, /64 truncation, HMAC determinism, output format                       |
| `consent.rs`    | `ec_consent_granted()`: each `Jurisdiction` variant, fail-closed `Unknown` case                |
| `cookie.rs`     | Cookie string format, Max-Age=0 for deletion, domain derivation                                |
| `kv.rs`         | Serialization/deserialization roundtrip, CAS merge logic, metadata extraction                  |
| `partner.rs`    | API key hash verification (constant-time), record serialization                                |
| `sync_pixel.rs` | All `ts_synced` redirect codes, 429 rate limit, return URL construction                        |
| `sync_batch.rs` | Status code selection (200/207/401/400/429), per-mapping rejection reasons, API-key rate limit |
| `pull_sync.rs`  | Trigger conditions, null/404 no-op, dispatch limit                                             |
| `identify.rs`   | All response codes (200/403/204), degraded flag, `uids` filtering                              |

### 18.2 Integration tests

KV behavior is tested with Viceroy (local Fastly Compute simulator) using real KV store operations. Key scenarios:

- Consent withdrawal: cookie deletion + KV delete in same request
- Concurrent writes: CAS retry logic under simulated generation conflicts
- KV degraded: EC cookie still set when KV create fails
- Full sync-and-identify flow: pixel sync writes, then `/identify` returns the uid

**Eventually-consistent caveat:** Fastly KV does not guarantee read-after-write consistency. Acceptance criteria that require a sync write to be immediately visible to a subsequent `/identify` read are written too strongly for the production platform. Integration tests under Viceroy may exhibit different consistency behavior than production. Tests for the sync→identify flow should either use retry with backoff (up to 1s) or be documented as a Viceroy-only behavior that is eventually consistent in production.

### 18.3 JS tests (if applicable)

If any JS changes are made for EC (e.g., publisher-side `/identify` fetch helper in `crates/js/`), use Vitest with `vi.hoisted()` for mocks.

---

## 19. Implementation Order

Suggested order to minimize risk and allow incremental testing. Each step should pass `cargo test --workspace` before the next begins.

| Step | Scope                                                     | Deliverable                                                                         |
| ---- | --------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| 1    | `ec/identity.rs` + constants + settings                   | `generate_ec()`, `normalize_ip()`, `EcContext`                                      |
| 2    | `ec/consent.rs`                                           | `ec_consent_granted()` gating layer (consent pipeline is a prerequisite)            |
| 3    | `ec/cookie.rs`                                            | Cookie creation, deletion, response header                                          |
| 4    | `ec/kv.rs`                                                | `KvIdentityGraph` CRUD with CAS                                                     |
| 5    | `ec/partner.rs` + `ec/admin.rs`                           | `PartnerStore`, `/admin/partners/register`                                          |
| 6    | EC middleware in `main.rs`, `publisher.rs`, `registry.rs` | `EcContext::read_from_request()` pre-routing, `generate_if_needed()`, `finalize_response()` |
| 7    | `ec/sync_pixel.rs`                                        | `GET /sync` handler + route                                                         |
| 8    | `ec/identify.rs`                                          | `GET /identify` handler + route                                                     |
| 9    | `ec/sync_batch.rs`                                        | `POST /api/v1/sync` handler + route                                                 |
| 10   | `ec/pull_sync.rs`                                         | Async pull dispatch after response                                                  |
| 11   | Auction integration                                       | EC injection into `user.id`, `user.eids`, `user.consent`                            |
| 12   | End-to-end integration tests                              | Viceroy-based flow tests                                                            |

---

## 20. Epic and Stories

### Epic: Implement Server-Side Cookie (SSC) identity system

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
  and `X-ts-ec` header, sets `cookie_was_present`, `ec_was_present`, `ec_value`.
  Does not generate. Does not write KV.
- `EcContext::generate_if_needed(req, settings, kv)` generates a new EC when
  `ec_value == None && consent == Granted`, sets `ec_generated = true`, and writes
  the initial KV entry via `kv.create()` (best-effort).
- `[ec]` settings block parses from TOML: `enabled`, `passphrase`, `ec_store`,
  `partner_store`, `pull_sync_concurrency`.
- All unit tests in `identity.rs` pass (HMAC determinism, format, IP normalization).

**Spec ref:** §2, §3, §4, §5.4, §14.1

---

### Story 2 — EC consent gating layer *(prerequisite: consent pipeline already merged)*

Add `ec_consent_granted()` — the thin EC-specific gating function that derives a
grant/deny decision from the pre-existing `ConsentContext`.

**Scope:** `ec/consent.rs` (new file; consent pipeline itself is a prerequisite)

**Acceptance criteria:**

- `ec_consent_granted(consent: &ConsentContext) -> bool` is implemented per §6.1.1.
  - `Jurisdiction::Gdpr` → requires `has_storage_consent()` and `!expired`
  - `Jurisdiction::UsState(_)` → requires `!gpc` and no CCPA opt-out
  - `Jurisdiction::NonRegulated` → `true`
  - `Jurisdiction::Unknown` → `false` (fail-closed)
- Unit tests cover each `Jurisdiction` variant × signal combination.

**Spec ref:** §6.1.1

---

### Story 3 — EC cookie helpers

Implement the functions that create and delete the `ts-ec` cookie on responses,
and wire them into `finalize_response()`.

**Scope:** `ec/cookie.rs`, `finalize_response()` in `main.rs`

**Acceptance criteria:**

- `create_ec_cookie()` produces a cookie with `Domain=.{publisher.domain}`,
  `Max-Age=31536000`, `SameSite=Lax; Secure`. `HttpOnly` is NOT set
  (JS on the publisher page must be able to read the cookie).
- `delete_ec_cookie()` produces a cookie with `Max-Age=0`, same attributes.
- `set_ec_on_response()` sets `Set-Cookie` and `X-ts-ec` response headers.
- `finalize_response()` signature updated to accept `ec_context: &EcContext` and `kv: &KvIdentityGraph`.
- `finalize_response()` deletes the cookie and calls `kv.write_withdrawal_tombstone()` when
  `!ec_consent_granted(&consent) && cookie_was_present`.
- `finalize_response()` sets the cookie only when `ec_generated == true`.
  No other cookie writes occur. No `suppress_mutation` flag.
- Unit tests cover cookie string format, Max-Age=0 deletion, domain derivation.

**Spec ref:** §5.1, §5.3, §5.4, §17 (finalize_response)

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
  `consent.ok = true` using CAS; retries up to 3 times on generation conflict.
- `KvIdentityGraph::create_or_revive(ec_hash, &entry)` creates a new entry OR
  overwrites an existing tombstone (`consent.ok = false`) with a fresh entry;
  no-ops if a live entry already exists. Called by `generate_if_needed()`.
- `KvIdentityGraph::update_last_seen(ec_hash)` updates `last_seen` without
  overwriting partner IDs (CAS merge), and only writes if the stored value is
  more than 300s old (debounce to avoid 1 write/sec KV limit).
- `KvIdentityGraph::write_withdrawal_tombstone(ec_hash)` sets `consent.ok = false`,
  clears partner IDs, and applies a 24-hour TTL (see §6.3).
- `kv.upsert_partner_id(ec_hash, partner_id, uid, timestamp)` writes to
  `ids[partner_id]` and skips if existing `synced >= timestamp` (idempotent).
- KV schema matches §7 exactly (JSON roundtrip test).
- Unit tests cover CAS merge logic, tombstone write, serialization/deserialization
  roundtrip, metadata extraction.

**Spec ref:** §4, §5.4, §6.3

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
- API key stored as SHA-256 hex; plaintext never written to KV.
- `verify_api_key()` uses constant-time comparison.
- `POST /admin/partners/register` validates `Authorization: Bearer <token>` inside
  the handler against `settings.ec.admin_token_hash` (constant-time SHA-256 comparison).
  Returns `401` if missing or invalid — before any request body is read.
- Admin endpoint validates: `pull_sync_url` hostname must be in
  `pull_sync_allowed_domains` when set — returns `400` otherwise.
- Returns `201 Created` with the stored record on success; `400` on validation
  failure; `503` on KV failure.
- `/admin/partners/register` is added to `Settings::ADMIN_ENDPOINTS` in
  `settings.rs` and the auth-coverage tests pass (`settings.rs:1363,1395`).
- Unit tests cover API key hash verification and record serialization.

**Spec ref:** §13

---

### Story 6 — EC middleware integration

Wire `EcContext` into the request pipeline following the two-phase model
(§5.4 and §17.1). `EcContext::read_from_request()` runs pre-routing like
`GeoInfo`; `generate_if_needed()` runs inside organic handlers only.

**Scope:** `main.rs`, `publisher.rs`, `registry.rs` (route wiring only — no new modules)

**Acceptance criteria:**

- `EcContext::read_from_request()` is called before the route match on every
  request, passed the existing `geo_info` (no duplicate geo header parsing).
- EC-specific and EC-read-only route handlers (`/sync`, `/identify`, `/auction`,
  `/api/v1/sync`, `/admin/*`) receive `ec_context` in read-only form — they never
  call `generate_if_needed()`. `/auction` consumes EC identity but never bootstraps it.
- `handle_publisher_request()` and `integration_registry.handle_proxy()` call
  `ec_context.generate_if_needed(&req, settings, &kv)` before their handler logic.
- `finalize_response()` receives `ec_context` and `kv` and:
  - Deletes the EC cookie and writes a withdrawal tombstone if consent is withdrawn (runs on all routes).
  - Sets a new `Set-Cookie` only when `ec_context.ec_generated == true`.
- No existing route behavior changes — EC context is additive.
- `cargo test --workspace` passes with no regressions.

**Spec ref:** §5, §17

---

### Story 7 — Pixel sync (`GET /sync`)

Implement the pixel-based ID sync endpoint that partners use to write their
user ID against an EC hash.

**Scope:** `ec/sync_pixel.rs`, router update

**Acceptance criteria:**

- Missing required query params (`partner`, `uid`, `return`) → `400`.
- No `ts-ec` cookie → redirect to `{return}?ts_synced=0&ts_reason=no_ec`.
- Unknown `partner` ID → `400`.
- `return` URL hostname not in `partner.allowed_return_domains` → `400`.
- Consent uses `ec_context.consent`. The optional `consent` query param is a fallback
  only: it is used exclusively when `ec_context.consent.is_empty()`
  (no X-consent-advertising header and no framework cookie on the request).
  When a fresher signal exists, the param is ignored. Does not mutate `ec_context`.
  Denied or absent → redirect to `{return}?ts_synced=0&ts_reason=no_consent`.
- Rate limit exceeded → `429 Too Many Requests` (no redirect).
- KV write failure → redirect to `{return}?ts_synced=0&ts_reason=write_failed`.
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

- No `ts-ec` cookie AND no `X-ts-ec` header (`ec_was_present == false`) and `!ec_consent_granted(consent)` → `403 Forbidden`.
- No `ts-ec` cookie AND no `X-ts-ec` header (`ec_was_present == false`) and consent not denied → `204 No Content`.
- Valid EC, consent granted, KV read succeeds → `200` with full JSON body
  including `ec`, `consent`, `uids`, `eids`.
- `uids` filtered to partners where `bidstream_enabled = true` and consent
  granted.
- KV read failure → `200` with `degraded: true` and empty `uids`/`eids`.
- No `Origin` header (server-side proxy): process normally, no CORS headers, no `403`.
- `Origin` header present and matches `publisher.domain` or subdomain: reflect in
  `Access-Control-Allow-Origin` + `Vary: Origin`.
- `Origin` header present but does not match: `403`, no body.
- `OPTIONS /identify` preflight → `200` with CORS headers, no body.
- `generate_if_needed()` is never called — no new EC generated, no `Set-Cookie`.
- Response time target: 30ms p95 (documented, not gate).
- Unit tests cover all response codes, degraded flag, `uids` filtering,
  CORS origin validation.

**Spec ref:** §11

---

### Story 9 — S2S batch sync (`POST /api/v1/sync`)

Implement the server-to-server batch sync endpoint for partners to bulk-write
their UIDs against a list of EC hashes.

**Scope:** `ec/sync_batch.rs`, router update

**Acceptance criteria:**

- Missing or invalid `Authorization: Bearer` → `401`.
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

Implement the async background task that calls partner resolution endpoints
after a response is flushed, when trigger conditions are met.

**Scope:** `ec/pull_sync.rs`

**Acceptance criteria:**

- Dispatch only when: EC present, consent granted, `pull_sync_enabled = true`,
  and either no existing partner entry or existing `synced` is older than
  `pull_sync_ttl_sec`.
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
- Dispatch is non-blocking — does not add latency to the user-facing response.
- Unit tests cover trigger conditions, null/404 no-op, domain allowlist check,
  dispatch limit enforcement.

**Spec ref:** §10

---

### Story 11 — Auction bidstream decoration

Inject EC identity data into outbound OpenRTB bid requests for publishers with
`bidstream_enabled = true` partners.

**Scope:** Auction handler (Mode B path in existing auction code)

**Acceptance criteria:**

- `user.id` set to `ec_context.ec_value` (the full `hash.suffix` string) when EC present and consent granted; falls back to `synthetic_id` when EC is absent (matching §12.1 coexistence table — EC is authoritative where present, synthetic otherwise).
- `user.eids` populated with one entry per `bidstream_enabled` partner that
  has a synced UID, using `partner.source_domain` and `partner.openrtb_atype`.
- `user.consent` set to `ec_context.consent.raw_tc_string` when present.
- No EID entry written for partners with no synced UID.
- KV read failure → `user.eids` omitted (empty); `user.id` still set from EC or synthetic fallback; auction proceeds without EID data (no 5xx).
- No EC present → `user.id` set from synthetic fallback; `user.eids` is empty.
- Unit tests cover EID structure, consent string threading, KV-degraded path.

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
  `/identify` with consent still denied returns `403` (no cookie + denied → §11.3);
  batch sync returns `consent_withdrawn` within the tombstone TTL.
- **KV degraded:** EC cookie is still set when KV create fails; `/identify`
  returns `degraded: true`.
- **Concurrent writes:** Two simultaneous EC creates for the same hash resolve
  without data loss (CAS retry).
- **Rate limits:** Pixel sync returns `429` after `sync_rate_limit` is
  exceeded; batch sync returns `429` after `batch_rate_limit` is exceeded.
- **Pull sync no-op:** Partner returning `{ "uid": null }` produces no KV
  write and no error log.
- All tests pass under `cargo test --workspace` with Viceroy.

**Spec ref:** §18.2

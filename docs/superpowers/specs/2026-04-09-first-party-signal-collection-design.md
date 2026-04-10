# First-Party Signal Collection — Design Spec

**Date:** 2026-04-09
**Status:** Draft

## Motivation

A Chrome user on a publisher domain carries first-party cookies from ~12 ad-tech
partners. When a household member visits on Safari or Firefox — where those cookies
were never set — the KV identity graph has no partner UIDs, bidstream is
unenriched, and CPM is degraded.

Seeding the KV entry from first-party cookie signals at request time means: by the
time the Safari browser arrives, the KV entry already has IDs and the `/identify`
response is fully populated without waiting for any sync event.

## Decisions

| #   | Decision                                                      | Rationale                                                                                                                                 |
| --- | ------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | **Post-send execution**                                       | Zero added latency for the Chrome user. Race window (Safari arriving within the same second) is acceptable.                               |
| 2   | **Extract pre-send, write post-send**                         | Cookie jar is available pre-send. Extraction is cheap (string ops). All KV I/O deferred to post-send.                                     |
| 3   | **Denormalized `_fp_signal_enabled` index**                   | Single KV read pre-send gives all partner extraction configs. Avoids 1+N reads on the hot path.                                           |
| 4   | **No Trade Desk `TDID_LOOKUP` guard**                         | YAGNI. If `TDID` is present and non-empty when `TDID_LOOKUP` is `"FALSE"`, it's a degenerate case handled by the standard empty-UID skip. |
| 5   | **UID2 expiry check during extraction (pre-send)**            | The 5-minute buffer makes the pre-send vs post-send timing difference (~100ms) irrelevant. Keeps extraction self-contained.               |
| 6   | **Batched CAS write (single read-modify-write)**              | All signals target the same KV entry. One read + one write instead of up to 12 CAS loops. Retry on conflict.                              |
| 7   | **Approach B: Core-driven extraction, adapter orchestration** | Core crate owns domain logic. Adapter orchestrates timing. Consistent with pull sync architecture.                                        |

## Architecture

### Approach

Core crate (`trusted-server-core`) exposes two functions:

- `extract_fp_signals(jar, configs, now_ms)` — pure extraction, called pre-send
- `write_fp_signals(kv, ec_id, signals, configs)` — batched CAS write, called post-send

The adapter crate orchestrates timing: extraction happens during request
handling, writing happens after the response is sent. This mirrors the pull
sync pattern (`dispatch_pull_sync` is a core function called by the adapter
post-send).

### Request Flow

```
Request received
  → EcContext::read_from_request_with_geo   (parse cookie jar, extract EC ID)
  → EcContext::generate_if_needed           (create EC + initial KV entry if new)
  → extract_fp_signals(jar, configs)        ← NEW (pre-send, cheap)
  → finalize + send response
  → run_fp_signal_collection_after_send     ← NEW (post-send, KV I/O)
  → run_pull_sync_after_send                (existing, post-send)
```

## Data Model

### New fields on `PartnerRecord`

Added to `PartnerRecord` in `ec/partner.rs`:

```rust
/// First-party cookie names that may carry this partner's UID.
/// Checked in order; first match wins.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub fp_signal_cookie_names: Vec<String>,

/// Dot-notation JSON path to extract the UID from a JSON cookie value.
/// When absent, the raw cookie value is used as the UID.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub fp_signal_json_path: Option<String>,

/// Minimum seconds between re-collection writes for this partner.
/// Defaults to 86400 (24 hours).
#[serde(default = "PartnerRecord::default_fp_signal_ttl_sec")]
pub fp_signal_ttl_sec: u64,
```

### Validation rules

- `fp_signal_cookie_names`: each name must be non-empty, ASCII, no semicolons
  or equals signs. Max 5 names per partner.
- `fp_signal_json_path`: if present, must be non-empty, only alphanumeric +
  dots + underscores. Max depth of 4 segments.
- `fp_signal_ttl_sec`: minimum 60 seconds, maximum 604800 (7 days).

### Denormalized `_fp_signal_enabled` index

KV key `_fp_signal_enabled` stores a `Vec<FpSignalPartnerConfig>`:

```rust
pub struct FpSignalPartnerConfig {
    pub partner_id: String,
    pub cookie_names: Vec<String>,
    pub json_path: Option<String>,
    pub ttl_sec: u64,
}
```

Maintained by `PartnerStore::upsert()` alongside the existing `_pull_enabled`
and `apikey:` indexes. A partner is included when `fp_signal_cookie_names` is
non-empty. Self-healing on next upsert.

### Extracted signal struct

```rust
pub struct FpSignal {
    pub partner_id: String,
    pub uid: String,
}
```

Passed from pre-send extraction to post-send writing.

## Extraction (pre-send)

### `extract_fp_signals`

```rust
pub fn extract_fp_signals(
    jar: &CookieJar,
    configs: &[FpSignalPartnerConfig],
    now_ms: u64,
) -> Vec<FpSignal>
```

**Algorithm:**

1. For each config, iterate `cookie_names` in order. First cookie found in the
   jar wins.
2. If `json_path` is `Some`, parse cookie value as JSON, walk dot-separated
   segments to a string leaf. On failure, log at debug and skip.
3. If `json_path` is `None`, use the raw cookie value.
4. If extracted UID is empty after trimming, skip.
5. **UID2 special case:** If the cookie value parses as JSON and contains
   `identity_expires`, check `identity_expires > now_ms + 300_000`. If not,
   skip. Do not store `refresh_token`.
6. Push `FpSignal { partner_id, uid }`.

### JSON path walker

```rust
fn extract_json_path(value: &str, path: &str) -> Option<String>
```

Split `path` on `.`, walk `serde_json::Value` tree, return `Some(string)` if
the leaf is a JSON string. Returns `None` on any failure.

### Cookie jar access

`EcContext` gains a `pub fn cookie_jar(&self) -> Option<&CookieJar>` accessor.
The jar is already parsed in `read_from_request_with_geo` — we store it in the
struct rather than discarding it.

### Caller site (adapter)

Runs when `is_real_browser && ec_context.ec_allowed()` and an EC value exists:

```rust
let fp_signals = if is_real_browser && ec_context.ec_allowed() && ec_context.ec_value().is_some() {
    let configs = partner_store.fp_signal_configs().unwrap_or_default();
    extract_fp_signals(ec_context.cookie_jar().unwrap(), &configs, now_ms)
} else {
    vec![]
};
```

## Writing (post-send)

### `write_fp_signals`

```rust
pub fn write_fp_signals(
    kv: &KvIdentityGraph,
    ec_id: &str,
    signals: &[FpSignal],
    configs: &[FpSignalPartnerConfig],
) -> Result<(), Report<FpSignalError>>
```

**Algorithm:**

1. Read KV entry for `ec_id` (entry + generation marker).
2. If missing or tombstoned (`consent.ok == false`), return — no writes.
3. Build `HashMap<partner_id, ttl_sec>` from configs for O(1) lookup.
4. For each signal: if `entry.ids` contains the partner and
   `now - existing.synced < ttl_sec`, skip (fresh).
5. Insert all non-skipped signals into `entry.ids` as
   `KvPartnerId { uid, synced: now }`.
6. If no insertions, return — no write needed.
7. CAS write with `if_generation_match`. On `ItemPreconditionFailed`, retry
   the full loop (re-read, re-check, re-insert) up to `MAX_CAS_RETRIES` (3).

### Adapter orchestration

```rust
fn run_fp_signal_collection_after_send(
    settings: &Settings,
    ec_id: &str,
    signals: &[FpSignal],
    configs: &[FpSignalPartnerConfig],
) {
    let kv = match require_identity_graph(settings) {
        Ok(kv) => kv,
        Err(err) => {
            log::debug!("FP signal collection: identity graph unavailable: {err:?}");
            return;
        }
    };
    if let Err(err) = write_fp_signals(&kv, ec_id, signals, configs) {
        log::warn!("FP signal collection failed: {err:?}");
    }
}
```

Called post-send, parallel to `run_pull_sync_after_send`. All errors logged and
swallowed.

## Index Maintenance

### `PartnerStore::upsert()` changes

After writing the partner record, a third index update step:

1. Read current `_fp_signal_enabled` index (empty vec if missing).
2. Remove any existing entry for this partner ID.
3. If the upserted record has non-empty `fp_signal_cookie_names`, push a new
   `FpSignalPartnerConfig`.
4. Write the index back.

Same best-effort, self-healing pattern as `_pull_enabled` and `apikey:` indexes.

### New accessor

```rust
pub fn fp_signal_configs(&self) -> Result<Vec<FpSignalPartnerConfig>, Report<PartnerStoreError>>
```

Reads and deserializes `_fp_signal_enabled`. Returns empty vec if key missing
or corrupt (degraded-behavior policy).

## Known Partner Cookie Mapping

Default partner configurations derived from autoblog.com Chrome cookie jar
(2026-04-02):

| Partner ID        | Cookie Names                             | JSON Path             | TTL (sec) | Notes                           |
| ----------------- | ---------------------------------------- | --------------------- | --------- | ------------------------------- |
| `id5`             | `["id5id"]`                              | `"universal_uid"`     | 86400     | JSON object                     |
| `trade_desk`      | `["pbjs-unifiedid"]`                     | `"TDID"`              | 86400     | JSON object                     |
| `liveramp_ats`    | `["idl_env"]`                            | —                     | 86400     | Raw envelope string             |
| `lockr`           | `["lockr_tracking_id"]`                  | —                     | 86400     | Raw UUID                        |
| `kargo`           | `["krg_uid"]`                            | `"v.userId"`          | 86400     | Doubly-nested JSON              |
| `prebid_sharedid` | `["sharedId", "_sharedid", "_sharedID"]` | —                     | 86400     | Multiple cookie names           |
| `lotame`          | `["panoramaId"]`                         | —                     | 86400     | Raw hex string                  |
| `audigent`        | `["_au_1d"]`                             | —                     | 86400     | Raw string                      |
| `yahoo_connectid` | `["connectId"]`                          | `"connectId"`         | 86400     | JSON object                     |
| `lotame_cc`       | `["_cc_id"]`                             | —                     | 86400     | Raw hex string                  |
| `uid2`            | `["__uid2_advertising_token"]`           | `"advertising_token"` | 3600      | Expiry-gated; see UID2 handling |
| `arena`           | `["ArenaID", "_ig"]`                     | —                     | 86400     | Arena Group first-party ID      |

This table ships as the default partner registry seed via
`POST /_ts/admin/partners/register`.

## UID2 Special Handling

The `__uid2_advertising_token` cookie contains a short-TTL JSON object:

```json
{
  "advertising_token": "A4AAADA...",
  "refresh_token": "AAAAMCQR...",
  "identity_expires": 1775421703943,
  "refresh_expires": 1777754503943,
  "refresh_from": 1775166103943
}
```

**Harvest policy:**

- Only write `advertising_token` if `identity_expires > now_ms + 300_000`
  (at least 5 minutes of validity remaining).
- Never store `refresh_token` — it is a credential, not an identity signal.
- `fp_signal_ttl_sec` is 3600 to align with token lifetime.
- UID2 token refresh (calling `/token/refresh`) is out of scope.

The expiry check runs during extraction (pre-send). If the token fails the
check, it is not included in the `FpSignal` vec.

## Consent Gating

First-party signal collection inherits the existing consent check. Extraction
only runs when `ec_context.ec_allowed()` is true. Writing only proceeds if the
KV entry is not tombstoned. No additional consent logic needed.

## Error Handling

- Extraction errors (bad JSON, missing path): logged at `debug`, partner skipped.
- Write errors (KV unavailable, CAS exhaustion): logged at `warn`, swallowed.
- A collection failure never affects the client response.

## Performance

**Pre-send (hot path):**

- One KV read for the `_fp_signal_enabled` index.
- String operations on cookie values — no I/O.
- Bounded by number of configured partners (~12).

**Post-send (off critical path):**

- One KV read (entry + generation) + one KV write (batched upsert).
- Returning users with all-fresh UIDs: one read, zero writes.
- First visit with 12 partner cookies: one read + one write.
- CAS retry on conflict: up to 3 attempts (matches existing pattern).

## Module Structure

### New file: `ec/fp_signals.rs`

Contains:

- `FpSignalPartnerConfig` — denormalized extraction config
- `FpSignal` — extracted partner ID + UID pair
- `FpSignalError` — module error type
- `extract_fp_signals()` — pure extraction function
- `extract_json_path()` — private JSON path walker
- `write_fp_signals()` — batched CAS write

### Files touched

| File                             | Change                                                                                                                                                            |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ec/partner.rs`                  | Add 3 fields to `PartnerRecord`, validation, `default_fp_signal_ttl_sec()`                                                                                        |
| `ec/partner.rs` (`PartnerStore`) | Maintain `_fp_signal_enabled` index in `upsert()`, add `fp_signal_configs()`                                                                                      |
| `ec/fp_signals.rs`               | New module                                                                                                                                                        |
| `ec/mod.rs`                      | Declare `pub mod fp_signals`, add `cookie_jar: Option<CookieJar>` field and `cookie_jar()` accessor to `EcContext`, store jar during `read_from_request_with_geo` |
| `main.rs` (adapter)              | Pre-send: extract signals. Post-send: write signals.                                                                                                              |

## Testing Strategy

### Unit tests in `ec/fp_signals.rs`

1. **JSON path extraction:** single key, nested dot path, missing key,
   non-string leaf, invalid JSON, empty path (raw value).
2. **Cookie extraction:** single cookie match, first-match-wins across
   multiple names, missing cookie skips, empty UID skips, JSON path from
   cookie value.
3. **UID2 special case:** valid token extracted, expired token skipped,
   missing `identity_expires` treated as non-UID2.
4. **Batched write:** all fresh (no write), mix of fresh and stale, all new,
   tombstoned entry (no write), missing entry (no write).

### Unit tests in `ec/partner.rs`

5. **Validation:** valid FP signal config accepted, empty cookie name
   rejected, invalid JSON path rejected, TTL out of bounds rejected.
6. **Index maintenance:** upsert with FP config adds to index, upsert
   without FP config removes from index.

### Integration

Existing `cargo test --workspace` via Viceroy exercises the full KV path.
Batched CAS write tests use Viceroy KV store.

## Out of Scope

- UID2 token refresh (requires separate operator integration).
- Trade Desk `TDID_LOOKUP` guard field.
- Automatic partner discovery (partners are registered via admin API).
- Client-side signal collection (this is server-side only).

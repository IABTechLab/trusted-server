---
status: draft
---

# EC KV Store Seeding

**Status:** Draft
**Author:** jevans / TS Product
**Last updated:** 2026-04-03
**Depends on:** IABTechLab/trusted-server#582 (EC identity system)
**Extends:** `docs/superpowers/specs/2026-03-24-ssc-prd-design.md` (§8 KV Store Identity Graph)
**Also see:** `docs/superpowers/specs/2026-04-02-ec-kv-schema-extensions-design.md` (§5 `KvDevice`)

---

## Overview

PR #582 establishes three KV write paths: pixel sync (`GET /sync`), S2S batch
push (`POST /_ts/api/v1/sync`), and pull sync (post-send background fetch). All
three require a prior sync event — a partner pixel, a batch job, or a registered
pull endpoint — before any UID lands in the identity graph.

This spec adds two seeding paths that populate the identity graph **before any
sync event**, using signals already present on the organic request:

1. **First-party signal collection** — read known partner ID cookies that the
   browser already carries, extract UIDs, and write them to KV at request time.
   Zero new infrastructure required on the publisher side; the partner's
   existing client-side tags have already done the work.

2. **HEM resolution** — when a publisher sends a hashed email via a request
   header, call LiveRamp's API server-side to resolve a RampID and write it to
   KV. Scoped to publishers with meaningful login rates; not applicable to
   open-web publishers like autoblog.com where login rates are sub-1%.

Both paths respect existing consent gating (`consent.ok = true`) and use the
existing `upsert_partner_id` write path in `kv.rs`.

A third, pre-seeding concern sits above both: **bot detection**. All KV write
paths — EC creation, first-party signal collection, HEM resolution, and
cross-browser propagation — are gated on `known_browser`. Non-human clients
bypass the graph entirely and receive a pass-through response.

---

## 0. Bot Detection Gate

### 0.1 Rationale

Fastly KV Store charges per operation at list price (Class A writes: $0.65 /
100 k; Class B reads: $0.55 / M; we assume other CDNs will follow similar
pricing or be cheaper). Bots, crawlers, and LLM scrapers are high-volume,
low-value clients: advertisers will not bid on them, they inflate write costs,
and their presence pollutes the identity graph. Writing a KV entry for a
non-human client produces zero CPM lift and measurable cost.

Bot detection also provides forward-compatibility with RSL / HTTP 402 responses
(see issue [#81](https://github.com/IABTechLab/trusted-server/issues/81)).

### 0.2 Detection signals

Bot classification uses three signals computed at the edge before any KV I/O:

| Signal                   | Source                  | How used                                               |
| ------------------------ | ----------------------- | ------------------------------------------------------ |
| `known_browser`          | JA4 Section 1 allowlist | Primary gate — see below                               |
| `ja4_class` cipher count | `req.get_tls_ja4()`     | Count > 25 → confirmed bot                             |
| UA string                | `User-Agent` header     | Platform class derivation; empty/absent → treat as bot |

JA4 is available via `req.get_tls_ja4()` in the Fastly Compute Rust SDK.
H2 fingerprint is available via `req.get_client_h2_fingerprint()`.

**Known browser allowlist** (Section 1 of JA4 only — browser class, not
unique device):

| Browser              | `ja4_class` (JA4 §1) |
| -------------------- | -------------------- |
| Chrome / Chromium    | `t13d1516h2`         |
| Safari (Mac and iOS) | `t13d2013h2`         |
| Firefox              | `t13d1717h2`         |

Any JA4 Section 1 value not in this allowlist sets `known_browser = null`.
Confirmed bot patterns (cipher count > 25, or curl/libcurl fingerprints) set
`known_browser = false`.

### 0.3 Gate logic

```
known_browser = classify_client(ja4, user_agent)

if known_browser is false or known_browser is null:
    → Skip EC creation
    → Skip cookie write
    → Skip first-party signal collection
    → Skip HEM resolution
    → Skip cross-browser propagation
    → Pass request through to origin unchanged
    → Return
```

Both `false` (confirmed bot) and `null` (unrecognised client) block all KV
operations. The distinction between the two values is preserved in `KvDevice`
for future routing logic but has no behavioural difference in this version.

### 0.4 Updated request flow

```
Request received
  → classify_client(ja4, ua)              ← NEW — bot gate
      known_browser false or null → pass through, return
  → EcContext::read_from_request
  → EcContext::generate_if_needed
  → collect_first_party_signals(ec, jar)
  → finalize + send response
  → dispatch_pull_sync (post-send)
  → dispatch_hem_resolution(ec, hem)      (post-send, if applicable)
```

### 0.5 Forward-compatibility: RSL / HTTP 402

The current behaviour for bots is a silent pass-through — the request reaches
origin as if TS were not present. This is intentionally minimal.

A future iteration (tracked in issue
[#81](https://github.com/IABTechLab/trusted-server/issues/81)) will evolve this
into a conditional HTTP 402 response for unlicensed crawlers, using the IAB
Real Simple Licensing (RSL) / Open License Protocol (OLP). When that work
lands, the `known_browser = false` branch becomes the insertion point for the
402 challenge. The `null` branch may continue to pass through, or may be routed
to a separate RSL handler — TBD in the issue #81 spec.

No structural changes to the gate logic will be needed; the branching point is
already isolated.

### 0.6 Cost impact

At 100 M monthly uniques, bot filtering is estimated to reduce KV writes by
~15–25 % based on industry crawl-rate benchmarks. Full cost modelling at
100 M uniques: ~$6,500 month 1 (high write volume), ~$3,400/month steady
state — with bot filtering applied before any KV operation.

---

## 1. First-party Signal Collection

### 1.1 Motivation

A Chrome user on autoblog.com already carries IDs from ~12 partners in their
first-party cookie jar. These IDs are readable by TS server-side on the
publisher's own domain. When the same user's household visits on Safari or
Firefox — where those cookies were never set — the identity graph has no
partner UIDs, bidstream is unenriched, and CPM is degraded.

Seeding the KV entry from first-party signals at request time means: by the
time that user's Safari browser arrives, the KV entry already has IDs, and the
`/identify` response is fully populated without waiting for any sync event.

### 1.2 Approach: PartnerRecord-driven signal table

First-party signal collection configuration lives on `PartnerRecord`, not in
`trusted-server.toml`. `PartnerRecord` is already the single source of truth
for all per-partner behaviour; adding signal collection config there keeps it
consistent with pull sync, batch sync, and bidstream settings.

**Alternative considered:** a separate `[[ec.fp_signals]]` TOML table.
Rejected because it splits partner config across two locations, requiring
operators to keep them in sync manually.

New fields on `PartnerRecord`:

```rust
/// One or more first-party cookie names that may carry this partner's UID.
/// Checked in order; first match wins.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub fp_signal_cookie_names: Vec<String>,

/// Optional JSON path to extract the UID from a cookie whose value is a
/// JSON object (e.g. `"universal_uid"` for id5, `"v.userId"` for kargo).
/// When absent, the raw cookie value is used as the UID.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub fp_signal_json_path: Option<String>,

/// Minimum seconds between re-collection writes for this partner.
/// Prevents write thrashing when the cookie changes on every request.
/// Defaults to 86400 (24 hours). Re-collection is skipped if
/// `ids[partner].synced` is within this window.
#[serde(default = "PartnerRecord::default_fp_signal_ttl_sec")]
pub fp_signal_ttl_sec: u64,
```

### 1.3 Known partner cookie mapping

Derived from a real autoblog.com Chrome cookie jar (2026-04-02):

| Partner ID        | `fp_signal_cookie_names`                 | `fp_signal_json_path` | Notes                                               |
| ----------------- | ---------------------------------------- | --------------------- | --------------------------------------------------- |
| `id5`             | `["id5id"]`                              | `"universal_uid"`     | Value is JSON object                                |
| `trade_desk`      | `["pbjs-unifiedid"]`                     | `"TDID"`              | Value is JSON object; check `TDID_LOOKUP == "TRUE"` |
| `liveramp_ats`    | `["idl_env"]`                            | —                     | Raw envelope string; opaque to TS                   |
| `lockr`           | `["lockr_tracking_id"]`                  | —                     | Raw UUID string                                     |
| `kargo`           | `["krg_uid"]`                            | `"v.userId"`          | Doubly-nested JSON                                  |
| `prebid_sharedid` | `["sharedId", "_sharedid", "_sharedID"]` | —                     | Multiple cookie names, same UID                     |
| `lotame`          | `["panoramaId"]`                         | —                     | Raw hex string                                      |
| `audigent`        | `["_au_1d"]`                             | —                     | Raw string                                          |
| `yahoo_connectid` | `["connectId"]`                          | `"connectId"`         | Value is JSON object                                |
| `lotame_cc`       | `["_cc_id"]`                             | —                     | Raw hex string                                      |
| `uid2`            | `["__uid2_advertising_token"]`           | `"advertising_token"` | Short-TTL token; see §1.6                           |
| `arena`           | `["ArenaID", "_ig"]`                     | —                     | Arena Group first-party ID                          |

This table ships as the default partner registry seed. Publishers register
partners via `POST /_ts/admin/partners/register`; the first-party signal
fields are included in the registration payload.

### 1.4 Request flow

First-party signal collection runs in the organic handler after
`EcContext::generate_if_needed` and before response finalization. It is
post-EC and post-consent — the EC must exist and consent must be `ok` before
any write occurs.

```
Request received
  → EcContext::read_from_request          (parse cookie jar, extract EC ID)
  → EcContext::generate_if_needed         (create EC + initial KV entry if new)
  → collect_first_party_signals(ec, jar)  ← NEW
  → finalize + send response
  → dispatch_pull_sync (post-send)
```

`collect_first_party_signals` pseudocode:

```
for partner in partner_store.all_with_fp_signal_config():
    uid = extract_uid_from_jar(jar, partner.fp_signal_cookie_names, partner.fp_signal_json_path)
    if uid is None: continue

    existing = kv.get(ec_id).ids.get(partner.id)
    if existing and now - existing.synced < partner.fp_signal_ttl_sec: continue

    kv.upsert_partner_id(ec_id, partner.id, uid, now)
    log::debug!("Collected first-party {} UID for EC {}", partner.id, ec_hash)
```

The existing `upsert_partner_id` CAS loop handles concurrent writes safely —
no new concurrency logic needed.

### 1.5 JSON path extraction

`fp_signal_json_path` uses dot-notation for nested fields:

| Path              | Input                                     | Extracted             |
| ----------------- | ----------------------------------------- | --------------------- |
| `"universal_uid"` | `{"universal_uid":"ID5*...","version":1}` | `"ID5*..."`           |
| `"v.userId"`      | `{"v":{"userId":"d8f4..."}}`              | `"d8f4..."`           |
| `"connectId"`     | `{"connectId":"7vsQ..."}`                 | `"7vsQ..."`           |
| _(absent)_        | `16d913a7-d56c-...`                       | `"16d913a7-d56c-..."` |

Implementation: split on `.`, walk the JSON tree, extract string leaf. If
parsing fails or path is missing, log at `debug` and skip — never error.

### 1.6 UID2 special handling

`__uid2_advertising_token` contains a short-TTL advertising token
(`identity_expires` is ~1 hour from issue). The cookie value is a JSON object:

```json
{
  "advertising_token": "A4AAADA...",
  "refresh_token": "AAAAMCQR...",
  "identity_expires": 1775421703943,
  "refresh_expires": 1777754503943,
  "refresh_from": 1775166103943
}
```

Harvest policy: only write the advertising token if `identity_expires >
now_ms + 300_000` (at least 5 minutes of validity remaining). Do not store
the refresh token in KV — it is a credential, not an identity signal.

Set `fp_signal_ttl_sec = 3600` for UID2 to align with token lifetime.

UID2 token refresh (calling the UID2 `/token/refresh` endpoint to extend
lifetime) is deferred — it requires a separate operator integration and is
not part of this spec.

### 1.7 Consent gating

First-party signal collection inherits the existing consent check: no write
occurs unless `ec_context.ec_allowed()` is true. This is already enforced at
`generate_if_needed`; collection runs after that gate, so no additional
consent check is needed.

### 1.8 Error handling

All collection errors are swallowed and logged at `warn`. A collection failure
must never affect the client response. This matches the degraded-behavior
policy from PRD §8.6.

### 1.9 Performance

First-party signal collection adds one KV read (existing entry for TTL check)
and up to N KV writes (one per partner with a missing or stale UID) to the
organic path. To bound latency:

- Partners with no first-party signal config are skipped in O(1) via the
  existing `pull_enabled_index` pattern — a `_fp_signal_enabled` secondary
  index in `partner_store` lists only partners with `fp_signal_cookie_names`
  populated.
- KV writes for already-fresh UIDs are skipped via the TTL check.
- In practice, most requests are returning users — all UIDs already collected,
  zero writes, one metadata read per relevant partner.

---

## 2. HEM Resolution (LiveRamp)

### 2.1 Scope

Scoped to publishers with logged-in user populations. Not applicable to
open-web publishers (login rates < 1%). A publisher like autoblog.com should
not enable this; a publisher with a subscription wall or registration gate
(trade media, sports, news) should.

For autoblog.com and similar properties, `idl_env` is already set by LiveRamp
ATS.js on Chrome, and is collected by strategy 1 (`fp_signal_cookie_names:
["idl_env"]`). Strategy 2 is specifically for the cold-start case: a
logged-in user whose `idl_env` was never set because they browse primarily
on Safari or Firefox.

### 2.2 Header convention

The publisher signals a logged-in user by adding a request header at their
origin or CDN layer before proxying to TS:

```
X-ts-hem: <sha256-hex-of-lowercase-email>
```

The publisher is responsible for normalizing (lowercase, trim) and hashing
the email before sending. TS never receives or processes plaintext email.

If the header is absent or malformed (not a 64-character lowercase hex string),
HEM resolution is skipped silently.

### 2.3 PartnerRecord extension for HEM resolution

HEM resolution is modeled as a pull-sync variant on `PartnerRecord`. New fields:

```rust
/// Whether this partner supports HEM-based identity resolution.
#[serde(default)]
pub hem_resolution_enabled: bool,

/// HTTPS endpoint to call with the hashed email for UID resolution.
/// Required when `hem_resolution_enabled` is true.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub hem_resolution_url: Option<String>,

/// Allowlist of domains TS may call for HEM resolution.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub hem_resolution_allowed_domains: Vec<String>,

/// JSON path to extract the resolved UID from the API response.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub hem_resolution_response_path: Option<String>,

/// Publisher ID or client ID required by the partner's API.
/// Included as a query parameter or header per partner spec.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub hem_resolution_publisher_id: Option<String>,

/// Minimum seconds between HEM resolution calls for the same EC.
/// Defaults to 86400 (24 hours).
#[serde(default = "PartnerRecord::default_hem_ttl_sec")]
pub hem_resolution_ttl_sec: u64,
```

Validation mirrors `validate_pull_sync_config`: when `hem_resolution_enabled`,
both `hem_resolution_url` and `hem_resolution_allowed_domains` must be present,
URL must be HTTPS, and hostname must be in the allowed domains list.

### 2.4 LiveRamp API integration

LiveRamp's server-side HEM resolution uses their RampID API. The exact endpoint
and request shape must be confirmed against LiveRamp's current API documentation
and the publisher's LiveRamp account configuration (publisher ID / placement ID).

**Expected shape (to be confirmed with LiveRamp):**

```
GET https://api.rlcdn.com/api/identity/v1/envelope
  ?pid=<publisher_id>
  &it=4               (identifier type: SHA-256 email)
  &iv=<sha256_hex>
Authorization: Bearer <ts_pull_token>
```

Response:

```json
{ "envelope": "<RampID-envelope-string>" }
```

`hem_resolution_response_path = "envelope"` extracts the value.

The resolved value is written to KV as `ids["liveramp_ats"].uid`, consistent
with how the `idl_env` cookie harvest writes it. Downstream auction code sees
no difference between a harvested `idl_env` and an HEM-resolved RampID.

**Action required before implementation:** confirm endpoint, auth scheme,
rate limits, and `pid` parameter with LiveRamp account team. Record confirmed
values in this spec before engineering begins.

### 2.5 Dispatch timing

HEM resolution dispatches post-send, after client response is flushed — same
pattern as `dispatch_pull_sync`. It must never add latency to the organic
request path.

```
Request received
  → EcContext::read_from_request
  → EcContext::generate_if_needed
  → collect_first_party_signals(ec, jar)
  → finalize + send response
  → dispatch_pull_sync (existing)        (post-send)
  → dispatch_hem_resolution(ec, hem)     (post-send) ← NEW
```

### 2.6 TTL and staleness

Once a RampID is written to KV for a given EC, re-resolution is skipped until
`hem_resolution_ttl_sec` has elapsed (default 24 hours). The `ids["liveramp_ats"].synced`
timestamp is used for the staleness check — same field as cookie harvest.

### 2.7 Consent gating

HEM resolution only dispatches when `ec_context.ec_allowed()` is true. No
additional consent check beyond the existing EC gate.

### 2.8 Error handling

All HEM resolution errors are swallowed post-send. Log at `warn` on API
failure, `debug` on skip (stale, no HEM header). Never propagate to client.

---

## 3. Cross-browser Propagation

### 3.1 Purpose

First-party signal collection (§1) populates the KV entry for the browser that
carries those cookies — typically Chrome on a desktop. Cross-browser propagation
copies those IDs to sibling entries sharing the same EC hash prefix, enabling
Safari and Firefox users to benefit from IDs already resolved on Chrome without
waiting for a sync event.

### 3.2 When to propagate

Propagation runs inside the `/identify` endpoint, which already performs the
prefix-match list query for `cluster_size`. No new I/O is required — device
signals from `KvMetadata` are read during the same operation.

Decision table (evaluated in order, first match wins):

| Condition                                                               | Decision                                                                    |
| ----------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `known_browser` is `false` or `null`                                    | **Never** — non-human client                                                |
| `cluster_size > threshold` (default 10)                                 | **Never** — corporate/shared network                                        |
| `geo.asn` is a known mobile carrier ASN                                 | **Propagate** — individual device confirmed                                 |
| Source `platform_class` == target `platform_class`, `ja4_class` differs | **Propagate** — same machine, different browser (e.g. Chrome→Safari on Mac) |
| Source `platform_class` != target `platform_class`, `cluster_size` <= 3 | **Propagate** — probable personal device                                    |
| Anything else                                                           | **Skip** — insufficient confidence                                          |

### 3.3 The same-machine case

The Chrome→Safari same-Mac case is **deterministic**, not probabilistic:

- Chrome/Mac: `ja4_class: t13d1516h2`, `platform_class: mac`, `is_mobile: 0`
- Safari/Mac: `ja4_class: t13d2013h2`, `platform_class: mac`, `is_mobile: 0`

`platform_class` matches, `ja4_class` differs → same OS, different browser →
propagate with full confidence. No cluster size check needed.

Safari/Mac and Safari/iOS share identical JA4 and H2 fingerprints (Apple uses
the same TLS stack across platforms). `platform_class` (`mac` vs `ios`) is the
sole distinguishing signal, making it load-bearing for this decision.

### 3.4 The `is_mobile: 2` signal

`is_mobile: 2` (unknown) now in practice means a non-standard client, not
Safari — because Safari always produces a recognizable UA platform string
(`iPhone`, `iPad`, `Macintosh`). An entry arriving with `is_mobile: 2`
alongside an unrecognized `ja4_class` should be treated as a potential bot
and excluded from propagation regardless of cluster size.

### 3.5 What is propagated

Only `ids` entries are copied — not `device`, `geo`, or `pub_properties`. Each
suffix entry retains its own device fingerprint and geo signals. Only the
resolved partner UIDs are shared, since those represent identity assertions
the user has already authorized across the consortium.

### 3.6 Compliance note

Propagation is scoped to suffix entries sharing the same hash prefix. All
entries in the prefix group derived from the same IP + passphrase combination.
Within a small cluster on a home ASN, propagation is equivalent to a CRM
recognizing the same household across devices — an established and accepted
practice in both digital advertising (CTV household graphs) and privacy
regulation guidance (ICO cookie guidance, EDPB shared device guidance).

The `cluster_size` threshold is the primary guard against household
cross-contamination. Publishers with concern about household-level matching
can set `cluster_trust_threshold = 1` in `trusted-server.toml` to disable
propagation entirely.

---

## 4. Configuration example

```toml
[ec]
passphrase = "<publisher-passphrase>"
ec_store = "ec_store"
partner_store = "partner_store"
```

Partners are registered via `POST /_ts/admin/partners/register`. Example
registration payload for id5 with first-party signal collection enabled:

```json
{
  "id": "id5",
  "name": "ID5",
  "allowed_return_domains": ["id5-sync.com"],
  "api_key": "<secret>",
  "bidstream_enabled": true,
  "source_domain": "id5-sync.com",
  "openrtb_atype": 3,
  "sync_rate_limit": 10,
  "fp_signal_cookie_names": ["id5id"],
  "fp_signal_json_path": "universal_uid",
  "fp_signal_ttl_sec": 86400
}
```

Example for LiveRamp with both first-party signal collection and HEM resolution:

```json
{
  "id": "liveramp_ats",
  "name": "LiveRamp ATS",
  "allowed_return_domains": ["ats.rlcdn.com"],
  "api_key": "<secret>",
  "bidstream_enabled": true,
  "source_domain": "liveramp.com",
  "openrtb_atype": 3,
  "sync_rate_limit": 10,
  "fp_signal_cookie_names": ["idl_env"],
  "fp_signal_ttl_sec": 86400,
  "hem_resolution_enabled": true,
  "hem_resolution_url": "https://api.rlcdn.com/api/identity/v1/envelope",
  "hem_resolution_allowed_domains": ["api.rlcdn.com"],
  "hem_resolution_response_path": "envelope",
  "hem_resolution_publisher_id": "<liveramp-pid>",
  "hem_resolution_ttl_sec": 86400
}
```

---

## 5. Legal and Compliance

### 5.1 Why this isn't considered ID Bridging

ID bridging — sometimes called ID laundering — is the practice of taking an
identifier established in one consent context and using it to resolve or track
a user in a different context where they have no relationship and have not
consented. It is non-compliant with GDPR Purpose Limitation (Art. 5(1)(b)),
TCF v2, and IAB Tech Lab guidance, and is under active regulatory scrutiny.

Trusted Server's EC system is categorically different on every dimension that
makes ID bridging problematic:

**Consent is a structural gate, not a policy.** An EC cannot be created without
a valid TCF Purpose 1 signal (EU/UK) or passing GPP opt-out check (applicable
US states). On consent withdrawal, the cookie and the KV entry are deleted in
real time. There is no path through the code that creates or maintains an EC
without consent.

**First-party signal collection persists authorizations the user already gave.**
When TS reads `idl_env`, `id5id`, or similar cookies from the browser, those
cookies were set by LiveRamp ATS.js, ID5's SDK, or equivalent — on the
publisher's own domain, under the publisher's CMP consent dialog, with the
user's prior consent. TS is durably storing an identity link that the user
already authorized. No new linkage is created; no new consent basis is required.

**The publisher controls the passphrase and the partner registry.** The EC hash
is `HMAC-SHA256(IP, publisher_passphrase)`. The publisher chooses who is in
their partner registry and which signals are collected. There is no third-party
infrastructure making inferences the publisher hasn't sanctioned.

**The system is deterministic and auditable.** Given an IP address and a
passphrase, the EC hash is reproducible. A regulator, an auditor, or a
publisher's legal team can verify exactly how a hash was derived. Probabilistic
ID bridging systems cannot offer this.

### 5.2 Consortium scope

Publishers sharing a passphrase (e.g. a media group across multiple
properties) are recognizing their own reader across their own publications —
the same practice as a CRM matching a subscriber across a company's owned
brands. This is first-party data activation: the publisher has a direct
relationship with the user on each property, and the user's relationship is
with the media group, not a single URL. The relevant privacy policy covers all
consortium properties.

This is distinct from cross-site tracking, where an identity vendor uses a
signal from Publisher A to buy media on Publisher B without the user's
knowledge of that link.

### 5.3 Deletion

TS implements the IAB Data Subject Rights DSR deletion framework. The EC hash
is the registered identifier. A deletion request triggers cookie expiry, KV
entry deletion, and propagation to all registered partners. This chain of
custody is a compliance requirement before any regulated publisher goes live
(see PRD §7.4).

---

## 6. What is not stored

Per the IP address storage policy in the schema extensions spec:

- Raw email addresses — never received by TS; publisher hashes before sending
- Plaintext IP addresses — never stored (hash derivation only)
- UID2 refresh tokens — credentials, not identity signals
- Cookie values that fail JSON path extraction — silently skipped, not stored
- Full JA4 fingerprint (Sections 2 and 3) — approaches unique device identification; only Section 1 (`ja4_class`) is stored
- Raw H2 fingerprint string — stored only as a 12-char SHA256 prefix (`h2_fp_hash`)
- Client Hints headers — not used; JA4 and UA platform parsing provide equivalent signal

---

## 7. Open questions

- Should `_fp_signal_enabled` secondary index in `partner_store` be implemented
  from day one, or is a full scan acceptable given typical partner counts (< 20)?
- What is the correct LiveRamp HEM API endpoint, auth scheme, and `pid`
  parameter format? Needs confirmation with LiveRamp account team before
  implementation begins.
- Should `trade_desk` harvest be conditional on `TDID_LOOKUP == "TRUE"` in the
  cookie JSON, or is presence of `TDID` sufficient? (The lookup flag indicates
  a confirmed server-side match, not just a local guess.)
- Should UID2 token refresh be a phase-2 follow-on to this spec, or deferred
  to a separate initiative?

---
status: draft
---

# EC KV Schema Extensions

**Status:** Draft
**Author:** Trusted Server Product
**Last updated:** 2026-04-03
**Extends:** `docs/superpowers/specs/2026-03-24-ssc-prd-design.md` (§8 KV Store Identity Graph)
**Based on:** IABTechLab/trusted-server#582

---

## Overview

This document specifies additive changes to the EC KV identity graph schema
introduced in PR #582. It does not replace the original PRD — it amends §8.2
(schema) with four new namespaces and extends two existing structs.

**Motivation:** Cross-property identity resolution for publisher consortiums
(e.g. Arena Group sharing an EC passphrase across autoblog.com, menshealth.com,
etc.) requires durable per-domain visit history. Corporate VPN disambiguation
requires a lazily-evaluated network cluster signal. Cross-browser identity
propagation (Chrome→Safari on the same device) requires durable device class
signals derived from JA4 TLS fingerprints and UA platform parsing.

---

## 1. Schema version bump: `v: 1` → `v: 2`

All new fields are `Option`-typed, so existing `v: 1` entries deserialize
without error. The version bump signals to future readers that `pub_properties`,
`network`, and `device` may be present.

---

## 2. `KvGeo` — add `asn` and `dma`

Both fields are available from Fastly's `geo_lookup()` on the client IP and
are non-PII network signals.

```rust
pub struct KvGeo {
    pub country: String,
    pub region: Option<String>,
    /// Autonomous System Number (e.g. `7922` = Comcast).
    /// Primary signal for distinguishing home ISP vs. corporate VPN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,
    /// DMA/metro code (e.g. `807` = San Francisco).
    /// Market-level targeting signal; not personal data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dma: Option<i64>,
}
```

**Written:** on initial `KvEntry::new()` from `GeoInfo`. Never updated after
creation — geo is a first-seen signal, not a real-time one.

**Source:** `GeoInfo::metro_code` → `dma`; a new `asn` field to be added to
`GeoInfo` from Fastly's `Geo::as_number()`.

---

## 3. New `KvPubProperties` — publisher domain history

Tracks which publisher properties a user has been seen on, keyed by apex domain.
Enables consortium-level identity sharing without cross-site tracking: history
only accumulates within a shared-passphrase group (same EC hash).

```rust
pub struct KvPubProperties {
    /// Apex domain where this EC entry was first created.
    pub origin_domain: String,
    /// Per-domain visit history, keyed by apex domain.
    /// Updated on each organic request; capped at 50 entries.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub seen_domains: HashMap<String, KvDomainVisit>,
}

pub struct KvDomainVisit {
    /// Unix timestamp (seconds) of first visit to this domain.
    pub first: u64,
    /// Unix timestamp (seconds) of most recent visit to this domain.
    pub last: u64,
    /// Lifetime visit count for this domain.
    pub visits: u32,
}
```

Added to `KvEntry`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub pub_properties: Option<KvPubProperties>,
```

**Written:** on `create_or_revive` (sets `origin_domain`, adds first
`seen_domains` entry). Updated on `update_last_seen` — the existing 300-second
debounce applies, so `visits` and `last` are incremented at most once per 5
minutes per key.

**Cap:** `seen_domains` is capped at 50 entries. If the cap is reached, new
domains are silently dropped (log at `debug`). This prevents unbounded growth
for shared-passphrase consortiums with many properties.

### JSON example

```json
"pub_properties": {
  "origin_domain": "autoblog.com",
  "seen_domains": {
    "autoblog.com": { "first": 1774921179, "last": 1774985000, "visits": 4 },
    "menshealth.com": { "first": 1774985001, "last": 1774990000, "visits": 1 }
  }
}
```

---

## 4. New `KvNetwork` — cluster disambiguation

Tracks how many distinct EC entries share the same hash prefix. A high count
indicates a shared network (corporate VPN, campus); a low count indicates an
individual or household.

```rust
pub struct KvNetwork {
    /// Number of distinct EC suffixes matching this hash prefix.
    /// `None` = not yet evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
    /// Unix timestamp (seconds) of last cluster check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_checked: Option<u64>,
}
```

Added to `KvEntry`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub network: Option<KvNetwork>,
```

**Written:** only by the `/identify` endpoint, never on the organic proxy path.
The prefix-match list API call required to compute `cluster_size` is too
expensive for the hot path.

**Re-evaluation TTL:** re-check if `cluster_checked` is older than 1 hour
(configurable via `trusted-server.toml`).

### Threshold guidance

| Cluster size | Likely scenario |
|---|---|
| 1–3 | Individual / household |
| 4–10 | Small shared space (family, small office) |
| 11–50 | Medium office, hotel, coworking |
| 50+ | Corporate VPN, university, campus |

**Default trust threshold:** entries with `cluster_size <= 10` are treated as
individual users for identity resolution purposes. Configurable per publisher
via `trusted-server.toml`:

```toml
[ec]
cluster_trust_threshold = 10  # default
```

B2B publishers (trade media, finance) should raise this to 50+ since their
readers are frequently on office networks.

---

## 5. New `KvDevice` — browser class and bot detection

Captures coarse, non-PII device signals derived from the TLS handshake and UA
at EC creation time. Used by the `/identify` endpoint to make cross-suffix
propagation decisions and to signal buyer-facing device quality.

### 5.1 Signal derivation

No Client Hints are used — JA4 and UA platform parsing provide equivalent or
superior signal for every browser including Safari and Firefox, which do not
send Client Hints.

**`is_mobile`** — derived in priority order:

| Condition | Value |
|---|---|
| UA contains `iPhone`, `iPad`, or `Android` | `1` — confirmed mobile |
| UA contains `Macintosh`, `Windows`, or `Linux` | `0` — confirmed desktop |
| Neither pattern matches | `2` — genuinely unknown (rare; typically bots or heavily hardened clients) |

Note: `is_mobile: 2` in practice signals a non-standard client rather than
Safari, since Safari always produces a recognizable UA platform string.

**`ja4_class`** — Section 1 of the JA4 fingerprint only (e.g. `t13d1516h2`).
Available via `req.get_tls_ja4()` in the Fastly Compute Rust SDK. Section 1
identifies browser family (cipher count, extension count, ALPN) without
uniquely fingerprinting a device. The full JA4 is never stored.

**`platform_class`** — coarse OS family parsed from UA:

| UA segment | `platform_class` |
|---|---|
| `Macintosh; Intel Mac OS X` | `mac` |
| `Windows NT` | `windows` |
| `iPhone; CPU iPhone OS` | `ios` |
| `iPad; CPU OS` | `ios` |
| `Linux; Android` | `android` |
| `Linux` (non-Android) | `linux` |
| No match | `null` |

**`h2_fp_hash`** — first 12 hex characters of SHA256 of the raw HTTP/2
SETTINGS fingerprint string, available via `req.get_client_h2_fingerprint()`.
Used alongside `ja4_class` to confirm browser family and detect bots.

**`known_browser`** — set `true` when `ja4_class` + `h2_fp_hash` match a
known legitimate browser pattern from the allowlist below. Set `false` when
they match a known bot/scraper pattern. `null` when unknown.

### 5.2 Known browser fingerprint allowlist

Empirically derived from Fastly Compute production responses (2026-04-03):

| Browser | `ja4_class` | `h2_fp` prefix | `known_browser` |
|---|---|---|---|
| Chrome/Mac (v146) | `t13d1516h2` | `1:65536;2:0;4:6291456;6:262144` | `true` |
| Safari/Mac (v26) | `t13d2013h2` | `2:0;3:100;4:2097152` | `true` |
| Safari/iOS (v26) | `t13d2013h2` | `2:0;3:100;4:2097152` | `true` |
| Firefox/Mac (v149) | `t13d1717h2` | `1:65536;2:0;4:131072;5:16384` | `true` |

Safari Mac and Safari iOS share identical TLS/H2 stacks — distinguished only
by `platform_class` (`mac` vs `ios`) and `is_mobile` (`0` vs `1`).

This allowlist will expand as new browser versions are observed in production.
Entries not matching any allowlist row get `known_browser: null` (not `false`)
unless they match a confirmed bot pattern.

### 5.3 Rust struct

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvDevice {
    /// Mobile signal: 0 = confirmed desktop, 1 = confirmed mobile,
    /// 2 = genuinely unknown (non-standard client).
    /// Derived from UA platform string — no Client Hints required.
    pub is_mobile: u8,
    /// JA4 Section 1 only — browser family class identifier.
    /// e.g. "t13d1516h2" = Chrome, "t13d2013h2" = Safari, "t13d1717h2" = Firefox.
    /// Never stores the full JA4 fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ja4_class: Option<String>,
    /// Coarse OS family from UA: "mac", "windows", "ios", "android", "linux".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_class: Option<String>,
    /// SHA256 prefix (12 hex chars) of the HTTP/2 SETTINGS fingerprint.
    /// Used alongside ja4_class for browser confirmation and bot detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_fp_hash: Option<String>,
    /// true = known legitimate browser; false = known bot/scraper; null = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_browser: Option<bool>,
}
```

Added to `KvEntry`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub device: Option<KvDevice>,
```

**Written:** on `create_or_revive`. Never updated after creation — device
signals are a first-seen record of how this EC entry was established.

### 5.4 Bot gate

Device signals are derived on every request as pure in-memory computation —
no KV I/O. The result gates all downstream KV and cookie operations:

| `known_browser` | KV entry created | Cookie set | Partner IDs written |
|---|---|---|---|
| `true` | Yes | Yes | Yes |
| `false` | **No** | **No** | **No** |
| `null` | **No** | **No** | **No** |

`null` (unrecognised client) is treated the same as `false`. An advertiser
cannot bid on a session we cannot verify as human — allowing `null` entries
into the identity graph would degrade buyer trust with no offsetting benefit.

**Current bot response:** the request is served normally (proxied to origin)
without any KV operations or cookie writes. The bot receives a valid HTML
response but leaves no trace in the identity graph.

**Future bot response (see issue #81):** this pass-through behaviour is a
placeholder. The detection point will evolve into conditional routing that
returns HTTP 402 + an RSL Open License Protocol challenge for crawlers that
can acquire a license. The detection logic (`known_browser` derivation) is
intentionally separated from the response decision to make this transition
additive rather than a rewrite.

### 5.5 Privacy rationale

`ja4_class` (Section 1 only) and `platform_class` are category signals, not
unique device identifiers. They are equivalent in precision to `geo.country`
— they identify a class of client, not an individual. The full JA4 fingerprint
(Sections 2 and 3) is never stored, as it approaches unique device
identification and would require explicit consent basis under GDPR Art. 4(1).

---

## 6. `KvMetadata` — add `cluster_size`, `is_mobile`, and `known_browser`

Allows batch sync and `/identify` fast paths to make propagation and quality
decisions without streaming the full body:

```rust
pub struct KvMetadata {
    pub ok: bool,
    pub country: String,
    pub v: u8,
    /// Mirrors [`KvNetwork::cluster_size`]. `None` = not yet evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
    /// Mirrors [`KvDevice::is_mobile`]. Enables propagation gating without body read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_mobile: Option<u8>,
    /// Mirrors [`KvDevice::known_browser`]. Buyer-facing quality signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_browser: Option<bool>,
}
```

Worst-case metadata size with all additions: ~90 bytes — well within the
2048-byte Fastly limit.

---

## 7. IP address storage policy

Raw IP addresses are personal data under GDPR (CJEU *Breyer v. Germany*, 2016)
and must not be stored in KV entries. The EC hash already derives from the IP
without persisting it.

Permitted IP-derived signals (written at creation time):
- `geo.country` — ISO 3166-1 alpha-2
- `geo.region` — ISO 3166-2 subdivision
- `geo.asn` — ASN number (network identifier, not personal data)
- `geo.dma` — DMA/metro code (market identifier, not personal data)

---

## 8. Updated full `KvEntry` shape (v: 2)

Three representative entries showing the Chrome seed, Safari/Mac propagation
target, and Safari/iOS mobile entry.

**Chrome/Mac (seed entry):**
```json
{
  "v": 2,
  "created": 1775162556,
  "last_seen": 1775162556,
  "consent": { "tcf": "CP...", "gpp": "DBA...", "ok": true, "updated": 1775162556 },
  "geo": { "country": "US", "region": "TN", "asn": 7922, "dma": 659 },
  "device": {
    "is_mobile": 0,
    "ja4_class": "t13d1516h2",
    "platform_class": "mac",
    "h2_fp_hash": "a3f9d21c8b04",
    "known_browser": true
  },
  "pub_properties": {
    "origin_domain": "autoblog.com",
    "seen_domains": {
      "autoblog.com": { "first": 1775162556, "last": 1775162556, "visits": 1 }
    }
  },
  "network": { "cluster_size": 2, "cluster_checked": 1775162556 },
  "ids": {
    "id5":             { "uid": "ID5*qe8VHv...", "synced": 1775162556 },
    "trade_desk":      { "uid": "226fb4b3-6032-405a-a5a5-4fe4d6303932", "synced": 1775162556 },
    "liveramp_ats":    { "uid": "Ag2z1TDAfChu...", "synced": 1775162556 },
    "lockr":           { "uid": "b545e78c-2c4f-4fd3-8a99-32c02ada962d", "synced": 1775162556 },
    "prebid_sharedid": { "uid": "16d913a7-d56c-4e0d-8036-d0dce637707e", "synced": 1775162556 }
  }
}
```

**Safari/Mac (same machine — `platform_class: mac` + differing `ja4_class` → propagate):**
```json
{
  "v": 2,
  "created": 1775165000,
  "last_seen": 1775165000,
  "consent": { "ok": true, "updated": 1775165000 },
  "geo": { "country": "US", "region": "TN", "asn": 7922, "dma": 659 },
  "device": {
    "is_mobile": 0,
    "ja4_class": "t13d2013h2",
    "platform_class": "mac",
    "h2_fp_hash": "f7c341a92e18",
    "known_browser": true
  },
  "pub_properties": {
    "origin_domain": "autoblog.com",
    "seen_domains": {
      "autoblog.com": { "first": 1775165000, "last": 1775165000, "visits": 1 }
    }
  },
  "network": { "cluster_size": 2, "cluster_checked": 1775165000 },
  "ids": {
    "id5":             { "uid": "ID5*qe8VHv...", "synced": 1775162556 },
    "trade_desk":      { "uid": "226fb4b3-6032-405a-a5a5-4fe4d6303932", "synced": 1775162556 },
    "liveramp_ats":    { "uid": "Ag2z1TDAfChu...", "synced": 1775162556 },
    "lockr":           { "uid": "b545e78c-2c4f-4fd3-8a99-32c02ada962d", "synced": 1775162556 },
    "prebid_sharedid": { "uid": "16d913a7-d56c-4e0d-8036-d0dce637707e", "synced": 1775162556 }
  }
}
```

**Safari/iOS (mobile carrier ASN 21928 — individual device signal):**
```json
{
  "v": 2,
  "created": 1775168000,
  "last_seen": 1775168000,
  "consent": { "ok": true, "updated": 1775168000 },
  "geo": { "country": "US", "region": "TN", "asn": 21928, "dma": 659 },
  "device": {
    "is_mobile": 1,
    "ja4_class": "t13d2013h2",
    "platform_class": "ios",
    "h2_fp_hash": "f7c341a92e18",
    "known_browser": true
  },
  "pub_properties": {
    "origin_domain": "autoblog.com",
    "seen_domains": {
      "autoblog.com": { "first": 1775168000, "last": 1775168000, "visits": 1 }
    }
  },
  "network": { "cluster_size": 1, "cluster_checked": 1775168000 },
  "ids": {}
}
```

**Updated `KvMetadata`:**
```json
{ "ok": true, "country": "US", "v": 2, "cluster_size": 2, "is_mobile": 0, "known_browser": true }
```

---

## 9. Open questions

- Should `seen_domains` cap (50) be configurable, or is a hardcoded sentinel sufficient?
- Should `cluster_checked` re-evaluation TTL (1 hour) be per-publisher config or global?
- Should `pub_properties.seen_domains` be written on sync-pixel requests (non-organic) or only on organic HTML proxy requests?
- Should the known browser allowlist be hardcoded or configurable via `partner_store`? Hardcoded is simpler but requires a deploy to add new browser versions.
- Should `ja4_class` and `h2_fp_hash` be surfaced in `/identify` responses and `user.ext` for buyer-facing device quality scoring?

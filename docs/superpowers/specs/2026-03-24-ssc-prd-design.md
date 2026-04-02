# Product Requirements: Edge Cookie (EC)

**Status:** Draft
**Author:** Trusted Server Product
**Last updated:** 2026-03-12

---

## Table of Contents

1. [Overview](#1-overview)
2. [Problem Statement](#2-problem-statement)
3. [Goals and Non-Goals](#3-goals-and-non-goals)
4. [Target Customers](#4-target-customers)
5. [TS Lite Deployment Mode](#5-ts-lite-deployment-mode)
6. [EC Identity and Cookie Structure](#6-ec-identity-and-cookie-structure)
7. [Consent Lifecycle](#7-consent-lifecycle)
8. [KV Store Identity Graph](#8-kv-store-identity-graph)
9. [Pixel Sync Endpoint](#9-pixel-sync-endpoint)
10. [S2S Batch Sync API](#10-s2s-batch-sync-api)
11. [S2S Pull Sync (TS-Initiated)](#11-s2s-pull-sync-ts-initiated)
12. [Bidstream Decoration](#12-bidstream-decoration)
13. [Configuration](#13-configuration)
14. [Documentation Updates](#14-documentation-updates)
15. [Open Questions](#15-open-questions)
16. [Success Metrics](#16-success-metrics)

---

## 1. Overview

Edge Cookie (EC) is a stable, privacy-respecting user identity mechanism built into Trusted Server. It replaces the existing SyntheticID system with a cleaner signal (IP address + publisher passphrase only), a consent-aware lifecycle, and a server-side identity graph backed by Fastly KV Store that accumulates resolved partner IDs over time.

The EC hash is derived from the user's IP address and a publisher-chosen passphrase. A publisher's passphrase is consistent across all their own domains, producing the same EC hash for the same user everywhere they operate. Publishers may also share their passphrase with trusted partners to form an **identity-federated consortium** — members sharing a passphrase produce the same EC hash for the same user, enabling cross-property identity resolution by mutual agreement. Publishers using different passphrases produce unrelated hashes with no cross-property linkage.

EC sets a cookie on the publisher's apex domain (e.g., `ec.publisher.com` sets `ts-ec` on `.publisher.com`) and optionally orchestrates real-time bidding or decorates outbound ad requests with resolved identity signals from configured partners.

---

## 2. Problem Statement

### 2.1 SyntheticID signal degradation

The current SyntheticID uses User-Agent, Accept-Language, Accept-Encoding, and IP address as HMAC inputs. Each of these signals is eroding:

- **User-Agent reduction**: Chrome's UA freeze has eliminated OS version and minor browser version. The UA string no longer meaningfully differentiates users.
- **Accept-Language homogenization**: Browser defaults increasingly converge, reducing entropy.
- **IPv6 privacy extensions**: Modern operating systems rotate the interface ID portion of IPv6 addresses on a per-session or daily basis, causing SyntheticID mismatches for returning users.

The result is degrading match rates and false new-user rates on browsers where these signals change.

### 2.2 No consent enforcement

SyntheticID is created unconditionally. There is no mechanism to check TCF (EU/UK) or GPP (US) consent before creating the ID. This is a compliance gap that must be closed before EC can be offered as a product to regulated publishers.

### 2.3 Publishers need a reliable, deterministic signal that can be explicitly shared

Today, regular cookies don't suffice for publisher and partner needs. Additionally, only having these identifiers in the 1st party domain's cookie have created slow, undesirable behaviour in the form of cookie syncs.

---

## 3. Goals and Non-Goals

### Goals

- Replace SyntheticID's unstable browser signal inputs with IP address + publisher salt only
- Enforce TCF and GPP consent before creating or maintaining the EC
- Implement real-time consent withdrawal: delete cookie and KV entry when consent is revoked
- Build a server-side identity graph in Fastly KV Store that accumulates resolved partner IDs over time
- Provide three KV write paths: real-time pixel sync redirects, S2S batch push from partners, and TS-initiated S2S pull from partner resolution endpoints
- Expose two bidstream integration modes: header decoration (`/identify`) and full auction orchestration (`/auction`)
- Expose a publisher-authenticated `/_ts/admin/partners/register` endpoint for partner provisioning without direct KV access

### Non-Goals

- Replacing the publisher's consent management platform (CMP): EC reads and enforces consent signals; it does not generate them
- Building a data management platform (DMP): EC stores resolved partner IDs as a sync spine, not audience segments
- Backward compatibility with SyntheticID: EC uses a different cookie name, header name, and ID generation method. No migration path is provided
- Real-time user matching across unrelated domains (cross-site tracking)
- Data deletion framework: out of scope for this PRD; flagged for a follow-on document
- **TS Lite deployment mode** (runtime feature flags to run EC without the full TS feature surface): requirements are captured in Section 5 but are deferred to a follow-on iteration. The current iteration targets publishers running full Trusted Server.

---

## 4. Target Customers

**This iteration** targets publishers running the full Trusted Server stack. SSP, DSP, and identity provider customers interact with EC via the sync and bidstream endpoints but do not require a separate TS deployment.

| Customer type       | Deployment mode                                     | Primary value                                                      | In scope                 |
| ------------------- | --------------------------------------------------- | ------------------------------------------------------------------ | ------------------------ |
| Publisher (full TS) | Full TS + EC enabled                                | Consent-aware first-party ID, bidstream enrichment, identity graph | **Yes**                  |
| SSP                 | Partner — integrates via pixel sync and/or S2S pull | Build match table against EC hash; receive enriched bid requests   | **Yes** (as partner)     |
| DSP                 | Partner — integrates via S2S batch and/or S2S pull  | Push/receive ID mappings; enriched bid requests                    | **Yes** (as partner)     |
| Identity provider   | Partner — integrates via S2S batch                  | Sync resolved IDs into the KV identity graph                       | **Yes** (as partner)     |
| Publisher (EC only) | TS Lite at `ec.publisher.com`                       | First-party cookie at apex domain without full TS                  | Deferred (see Section 5) |

---

## 5. TS Lite Deployment Mode (Deferred - out of scope)

> **This section is out of scope for the current iteration.** Requirements are captured here for planning purposes and will be promoted to an active PRD in a follow-on phase. The current iteration delivers EC, the KV identity graph, all three sync mechanisms, and bidstream decoration — all within the existing full Trusted Server deployment model. No feature flags or route-disabling infrastructure will be built now.

### 5.1 Concept

TS Lite is a runtime configuration of the existing Trusted Server binary. It is not a separate binary or separate codebase. A publisher (or SSP/DSP deploying on behalf of a publisher) creates a Fastly service pointing to a subdomain — typically `ec.publisher.com` — and deploys the standard TS WASM binary with a `trusted-server.toml` that disables all routes except EC-related functionality.

### 5.2 Route surface in TS Lite

| Route                                  | Full TS  | TS Lite                 |
| -------------------------------------- | -------- | ----------------------- |
| `GET /static/tsjs=<ids>`               | Enabled  | Disabled                |
| `POST /auction`                        | Enabled  | Optional (configurable) |
| `GET /first-party/proxy`               | Enabled  | Disabled                |
| `GET /first-party/click`               | Enabled  | Disabled                |
| `POST /first-party/sign`               | Enabled  | Disabled                |
| `GET /first-party/proxy-rebuild`       | Enabled  | Disabled                |
| HTML injection pipeline                | Enabled  | Disabled                |
| GTM integration                        | Enabled  | Disabled                |
| `GET /sync`                            | Disabled | **Enabled**             |
| `GET /identify`                        | Disabled | **Enabled**             |
| `POST /_ts/api/v1/sync`                | Disabled | **Enabled**             |
| `GET /.well-known/trusted-server.json` | Enabled  | Enabled                 |

When a disabled route is requested, TS returns `404` with the header `X-ts-error: feature-disabled`.

### 5.3 Cookie domain and subdomain setup

The publisher points a subdomain of their choosing (e.g., `ec`) via DNS CNAME to their Fastly service. They configure `publisher.domain = "publisher.com"` in `trusted-server.toml`. Trusted Server derives `cookie_domain = ".publisher.com"` from this setting and sets the EC cookie with that domain attribute.

This gives the cookie read access across all subdomains of `publisher.com` — including `www.publisher.com` — without requiring a separate verification step. The publisher's control over their DNS and Fastly service implicitly proves TLD+1 ownership, following the same trust model as the existing `publisher.cookie_domain` setting.

**Constraint:** A publisher cannot configure a cookie domain outside their declared `publisher.domain`. Attempting to set `cookie_domain = ".otherdomain.com"` is rejected at startup validation.

### 5.4 Safari and browser compatibility

The EC is set as an HTTP `Set-Cookie` response header (not via JavaScript). For server-set cookies on first-party publisher domains that are not classified as cross-site trackers by Safari's ITP, the effective maximum lifetime is 1 year — the same as the configured `Max-Age`. Since `ec.publisher.com` is a publisher-owned domain, it is unlikely to be classified as a tracker.

The ITP interaction for users who arrive exclusively via third-party sync pixel redirects (where `ec.publisher.com` may be seen as a cross-site recipient) will be monitored post-launch. A cookie refresh strategy — re-issuing `Set-Cookie` on every same-site organic request — is deferred pending production data.

---

## 6. EC Identity and Cookie Structure

### 6.1 ID generation

The EC is generated by HMAC-SHA256 of a fixed input set, using a publisher-specific secret key.

**Inputs (IP address + salt only):**

| Input      | Value                                                                                                                                                                                                                                                                                                                                                                                                 |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| IP address | IPv4 as-is; IPv6 summarized to /64 prefix (first 4 hextets) — discards rotating interface ID. On dual-stack, IPv6 is preferred.                                                                                                                                                                                                                                                                       |
| Secret key | Publisher-chosen passphrase, configured in `trusted-server.toml`. Consistent across all of the publisher's own domains. Publishers who share the same passphrase with other publishers form an identity-federated consortium — the same user produces the same EC hash across all consortium members. Publishers using different passphrases produce unrelated hashes with no cross-property linkage. |

**Removed from SyntheticID:**

- `User-Agent`
- `Accept-Language`
- `Accept-Encoding`
- Handlebars template (input is now fixed, not configurable)

**Output format (unchanged from SyntheticID):**

```
{64-character hex HMAC-SHA256}.{6-character random alphanumeric suffix}
```

The 64-character prefix is the stable, deterministic portion used as the KV store key. The 6-character suffix is random, regenerated each time a fresh EC is created. Once an EC is set in a cookie, the full value (prefix + suffix) is preserved on subsequent requests.

**IPv6 /64 prefix rationale:** The first 64 bits of an IPv6 address identify the network prefix assigned by the ISP or home router. The remaining 64 bits (the interface ID) are rotated by privacy extensions on most modern operating systems. Using only the /64 prefix produces a stable hash for returning users while discarding the rotating portion that would cause false new-user signals.

### 6.2 Cookie attributes

| Attribute | Value                                                                                     |
| --------- | ----------------------------------------------------------------------------------------- |
| Name      | `ts-ec`                                                                                   |
| Domain    | `.publisher.com` (derived from `publisher.domain` in TOML)                                |
| Path      | `/`                                                                                       |
| Secure    | Yes                                                                                       |
| SameSite  | `Lax`                                                                                     |
| Max-Age   | `31536000` (1 year)                                                                       |
| HttpOnly  | No — JavaScript on `www.publisher.com` may need to read the value for ad stack decoration |

### 6.3 Response header

The EC value is also set as a response header for server-side consumers:

```
X-ts-ec: <ec_hash.suffix>
```

This header is internal to Trusted Server and is stripped before proxying requests to downstream backends, consistent with how other `X-ts-*` headers are handled.

### 6.4 Retrieval priority

On each request, Trusted Server looks for an existing EC in this order:

1. `X-ts-ec` request header (set by TS on a prior response, forwarded by the publisher's infrastructure)
2. `ts-ec` cookie
3. Generate fresh EC (subject to consent check — see Section 7)

### 6.5 No backward compatibility with SyntheticID

EC uses a different cookie name (`ts-ec` vs `synthetic_id`), a different header name (`X-ts-ec` vs `x-synthetic-id`), and a different ID generation method. No fallback to reading the `synthetic_id` cookie is provided. SyntheticID code remains in full TS and continues to function; EC is a parallel system.

---

## 7. Consent Lifecycle

Consent enforcement is a core requirement of EC. The system must not create or maintain an EC for users who have not given consent, and must actively revoke the EC when consent is withdrawn.

### 7.1 Consent signal sources and precedence

Section 7.1 describes **how** consent signals are read. Section 7.2 describes **whether** a signal is required at all for a given region. These two sections work in sequence: TS first determines the region (7.2), then — only if that region requires a consent signal — reads and evaluates the signal using the precedence order below.

When a consent signal is required for the user's region, Trusted Server checks sources in the following order. The first signal found wins:

1. **`X-consent-advertising` request header** — set by the Didomi integration (or another CMP proxy) in a prior server-side decode. This is the freshest signal and takes precedence over browser-stored values.
2. **`euconsent-v2` cookie** — the TCF v2 consent string stored by the publisher's CMP.
3. **`gpp` cookie** — the IAB Global Privacy Platform string for US state-level consent.
4. **Default: no consent** — if the region requires a signal and none is found, do not create the EC (fail safe). This step does not apply to regions where no signal is required — a user in a rest-of-world region with no consent cookies present is not subject to this fail-safe.

### 7.2 Pre-creation consent check

Before creating a new EC, Trusted Server first evaluates the user's region (via Fastly's `x-geo-country` header) to determine whether a consent signal is required. If the region requires a signal, TS reads it using the precedence order in Section 7.1; if no signal is found, creation is blocked (the fail-safe in step 4 applies). If the region does not require a signal, TS creates the EC unconditionally.

| Region                                                                                               | Required signal | Rule                                                                                                                                                              |
| ---------------------------------------------------------------------------------------------------- | --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| EU member states                                                                                     | TCF string      | Create EC only if `purposeConsents[1]` (store and/or access information on a device) is `true`. If no TCF signal is found, do not create EC (7.1 step 4 applies). |
| United Kingdom                                                                                       | TCF string      | Same as EU                                                                                                                                                        |
| US states with privacy laws (CA, CO, CT, VA, TX, OR, MT, DE, NH, NJ, TN, IN, IA, KY, NE, MD, MN, RI) | GPP string      | Create EC unless user has opted out of sale or sharing of personal data. If no GPP signal is found, do not create EC (7.1 step 4 applies).                        |
| Rest of world                                                                                        | None required   | Create EC on first visit regardless of whether any consent signal is present. Section 7.1 step 4 does not apply.                                                  |

### 7.3 Consent withdrawal (real-time enforcement)

On every request, Trusted Server decodes the consent signal (a microsecond in-memory operation with no I/O). If consent is not present or has been revoked:

**If `ts-ec` cookie is present:**

1. Delete the cookie by issuing `Set-Cookie: ts-ec=; Max-Age=0; Domain=.publisher.com; Path=/; Secure; SameSite=Lax`
2. Delete the KV identity graph entry: `kv_store.delete(ec_hash)` — this operation takes approximately 25ms and runs in the request path

**If no `ts-ec` cookie is present:**

- Do nothing

**If consent is present:**

- Proceed with normal EC create-or-refresh flow

**Known tradeoff:** The KV delete adds approximately 25ms of latency to the first request after consent withdrawal. This is an intentional product decision — real-time consent enforcement is a differentiating capability of Trusted Server, and the latency cost is acceptable.

### 7.4 Data deletion framework

Trusted Server implements the [IAB Data Subject Rights — Data Deletion Request Framework](https://github.com/InteractiveAdvertisingBureau/Data-Subject-Rights/blob/main/Data%20Deletion%20Request%20Framework.md) as its mechanism for honoring data deletion requests from users and partners. This is the authoritative answer for partners and regulators asking "how do I delete a user?" — there is no separate interim process.

**TS role in the framework:** Trusted Server acts as the **1st party** (it has the direct user relationship via the publisher's domain). It both receives deletion requests and initiates them downstream to registered partners who hold the same user's data.

**How it works:**

1. TS publishes a `dsrdelete.json` discovery file at `ec.publisher.com/.well-known/dsrdelete.json` listing its deletion endpoint, supported identifier types (EC hash), and public key.
2. A deletion request arrives as an HTTP `POST` containing a signed `rqJWT` (wrapping an `idJWT` identifying the user by EC hash).
3. TS verifies the JWT signatures, looks up the EC hash in the KV identity graph, deletes the KV entry and issues `Set-Cookie: ts-ec=; Max-Age=0` to expire the cookie.
4. TS returns a signed `acJWT` with result code `0` (success) or the appropriate error code.
5. TS propagates the deletion request to all registered partners in `partner_store` who have a resolved UID for this user, using their declared deletion endpoints.

**Identifier type:** The EC hash (64-character hex prefix, without `.suffix`) is the stable identifier registered in `dsrdelete.json`. The `.suffix` portion is not used for deletion matching — the hash is sufficient to locate the KV entry.

**Interim answer for partners during onboarding (before TS's deletion endpoint ships):** Publishers can manually delete a KV entry by EC hash via the Fastly KV management API or console. The EC cookie expires naturally within 1 year. A formal `POST` endpoint implementing the full JWT protocol above is required before any regulated publisher goes live.

**Implementation status:** The `dsrdelete.json` discovery file and the JWT-based deletion endpoint are a follow-on engineering deliverable, to be completed before regulated publisher onboarding.

---

## 8. KV Store Identity Graph

### 8.1 Purpose

The Fastly KV Store serves as a persistent identity graph keyed on the EC hash. It accumulates resolved partner IDs over time through two write paths: real-time pixel sync redirects and S2S batch pushes from partners. This graph is read at auction time to populate `user.eids` in outbound OpenRTB requests.

### 8.2 Schema

**KV key:** The 64-character hex hash portion of the EC (without the `.suffix`). The hash is stable across sessions for the same user+network+key combination and is safe to use as a long-lived identifier.

**KV value (JSON body, max ~5KB):**

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

The metadata field is used for consent withdrawal checks. When consent status must be evaluated for a user with an existing EC, Trusted Server reads metadata only — not the full body — keeping the hot-path latency minimal.

### 8.3 TTL

KV entries are created or refreshed with a `time_to_live_sec=31536000` parameter (1 year), matching the cookie `Max-Age`. Fastly's TTL mechanism is eventual garbage collection — entries may persist up to 24 hours past expiry before being removed. This is acceptable for identity data; EC does not use KV TTL for security-critical expiration.

### 8.4 Conflict resolution

Concurrent writes from different partners to the same KV entry must not overwrite each other's data. Each partner's ID is stored under its own namespace in the `ids` map — a write for `ssp_x` must never clobber an existing entry for `liveramp`. Implementation must guarantee this isolation under concurrent write conditions.

### 8.5 KV store names

Two KV stores are required:

| Store            | TOML key        | Contents                           |
| ---------------- | --------------- | ---------------------------------- |
| Identity graph   | `ec_store`      | EC hash → identity graph JSON      |
| Partner registry | `partner_store` | Partner ID → config + API key hash |

The existing `counter_store` and `opid_store` settings (currently defined but unused in `settings.rs`) can be deprecated in a follow-on cleanup.

### 8.6 KV Store degraded behavior

The EC cookie is deterministic (derived from IP + publisher salt) and lives in the browser. It does not depend on KV Store availability. KV Store holds identity enrichment only — resolved partner UIDs accumulated over time. The degraded behavior policy follows from this: **EC always works; enrichment degrades gracefully.**

| Operation                            | KV unavailable or error                                                                                                           | Rationale                                                                                                                                                                                                                                                         |
| ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| EC cookie creation                   | Set the cookie. Skip the KV entry creation silently. Log the failure at `warn` level.                                             | The cookie is the identity anchor — it does not require KV. The KV entry will be created on the next request once KV recovers.                                                                                                                                    |
| EC cookie refresh (existing user)    | Refresh the cookie. Skip the KV `last_seen` update silently. Log at `warn`.                                                       | Same as above — the cookie continues working. Stale `last_seen` is acceptable.                                                                                                                                                                                    |
| `/sync` KV write                     | Redirect to `return` with `ts_synced=0&ts_reason=write_failed`.                                                                   | The browser redirect must not be blocked by KV availability. This case is already specified in Section 9.4.                                                                                                                                                       |
| `/identify` KV read                  | Return `200` with `ec` hash (from cookie) and `degraded: true`. Set `uids: {}` and `eids: []`.                                    | The EC hash is still valid and useful for attribution and analytics. Empty uids signal that enrichment is unavailable, not that the user has no synced partners. `degraded: true` lets callers distinguish transient KV failure from a genuinely unenriched user. |
| S2S batch write (`/_ts/api/v1/sync`) | Return `207` with all mappings rejected, `reason: "kv_unavailable"`.                                                              | The request was valid; the failure is infrastructure. Partners should retry the batch.                                                                                                                                                                            |
| S2S pull sync write (async)          | Discard the resolved uid. Log at `warn`. Retry will occur on the next qualifying request per the `pull_sync_ttl_sec` window.      | Async path — no user-facing impact.                                                                                                                                                                                                                               |
| Consent withdrawal KV delete         | Expire the cookie immediately. Log the KV delete failure at `error` level. Retry the KV delete on the next request for this user. | Cookie deletion is the primary enforcement mechanism. KV delete failure must not block or delay the cookie expiry.                                                                                                                                                |

**`degraded: true` in `/identify` responses**

When a KV read fails, the `/identify` response includes `"degraded": true` in the JSON body alongside an empty `uids` and `eids`. The `ec` field is still populated from the cookie. Callers should proceed with identity-only targeting (EC hash) and omit partner UID parameters from downstream requests.

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": true,
  "uids": {},
  "eids": []
}
```

### 8.7 Buyer confidence in KV entries (Deferred - out of scope)

#### Problem

Code attestation (reproducible WASM builds + published binary hashes) proves that the TS binary running on Fastly's infrastructure matches the open-source repository. It does not, however, prove that the _data_ inside `ec_store` was written by that attested binary. A malicious or compromised operator could write arbitrary identity mappings directly into the KV store — bypassing all code paths — and buyers would have no way to detect it.

#### Solution: JOSE-signed KV entry bodies

Every identity graph entry written to `ec_store` by the TS WASM binary is signed using JSON Web Signatures (JWS, RFC 7515) before storage. The signing key is generated at binary load time and is bound to the running instance; the corresponding public key is published alongside the binary hash in the attestation record.

At read time, the TS binary verifies the JWS signature before consuming any fields from the entry. An entry that fails signature verification is treated as absent, the request proceeds as if the KV key does not exist, and the failure is logged at `error` level.

**What a valid signature proves:**

- The entry was written by a TS binary instance whose signing key corresponds to a published, attested binary hash.
- The entry body has not been modified since it was written.
- A buyer who trusts the attested binary can transitively trust any entry that carries a valid signature.

**What it does not prove:**

- That the _input data_ (e.g., a partner-supplied UID) was accurate at the time of write. Signal accuracy remains the partner's responsibility.
- Anything about entries written before this feature was deployed. A migration pass will resign existing entries or treat them as unsigned (degraded) until they are refreshed by a normal TS write.

#### Attestation record endpoint

The signing public key is published as a namespaced field inside the existing `/.well-known/trusted-server.json` discovery document — the same endpoint partners already fetch for request signing key distribution. No new endpoint is required.

```
GET /.well-known/trusted-server.json
```

Response (application/json):

```json
{
  "version": "1.0",
  "jwks": {
    "keys": [
      {
        "kty": "OKP",
        "crv": "Ed25519",
        "kid": "ts-2026-A",
        "use": "sig",
        "x": "..."
      }
    ]
  },
  "attestation": {
    "binary_hash": "<sha256-hex of deployed WASM>",
    "alg": "ES256",
    "jwk": { "kty": "EC", "crv": "P-256", "x": "...", "y": "..." },
    "expires_at": "2026-06-18T00:00:00Z"
  }
}
```

The `jwks` field is unchanged — it continues to serve request signing keys on its existing rotation schedule. The `attestation` object is a separate namespace and does not affect existing consumers of this endpoint.

| Field                     | Description                                                                                                                                         |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `attestation.binary_hash` | SHA-256 hex of the deployed WASM binary. Cross-referenced with Fastly's signed deployment manifest in the reproducible builds PRD.                  |
| `attestation.alg`         | JWS algorithm used for all KV entry signatures. Fixed at `ES256` (ECDSA P-256).                                                                     |
| `attestation.jwk`         | Public key in JWK format (RFC 7517). Buyers use this to verify signatures in KV-derived `user.eids`. Distinct from the `jwks` request-signing keys. |
| `attestation.expires_at`  | UTC timestamp after which the attestation key should be considered untrustworthy. Buyers must re-fetch before this time.                            |

**Key TTL:** 90 days. The attestation key rotates on each new TS deployment. The previous key's `expires_at` is set 7 days after rotation to allow in-flight impressions to drain.

**Key storage:** The signing private key lives in the Fastly Secret Store under `ec_signing_key`. It is provisioned at deploy time and never exposed in responses or logs.

**Caching:** `trusted-server.json` should be served with `Cache-Control: max-age=3600` to ensure buyers pick up a rotated attestation key within one hour of a new deployment. This is shorter than the JWKS key rotation window and is safe for both key types.

> **Future:** When the reproducible builds PRD ships, the `attestation` object may be graduated to a dedicated `/.well-known/ts-attestation.json` endpoint if the data (multiple binary hashes, Fastly co-signatures) outgrows the shared document. The field names will remain compatible.

#### Buyer-facing verification flow

1. Publisher includes `site.ext.ts_discovery` pointing to `/.well-known/trusted-server.json` in the bid request.
2. Buyer fetches `trusted-server.json` and caches it until `attestation.expires_at`.
3. Buyer independently verifies `attestation.binary_hash` against Fastly's signed deployment manifest (see separate PRD).
4. Buyer verifies the JWS signature on each `user.eids` entry against `attestation.jwk`.
5. Buyer trusts KV-derived signals only for entries with a valid signature from a non-expired attestation key.

#### Relationship to reproducible builds PRD

JOSE-signed KV entries close the _data integrity_ gap that code attestation leaves open. Reproducible builds and published binary hashes address the _code integrity_ layer — proving that the deployed binary matches the audited source. These are complementary controls that together form a complete trust chain for buyers.

The reproducible builds feature has broader scope than the identity graph (it applies to all TS behaviour, not just KV writes) and will be specified in a dedicated PRD. The `attestation.binary_hash` field in `trusted-server.json` anticipates that PRD — buyers can record it today, and the reproducible builds PRD will define the process for independently verifying it against Fastly's signed deployment manifest.

---

## 9. Pixel Sync Endpoint

### 9.1 Purpose

The pixel sync endpoint allows SSPs and DSPs to synchronize their user IDs with the EC hash via a browser-side redirect. When a partner's sync pixel fires, the user's browser is redirected through `ec.publisher.com/sync`, Trusted Server reads the existing `ts-ec` cookie, and writes the partner's user ID into the KV identity graph.

This is the primary real-time write path for building the identity graph from existing cookie sync infrastructure.

### 9.2 Endpoint

```
GET /sync
```

### 9.3 Parameters

| Parameter | Required | Description                                                                                       |
| --------- | -------- | ------------------------------------------------------------------------------------------------- |
| `partner` | Yes      | Partner ID, must match a registered partner in `partner_store` KV                                 |
| `uid`     | Yes      | Partner's user ID for this user                                                                   |
| `return`  | Yes      | Callback URL to redirect to after sync (must match partner's `allowed_return_domains`)            |
| `consent` | No       | TCF or GPP string from the partner's context, used if no consent signal is present on the request |

### 9.4 Flow

1. Read the `ts-ec` cookie. If absent, redirect to `return` URL with `ts_synced=0` appended. Do not create a new EC during a sync — a sync redirect is not an organic user visit and must not be used to bootstrap identity.
2. Look up the partner record in `partner_store` KV using the `partner` parameter. Return `400` if the partner is not found.
3. Validate the `return` URL against the partner's `allowed_return_domains`. Return `400` if the domain is not on the allowlist.
4. Evaluate consent for this user by decoding from request cookies (or the optional `consent` query parameter if no cookie signal is present). If consent is absent or invalid, redirect to `return` with `ts_synced=0&ts_reason=no_consent`. No KV write is performed.
5. Perform an atomic read-modify-write to update `ids[partner_id]` in the KV identity graph (with generation marker — see Section 8.4). If the write fails after all retries, redirect to `return` with `ts_synced=0&ts_reason=write_failed`.
6. On successful KV write, redirect to `return` with `ts_synced=1` appended as a query parameter.

**`ts_synced` values:**

| Value                                | Meaning                                                                     |
| ------------------------------------ | --------------------------------------------------------------------------- |
| `ts_synced=1`                        | KV write succeeded — partner uid is now in the identity graph               |
| `ts_synced=0&ts_reason=no_ec`        | No EC cookie present — user has not established an EC on this publisher     |
| `ts_synced=0&ts_reason=no_consent`   | Consent absent or invalid — write suppressed                                |
| `ts_synced=0&ts_reason=write_failed` | KV write failed after retries — partner should retry on a future pixel fire |

Partners should treat `ts_synced=0` as a signal that the mapping was not stored. The `ts_reason` parameter is informational; partners should not gate their own behavior on specific reason values.

### 9.5 Security

- The `return` URL is validated against the partner's `allowed_return_domains` using **exact hostname match** — `sync.example-ssp.com` does not match `a.sync.example-ssp.com`. Suffix or wildcard matching is not supported. This prevents subdomain takeover abuse where an attacker controlling an abandoned subdomain of a legitimate partner could exploit TS as an open redirect. Partners needing multiple callback hostnames must register each one explicitly in `allowed_return_domains`. Open redirects are not permitted.
- Partners control when to fire their sync pixel; no HMAC signature is required on the inbound sync request.
- Anti-stuffing rate limit: a maximum of `sync_rate_limit` sync writes per EC hash per hour per partner (configurable per partner in `partner_store`, default 100).

### 9.6 User stories

**As an SSP**, I want to fire a sync pixel when I see a user so that I can associate my user ID with the EC hash and receive enriched bid requests when the publisher calls Trusted Server for auction.

**Acceptance criteria:**

- [ ] `GET /sync?partner=ssp_x&uid=abc&return=https://sync.ssp.com/ack` returns a redirect to the `return` URL within 50ms (excluding KV write time)
- [ ] KV entry for the EC hash contains `ids.ssp_x.uid = "abc"` after a successful sync; response redirects to `return` with `ts_synced=1`
- [ ] If no `ts-ec` cookie is present, redirects to `return` with `ts_synced=0&ts_reason=no_ec`; no KV write performed
- [ ] If consent is absent or invalid, redirects to `return` with `ts_synced=0&ts_reason=no_consent`; no KV write performed
- [ ] If KV write fails after all retries, redirects to `return` with `ts_synced=0&ts_reason=write_failed`
- [ ] `return` URL domains not in partner's `allowed_return_domains` receive a `400` response (no redirect)
- [ ] Rate limit is enforced: more than `sync_rate_limit` writes per hour per EC hash per partner are rejected with `429`

---

## 10. S2S Batch Sync API

### 10.1 Purpose

The S2S batch sync API allows partners to push ID mappings to Trusted Server in bulk via an authenticated REST endpoint. This write path handles large-scale partner-initiated syncs, back-fills for users whose browser-side pixel sync has not fired, and DSP-side match data that originates from non-browser contexts.

### 10.2 Endpoint

```
POST /_ts/api/v1/sync
```

### 10.3 Authentication

Partners authenticate with a rotatable API key. Key rotation must not require redeploying the binary. Partner provisioning is handled via the `/_ts/admin/partners/register` endpoint (see Section 15, Open Questions).

### 10.4 Request

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
    },
    ...
  ]
}
```

Maximum batch size per request: 1000 mappings (subject to revision based on KV write throughput testing).

### 10.5 Response

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

**HTTP status rules:**

| Condition                                              | Status                                  |
| ------------------------------------------------------ | --------------------------------------- |
| All mappings accepted                                  | `200 OK`                                |
| Some mappings accepted, some rejected                  | `207 Multi-Status`                      |
| Auth valid, batch valid, but **all** mappings rejected | `207 Multi-Status` with `accepted: 0`   |
| Auth invalid                                           | `401 Unauthorized` (no body processing) |
| Batch exceeds 1000 mappings or malformed JSON          | `400 Bad Request` (no body processing)  |

A `207` with `accepted: 0` signals "your request was received and processed correctly, but none of the submitted EC hashes were found or eligible." This is distinct from an auth or protocol error. Partners should treat this as a data signal — either the EC hashes are stale/unknown, or consent has been withdrawn for all submitted users — and should not retry the same batch without investigating the underlying cause.

### 10.6 Consent enforcement

Before writing a mapping, Trusted Server checks the KV metadata for the given EC hash. Mappings for users with `consent.ok = false` are rejected with reason `consent_withdrawn`. Partners must not submit mappings for users who have withdrawn consent; this enforcement is a safeguard, not the primary compliance mechanism.

### 10.7 Conflict resolution

- If the KV entry does not exist for a given `ec_hash`, the mapping is rejected with reason `ec_hash_not_found`. The S2S API does not create new KV entries — only the EC creation flow (from organic browser visits) can create entries.
- If the partner has an existing entry for the same `ec_hash` and the request's `timestamp` is older than the stored `synced` timestamp, the mapping is skipped (no error, counted as accepted).
- Otherwise, atomic read-modify-write with generation markers (see Section 8.4).

### 10.8 User stories

**As a DSP**, I want to push my user ID mappings to Trusted Server in bulk so that the publisher's auction requests are enriched with my resolved ID and I can bid on users I recognize.

**Acceptance criteria:**

- [ ] `POST /_ts/api/v1/sync` with a valid Bearer token and a batch of up to 1000 mappings returns a response within 5 seconds
- [ ] Accepted mappings are written to the corresponding KV identity graph entries within 1 second
- [ ] Mappings for unknown `ec_hash` values are rejected with `ec_hash_not_found`
- [ ] Mappings for users with withdrawn consent are rejected with `consent_withdrawn`
- [ ] Invalid or expired Bearer tokens receive `401 Unauthorized`
- [ ] Requests exceeding 1000 mappings receive `400 Bad Request`
- [ ] Rate limiting by API key is enforced

---

## 11. S2S Pull Sync (TS-Initiated)

### 11.1 Purpose

The pixel sync endpoint (Section 9) requires the user's browser to initiate a redirect, which can be blocked by ad blockers or ITP. The S2S batch API (Section 10) requires the partner to proactively push mappings. Neither path helps when the publisher wants to opportunistically ask a partner "do you know this user?" without waiting for a pixel to fire.

S2S pull sync inverts the S2S batch model: Trusted Server calls the partner's resolution endpoint directly, server-to-server, and writes the returned uid into the KV identity graph. No browser pixel is involved. The HTTP return path is the response body — no redirect required.

**What the partner resolves against**

The partner's resolution endpoint receives the EC hash and IP address. The partner must look these up against their own **server-side user database** — not a browser cookie. Common sources partners use:

- **IP-based user graph**: major SSPs and DSPs maintain server-side mappings of IP → their own uid, built from bid stream traffic and direct visits. If a user has hit any page on which this partner runs, they may have an IP mapping.
- **Prior bid stream observation**: once the EC hash begins appearing in outbound bid requests (Mode B), partners who have bid on those requests can build their own reverse map of EC hash → their uid. Subsequent pull calls can then be resolved against this map.
- **Authenticated / hashed-email graph**: for partners with deterministic identity (UID2, RampID), they may resolve from email-hash mappings independently of IP.

**Implication:** pull sync only returns a uid for users the partner already knows by some server-side signal. If the partner has never seen this user by any channel, they return null and the call is a no-op. This is not a general solution for new users — it is a reliable, pixel-free path for users the partner already knows.

**What it solves and what it doesn't:**

| User scenario                             | Pixel sync                | S2S batch                      | S2S pull                                     |
| ----------------------------------------- | ------------------------- | ------------------------------ | -------------------------------------------- |
| New user, Chrome, 3p cookies available    | Works (bootstraps KV)     | Not applicable                 | No server-side mapping yet — no-op           |
| Returning user after prior pixel sync     | Redundant (already in KV) | Works                          | Works (partner has IP or bid-stream mapping) |
| Safari user, partner has IP-based mapping | Blocked / unreliable      | Works if partner knows EC hash | Works — partner resolves from their IP graph |
| User unknown to partner by any signal     | No uid to sync            | No uid to push                 | No uid to return — no-op                     |
| Authenticated user with hashed email      | Works                     | Works                          | Works                                        |

S2S pull does not solve the cold-start problem for users the partner has never seen. It degrades gracefully to a no-op in those cases.

### 11.2 When TS initiates a pull

Trusted Server initiates a pull sync for a given partner when all of the following are true on an incoming request:

1. A valid `ts-ec` cookie is present (user has an established EC)
2. Consent is valid for this user
3. The partner has `pull_sync_enabled: true` in their `partner_store` record
4. The KV identity graph for this EC hash has no entry for this partner, **or** the existing entry's `synced` timestamp is older than `pull_sync_ttl_sec` (configurable per partner, default 86400 — 1 day)

### 11.3 Execution model

Pull sync calls are dispatched **asynchronously after the response is sent** using Fastly's `send_async` / background task model. They do not add latency to the user-facing request.

A maximum of `pull_sync_concurrency` partner calls are dispatched per request (configurable globally, default 3). If more partners qualify, they are queued and dispatched on subsequent requests for the same user.

### 11.4 Partner resolution endpoint

Each partner exposes a resolution endpoint declared in their `partner_store` record as `pull_sync_url`. Trusted Server calls it with a `GET` request:

```
GET <pull_sync_url>?ec_hash=<64-char-hex>&ip=<ip_address>
Authorization: Bearer <ts_pull_token>
```

`ts_pull_token` is a per-partner token provisioned during partner registration, used so the partner can authenticate inbound requests from Trusted Server. It is stored in `partner_store` KV in plaintext (outbound credential, not inbound).

**Expected response (`200 OK`):**

```json
{ "uid": "abc123" }
```

**If the partner does not recognize the user:**

```json
{ "uid": null }
```

or `404 Not Found`. Both are treated as a no-op — no KV write.

Any response other than `200` with a valid body is treated as a transient failure. Trusted Server does not retry on failure; the next qualifying request for this user will trigger a new attempt.

### 11.5 KV write

On a successful resolution (`uid` is non-null), Trusted Server performs the same atomic read-modify-write used by the pixel sync path (Section 8.4): read the existing KV entry with a generation marker, merge `ids[partner_id].uid`, write back with `if-generation-match`.

The `synced` timestamp is set to the current Unix timestamp, which resets the `pull_sync_ttl_sec` clock.

### 11.6 Partner configuration additions

The following fields are added to the partner record schema (Section 13.3):

```json
{
  "pull_sync_enabled": true,
  "pull_sync_url": "https://api.example-ssp.com/ts/resolve",
  "pull_sync_ttl_sec": 86400,
  "ts_pull_token": "<outbound bearer token for this partner>"
}
```

### 11.7 Security

- The `pull_sync_url` domain must be on an allowlist declared in the partner record. Trusted Server will not call arbitrary URLs.
- Pull sync calls are one-way data flows: TS sends only the EC hash and IP. No other user data (consent string, geo, other partner IDs) is included in the pull request.
- Rate limiting: a maximum of `pull_sync_rate_limit` pull calls per EC hash per partner per hour (configurable per partner, default 10). This prevents the pull mechanism from being used as a polling channel.

### 11.8 User stories

**As an SSP**, I want Trusted Server to call my resolution endpoint when it sees a user I might know, so that my uid is available for bidstream decoration without requiring the publisher to include a sync pixel in their page.

**Acceptance criteria:**

- [ ] When a request arrives with a valid `ts-ec` cookie and a partner with `pull_sync_enabled: true` has no KV entry (or a stale entry), a pull call is dispatched asynchronously after the response is sent
- [ ] A successful pull response with a non-null `uid` results in a KV write within 1 second
- [ ] A `null` or `404` response results in no KV write and no error logged above `DEBUG` level
- [ ] Pull calls are not initiated during the pixel sync flow (no double-write)
- [ ] Rate limit is enforced: more than `pull_sync_rate_limit` pull calls per EC hash per partner per hour are suppressed
- [ ] Pull calls do not add measurable latency to the user-facing response (async dispatch)

---

## 12. Bidstream Decoration

### 12.1 Two integration modes

Trusted Server exposes two modes for injecting EC identity into the bidstream. Publishers choose the mode that fits their existing ad stack.

### 12.2 Mode A: Identity resolution (`/identify`)

Trusted Server exposes `/identify` as a standalone identity resolution endpoint for callers that need EC identity and resolved partner UIDs outside of TS's own auction orchestration. TS builds the OpenRTB request in Mode B — `/identify` is not part of that path. It serves three distinct use cases:

**Use case 1 — Attribution and analytics**
Any server-side or browser-side system that needs to tag an event, impression, or conversion with the user's EC hash. Examples: analytics pipelines, attribution platforms, reporting dashboards.

**Use case 2 — Publisher ad server outbid context**
After TS's auction completes and winners are delivered to the publisher's ad server endpoint, the publisher's ad server may need EC identity and resolved partner UIDs to evaluate whether to accept the programmatic winner or outbid with a direct-sold placement. For this use case, TS includes the EC identity in the winner notification payload directly (see Section 12.3) — a separate `/identify` call is only needed if the publisher's ad server receives the winner through a path that does not carry TS headers.

**Use case 3 — Client-side wrappers for non-TS SSPs**
Some SSPs run client-side header bidding wrappers (e.g., Amazon TAM, certain Index Exchange configurations) that do not participate in TS's server-side auction orchestration. A Prebid.js module or custom wrapper script calls `/identify` from the browser to obtain the EC hash and resolved partner UIDs, then injects those values into bid requests sent to those SSPs. This ensures non-TS demand sources bid with the same identity enrichment as TS-orchestrated bids, enabling a fair comparison at winner selection.

> **Prerequisite for use case 3:** For a non-TS SSP to receive a useful UID from `/identify`, that SSP must already be a registered partner in `partner_store` and must have a resolved uid in the KV identity graph for this user (via pixel sync, S2S batch, or S2S pull). Without a prior sync, `/identify` returns no uid for that partner.

**Endpoint:** `GET /identify`

**When to call:** Once per auction event — not per-pageview. For use case 3, call before sending bid requests to non-TS SSPs.

#### Call patterns

**Pattern 1 — Browser-direct (recommended for use cases 1 and 3)**

A script on the publisher's page calls `/identify` via `fetch()`. Because `ec.publisher.com` is same-site with the publisher's domain, the browser sends the `ts-ec` cookie and consent cookies automatically. No forwarding required.

```js
const identity = await fetch('https://ec.publisher.com/identify').then((r) =>
  r.json()
)

// GAM key-value targeting
googletag.pubads().setTargeting('ts_ec', identity.ec)
googletag.pubads().setTargeting('ts_uid2', identity.uids.uid2)

// Prebid.js userIds injection
pbjs.setConfig({
  userSync: { userIds: [{ name: 'uid2', value: { id: identity.uids.uid2 } }] },
})
```

**Pattern 2 — Origin-server proxy (for use case 2 when TS winner headers are unavailable)**

A server-side caller must forward the following from the original browser request:

| Header to forward                                       | Required                                                              |
| ------------------------------------------------------- | --------------------------------------------------------------------- |
| `Cookie: ts-ec=<value>` or `X-ts-ec: <value>`           | Yes — without this, TS cannot identify the user                       |
| `Cookie: euconsent-v2=<value>` or `Cookie: gpp=<value>` | Yes — without this, TS returns `consent: denied` and no identity data |
| `X-consent-advertising: <value>`                        | Optional — takes precedence over cookie consent if present            |

#### Cookie and consent handling

`/identify` follows the EC retrieval priority from Section 6.4. It does not generate a new EC — if no EC is present, the response body contains `consent: denied` and empty identity fields. Consent is evaluated per Section 7.1. `/identify` never sets or modifies cookies.

#### Response

**`200 OK` — identity resolved**

EC is present and consent is valid. Identity values are returned as a JSON body. Callers use these values to construct URL parameters for GAM, SSP bid requests, analytics events, or any other downstream system.

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": false,
  "uids": {
    "uid2": "A4A...",
    "liveramp": "LR_xyz",
    "id5": "ID5-abc"
  },
  "eids": [
    { "source": "uidapi.com", "uids": [{ "id": "A4A...", "atype": 3 }] },
    { "source": "liveramp.com", "uids": [{ "id": "LR_xyz", "atype": 3 }] }
  ]
}
```

`uids` contains one key per partner with `bidstream_enabled: true` and a resolved UID in the KV graph. Partners with no resolved UID for this user are omitted.

**`200 OK` — KV unavailable (degraded)**

EC is present and consent is valid, but the KV read failed. The EC hash is returned; `uids` and `eids` are empty. `degraded: true` distinguishes this from a user who simply has no synced partners yet — callers should proceed with EC-only targeting and may retry on the next auction.

```json
{
  "ec": "a1b2c3...AbC123",
  "consent": "ok",
  "degraded": true,
  "uids": {},
  "eids": []
}
```

**`403 Forbidden` — consent denied**

EC is present but the user has not given consent (or consent has been withdrawn). Callers must omit identity parameters from all downstream requests. The status code alone is sufficient to detect this case — body parsing is not required.

```json
{ "consent": "denied" }
```

**`204 No Content` — no EC present**

No `ts-ec` cookie and no `X-ts-ec` header was found on the request. The user has not yet established an EC on this publisher. No body is returned. Callers should proceed without identity enrichment.

#### Response headers (supplementary)

In addition to the JSON body, TS sets the following response headers for server-to-server callers, logging, and future use. These are not the primary integration contract — callers should read the JSON body.

| Header              | Value                                                         |
| ------------------- | ------------------------------------------------------------- |
| `X-ts-ec`           | `<ec_hash.suffix>` or absent if no EC                         |
| `X-ts-eids`         | Base64-encoded JSON array of OpenRTB 2.6 `user.eids` objects  |
| `X-ts-<partner_id>` | Resolved UID per partner (e.g., `X-ts-uid2`, `X-ts-liveramp`) |
| `X-ts-ec-consent`   | `ok` or `denied`                                              |

### 12.3 Mode B: Full auction orchestration (`/auction`)

Trusted Server owns the full auction path in Mode B. TS builds the OpenRTB request, injects EC identity and resolved partner UIDs, sends it to Prebid Server, receives bids, selects winners, and delivers the winner set to the publisher's ad server endpoint. The publisher's ad server does not build the OpenRTB request — it receives auction winners from TS and either accepts the programmatic winner or outbids it with a direct-sold placement.

**EC injection into the outbound OpenRTB request (changes from current behavior):**

- `user.id` is set to the full EC value (`hash.suffix`)
- `user.eids` is populated from the KV identity graph for this user (see OpenRTB structure below)
- `user.consent` is set to the decoded TCF string (currently always `null`)
- SSP-specific `ext.eids`: when calling a specific PBS adapter, only that SSP's resolved ID is included in the adapter-level `ext.eids`. All configured identity providers are included at the top-level `user.eids`.

**EC context in winner notification to publisher's ad server:**

When TS delivers auction winners to the publisher's ad server endpoint, the response includes EC identity so the publisher's ad server has full context for its outbid decision without needing to call `/identify` separately:

| Header            | Value                                                        |
| ----------------- | ------------------------------------------------------------ |
| `X-ts-ec`         | `<ec_hash.suffix>`                                           |
| `X-ts-eids`       | Base64-encoded JSON array of OpenRTB 2.6 `user.eids` objects |
| `X-ts-ec-consent` | `ok` or `denied`                                             |

### 12.4 OpenRTB 2.6 `user.eids` structure

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
        "source": "id5-sync.com",
        "uids": [{ "id": "ID5-abc", "atype": 3 }]
      },
      {
        "source": "uidapi.com",
        "uids": [{ "id": "A4A...", "atype": 3 }]
      }
    ]
  }
}
```

`atype` values follow the OpenRTB 2.6 specification: `1` = cookie/device, `2` = hashed email, `3` = partner-defined. All EC-derived IDs use `atype: 3`.

### 12.5 Partner taxonomy

Each partner registered in `partner_store` declares:

- `source_domain`: the OpenRTB `source` value for their EID (e.g., `"liveramp.com"`)
- `openrtb_atype`: integer (typically `3`)
- `bidstream_enabled`: boolean — whether this partner's UID should appear in `user.eids` on auction requests

### 12.6 User stories

**As a publisher using Mode A for analytics/attribution**, I want to call `/identify` from a browser script so that I can tag events and impressions with the user's EC hash and resolved partner UIDs using URL parameters.

**Acceptance criteria:**

- [ ] `GET /identify` returns `200` with a valid JSON body within 30ms when EC is present and consent is valid
- [ ] `uids` object contains one key per partner with `bidstream_enabled: true` and a resolved UID; partners with no resolved UID are omitted
- [ ] If consent is denied, response is `403 Forbidden` with body `{"consent": "denied"}`
- [ ] If no EC is present, response is `204 No Content` with no body
- [ ] Response headers `X-ts-ec`, `X-ts-eids`, `X-ts-<partner_id>`, and `X-ts-ec-consent` are present on `200` responses as supplementary signals

**As a publisher using a client-side wrapper for non-TS SSPs**, I want to call `/identify` from my Prebid.js configuration so that SSPs outside TS's auction receive the same identity enrichment as TS-orchestrated bids, enabling a fair winner comparison.

**Acceptance criteria:**

- [ ] `GET /identify` called from the browser returns resolved UIDs for all registered partners with a KV entry for this user
- [ ] A partner with no KV entry for this user is omitted from `uids` — no empty or null entries
- [ ] Response is available within 30ms so it does not block Prebid.js auction timeout

**As a publisher using Mode B**, I want Trusted Server to build and send enriched OpenRTB requests to Prebid Server and deliver winners to my ad server with full EC context, so my ad server can make outbid decisions without additional identity lookups.

**Acceptance criteria:**

- [ ] Outbound OpenRTB request to PBS contains `user.id` equal to the EC value
- [ ] `user.eids` contains one entry per partner with `bidstream_enabled: true` and a resolved UID in the KV graph
- [ ] `user.consent` contains the decoded TCF string when available
- [ ] Partners without a resolved UID for this user are omitted from `user.eids` (no empty entries)
- [ ] Winner notification to publisher's ad server includes `X-ts-ec`, `X-ts-eids`, and `X-ts-ec-consent` headers

---

## 13. Configuration

The following capabilities must be configurable without redeploying the binary:

- **EC enable/disable** — EC can be turned on or off per deployment
- **Publisher passphrase** — the HMAC key used for EC hash generation; same value across all of the publisher's domains; shared with trusted partners to form an identity-federated consortium
- **Identity graph store** — the KV store backing the EC hash → identity graph
- **Partner registry store** — the KV store backing partner configuration and API key validation
- **Partner records** — each partner's allowed sync domains, bidstream settings, pull sync configuration, and API credentials; managed via `/_ts/admin/partners/register` without redeployment

The exact configuration format (TOML keys, KV schema, JSON field names) is an engineering decision and will be documented in the technical design doc.

---

## 14. Documentation Updates

The following documentation changes are required alongside the EC feature:

- **Rename SyntheticID → Edge Cookie** across the entire `docs/` GitHub Pages site. The underlying concept is the same but the product name changes.
- **New integration guides**, one per customer type:
  - Publisher (full TS): enabling EC in `trusted-server.toml`, partner onboarding via `/_ts/admin/partners/register`
  - SSP: pixel sync integration guide, sync pixel URL format, callback handling, optional pull resolution endpoint
  - DSP: S2S batch API reference, authentication, conflict resolution behavior, optional pull resolution endpoint
  - Identity Provider: registering as a partner, `source_domain` and `openrtb_atype` configuration, sync patterns
- **API reference** for the four new endpoints: `GET /sync`, `GET /identify`, `POST /_ts/api/v1/sync`, and the partner-side pull resolution contract
- **Pull sync integration guide**: partner requirements for exposing a resolution endpoint, authentication, expected response shape, rate limit behavior
- **Consent enforcement guide**: how TCF and GPP signals are read, precedence rules, what happens on withdrawal

---

## 15. Open Questions

| #   | Question                                                                                                                                                                                                                                                                                         | Owner       | Status                                                                          |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------- | ------------------------------------------------------------------------------- |
| 1   | Partner provisioning: TS will expose a `/_ts/admin/partners/register` endpoint authenticated at the publisher level (bearer token issued per publisher Fastly service), so publishers can onboard SSP/DSP partners without touching KV directly. Engineering to define the exact auth mechanism. | Engineering | **Resolved** — `/_ts/admin/partners/register` endpoint, publisher-authenticated |
| 2   | Should TS Lite expose a `GET /health` endpoint so partners can programmatically verify their service is running and their partner config is active in KV?                                                                                                                                        | Product     | **N/A** — TS Lite deferred (see Section 5)                                      |

---

## 16. Success Metrics

| Metric                          | Target                                                                                  | Measurement method                                                                           |
| ------------------------------- | --------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| EC match rate (returning users) | >90% within 30 days                                                                     | Fastly real-time logs: ratio of requests with existing `ts-ec` cookie vs. new EC generations |
| Consent enforcement accuracy    | 0 ECs created for opted-out EU/UK users                                                 | Log audit: verify no `ts-ec` `Set-Cookie` in responses where consent signal is absent        |
| KV sync latency (pixel sync)    | p99 <75ms end-to-end                                                                    | Fastly log timing on `/sync` endpoint                                                        |
| S2S batch API throughput        | >500 mappings/sec sustained                                                             | Load test prior to partner onboarding                                                        |
| S2S pull sync resolution rate   | >30% of pull calls return a non-null uid within 60 days of first partner go-live        | Fastly log: pull call outcomes per partner                                                   |
| Identity graph fill rate        | >50% of EC hashes with at least 1 resolved partner ID within 60 days of partner go-live | KV scan sample                                                                               |

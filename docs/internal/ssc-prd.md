# Product Requirements: Server-Side Cookie (SSC)

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
6. [SSC Identity and Cookie Structure](#6-ssc-identity-and-cookie-structure)
7. [Consent Lifecycle](#7-consent-lifecycle)
8. [KV Store Identity Graph](#8-kv-store-identity-graph)
9. [Pixel Sync Endpoint](#9-pixel-sync-endpoint)
10. [S2S Batch Sync API](#10-s2s-batch-sync-api)
11. [Bidstream Decoration](#11-bidstream-decoration)
12. [Configuration](#12-configuration)
13. [Documentation Updates](#13-documentation-updates)
14. [Open Questions](#14-open-questions)
15. [Success Metrics](#15-success-metrics)

---

## 1. Overview

Server-Side Cookie (SSC) is a stable, privacy-respecting user identity mechanism built into Trusted Server. It replaces the existing SyntheticID system with a cleaner signal (IP address + publisher salt only), a consent-aware lifecycle, a server-side identity graph backed by Fastly KV Store, and a standalone "TS Lite" deployment mode that allows SSPs, DSPs, identity providers, and publishers to adopt SSC without deploying the full Trusted Server feature set.

SSC runs at a publisher-controlled first-party subdomain (e.g., `ssc.publisher.com`), sets a cookie scoped to the publisher's apex domain, and optionally orchestrates real-time bidding or decorates outbound ad requests with resolved identity signals from configured partners.

---

## 2. Problem Statement

### 2.1 SyntheticID signal degradation

The current SyntheticID uses User-Agent, Accept-Language, Accept-Encoding, and IP address as HMAC inputs. Each of these signals is eroding:

- **User-Agent reduction**: Chrome's UA freeze has eliminated OS version and minor browser version. The UA string no longer meaningfully differentiates users.
- **Accept-Language homogenization**: Browser defaults increasingly converge, reducing entropy.
- **IPv6 privacy extensions**: Modern operating systems rotate the interface ID portion of IPv6 addresses on a per-session or daily basis, causing SyntheticID mismatches for returning users.

The result is degrading match rates and false new-user rates on browsers where these signals change.

### 2.2 No consent enforcement

SyntheticID is created unconditionally. There is no mechanism to check TCF (EU/UK) or GPP (US) consent before creating the ID. This is a compliance gap that must be closed before SSC can be offered as a product to regulated publishers.

### 2.3 Adoption blocked by full TS requirement

SSPs, DSPs, and identity providers want the identity and sync capabilities of Trusted Server without the JS injection pipeline, HTML processing, proxy routing, and auction orchestration that full TS requires. There is no lightweight deployment path today, which blocks a large class of potential adopters.

---

## 3. Goals and Non-Goals

### Goals

- Replace SyntheticID's unstable browser signal inputs with IP address + publisher salt only
- Enforce TCF and GPP consent before creating or maintaining the SSC
- Implement real-time consent withdrawal: delete cookie and KV entry when consent is revoked
- Build a server-side identity graph in Fastly KV Store that accumulates resolved partner IDs over time
- Provide two KV write paths: real-time pixel sync redirects and S2S batch push from partners
- Expose two bidstream integration modes: header decoration (`/identify`) and full auction orchestration (`/auction`)
- Enable a "TS Lite" deployment mode via runtime TOML feature flags so SSC can run without the full TS feature surface

### Non-Goals

- Replacing the publisher's consent management platform (CMP): SSC reads and enforces consent signals; it does not generate them
- Building a data management platform (DMP): SSC stores resolved partner IDs as a sync spine, not audience segments
- Backward compatibility with SyntheticID: SSC uses a different cookie name, header name, and ID generation method. No migration path is provided
- Real-time user matching across unrelated domains (cross-site tracking)
- Data deletion framework: out of scope for this PRD; flagged for a follow-on document

---

## 4. Target Customers

| Customer type        | Deployment mode                | Primary value                                                       |
| -------------------- | ------------------------------ | ------------------------------------------------------------------- |
| Publisher (full TS)  | Full TS + SSC enabled          | Consent-aware first-party ID, bidstream enrichment, identity graph  |
| Publisher (SSC only) | TS Lite at `ssc.publisher.com` | First-party cookie at apex domain, identity sync                    |
| SSP                  | TS Lite                        | Pixel sync endpoint to build match table against SSC hash           |
| DSP                  | TS Lite                        | S2S batch API to push/receive ID mappings, enriched bid requests    |
| Identity provider    | TS Lite                        | Register as a partner, sync resolved IDs into the KV identity graph |

---

## 5. TS Lite Deployment Mode

### 5.1 Concept

TS Lite is a runtime configuration of the existing Trusted Server binary. It is not a separate binary or separate codebase. A publisher (or SSP/DSP deploying on behalf of a publisher) creates a Fastly service pointing to a subdomain — typically `ssc.publisher.com` — and deploys the standard TS WASM binary with a `trusted-server.toml` that disables all routes except SSC-related functionality.

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
| `POST /api/v1/sync`                    | Disabled | **Enabled**             |
| `GET /.well-known/trusted-server.json` | Enabled  | Enabled                 |

When a disabled route is requested, TS returns `404` with the header `X-ts-error: feature-disabled`.

### 5.3 Cookie domain and subdomain setup

The publisher points a subdomain of their choosing (e.g., `ssc`) via DNS CNAME to their Fastly service. They configure `publisher.domain = "publisher.com"` in `trusted-server.toml`. Trusted Server derives `cookie_domain = ".publisher.com"` from this setting and sets the SSC cookie with that domain attribute.

This gives the cookie read access across all subdomains of `publisher.com` — including `www.publisher.com` — without requiring a separate verification step. The publisher's control over their DNS and Fastly service implicitly proves TLD+1 ownership, following the same trust model as the existing `publisher.cookie_domain` setting.

**Constraint:** A publisher cannot configure a cookie domain outside their declared `publisher.domain`. Attempting to set `cookie_domain = ".otherdomain.com"` is rejected at startup validation.

### 5.4 Safari and browser compatibility

The SSC is set as an HTTP `Set-Cookie` response header (not via JavaScript). For server-set cookies on first-party publisher domains that are not classified as cross-site trackers by Safari's ITP, the effective maximum lifetime is 1 year — the same as the configured `Max-Age`. Since `ssc.publisher.com` is a publisher-owned domain, it is unlikely to be classified as a tracker.

The ITP interaction for users who arrive exclusively via third-party sync pixel redirects (where `ssc.publisher.com` may be seen as a cross-site recipient) will be monitored post-launch. A cookie refresh strategy — re-issuing `Set-Cookie` on every same-site organic request — is deferred pending production data.

---

## 6. SSC Identity and Cookie Structure

### 6.1 ID generation

The SSC is generated by HMAC-SHA256 of a fixed input set, using a publisher-specific secret key.

**Inputs (IP address + salt only):**

| Input      | Value                                                                                                                           |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------- |
| IP address | IPv4 as-is; IPv6 summarized to /64 prefix (first 4 hextets) — discards rotating interface ID. On dual-stack, IPv6 is preferred. |
| Secret key | Publisher-specific salt, configured in `trusted-server.toml`                                                                    |

**Removed from SyntheticID:**

- `User-Agent`
- `Accept-Language`
- `Accept-Encoding`
- Handlebars template (input is now fixed, not configurable)

**Output format (unchanged from SyntheticID):**

```
{64-character hex HMAC-SHA256}.{6-character random alphanumeric suffix}
```

The 64-character prefix is the stable, deterministic portion used as the KV store key. The 6-character suffix is random, regenerated each time a fresh SSC is created. Once an SSC is set in a cookie, the full value (prefix + suffix) is preserved on subsequent requests.

**IPv6 /64 prefix rationale:** The first 64 bits of an IPv6 address identify the network prefix assigned by the ISP or home router. The remaining 64 bits (the interface ID) are rotated by privacy extensions on most modern operating systems. Using only the /64 prefix produces a stable hash for returning users while discarding the rotating portion that would cause false new-user signals.

### 6.2 Cookie attributes

| Attribute | Value                                                                                     |
| --------- | ----------------------------------------------------------------------------------------- |
| Name      | `ts-ssc`                                                                                  |
| Domain    | `.publisher.com` (derived from `publisher.domain` in TOML)                                |
| Path      | `/`                                                                                       |
| Secure    | Yes                                                                                       |
| SameSite  | `Lax`                                                                                     |
| Max-Age   | `31536000` (1 year)                                                                       |
| HttpOnly  | No — JavaScript on `www.publisher.com` may need to read the value for ad stack decoration |

### 6.3 Response header

The SSC value is also set as a response header for server-side consumers:

```
X-ts-ssc: <ssc_hash.suffix>
```

This header is internal to Trusted Server and is stripped before proxying requests to downstream backends, consistent with how other `X-ts-*` headers are handled.

### 6.4 Retrieval priority

On each request, Trusted Server looks for an existing SSC in this order:

1. `X-ts-ssc` request header (set by TS on a prior response, forwarded by the publisher's infrastructure)
2. `ts-ssc` cookie
3. Generate fresh SSC (subject to consent check — see Section 7)

### 6.5 No backward compatibility with SyntheticID

SSC uses a different cookie name (`ts-ssc` vs `synthetic_id`), a different header name (`X-ts-ssc` vs `x-synthetic-id`), and a different ID generation method. No fallback to reading the `synthetic_id` cookie is provided. SyntheticID code remains in full TS and continues to function; SSC is a parallel system.

---

## 7. Consent Lifecycle

Consent enforcement is a core requirement of SSC. The system must not create or maintain an SSC for users who have not given consent, and must actively revoke the SSC when consent is withdrawn.

### 7.1 Consent signal sources and precedence

When evaluating consent on a given request, Trusted Server checks signals in the following order. The first signal found wins:

1. **`X-consent-advertising` request header** — set by the Didomi integration (or another CMP proxy) in a prior server-side decode. This is the freshest signal and takes precedence over browser-stored values.
2. **`euconsent-v2` cookie** — the TCF v2 consent string stored by the publisher's CMP.
3. **`gpp` cookie** — the IAB Global Privacy Platform string for US state-level consent.
4. **Default: no consent** — if no signal is found, do not create the SSC (fail safe).

### 7.2 Pre-creation consent check

Before creating a new SSC, Trusted Server evaluates the user's region (via Fastly's `x-geo-country` header) and applies the appropriate consent rule:

| Region                                                                                               | Required signal | Rule                                                                                            |
| ---------------------------------------------------------------------------------------------------- | --------------- | ----------------------------------------------------------------------------------------------- |
| EU member states                                                                                     | TCF string      | Create SSC only if `purposeConsents[1]` (store and/or access information on a device) is `true` |
| United Kingdom                                                                                       | TCF string      | Same as EU                                                                                      |
| US states with privacy laws (CA, CO, CT, VA, TX, OR, MT, DE, NH, NJ, TN, IN, IA, KY, NE, MD, MN, RI) | GPP string      | Create SSC unless user has opted out of sale or sharing of personal data                        |
| Rest of world                                                                                        | None required   | Create SSC on first visit                                                                       |

### 7.3 Consent withdrawal (real-time enforcement)

On every request, Trusted Server decodes the consent signal (a microsecond in-memory operation with no I/O). If consent is not present or has been revoked:

**If `ts-ssc` cookie is present:**

1. Delete the cookie by issuing `Set-Cookie: ts-ssc=; Max-Age=0; Domain=.publisher.com; Path=/; Secure; SameSite=Lax`
2. Delete the KV identity graph entry: `kv_store.delete(ssc_hash)` — this operation takes approximately 25ms and runs in the request path

**If no `ts-ssc` cookie is present:**

- Do nothing

**If consent is present:**

- Proceed with normal SSC create-or-refresh flow

**Known tradeoff:** The KV delete adds approximately 25ms of latency to the first request after consent withdrawal. This is an intentional product decision — real-time consent enforcement is a differentiating capability of Trusted Server, and the latency cost is acceptable.

### 7.4 Future: Data deletion framework

A formal data deletion endpoint (`POST /api/v1/delete-user`) that allows authenticated partners to trigger deletion of a user's KV entry and cookie is out of scope for this PRD. It is flagged as a follow-on requirement.

---

## 8. KV Store Identity Graph

### 8.1 Purpose

The Fastly KV Store serves as a persistent identity graph keyed on the SSC hash. It accumulates resolved partner IDs over time through two write paths: real-time pixel sync redirects and S2S batch pushes from partners. This graph is read at auction time to populate `user.eids` in outbound OpenRTB requests.

### 8.2 Schema

**KV key:** The 64-character hex hash portion of the SSC (without the `.suffix`). The hash is stable across sessions for the same user+network+key combination and is safe to use as a long-lived identifier.

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

The metadata field is used for consent withdrawal checks. When consent status must be evaluated for a user with an existing SSC, Trusted Server reads metadata only — not the full body — keeping the hot-path latency minimal.

### 8.3 TTL

KV entries are created or refreshed with a `time_to_live_sec=31536000` parameter (1 year), matching the cookie `Max-Age`. Fastly's TTL mechanism is eventual garbage collection — entries may persist up to 24 hours past expiry before being removed. This is acceptable for identity data; SSC does not use KV TTL for security-critical expiration.

### 8.4 Conflict resolution and atomic updates

When two write paths (pixel sync and S2S batch) attempt to update the same KV entry concurrently, Trusted Server uses Fastly's generation markers to perform atomic read-modify-write:

1. Read the current KV entry; capture the `generation` header
2. Merge the new partner ID into the `ids` map in memory
3. Write back with `if-generation-match: <generation>`
4. On 412 (Precondition Failed), retry from step 1 (up to 3 retries)

Within a successful write, conflicts between two different partners updating the same SSC key are resolved by last-write-wins per partner namespace. Partner IDs are keyed by partner ID in the `ids` map; different partners never overwrite each other's entries.

### 8.5 KV store names

Two KV stores are required:

| Store            | TOML key        | Contents                           |
| ---------------- | --------------- | ---------------------------------- |
| Identity graph   | `ssc_store`     | SSC hash → identity graph JSON     |
| Partner registry | `partner_store` | Partner ID → config + API key hash |

The existing `counter_store` and `opid_store` settings (currently defined but unused in `settings.rs`) can be deprecated in a follow-on cleanup.

---

## 9. Pixel Sync Endpoint

### 9.1 Purpose

The pixel sync endpoint allows SSPs and DSPs to synchronize their user IDs with the SSC hash via a browser-side redirect. When a partner's sync pixel fires, the user's browser is redirected through `ssc.publisher.com/sync`, Trusted Server reads the existing `ts-ssc` cookie, and writes the partner's user ID into the KV identity graph.

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

1. Read the `ts-ssc` cookie. If absent, redirect to `return` URL immediately without writing to KV. Do not create a new SSC during a sync — a sync redirect is not an organic user visit and must not be used to bootstrap identity.
2. Look up the partner record in `partner_store` KV using the `partner` parameter. Return `400` if the partner is not found.
3. Validate the `return` URL against the partner's `allowed_return_domains`. Return `400` if the domain is not on the allowlist.
4. Evaluate consent for this user (from KV metadata or decode from request cookies). If consent is not present, redirect to `return` without writing KV.
5. If consent is valid, perform an atomic read-modify-write to update `ids[partner_id]` in the KV identity graph (with generation marker — see Section 8.4).
6. Redirect to the `return` URL with `ts_synced=1` appended as a query parameter.

### 9.5 Security

- The `return` URL must match an allowlisted domain configured per partner. Open redirects are not permitted.
- Partners control when to fire their sync pixel; no HMAC signature is required on the inbound sync request.
- Anti-stuffing rate limit: a maximum of `sync_rate_limit` sync writes per SSC hash per hour per partner (configurable per partner in `partner_store`, default 100).

### 9.6 User stories

**As an SSP**, I want to fire a sync pixel when I see a user so that I can associate my user ID with the SSC hash and receive enriched bid requests when the publisher calls Trusted Server for auction.

**Acceptance criteria:**

- [ ] `GET /sync?partner=ssp_x&uid=abc&return=https://sync.ssp.com/ack` returns a redirect to the `return` URL within 50ms (excluding KV write time)
- [ ] KV entry for the SSC hash contains `ids.ssp_x.uid = "abc"` after a successful sync
- [ ] Sync is a no-op (redirect only, no KV write) if no `ts-ssc` cookie is present
- [ ] Sync is a no-op if the user has not given consent
- [ ] `return` URL domains not in partner's `allowed_return_domains` receive a `400` response
- [ ] Rate limit is enforced: more than `sync_rate_limit` writes per hour per SSC hash per partner are rejected with `429`

---

## 10. S2S Batch Sync API

### 10.1 Purpose

The S2S batch sync API allows partners to push ID mappings to Trusted Server in bulk via an authenticated REST endpoint. This write path handles large-scale partner-initiated syncs, back-fills for users whose browser-side pixel sync has not fired, and DSP-side match data that originates from non-browser contexts.

### 10.2 Endpoint

```
POST /api/v1/sync
```

### 10.3 Authentication

Partners authenticate using a Bearer token. The token is validated against a bcrypt hash stored in the partner's record in `partner_store` KV. This requires one KV lookup per API call but allows API key rotation without redeploying the binary.

```
Authorization: Bearer <api_key>
```

Partner provisioning (writing a partner record into `partner_store`) is performed as a manual admin operation. An automated provisioning endpoint is deferred to a follow-on.

### 10.4 Request

```
POST /api/v1/sync
Content-Type: application/json
Authorization: Bearer <api_key>

{
  "mappings": [
    {
      "ssc_hash": "<64-character hex hash>",
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
    { "index": 45, "reason": "ssc_hash_not_found" },
    { "index": 72, "reason": "consent_withdrawn" }
  ]
}
```

HTTP status `207 Multi-Status` when any mappings are rejected; `200 OK` when all are accepted.

### 10.6 Consent enforcement

Before writing a mapping, Trusted Server checks the KV metadata for the given SSC hash. Mappings for users with `consent.ok = false` are rejected with reason `consent_withdrawn`. Partners must not submit mappings for users who have withdrawn consent; this enforcement is a safeguard, not the primary compliance mechanism.

### 10.7 Conflict resolution

- If the KV entry does not exist for a given `ssc_hash`, the mapping is rejected with reason `ssc_hash_not_found`. The S2S API does not create new KV entries — only the SSC creation flow (from organic browser visits) can create entries.
- If the partner has an existing entry for the same `ssc_hash` and the request's `timestamp` is older than the stored `synced` timestamp, the mapping is skipped (no error, counted as accepted).
- Otherwise, atomic read-modify-write with generation markers (see Section 8.4).

### 10.8 User stories

**As a DSP**, I want to push my user ID mappings to Trusted Server in bulk so that the publisher's auction requests are enriched with my resolved ID and I can bid on users I recognize.

**Acceptance criteria:**

- [ ] `POST /api/v1/sync` with a valid Bearer token and a batch of up to 1000 mappings returns a response within 5 seconds
- [ ] Accepted mappings are written to the corresponding KV identity graph entries within 1 second
- [ ] Mappings for unknown `ssc_hash` values are rejected with `ssc_hash_not_found`
- [ ] Mappings for users with withdrawn consent are rejected with `consent_withdrawn`
- [ ] Invalid or expired Bearer tokens receive `401 Unauthorized`
- [ ] Requests exceeding 1000 mappings receive `400 Bad Request`
- [ ] Rate limiting by API key is enforced

---

## 11. Bidstream Decoration

### 11.1 Two integration modes

Trusted Server exposes two modes for injecting SSC identity into the bidstream. Publishers choose the mode that fits their existing ad stack.

### 11.2 Mode A: Header decoration (`/identify`)

For publishers whose existing ad server handles auction calls, Trusted Server provides an identification-only endpoint that returns the SSC value and resolved identity signals as response headers. The publisher's ad server reads these headers and injects them into its own OpenRTB bid requests.

**Endpoint:** `GET /identify`

**Response:** `204 No Content` with the following headers:

| Header              | Value                                                         |
| ------------------- | ------------------------------------------------------------- |
| `X-ts-ssc`          | `<ssc_hash.suffix>`                                           |
| `X-ts-eids`         | Base64-encoded JSON array of OpenRTB 2.6 `user.eids` objects  |
| `X-ts-<partner_id>` | Resolved UID per partner (e.g., `X-ts-uid2`, `X-ts-liveramp`) |

**If consent is not present:**

```
HTTP 204 No Content
X-ts-ssc-consent: denied
```

No identity headers are returned. The publisher's ad server must handle this case — typically by omitting `user.eids` from the bid request.

### 11.3 Mode B: Full auction orchestration (`/auction`)

For publishers using Trusted Server as their auction endpoint, SSC identity is injected directly into outbound OpenRTB requests to Prebid Server. This is an extension of the existing `/auction` endpoint behavior.

**Changes from current behavior:**

- `user.id` is set to the full SSC value (`hash.suffix`)
- `user.eids` is populated from the KV identity graph for this user (see OpenRTB structure below)
- `user.consent` is set to the decoded TCF string (currently always `null`)
- SSP-specific `ext.eids`: when calling a specific PBS adapter, only that SSP's resolved ID is included in the adapter-level `ext.eids`. All configured identity providers are included at the top-level `user.eids`.

### 11.4 OpenRTB 2.6 `user.eids` structure

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

`atype` values follow the OpenRTB 2.6 specification: `1` = cookie/device, `2` = hashed email, `3` = partner-defined. All SSC-derived IDs use `atype: 3`.

### 11.5 Partner taxonomy

Each partner registered in `partner_store` declares:

- `source_domain`: the OpenRTB `source` value for their EID (e.g., `"liveramp.com"`)
- `openrtb_atype`: integer (typically `3`)
- `bidstream_enabled`: boolean — whether this partner's UID should appear in `user.eids` on auction requests

### 11.6 User stories

**As a publisher using Mode A**, I want to call `/identify` from my ad server so that I can enrich my own auction requests with SSC identity signals without changing my auction infrastructure.

**Acceptance criteria:**

- [ ] `GET /identify` returns `204` with `X-ts-ssc` and `X-ts-eids` headers within 30ms (KV read + response)
- [ ] If consent is denied, response contains `X-ts-ssc-consent: denied` and no identity headers
- [ ] `X-ts-eids` is a valid base64-encoded OpenRTB 2.6 `user.eids` array
- [ ] Individual `X-ts-<partner_id>` headers are present for each partner with `bidstream_enabled: true` and a resolved UID

**As a publisher using Mode B**, I want Trusted Server to include resolved partner IDs in every auction request so that SSPs receive enriched bid requests without additional publisher-side configuration.

**Acceptance criteria:**

- [ ] Outbound OpenRTB request to PBS contains `user.id` equal to the SSC value
- [ ] `user.eids` contains one entry per partner with `bidstream_enabled: true` and a resolved UID in the KV graph
- [ ] `user.consent` contains the decoded TCF string when available
- [ ] Partners without a resolved UID for this user are omitted from `user.eids` (no empty entries)

---

## 12. Configuration

### 12.1 New `[ssc]` section in `trusted-server.toml`

```toml
[ssc]
enabled = true
ssc_store = "ssc_identity_store"   # Fastly KV store: SSC hash → identity graph
partner_store = "ssc_partners"     # Fastly KV store: partner ID → config + API key hash
secret_key = "<publisher-specific salt>"

# Partner configs live in partner_store KV, not in TOML.
# Use the admin tooling to provision new partners.
# This allows key rotation without redeploying the binary.
```

### 12.2 New `[features]` section

```toml
[features]
# Full TS defaults: all true
# TS Lite defaults: set the following to false
auction = true
js_injection = true
html_processing = true
proxy_routes = true
request_signing = true
ssc = true
```

### 12.3 Partner record schema (in `partner_store` KV)

KV key: the partner ID string (e.g., `"ssp_x"`)

```json
{
  "name": "Example SSP",
  "key_hash": "$2b$12$...",
  "source_domain": "example-ssp.com",
  "openrtb_atype": 3,
  "bidstream_enabled": true,
  "allowed_return_domains": ["sync.example-ssp.com"],
  "sync_rate_limit": 100
}
```

---

## 13. Documentation Updates

The following documentation changes are required alongside the SSC feature:

- **Rename SyntheticID → Server-Side Cookie** across the entire `docs/` GitHub Pages site. The underlying concept is the same but the product name changes.
- **New integration guides**, one per customer type:
  - Publisher (TS Lite): setting up `ssc.publisher.com`, configuring `trusted-server.toml`, DNS CNAME setup
  - SSP: pixel sync integration guide, sync pixel URL format, callback handling
  - DSP: S2S batch API reference, authentication, conflict resolution behavior
  - Identity Provider: registering as a partner, `source_domain` and `openrtb_atype` configuration, sync patterns
- **API reference** for the three new endpoints: `GET /sync`, `GET /identify`, `POST /api/v1/sync`
- **Consent enforcement guide**: how TCF and GPP signals are read, precedence rules, what happens on withdrawal

---

## 14. Open Questions

| #   | Question                                                                                                                                                                                                                                     | Owner   | Target resolution          |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- | -------------------------- |
| 1   | Partner provisioning flow: should partner records be written manually by a TS admin, or via a `/admin/partners/register` endpoint using the existing admin auth pattern? The latter is more scalable but requires additional implementation. | Product | Before engineering kickoff |
| 2   | Should TS Lite expose a `GET /health` endpoint so partners can programmatically verify their service is running and their partner config is active in KV?                                                                                    | Product | Before engineering kickoff |

---

## 15. Success Metrics

| Metric                           | Target                                                                                   | Measurement method                                                                             |
| -------------------------------- | ---------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| SSC match rate (returning users) | >90% within 30 days                                                                      | Fastly real-time logs: ratio of requests with existing `ts-ssc` cookie vs. new SSC generations |
| Consent enforcement accuracy     | 0 SSCs created for opted-out EU/UK users                                                 | Log audit: verify no `ts-ssc` `Set-Cookie` in responses where consent signal is absent         |
| KV sync latency (pixel sync)     | p99 <75ms end-to-end                                                                     | Fastly log timing on `/sync` endpoint                                                          |
| S2S batch API throughput         | >500 mappings/sec sustained                                                              | Load test prior to partner onboarding                                                          |
| Identity graph fill rate         | >50% of SSC hashes with at least 1 resolved partner ID within 60 days of partner go-live | KV scan sample                                                                                 |
| TS Lite adoption                 | First non-publisher customer (SSP or DSP) live within 90 days of launch                  | Customer record                                                                                |

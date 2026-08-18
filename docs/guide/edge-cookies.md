# Edge Cookies (EC)

Trusted Server persists a stable per-device identifier in a first-party
cookie on the publisher's domain. The cookie name is `ts-ec`. Trusted
Server also surfaces the current EC ID on the `x-ts-ec` response
header, and strips that header when the consent evaluation does not
permit EC use.

## Policy Posture

Trusted Server is technology. It is neutral on policy. The Edge Cookie
gives the deployer a cookie slot and configuration over the surrounding
attributes. The deployer determines the policy posture based on the
laws and contractual arrangements that apply to their deployment.
Privacy outcomes follow from that configuration, not from the cookie
mechanism itself.

For full operational onboarding (partner configuration, batch sync, identify, and auction verification), use the [EC Setup Guide](/guide/ec-setup-guide).

## How They Work

EC IDs are generated on first request using HMAC-SHA256 over the
normalized client IP and a configured secret, with a short random
suffix appended. On subsequent requests the value is read from the
`ts-ec` cookie and reused.

**Format:** `64-hex-hmac`.`6-alphanumeric-suffix`

**IP normalization:** IPv4 addresses pass through unchanged. IPv6
addresses are masked to the /64 prefix before hashing, so a device
that rotates its interface identifier under Privacy Extensions still
maps to a stable base.

### Determinism and Stability

| Scenario                                                    | Result                                               |
| ----------------------------------------------------------- | ---------------------------------------------------- |
| Same client IP, same secret, no cookie                      | Same 64-hex base; fresh suffix each mint.            |
| Same client IP, same secret, existing cookie                | Existing cookie value reused; no fresh mint.         |
| Same client IP, different secret                            | Different 64-hex base. Useful for rotating identity. |
| Multiple clients behind shared NAT, same secret, no cookies | Same 64-hex base; the suffix distinguishes them.     |

### Request Lifecycle

Every request passes through four phases. EC generation only happens on organic routes (publisher proxy, integration proxy, auction) — read-only endpoints like `/identify` and `/batch-sync` skip generation entirely. During pre-routing, Trusted Server builds consent from request-local cookies, headers, geolocation, and policy defaults; it does not load consent from a separate KV store.

```mermaid
sequenceDiagram
    participant B as Browser
    participant TS as Trusted Server
    participant KV as KV Store

    B->>TS: Request (ts-ec cookie + consent signals)
    Note over TS: Phase 1: Pre-routing<br/>Read EC from cookie<br/>Build consent context<br/>Extract device signals

    alt First Visit (no EC cookie)
        Note over TS: Phase 2: Routing (organic only)<br/>generate_if_needed()
        TS->>TS: HMAC-SHA256(IP) + random suffix
        TS->>KV: Create entry (consent, geo, device)
        Note over TS: Phase 3: Finalize<br/>Ingest Prebid EID cookies
        TS-->>B: Response + Set-Cookie: ts-ec=...
    else Return Visit (EC cookie present)
        Note over TS: Phase 2: Routing<br/>EC exists — skip generation
        Note over TS: Phase 3: Finalize<br/>Ingest Prebid EID cookies
        TS-->>B: Response<br/>(no cookie refresh)
    end

    Note over TS,KV: Phase 4: Post-send (background)<br/>Dispatch pull-sync to partners
```

### Response Finalization

After routing completes, the server evaluates consent state and cookie presence to decide what to do with the EC cookie on the response.

```mermaid
flowchart TD
    Start[ec_finalize_response] --> ConsentCheck{Consent<br/>allows EC?}

    ConsentCheck -- "No" --> ExplicitWithdrawal{Explicit<br/>withdrawal?}
    ExplicitWithdrawal -- "Yes" --> CookiePresent{Cookie was<br/>present?}
    CookiePresent -- "Yes" --> Withdraw["Expire ts-ec cookie<br/>Write withdrawal tombstone in ec_identity_store (24h TTL)<br/>Strip all x-ts-* headers"]
    CookiePresent -- "No" --> HeaderOnly["Strip all x-ts-* headers only<br/>(no cookie expiry or KV tombstone)"]
    ExplicitWithdrawal -- "No" --> HeaderOnly

    ConsentCheck -- "Yes" --> WasPresent{EC was present<br/>in request?}
    WasPresent -- "Yes, not generated" --> Returning["Ingest Prebid EID cookies<br/>No cookie or KV TTL refresh"]
    WasPresent -- "No, just generated" --> NewEc["Ingest Prebid EID cookies<br/>Set ts-ec cookie"]
```

When consent cannot be verified for the current request — for example, unknown jurisdiction or missing/undecodable consent signals in a regulated region — Trusted Server fails closed for EC use by stripping EC headers, but it does **not** treat that as authoritative revocation of an already-issued EC.

## Consent Model

EC creation is gated by jurisdiction. The server detects jurisdiction from geolocation data attached to the request and applies the corresponding consent rules. Live consent comes from request-local signals (`euconsent-v2`, `__gpp`, `__gpp_sid`, `us_privacy`, `Sec-GPC`) plus geolocation and policy defaults; there is no separate consent KV fallback.

```mermaid
flowchart TD
    Start[Detect Jurisdiction] --> J{Jurisdiction?}

    J -- "GDPR<br/>(EU/UK)" --> TCF{TCF string<br/>present?}
    TCF -- "Yes" --> P1{Purpose 1<br/>granted?}
    P1 -- "Yes" --> Allow([Allow EC])
    P1 -- "No" --> Deny([Deny EC])
    TCF -- "No" --> Deny

    J -- "US State" --> GPC{GPC header<br/>set?}
    GPC -- "Yes" --> Deny
    GPC -- "No" --> USTCF{TCF from CMP<br/>e.g. Didomi?}
    USTCF -- "Yes" --> USP1{Purpose 1<br/>granted?}
    USP1 -- "Yes" --> Allow
    USP1 -- "No" --> Deny
    USTCF -- "No" --> USP{US Privacy<br/>string?}
    USP -- "Yes" --> OptOut{Opt-out<br/>sale?}
    OptOut -- "No" --> Allow
    OptOut -- "Yes" --> Deny
    USP -- "No" --> Deny

    J -- "Non-regulated" --> Allow
    J -- "Unknown<br/>(no geo data)" --> Deny
```

- **GDPR**: Opt-in required. TCF Purpose 1 (store/access device) must be explicitly consented.
- **US State**: Opt-out model with three-tier fallback — GPC always blocks, then TCF if a CMP uses it, then US Privacy string, then fail-closed.
- **Non-regulated**: EC always allowed.
- **Unknown**: Fail-closed when jurisdiction cannot be determined.

The `ec_identity_store` KV store is the only EC lifecycle store. It holds identity graph state, source-domain keyed partner UIDs, a minimal consent snapshot used for EC entry metadata, and withdrawal tombstones. Consent interpretation for each request remains based on the live request signals listed above.

## Partner Sync Channels

Partner identities flow into the KV identity graph through three channels. Each writes to the same `ids` map in the KV entry via idempotent upsert logic: unchanged UIDs are accepted without a KV write, while different UIDs replace the stored value.

```mermaid
flowchart LR
    subgraph Browser-initiated
        Prebid["Prebid EID Cookies<br/><i>ts-eids + sharedId</i><br/>Passive cookie ingestion"]
    end

    subgraph Server-initiated
        Batch["Batch Sync (S2S)<br/><i>POST /_ts/api/v1/batch-sync</i><br/>Partner POST + Bearer auth"]
        Pull["Pull Sync (Background)<br/><i>TS calls partner URL</i><br/>Post-send on organic routes"]
    end

    Prebid --> KV[(KV Identity Graph<br/>ids map)]
    Batch --> KV
    Pull --> KV
```

### Prebid EID Cookie Flow

The `ts-eids` cookie bridges client-side Prebid user ID modules with the server-side identity graph.

```mermaid
sequenceDiagram
    participant Prebid as Prebid.js
    participant TSJS as TSJS Prebid Module
    participant B as Browser Cookie Jar
    participant TS as Trusted Server
    participant KV as KV Store

    Prebid->>Prebid: Auction completes
    Prebid->>TSJS: bidsBackHandler fires
    TSJS->>Prebid: getUserIdsAsEids()
    Prebid-->>TSJS: [{source, uids: [{id, atype}]}]
    TSJS->>TSJS: Base64 encode full OpenRTB-style EID array<br/>[{source, uids:[{id, atype, ext?}]}]
    TSJS->>B: document.cookie = "ts-eids=..."

    Note over B,TS: Next page request
    B->>TS: Request with ts-eids cookie
    TS->>TS: Base64 decode → parse OpenRTB-style EIDs<br/>match source domains to partners
    TS->>KV: upsert_partner_id() per match<br/>(skips write when UID unchanged)
```

Current TSJS writers preserve the full OpenRTB-style `{source, uids:[...]}` shape in `ts-eids`. The server remains backward-compatible with earlier flattened `{source, id, atype}` cookies during rollout, but new cookies use the structured `uids[]` form.

The `sharedId` cookie follows a similar path but is written directly by Prebid's SharedID module rather than by TSJS. The server reads it separately and maps it via the `sharedid.org` source domain.

### EID Seeding and Prebid Bidstream Forwarding

EIDs can reach the EC identity graph from either server-side pull sync or browser-side Prebid sync. During a Prebid-routed auction, Trusted Server combines those stored IDs with any same-request EIDs from Prebid.js, applies consent gating, and forwards the merged set to Prebid Server as OpenRTB `user.ext.eids`. Prebid Server then passes those EIDs downstream to demand partners in its OpenRTB requests.

```mermaid
sequenceDiagram
    participant B as Browser / Prebid.js
    participant TSJS as TSJS Prebid Module
    participant TS as Trusted Server
    participant KV as EC KV Identity Graph
    participant PS as Prebid Server
    participant DSP as Downstream Partners / DSPs

    alt Pull sync seeds partner UID
        TS->>DSP: Background pull sync request<br/>(EC ID + consent context)
        DSP-->>TS: Partner UID for EC
        TS->>KV: Upsert ids[source_domain] = UID
    else Prebid sync seeds browser EIDs
        B->>B: Prebid User ID modules resolve IDs
        B->>TSJS: getUserIdsAsEids()
        TSJS->>B: Write ts-eids cookie<br/>Base64 OpenRTB-style EIDs
        B->>TS: Next request with ts-eids
        TS->>KV: Decode cookie and upsert matched partner UIDs
    end

    Note over B,TS: Prebid-routed auction
    B->>B: getUserIdsAsEids() for current auction
    B->>TS: POST /auction<br/>adUnits + eids[] + ts-ec cookie
    TS->>KV: Resolve EC-backed source-domain IDs
    KV-->>TS: Stored partner UIDs
    TS->>TS: Convert stored UIDs to EIDs<br/>Merge + dedupe with request eids[]<br/>Apply consent gating
    TS->>PS: OpenRTB request<br/>user.ext.eids = merged EID set
    PS->>DSP: OpenRTB bid request<br/>user.ext.eids preserved for bidders
    DSP-->>PS: OpenRTB bid response
    PS-->>TS: OpenRTB seatbid response
    TS-->>B: Auction response + x-ts-eids header when available
```

The relevant OpenRTB structure forwarded to Prebid Server and downstream partners is:

```json
{
  "user": {
    "id": "<ec-id-when-forwarding-is-allowed>",
    "ext": {
      "eids": [
        {
          "source": "id5-sync.com",
          "uids": [
            {
              "id": "ID5-abc123",
              "atype": 1
            }
          ]
        },
        {
          "source": "liveramp.com",
          "uids": [
            {
              "id": "LR-xyz789",
              "atype": 3,
              "ext": {
                "rtiPartner": "idl"
              }
            }
          ]
        }
      ]
    }
  }
}
```

Server-resolved EIDs and current-request Prebid EIDs are deduplicated by `source + uid.id`. When a partner UID already exists in KV, pull sync does not periodically refresh it; browser-side Prebid sync can still replace the stored UID if a later `ts-eids` cookie carries a different value for the same configured partner source.

## Configuration

Configure EC settings in the `[ec]` section of `trusted-server.toml`. See the [Configuration Reference](/guide/configuration) for the full surface and environment variable overrides.

The shipped configuration carries a local-development passphrase, and
known placeholder values are rejected at startup with a settings load
error, because an HMAC computed with a known secret can be forged by
anyone who knows it. Replace the development passphrase before
running outside local development.

## What Goes in the Cookie

The EC value is the deterministic HMAC base plus a random suffix. It
contains no name, email, account identifier, or other field supplied
by the user. The value is written back as `Set-Cookie` only when the
consent evaluation permits EC creation for the detected jurisdiction.
See [GDPR Compliance](/guide/gdpr-compliance) for how signals are
interpreted.

The value passes a base64url-compatible allowlist. The cookie envelope
sets `Path=/`, `Secure`, `HttpOnly`, `SameSite=Lax`, and a `Max-Age`.
`Domain` is computed as `.{publisher.domain}`. The separate
`cookie_domain` setting applies only to non-EC cookies.

## Operational Notes

- Rotate the secret periodically. Rotation produces a new 64-hex base
  for subsequent mints.
- Watch the logs for cookie value rejections, which happen when a
  `ts-ec` cookie value carries characters outside the allowlist.

## Runtime Behavior Notes

- Returning requests with consent and an existing `ts-ec` do not refresh the EC cookie or KV TTL.
- Newly generated ECs receive `Set-Cookie: ts-ec=...`.
- When consent is blocked but not explicitly withdrawn, Trusted Server strips EC response headers for that request but leaves any existing `ts-ec` cookie intact; cookie expiry and tombstones happen only on explicit withdrawal.
- `/_ts/api/v1/identify` is read-oriented and returns identity enrichment for the authenticated partner. It computes `cluster_size` only when the EC entry does not already store one.
- `/_ts/api/v1/batch-sync` writes mappings into the EC identity graph. Mapping timestamps are retained for API compatibility but no longer order writes; valid mappings use idempotent last-write-wins semantics.
- Pull sync fills missing partner UIDs only. Existing partner UIDs are not periodically refreshed because EC entries no longer store per-partner sync timestamps.

## Next Steps

- Follow the [EC Setup Guide](/guide/ec-setup-guide)
- [Configuration Reference](/guide/configuration)
- [GDPR Compliance](/guide/gdpr-compliance) for consent signal handling
- Configure [Ad Serving](/guide/ad-serving)
- [Collective Sync](/guide/collective-sync) for cross-publisher data sharing details and diagrams

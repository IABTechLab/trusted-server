# API Reference

Quick reference for all Trusted Server HTTP endpoints.

## Endpoint Categories

- [First-Party Endpoints](#first-party-endpoints) - Core ad serving and proxying
- [Edge Cookie Endpoints](#edge-cookie-endpoints) - Identity sync and enrichment
- [Request Signing](#request-signing-endpoints) - Cryptographic signing and key management
- [The Administration Prefix](#the-administration-prefix) - Paths answered here and never forwarded
- [Inspection Endpoints](#inspection-endpoints) - What a deployment shows to anyone who asks
- [TSJS Library](#tsjs-library-endpoint) - JavaScript library serving
- [Utility Endpoints](#utility-endpoints) - Optional operational helpers
- [Integration Endpoints](#integration-endpoints) - Third-party service proxying

---

## Adapter and startup support

| Adapter      | Release status | Health     | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling     | Request normalization            |
| ------------ | -------------- | ---------- | -------------- | -------------- | ---------------- | ------------------------------ | -------------------------------- |
| `axum`       | development    | real       | `500`          | no             | multiple         | outermost sanitize             | none                             |
| `cloudflare` | development    | absent     | `500`          | no             | single           | outermost sanitize             | none                             |
| `fastly`     | production     | pre router | `500`          | yes            | multiple         | entry-point resolve + sanitize | none                             |
| `spin`       | experimental   | real       | `500`          | yes            | single           | outermost sanitize             | innermost Spin-header derivation |

`trusted client IP handling` describes the optional
`[trusted_client_ip]` configuration. Fastly resolves the authenticated header
at the entry point and sanitizes all forwarding headers. The other adapters
install the sanitizer as the outermost router middleware but do not resolve
that configuration. Spin additionally normalizes its runtime-provided
authority, scheme, and client-address headers in the innermost middleware.

## Route availability

| Router  | Path                                   | Methods                                                    | Shape          | Predicate                             | Fastly                | Axum                  | Cloudflare            | Spin                  |
| ------- | -------------------------------------- | ---------------------------------------------------------- | -------------- | ------------------------------------- | --------------------- | --------------------- | --------------------- | --------------------- |
| normal  | `/.well-known/trusted-server.json`     | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/__ts/page-bids`                      | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/__ts/page-bids`                      | `OPTIONS`                                                  | literal        | `always`                              | guarded               | guarded               | guarded               | guarded               |
| normal  | `/_ts/api/v1/batch-sync`               | `POST`                                                     | literal        | `always`                              | real                  | —                     | —                     | —                     |
| normal  | `/_ts/api/v1/ec/resolve`               | `POST`                                                     | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/api/v1/identify`                 | `GET`, `OPTIONS`                                           | literal        | `always`                              | real                  | —                     | —                     | —                     |
| normal  | `/_ts/clear-tester`                    | `GET`                                                      | literal        | `settings.tester_cookie.enabled`      | real                  | —                     | —                     | —                     |
| normal  | `/_ts/config.json`                     | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/config`                          | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/data`                            | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/debug/ja4`                       | `GET`                                                      | conditional    | `settings.debug.ja4_endpoint_enabled` | real                  | —                     | —                     | —                     |
| normal  | `/_ts/page-bids`                       | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/page-bids`                       | `OPTIONS`                                                  | literal        | `always`                              | guarded               | guarded               | guarded               | guarded               |
| normal  | `/_ts/permissions.json`                | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/permissions`                     | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/set-tester`                      | `GET`                                                      | literal        | `settings.tester_cookie.enabled`      | real                  | —                     | —                     | —                     |
| normal  | `/auction`                             | `POST`                                                     | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/click`                   | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/proxy-rebuild`           | `GET`, `POST`                                              | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/proxy`                   | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/sign`                    | `GET`, `POST`                                              | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/health`                              | `GET`                                                      | literal        | `always`                              | real                  | real                  | —                     | real                  |
| normal  | `/static/tsjs=<file>`                  | `GET`                                                      | template       | `path.starts_with(/static/tsjs=)`     | real                  | real                  | real                  | real                  |
| normal  | `/verify-signature`                    | `POST`                                                     | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/{*rest}`                             | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | template       | `publisher_fallback`                  | publisher fallback    | publisher fallback    | publisher fallback    | publisher fallback    |
| normal  | `/`                                    | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | literal        | `publisher_fallback`                  | publisher fallback    | publisher fallback    | publisher fallback    | publisher fallback    |
| normal  | `<proxy.asset_routes[].prefix>{*rest}` | `GET`, `HEAD`                                              | config derived | `settings.proxy.asset_routes[]`       | real                  | —                     | —                     | —                     |
| startup | `/health`                              | `GET`                                                      | literal        | `startup_error`                       | real                  | —                     | —                     | real                  |
| startup | `/{*rest}`                             | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | template       | `startup_error`                       | startup error (`500`) | startup error (`500`) | startup error (`500`) | startup error (`500`) |
| startup | `/`                                    | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | literal        | `startup_error`                       | startup error (`500`) | startup error (`500`) | startup error (`500`) | startup error (`500`) |

`real`, `unsupported`, `guarded`, `publisher fallback`, and `startup error` are
distinct observable dispositions. An em dash means that the adapter has no
local route; the publisher fallback can therefore receive that path. The
startup rows are a separate degraded router, not additional normal routes.

## Contract conventions

Each endpoint contract below states authentication, request and response shape,
status behavior, cache and CORS behavior, configuration gates, rate limiting,
and an example or an explicit not-applicable value.

`Auth: none` means that the endpoint has no built-in authentication. Unless an
endpoint says otherwise, it has no endpoint-specific CORS policy and no
in-process rate limiter. Responses still pass through the adapter's standard
response finalizer. Upstream proxies can preserve selected upstream headers;
that is not a blanket CORS grant.

There is no repository-wide error body schema. Some handlers return structured
JSON, some return an empty body, and shared adapter errors are plain text with
safe client-facing messages. Treat the status and body documented for each
endpoint as authoritative.

## Inspection Endpoints

What a deployment shows to anyone who asks, with no credential. A publisher
can point a reader, a regulator or an auditor at these addresses, and each
sees what the deployment decided for their own request.

### GET /\_ts/config and GET /\_ts/config.json

Shows the settings the deployment is running, after defaults are applied.
Anyone can read the scripts a page runs, and this lets anyone read the
configuration that runs at the edge too. Every secret is masked, whatever the
publisher's document says, and every value that is sensitive by default is
masked unless the publisher shows it. The publisher decides what else is shown
or hidden in [`[inspect]`](/guide/configuration#inspect), and `config = false`
there answers `404 Not Found`. The first address answers a page, and the
second answers the same information as JSON.

**Contract:** Auth: none. Request body: not applicable. Rate limit: none.
Endpoint-specific CORS: `Access-Control-Allow-Origin: *`.

**Response:**

- **Status:** `200 OK`
- **Headers:** `Cache-Control: no-store`

```json
{
  "masked": ["publisher.proxy_secret", "source.origin.url"],
  "settings": {
    "publisher": {
      "cookie_domain": ".example.com",
      "domain": "example.com",
      "proxy_secret": "XXXX"
    },
    "source": {
      "module": "origin",
      "origin": {
        "url": "XXXX"
      }
    }
  },
  "version": "0.1.0"
}
```

| Field      | Meaning                                                                                                                                         |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `settings` | The running settings, with each masked value replaced by `XXXX` and every object's keys sorted, so the same settings always give the same bytes |
| `masked`   | Every masked path, sorted, so a reader can see what was withheld                                                                                |
| `version`  | The version of the core crate that answered                                                                                                     |

A secret is masked twice over. The settings loader records each value it fills
from the secret store, and those paths are masked. Then any string that still
contains one of those values is masked wherever it sits, a key included.

`permissions` shows the policy file's name as `policy` and its SHA-256 as
`sha256`, neither masked by default, so a publisher can check which exact file
a domain runs. The file itself is never shown, because the settings loader
takes it out of the settings when it loads the policy.

**Example:**

```bash
curl -s "https://edge.example.com/_ts/config.json"
```

### GET /\_ts/data

Shows a reader what the deployment holds against the Edge Cookie their own
browser sent, being when the record was made, the consent recorded, the
location and device class kept, and which partners hold an identifier of their
own for the same browser. It answers as a page only, because the answer is for
a person.

**Contract:** Auth: none. The record shown is the one stored against the
identifier in the request's own `ts-ec` cookie, and no other can be asked for.
Request body: not applicable. Rate limit: none. Endpoint-specific CORS: none,
so no other origin may read it.

The Edge Cookie is `HttpOnly`, so a script on the page cannot read the
identifier, and this page does not undo that.

1. Only a browser opening the address as a page is answered, which the browser
   marks with `Sec-Fetch-Mode: navigate` and `Sec-Fetch-Dest: document`. A
   script cannot set either header, so a `fetch` from a page, a frame and an
   embedded object are each answered `403 Forbidden`. A browser sends these
   headers only to a trustworthy origin, so every browser on a plain `http://`
   host other than localhost is answered `403 Forbidden` too.
2. The page is sent with `Content-Security-Policy: sandbox`, which gives the
   document an origin of its own, so a page that opened it cannot read it.
3. No identifier is shown. The Edge Cookie's value and each partner's
   identifier show as `XXXX`.

Two parties other than the reader can still read the page, and neither finds
an identifier in it. The first is whoever holds an identifier, because a tool
can send it as the cookie along with both headers. The second is a service
worker the publisher's site registers for the whole site, because it handles
this navigation as it does every other on the site.

**Response:**

- **Status:** `200 OK`, or `403 Forbidden` when the request is not a browser
  opening the page
- **Headers:** `Cache-Control: no-store, private`, `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
  `Cross-Origin-Resource-Policy: same-origin` and the policy below

```http
Content-Security-Policy: sandbox; default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'
```

The page shows this information.

```json
{
  "consent_updated": "2026-03-14T09:30:00Z",
  "created": "2026-03-14T09:30:00Z",
  "held": true,
  "identifier": "XXXX",
  "record": {
    "consent": { "ok": true, "tcf": "CP...", "updated": 1773480600 },
    "created": 1773480600,
    "geo": { "country": "GB", "region": "ENG" },
    "ids": { "partner.example.com": { "uid": "XXXX" } },
    "pub_properties": {
      "origin_domain": "example.com",
      "seen_domains": ["example.com"]
    },
    "v": 1
  },
  "version": "0.1.0",
  "withdrawn": false
}
```

| Field                           | Meaning                                                                       |
| ------------------------------- | ----------------------------------------------------------------------------- |
| `held`                          | Whether a record is held against the request's Edge Cookie                    |
| `why`                           | The reason, when nothing is held                                              |
| `identifier`                    | Always `XXXX`, because the Edge Cookie's value is never shown                 |
| `record`                        | The record as it is stored, with each partner's identifier replaced by `XXXX` |
| `created` and `consent_updated` | The two times in the record, written as dates                                 |
| `withdrawn`                     | Whether the record is the one a withdrawal of consent leaves                  |
| `version`                       | The version of the core crate that answered                                   |

Nothing is held, and `why` gives the reason, when the deployment keeps no
identity graph, when the request carried no Edge Cookie, when the cookie is
not one the deployment issued, and when no record is stored against it. Only
the Fastly adapter keeps an identity graph, so every other adapter answers
that nothing is held.

Nothing is created or written for the reader. The Edge Cookie lifecycle does
not run for this request, so no cookie is set and no identity row is written.

### GET /\_ts/permissions and GET /\_ts/permissions.json

Shows the permissions resolved for the request that asks, the signals that
produced them and the terms the data is held under. It is the evaluation a
page receives as `window.tsjs.permissions`, resolved from the request's own
signals and location. The first address answers a page, and the second
answers the same information as JSON.

**Contract:** Auth: none. Request body: not applicable. Rate limit: none.
Endpoint-specific CORS: `Access-Control-Allow-Origin: *`, so a page on another
site can render the data. A request from another origin carries no cookies, so
that page reads the resolution of a request with no stored signal.

**Response:**

- **Status:** `200 OK`
- **Headers:** `Cache-Control: no-store`, because each answer is the asking
  request's own.

```json
{
  "awaiting": [],
  "modules": { "configured": ["gpc", "tcf"], "contributed": ["tcf"] },
  "policy": "standard",
  "set": ["necessary.operations.storage"],
  "signals": [{ "module": "tcf", "scheme": "tcf", "value": "CP..." }],
  "storageWithdrawn": false,
  "tdls": [],
  "version": "0.1.0"
}
```

| Field                 | Meaning                                                                                                   |
| --------------------- | --------------------------------------------------------------------------------------------------------- |
| `set`                 | The Data Use identifiers that are set for this request                                                    |
| `awaiting`            | The ones still waiting for a signal that a configured module could give                                   |
| `signals`             | Each signal a module read and found valid, as it was received, with the module that read it               |
| `tdls`                | The terms documents the data is available under                                                           |
| `storageWithdrawn`    | Whether the request explicitly withdrew device storage, as distinct from not setting it                   |
| `modules.contributed` | The signal modules that produced a signal used in this resolution                                         |
| `modules.configured`  | The signal modules the publisher configured, or `null` when it named none and the adapter's own list runs |
| `policy`              | The permission policy file whose rules the answer resolved, by the name `[permissions] policy` gives it   |
| `version`             | The version of the core crate that answered                                                               |

A module in `configured` and not in `contributed` ran and found nothing to
work with, which is a different fact from a module that was never configured.

A policy's name is its file's stem, which two versions of one file share, so
the SHA-256 that `/_ts/config` shows is what matches the answer to one exact
copy of the file.

Nothing is created or written for the reader. The Edge Cookie lifecycle does
not run for this request, so no cookie is set and no identity row is written.

**Example:**

```bash
curl -s "https://edge.example.com/_ts/permissions.json" -H "Sec-GPC: 1"
```

## Utility Endpoints

### GET /\_ts/set-tester

Sets a first-party tester marker cookie for QA or troubleshooting workflows.

**Contract:** Auth: none. Request body: not applicable. Rate limit: none.
Endpoint-specific CORS: none.

**Configuration:** Disabled by default. Enable with:

```toml
[tester_cookie]
enabled = true
```

**Response when enabled:**

- **Status:** `204 No Content`
- **Headers:**

```http
Set-Cookie: ts-tester=true; Domain=<publisher.cookie_domain>; Path=/; Secure; SameSite=Lax; Max-Age=<max_age_seconds>
Cache-Control: no-store, private
```

The cookie domain comes from `[publisher].cookie_domain`. The lifetime comes
from `[tester_cookie].max_age_seconds` and defaults to 30 days (2592000).

**Response when disabled:**

- **Status:** `404 Not Found`
- **Set-Cookie:** none

**Example:**

```bash
curl -i "https://edge.example.com/_ts/set-tester"
```

### GET /\_ts/clear-tester

Clears the first-party tester marker cookie for QA or troubleshooting workflows.

**Contract:** Auth: none. Request body: not applicable. Rate limit: none.
Endpoint-specific CORS: none.

**Configuration:** Uses the same `[tester_cookie].enabled` flag as `/_ts/set-tester`.

**Response when enabled:**

- **Status:** `204 No Content`
- **Headers:**

```http
Set-Cookie: ts-tester=; Domain=<publisher.cookie_domain>; Path=/; Secure; SameSite=Lax; Max-Age=0
Cache-Control: no-store, private
```

The cookie domain comes from `[publisher].cookie_domain`.

**Response when disabled:**

- **Status:** `404 Not Found`
- **Set-Cookie:** none

**Example:**

```bash
curl -i "https://edge.example.com/_ts/clear-tester"
```

---

## First-Party Endpoints

### POST /auction

Browser and programmatic auction endpoint. It accepts the Trusted Server ad-unit
request shape and returns an OpenRTB response. Creative markup follows the
independent `[auction].sanitize_creatives` and `[auction].rewrite_creatives`
settings; sanitization is opt-in and rewriting is enabled by default.

Configured provider IDs appear in response metadata and provider responses.
Consumers that previously matched the literal provider name `prebid` must use
the configured demand source name, such as `pbs_main`.

**Contract:** Auth: none. The buffered JSON body is limited to 256 KiB. A
successful auction and an intentional no-bid both return `200` JSON; disabling
`[auction]` or denying consent produces the no-bid form without calling a
provider. An oversized body returns `413`; malformed input and validation
failures use the shared adapter error mapping; provider failures use the shared
`5xx` mapping. The endpoint sets no dedicated cache or CORS policy and has no
in-process rate limiter. Provider selection, timeouts, and fan-out are governed
by `[auction]` and the demand sources `[demand]` selects.

**Request Body:**

```json
{
  "adUnits": [
    {
      "code": "header-banner",
      "mediaTypes": { "banner": { "sizes": [[728, 90]] } },
      "bids": [
        {
          "bidder": "example-server-bidder",
          "params": { "placement": "example-placement" }
        }
      ]
    }
  ]
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/auction \
  -H "Content-Type: application/json" \
  -d '{"adUnits":[{"code":"banner","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}'
```

#### PBS stored-request intent

The reserved `trustedServer` bid accepts `storedRequest` inside `params`, beside
`bidderParams` and `zone`. It does not accept provider IDs, endpoints, or stored IDs.
Bidder keys still resolve through the server's `[auction.bidders]` routes.

| `params.storedRequest` | PBS behavior                                                                                                                                                                                                         |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `false`                | Disable stored fallback for this slot on every PBS provider. Usable inline demand still runs.                                                                                                                        |
| `true`                 | Permit stored fallback using the slot `code` as the stored impression ID. Usable inline params take precedence within each provider.                                                                                 |
| Omitted                | Preserve legacy inference for existing callers and server-generated opportunities. Missing, `null`, or empty `bidderParams` permits stored demand; routed empty bidder objects also retain fallback after overrides. |

`storedRequest: null` is invalid, as are strings, numbers, arrays, and objects.
JavaScript `undefined` is removed by JSON serialization, so the server receives an
omitted value and applies legacy inference. An invalid serialized value rejects the
whole envelope, including its inline params and zone, and increments the
malformed-envelope diagnostic. Independent valid direct bidder entries and eligible
non-PBS providers still run; this is not whole-request HTTP rejection. An absent or
empty `bids` list also retains legacy stored inference.

PBS applies provider-local overrides before checking inline demand. If no usable
inline params remain, it uses stored demand only when permitted, otherwise it
omits the impression. If none remain, it makes no PBS request. Eligible APS and
standard providers are unaffected.

A generated envelope that should not request PBS stored demand:

```json
{
  "bidder": "trustedServer",
  "params": { "bidderParams": {}, "storedRequest": false }
}
```

An intentional stored request, posted to `https://edge.example.com/auction`:

```json
{
  "adUnits": [
    {
      "code": "homepage-banner",
      "mediaTypes": { "banner": { "sizes": [[728, 90]] } },
      "bids": [
        {
          "bidder": "trustedServer",
          "params": { "bidderParams": {}, "storedRequest": true }
        }
      ]
    }
  ]
}
```

Stored demand retains existing PBS fanout. Each participating PBS instance must
have the requested slot-code ID. This filtering does not prevent errors from
unrelated invalid bidder params. See [deployment ordering](/guide/integrations/prebid#stored-intent-deployment).

### GET /\_ts/page-bids and GET /\_\_ts/page-bids

Runs the page-level auction used by the TSJS single-page-application hook. The
`/__ts/page-bids` spelling is a deprecated compatibility alias and adds a
`Link` header pointing to its removal issue.

**Request schema:** `path` is optional and defaults to `/`; query and fragment
text inside it are removed before creative-opportunity matching. `format` is
optional and must be `json` when present. A request is admitted only when
`Sec-Fetch-Site: same-origin`, or when `Sec-Fetch-Site` is absent and the
request carries `X-TSJS-Page-Bids`. Every `OPTIONS` request is denied so a
cross-origin caller cannot acquire permission to send that non-simple header.

**Response schema:** `200 application/json` with `{ "slots": [...], "bids":
{...} }`. The ad-stack kill switch, an auction kill switch, or denied consent
returns empty collections. Unknown `format` returns `400`; a rejected GET or
any `OPTIONS` returns `403`; missing `[creative_opportunities]` returns `404`.
The `200`, `400`, and `403` terminal responses are `private, no-store`. The
endpoint has no built-in authentication, CORS grant, or rate limiter.

**Configuration:** `[creative_opportunities]`, `[auction]`, slot page patterns,
demand sources, and consent settings. Bot and prefetch requests retain slot
shape but skip live provider calls.

**Example:**

```bash
curl 'https://edge.example.com/_ts/page-bids?path=%2Fnews&format=json' \
  -H 'Sec-Fetch-Site: same-origin'
```

---

## Edge Cookie Endpoints

Partners are configured statically in `[[ec.partners]]` and loaded into an in-memory registry at startup. There is no runtime partner-registration endpoint and the legacy browser pixel sync endpoint has been removed; browser-resolved IDs are ingested through the identifier modules a build links, such as the Prebid module reading its `ts-eids` cookie.

---

### GET /\_ts/api/v1/identify

Returns EC identity plus the authenticated partner's UID and EID for the current user.

**Auth:** Bearer token (`Authorization: Bearer <partner-api-key>`)

**Contract:** The request has no body and uses the `ts-ec` cookie plus the
request consent context. `200` returns the JSON shape below; `204` means there
is no active EC ID; `401` means the Bearer token is unknown; and `403` means
consent or the request origin was denied. A KV read failure still returns `200`
with `degraded: true`. Every response is `no-store`, varies on `Origin` and
`Authorization`, and also sends `Pragma: no-cache`. HTTPS origins equal to or
below `[publisher].domain` receive reflected credentialed CORS for
`GET, OPTIONS`; every other supplied origin receives `403`. `OPTIONS` returns
`200` for no origin or an allowed origin. There is no endpoint rate limiter.
The endpoint is Fastly-only and requires the EC store and a statically
configured `[[ec.partners]]` token.

**Request:**

- Uses `ts-ec` cookie and consent signals

**Response (example):**

```json
{
  "ec": "954d...e0c3.nZ1GxL",
  "consent": "ok",
  "degraded": false,
  "source_domain": "ssp.example.com",
  "uid": "mock-user-123",
  "eid": {
    "source": "ssp.example.com",
    "uids": [{ "id": "mock-user-123", "atype": 3 }]
  },
  "cluster_size": 3
}
```

`uid`, `eid`, and `cluster_size` are optional and omitted when unavailable
(e.g. no partner UID synced yet, KV read degraded, or cluster size not
re-evaluated within the recheck window).

---

### POST /\_ts/api/v1/batch-sync

Server-to-server batch sync endpoint for writing EC ID to partner UID mappings. Mapping timestamps are retained in the request schema for compatibility, but they no longer order writes because EC identity entries do not store per-partner sync timestamps.

**Auth:** Bearer token (`Authorization: Bearer <partner-api-key>`)

**Contract:** The JSON body is limited to 2 MiB and 1,000 mappings. The
per-partner, per-minute limit is `ec.partners[].batch_rate_limit`. Responses are
`200` when every mapping is accepted or `207` when at least one is rejected;
item reasons are `invalid_ec_id`, `invalid_partner_uid`, `ineligible`, or
`kv_unavailable`. Authentication failure returns `401 {"error":"invalid_token"}`;
rate exhaustion returns `429`; an oversized body returns `413`; too many
mappings returns `400`. The endpoint emits JSON but defines no dedicated cache
or CORS headers. It is Fastly-only and requires the EC KV store.

**Batch processing behavior:**

- Every mapping is validated before any KV update. Validation errors retain their
  original input index.
- Valid mappings are grouped by normalized EC ID: only the 64-character hex
  prefix is lowercased; the six-character suffix remains case-sensitive.
- Groups are processed in first-valid-occurrence order, with one call to the
  CAS-protected KV update path per distinct normalized EC ID. Within a group, the
  last valid `partner_uid` in request order is persisted. An invalid mapping
  never replaces a group's final UID.
- A successful or unchanged update accepts every valid mapping in its group.
  Missing and withdrawn EC entries reject every valid mapping in their group as
  `ineligible`.
- If a KV infrastructure failure occurs, every valid mapping in the failing
  group and each unprocessed valid group is rejected as `kv_unavailable`; no
  later group is updated. Already processed groups keep their outcomes, and
  validation errors are preserved.
- Each input receives exactly one outcome, so `accepted + rejected` equals the
  number of submitted mappings. `errors` is sorted by original input index. The
  endpoint returns `200 OK` only when all mappings are accepted; otherwise it
  returns `207 Multi-Status`.

Groupwise failure behavior is intentional: for `A(valid), B(valid), A(valid)`,
if A's group succeeds and B's group has an infrastructure failure, both A
mappings are accepted even though the second A appears after B in the input.

**Request Body:**

```json
{
  "mappings": [
    {
      "ec_id": "954d8e7398dd993f78e3875ca1ef7841249781240e913157c1f2d6a6c960e0c3.nZ1GxL",
      "partner_uid": "mock-user-123",
      "timestamp": 1775147300
    }
  ]
}
```

**Response:**

```json
{
  "accepted": 1,
  "rejected": 0,
  "errors": []
}
```

**Example:** `curl -X POST https://edge.example.com/_ts/api/v1/batch-sync -H
'Authorization: Bearer <partner-api-key>' -H 'Content-Type: application/json'
--data @mappings.json`.

---

### POST /\_ts/api/v1/ec/resolve

Resolve endpoint for client-side Edge Cookie modules. The page posts a value that the module verifies and creates the Edge Cookie value. Used only when a client-side module is selected (for example the `client_fixed` demonstration module). Server-side modules such as HMAC do not use it.

**Auth:** None, but the request must carry an `Origin` on the publisher's own domain (a foreign or missing `Origin` answers `403`). This is a first-party POST from the page. The module is responsible for verifying the posted value before trusting it.

**Request Body:** the module's value, opaque to the core. For `client_fixed` this is the fixed known word sent as `text/plain`.

**Behavior:** gated by the [permission model](/guide/permission-model) exactly like organic generation. The module is asked before the [bot gate](/guide/configuration#device-configuration) is checked, so a request that does not pass the bot gate still reaches the module and can get the `400` and `409` rejections below. On success the identifier is written to the identity graph first, where there is one, then the EC cookie is set on this response (`HttpOnly`, `Secure`, `SameSite=Lax`) together with the `ts-ecr` marker cookie the page script can read, and the status is `200`. When the permission gate is closed, the request does not pass the bot gate, no client-side module is configured, or the module produces no identifier, the response is `204` with no cookie. With no identity graph, as on the Axum, Cloudflare and Spin adapters and on a Fastly service with no `[ec] ec_store`, the cookie is set with no graph row. Rejections: `403` for a missing or foreign `Origin`, `415` for a content type other than `text/plain` or `application/json`, `413` for an oversized body, `400` when the created identifier is outside the identifier bounds, `409` when the request already carries a different identity, and `503` when the identity-graph write fails. Every response the handler builds carries `Cache-Control: no-store`.

---

### GET /first-party/proxy

Unified proxy for resources referenced by creatives (images, scripts, CSS, etc.).

**Query Parameters:**

| Parameter | Type    | Required | Description                                                                 |
| --------- | ------- | -------- | --------------------------------------------------------------------------- |
| `tsurl`   | string  | Yes      | Target URL without query parameters (base URL)                              |
| `tstoken` | string  | Yes      | Base64url SHA-256 token derived from the encrypted reconstructed target URL |
| `tsexp`   | integer | No       | Unix expiry carried by newly minted URLs and covered by `tstoken`           |
| `*`       | any     | No       | Original target URL query parameters (preserved as-is)                      |

**Response:**

- **Content-Type:** Mirrors upstream or inferred from content
- **Body:** Proxied resource content
  - HTML responses: Rewritten with creative processor
  - Image responses: Proxied with content-type inference
  - Other: Passed through

**Behavior:**

- Validates `tstoken` against reconstructed URL
- Follows redirects (301/302/303/307/308, max 4 hops)
- Injects EC ID as `ts-ec` query parameter
- Logs 1×1 pixel impressions
- Enforces `proxy.allowed_domains` on the initial fetch and every redirect;
  an empty list is open mode

**Contract:** Auth: signed query token, not HTTP credentials. Success preserves
the upstream status and selected headers; HTML and CSS can be rewritten and
image content type can be inferred. Missing `tsurl`/`tstoken`, malformed or
expired `tsexp`, and upstream proxy failures currently use the shared `502`
proxy-error mapping; a mismatched token or blocked host returns `403`.
Redirects are followed for 301/302/303/307/308, with at most four hops. Cache
and CORS are derived from the sanitized upstream response and response
finalization; there is no endpoint CORS grant or rate limiter. This endpoint
validates an existing signature—it does not mint one. Signing depends on
`publisher.proxy_secret`; server-side fetches also enforce
`proxy.allowed_domains`.

**Example:**

```bash
# Original URL: https://ad.doubleclick.net/pixel?id=123&type=view
# Signed proxy URL:
curl "https://edge.example.com/first-party/proxy?tsurl=https://ad.doubleclick.net/pixel&id=123&type=view&tstoken=abc123xyz..."
```

**Error Responses:**

- `403 Forbidden` - Token validation or allowed-domain validation failed
- `502 Bad Gateway` - Missing signing fields, invalid/expired expiration, or
  upstream proxy failure

---

### GET /first-party/click

Click tracking redirect endpoint.

**Query Parameters:**

| Parameter | Type    | Required | Description                                                                 |
| --------- | ------- | -------- | --------------------------------------------------------------------------- |
| `tsurl`   | string  | Yes      | Target redirect URL without query parameters                                |
| `tstoken` | string  | Yes      | Base64url SHA-256 token derived from the encrypted reconstructed target URL |
| `tsexp`   | integer | No       | Optional Unix expiry covered by `tstoken`                                   |
| `*`       | any     | No       | Original target URL query parameters                                        |

**Response:**

- **Status:** `302 Found`
- **Location:** Reconstructed target URL with EC ID injected

**Behavior:**

- Validates `tstoken` against reconstructed URL
- Injects `ts-ec` query parameter
- Logs click metadata (tsurl, referer, user agent)
- Does not proxy content (redirect only)

**Contract:** Auth: signed query token. A valid request returns `302` with
`Cache-Control: no-store, private`; it does not apply `proxy.allowed_domains`
because it performs no server-side fetch. Signature errors have the same `403`
versus shared `502` split as `/first-party/proxy`. The route has no dedicated
CORS grant or rate limiter and validates rather than mints a signature.

**Example:**

```bash
curl -I "https://edge.example.com/first-party/click?tsurl=https://advertiser.com/landing&campaign=123&tstoken=xyz..."
# → 302 Location: https://advertiser.com/landing?campaign=123&ts-ec=abc123
```

---

### GET/POST /first-party/sign

URL signing endpoint. Returns a signed first-party proxy URL for a valid HTTP or HTTPS target. When `proxy.allowed_domains` is non-empty, the endpoint checks the parsed target host before signing. An empty list permits every valid host.

**Contract:** Auth: none. GET accepts a `url`
query value; POST accepts `{ "url": "..." }` and is limited to 64 KiB. `200`
returns `{href, base}` and gives `href` a 30-second `tsexp`. A disallowed host
returns `403`, an oversized POST returns `413`, and malformed/unsupported URLs
or JSON use the current shared error mapping. The endpoint sets JSON content
type but no dedicated cache or CORS policy and has no rate limiter. It mints a
new proxy signature; it does not validate a caller-supplied `tstoken`.
Signing depends on `publisher.proxy_secret` and checks
`proxy.allowed_domains` before minting.

**Request Methods:** GET or POST

**GET Request:**

```bash
curl "https://edge.example.com/first-party/sign?url=https://cdn.example.com/pixel.gif"
```

**POST Request:**

```bash
curl -X POST https://edge.example.com/first-party/sign \
  -H "Content-Type: application/json" \
  -d '{"url":"https://cdn.example.com/pixel.gif"}'
```

**Response:**

```json
{
  "href": "/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fpixel.gif&tstoken=abc123...&tsexp=1234567890",
  "base": "https://cdn.example.com/pixel.gif"
}
```

`href` is the signed proxy path. `base` is the normalized target without its query or fragment.

**Error Responses:**

- `403 Forbidden`: The target has a valid host that does not match a non-empty `proxy.allowed_domains` list
- `413 Payload Too Large`: The POST body exceeds 64 KiB
- Shared `5xx`: Malformed JSON or an invalid/unsupported target under the
  current proxy-error mapping

**Use Cases:**

- TSJS creative runtime (image/iframe proxying)
- Dynamic URL signing in client-side code
- Testing proxy URL generation

---

### POST /first-party/proxy-rebuild

URL mutation recovery endpoint. Re-signs a click URL after creative JavaScript modifies its query parameters. The original `tstoken` is validated first, and `tsurl`, `tstoken`, and `tsexp` can never be added or removed.

**Contract:** Auth: the original signed click URL. JSON and form POST bodies
are limited to 64 KiB. A JSON POST returns `200` with the response below; GET
and form-encoded POST return `302` to the rebuilt click URL. All success forms
are `no-store, private`. A bad original token returns `403`; malformed input,
an expired token, a non-click `tsclick`, a reserved-field mutation, or an
attempt to overwrite an existing query parameter uses the shared error
mapping; oversized bodies return `413`. There is no dedicated CORS grant or
rate limiter. Rebuild validates the old signature before minting its
replacement and never permits `tsurl`, `tstoken`, or `tsexp` mutation.
Validation and re-signing depend on `publisher.proxy_secret`.

**Request Body:**

```json
{
  "tsclick": "/first-party/click?tsurl=https%3A%2F%2Fadvertiser.example&campaign=123&tstoken=original...",
  "add": {
    "utm_source": "banner"
  },
  "del": ["old_param"]
}
```

`tsclick` may be root-relative (the form the rewriter emits) or absolute.

**Response:**

```json
{
  "href": "/first-party/click?tsurl=https%3A%2F%2Fadvertiser.example&campaign=123&utm_source=banner&tstoken=new...",
  "base": "https://advertiser.example",
  "added": { "utm_source": "banner" },
  "removed": ["old_param"]
}
```

**Use Cases:**

- TSJS click guard (automatic URL repair) on same-origin pages
- Handling creative JavaScript that modifies tracking URLs

---

### GET /first-party/proxy-rebuild

Navigation form of the same recovery, for creatives rendered in a sandboxed iframe without `allow-same-origin`. Their opaque origin makes the JSON POST a CORS-preflighted cross-origin request that the endpoint does not answer, so the click guard navigates here instead — navigations are not subject to CORS.

**Query Parameters:**

| Parameter | Required | Description                                       |
| --------- | -------- | ------------------------------------------------- |
| `tsclick` | Yes      | URL-encoded signed click URL                      |
| `add`     | No       | URL-encoded JSON object of parameters to add      |
| `del`     | No       | URL-encoded JSON array of parameter names to drop |

**Response:** `302` with the rebuilt `/first-party/click?...` URL in `Location` and `Cache-Control: no-store, private`. The browser follows it to `/first-party/click`, which redirects on to the advertiser.

Validation is identical to the POST form.

**Form-encoded POST (navigation, no URL length limit):** when the GET recovery URL would exceed the platform's request-URL limit (Fastly Compute rejects request URLs over 8192 bytes before the handler runs), the click guard submits a form instead. A `POST` carrying `Content-Type: application/x-www-form-urlencoded` with the same `tsclick`/`add`/`del` fields is treated as a navigation and answered with the same `302`, not the JSON body.

---

## Request Signing Endpoints

### GET /.well-known/trusted-server.json

Returns the Trusted Server discovery document, which includes active public keys in JWKS
format for signature verification.

**Contract:** Auth: none. Request body: not applicable. `200 application/json`
returns `{version, jwks}`; signing-store lookup, parse, or serialization failure
uses the shared `500` response. The endpoint has no dedicated cache, CORS, or
rate-limit policy. It requires usable `[request_signing]` storage and at least
the store state needed to enumerate active public keys.

**Response:**

```json
{
  "version": "1.0",
  "jwks": {
    "keys": [
      {
        "kty": "OKP",
        "crv": "Ed25519",
        "kid": "ts-2025-01-A",
        "use": "sig",
        "x": "UVTi04QLrIuB7jXpVfHjUTVN5aIdcbPNr50umTtN8pw"
      }
    ]
  }
}
```

**Example:**

```bash
curl https://edge.example.com/.well-known/trusted-server.json
```

**Use Cases:**

- Signature verification by downstream systems
- Key rotation validation
- Integration testing

---

### POST /verify-signature

Verifies a signature against a payload and key ID.

**Contract:** Auth: none. The JSON body is limited to 4 KiB. A syntactically
valid request always returns `200 application/json`: `verified` is false for an
invalid signature, missing key, or internal verification failure, and the
internal failure is exposed only as the sanitized `internal verification error`
value. An oversized body returns `413`; malformed JSON uses the shared `500`
configuration-error mapping. There is no dedicated cache, CORS, or rate-limit
policy. `[request_signing]` storage must be readable.

**Request Body:**

```json
{
  "payload": "base64-encoded-data",
  "signature": "base64-signature",
  "kid": "ts-2025-01-A"
}
```

**Response (Success):**

```json
{
  "verified": true,
  "kid": "ts-2025-01-A",
  "message": "Signature verified successfully"
}
```

**Response (Failure):**

```json
{
  "verified": false,
  "kid": "ts-2025-01-A",
  "message": "Signature verification failed",
  "error": "Invalid signature"
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/verify-signature \
  -H "Content-Type: application/json" \
  -d '{"payload":"SGVsbG8gV29ybGQ=","signature":"abc123...","kid":"ts-2025-01-A"}'
```

---

### Key rotation

Key rotation has no HTTP route. An operator rotates and retires keys with
`ts keys rotate` and `ts keys deactivate`, which run the rotation library in
core against the signing stores. See [Key Rotation](./key-rotation.md).

---

## The Administration Prefix

The whole `/_ts/admin` prefix is closed. No route is served beneath it, and a request to any path beneath it, as sent or percent-encoded (`/_ts/admin%2Fec`), is answered here with `404` and `Cache-Control: no-store` before the publisher fallback, so an `Authorization` header and a request body sent to it never reach the publisher's origin. The `/admin/keys` aliases outside `/_ts` are closed the same way.

No request is asked for a password.

What is held against a reader's own Edge Cookie is shown to that reader at [`GET /_ts/data`](#get-ts-data).

---

## Operational and fallback endpoints

### GET /health

**Contract:** Auth: none. Request and JSON schema: not applicable. Fastly,
Axum, and Spin return `200 text/plain` with `ok`; Cloudflare has no local health
route. Fastly short-circuits before application construction. Spin keeps the
probe live in its startup-error router; Fastly also remains live because it is
pre-router. The route has no configuration gate, dedicated cache or CORS
policy, or rate limiter. Example: `curl -fsS https://edge.example.com/health`.

This is liveness, not readiness: a `200` does not prove that configuration,
stores, publisher proxying, or integrations are usable.

### GET /\_ts/debug/ja4

**Contract:** Fastly-only; gated by `[debug].ja4_endpoint_enabled`. Auth: none.
The request has no body. Enabled requests return
`200 text/plain` with JA4, HTTP/2 fingerprint, cipher, TLS version, user-agent,
and client-hint values. Disabled requests return `404`. The response is
`no-store, private` and varies on the user-agent/client-hint fields. It defines
no CORS policy or rate limiter. Example: `curl -i
https://edge.example.com/_ts/debug/ja4`.

### GET or HEAD `<proxy.asset_routes[].prefix>{path}`

**Contract:** Fastly-only and registered once for each
`[[proxy.asset_routes]]` entry. Auth: optional origin signing by the signer
the route's `auth` table names, which authenticates Trusted Server to the
origin rather than the caller.
Request body: not applicable. The route maps the remainder of the path and the
configured query policy to the selected origin, preserves range and conditional
request semantics, and returns the origin status/body after stripping unsafe
response headers. Cache behavior follows the route's normalized cache policy;
credentialed or otherwise private responses are forced `private, no-store`.
No dedicated CORS grant or rate limiter is installed. Example: if the prefix is
`/assets/`, request `curl -I https://edge.example.com/assets/logo.svg`.

### Publisher fallback: `/` and `/{*rest}`

**Contract:** `GET`, `POST`, `HEAD`, `OPTIONS`, `PUT`, `PATCH`, and `DELETE`
proxy to `[publisher].backend`. The request and response schemas are the
publisher origin's schemas. Trusted Server may rewrite eligible HTML, assemble
templates, inject configured integrations, apply EC finalization, and preserve
streaming where supported. The origin status is normally preserved; fetch or
processing failures use shared adapter errors. Cacheability is decided from
the origin response plus personalization, cookie, template, and rewrite
effects. CORS is therefore the sanitized publisher-origin policy, not a global
Trusted Server policy. There is no built-in endpoint rate limiter. Example:
`curl -i https://edge.example.com/article`.

When startup fails, the degraded router replaces normal routing. Every
adapter answers both fallback shapes and all seven methods with the status of
the startup error's kind, which is `500` for a configuration or settings
error and `503` for a key-value store error, and Spin's body names only the
status. Only the health behavior shown in the table survives.

---

## TSJS Library Endpoint

### GET /static/tsjs=`<filename>`

Serves the TSJS (Trusted Server JavaScript) library.

**Path Pattern:** `/static/tsjs=<filename>?v=<hash>`

**Supported filenames:** `tsjs-unified.js` and `tsjs-unified.min.js` serve the
core plus enabled immediate modules. `tsjs-<integration>.js` and
`tsjs-<integration>.min.js` serve a known enabled deferred module, plus the
enabled standalone `gpt_diagnostics` module. An unknown, disabled, or
non-deferred module filename returns `404`.

**Query Parameters:**

| Parameter | Type   | Required | Description                                    |
| --------- | ------ | -------- | ---------------------------------------------- |
| `v`       | string | No       | Cache-busting hash (SHA256 of bundle contents) |

**Response:**

- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** TSJS bundle (IIFE format)
- **Headers:** ETag for caching

**Contract:** Auth: none. Request body: not applicable. A known eligible bundle
returns `200 application/javascript`; conditional requests can return `304`;
unknown/disabled filenames return `404`. When `v` exactly equals the generated
content hash, the response is public and immutable for one year. Other bundle
responses retain validator-based static caching without the immutable promise.
The route defines no CORS policy or rate limiter. Its module gate is the
compiled integration registry, not an arbitrary filename lookup.

**Example:**

```html
<script
  src="/static/tsjs=tsjs-unified.min.js?v=a1b2c3d4..."
  id="trustedserver-js"
></script>
```

**Module Selection:**
All integration modules are built at compile time. At runtime, the server concatenates only the modules of the integrations the module sections select in `trusted-server.toml`. No rebuild is required to change the module set.

---

## Integration Endpoints

Every row below records a compiled integration registration predicate.
An integration with no HTTP route can still contribute a browser module,
middleware, request filter, or ad server. In the
predicates below, `named` means a section selects the integration's module.

| Integration          | Registration predicate                                              | HTTP routes                                       |
| -------------------- | ------------------------------------------------------------------- | ------------------------------------------------- |
| `adserver_mock`      | `[ad-server] module = "iabtl"`                                      | None                                              |
| `aps`                | `demand implementation=auction.aps;rendering_mode=publisher_native` | None                                              |
| `aps`                | `demand implementation=auction.aps;rendering_mode=trusted_server`   | `GET /integrations/aps/renderer`                  |
| `creative`           | `always`                                                            | None                                              |
| `datadome`           | `named;enable_protection=false`                                     | `GET /integrations/datadome/js/*`                 |
| `datadome`           | `named;enable_protection=false`                                     | `GET /integrations/datadome/js/`                  |
| `datadome`           | `named;enable_protection=false`                                     | `GET /integrations/datadome/tags.js`              |
| `datadome`           | `named;enable_protection=false`                                     | `POST /integrations/datadome/js/*`                |
| `datadome`           | `named;enable_protection=false`                                     | `POST /integrations/datadome/js/`                 |
| `datadome`           | `named;enable_protection=true`                                      | `GET /integrations/datadome/js/*`                 |
| `datadome`           | `named;enable_protection=true`                                      | `GET /integrations/datadome/js/`                  |
| `datadome`           | `named;enable_protection=true`                                      | `GET /integrations/datadome/tags.js`              |
| `datadome`           | `named;enable_protection=true`                                      | `POST /integrations/datadome/js/*`                |
| `datadome`           | `named;enable_protection=true`                                      | `POST /integrations/datadome/js/`                 |
| `didomi`             | `named;prefix=proxy_path\|\|/integrations/didomi/consent`           | `GET <prefix>/*`                                  |
| `didomi`             | `named;prefix=proxy_path\|\|/integrations/didomi/consent`           | `POST <prefix>/*`                                 |
| `google_tag_manager` | `named`                                                             | `GET /integrations/google_tag_manager/collect`    |
| `google_tag_manager` | `named`                                                             | `GET /integrations/google_tag_manager/g/collect`  |
| `google_tag_manager` | `named`                                                             | `GET /integrations/google_tag_manager/gtag.js`    |
| `google_tag_manager` | `named`                                                             | `GET /integrations/google_tag_manager/gtag/js`    |
| `google_tag_manager` | `named`                                                             | `GET /integrations/google_tag_manager/gtm.js`     |
| `google_tag_manager` | `named`                                                             | `POST /integrations/google_tag_manager/collect`   |
| `google_tag_manager` | `named`                                                             | `POST /integrations/google_tag_manager/g/collect` |
| `gpt_diagnostics`    | `named`                                                             | None                                              |
| `js_asset_proxy`     | `named;asset.proxy=enabled`                                         | `GET <asset.path>` per configured asset           |
| `gpt`                | `named`                                                             | `GET /integrations/gpt/pagead/*`                  |
| `gpt`                | `named`                                                             | `GET /integrations/gpt/script`                    |
| `gpt`                | `named`                                                             | `GET /integrations/gpt/tag/*`                     |
| `lockr`              | `named`                                                             | `GET /integrations/lockr/api/*`                   |
| `lockr`              | `named`                                                             | `GET /integrations/lockr/sdk`                     |
| `lockr`              | `named`                                                             | `POST /integrations/lockr/api/*`                  |
| `nextjs`             | `named`                                                             | None                                              |
| `osano`              | `named`                                                             | None                                              |
| `permutive`          | `named`                                                             | `GET /integrations/permutive/api/*`               |
| `permutive`          | `named`                                                             | `GET /integrations/permutive/cdn/*`               |
| `permutive`          | `named`                                                             | `GET /integrations/permutive/events/*`            |
| `permutive`          | `named`                                                             | `GET /integrations/permutive/sdk`                 |
| `permutive`          | `named`                                                             | `GET /integrations/permutive/secure-signal/*`     |
| `permutive`          | `named`                                                             | `GET /integrations/permutive/sync/*`              |
| `permutive`          | `named`                                                             | `POST /integrations/permutive/api/*`              |
| `permutive`          | `named`                                                             | `POST /integrations/permutive/events/*`           |
| `permutive`          | `named`                                                             | `POST /integrations/permutive/secure-signal/*`    |
| `permutive`          | `named`                                                             | `POST /integrations/permutive/sync/*`             |
| `prebid`             | `named;script_patterns=config-derived`                              | `GET /integrations/prebid/bundle.js`              |
| `prebid`             | `named;script_patterns=config-derived`                              | `GET <auction.prebid.script_patterns[]>`          |
| `sourcepoint`        | `named`                                                             | `GET /integrations/sourcepoint/cdn/*`             |
| `sourcepoint`        | `named`                                                             | `HEAD /integrations/sourcepoint/cdn/*`            |
| `sourcepoint`        | `named`                                                             | `OPTIONS /integrations/sourcepoint/cdn/*`         |
| `sourcepoint`        | `named`                                                             | `POST /integrations/sourcepoint/cdn/*`            |
| `testlight`          | `named`                                                             | `POST /integrations/testlight/auction`            |

### Integration proxy contracts

All integration routes are registered only when the documented predicate is
true. An integration no section selects registers no route,
so its path continues through
normal routing and can reach the publisher fallback. Duplicate registrations
are startup errors. None of these routes has built-in caller authentication or
an in-process rate limiter; platform controls remain available when a
deployment needs either.

| Route family      | Request and success contract                                                                                                                                           | Errors, cache, and CORS                                                                                                                                                                       | Example                                                                                                                                   |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| APS renderer      | `GET /integrations/aps/renderer`; no body; `200 text/html` static opaque-frame renderer with CSP, `nosniff`, and `no-referrer`                                         | Present only for an APS demand source in `trusted_server` rendering mode; no dedicated cache/CORS headers                                                                                     | Browser-internal iframe target; curl is not a meaningful auction test                                                                     |
| DataDome tag      | `GET /integrations/datadome/tags.js`; query forwarded; successful upstream `200` is rewritten and returned as JavaScript                                               | Non-200 upstream status is preserved; rewritten `200` uses `cache_ttl_seconds`; upstream `Access-Control-Allow-Origin` is copied when present                                                 | `curl -i https://edge.example.com/integrations/datadome/tags.js`                                                                          |
| DataDome signals  | `GET` or `POST /integrations/datadome/js/` and `/js/*`; method, query, bounded POST body, and selected browser headers forwarded to `api_origin`                       | Upstream status/body/headers preserved; transport or size failures use shared integration errors; no added cache/CORS policy                                                                  | Browser SDK traffic; manual payload is upstream-schema-specific                                                                           |
| Didomi consent    | `GET` or `POST` under the configured prefix (default `/integrations/didomi/consent/*`); path selects SDK or API origin; query and bounded POST body forwarded          | Upstream status/body preserved; SDK responses receive the integration's CORS headers; API responses retain selected upstream headers; no local cache policy                                   | `curl -i https://edge.example.com/integrations/didomi/consent/loader.js`                                                                  |
| GTM/gtag scripts  | `GET` the generated `gtm.js`, `gtag.js`, or `gtag/js` paths; query forwarded or configured container ID supplied; successful script is rewritten                       | Non-success upstream status preserved; rewritten scripts use `cache_max_age`; oversized rewritten upstream bodies use shared integration errors                                               | `curl -i 'https://edge.example.com/integrations/google_tag_manager/gtm.js?id=GTM-XXXX'`                                                   |
| Google collect    | `GET` or `POST` the generated `collect` or `g/collect` paths; query, selected headers, and bounded body proxy to the configured Google origin                          | Malformed `Content-Length` returns `400`; body over `max_beacon_body_size` returns `413`; stream-read failure returns `502`; upstream response otherwise preserved                            | Browser beacon; body schema belongs to Google Analytics                                                                                   |
| JS asset proxy    | `GET` each configured `[[proxy.js_asset_proxy.assets]]` path whose `proxy = "enabled"`; the exact `origin_url` is fetched and served first-party                       | Upstream failures use shared integration errors; successful responses honor the per-asset or integration `cache_ttl_seconds`; `blocked` assets register no route and strip matching tags      | Path is operator-configured, for example `curl -i https://edge.example.com/js/vendor-tag.js`                                              |
| GPT               | `GET` `/script`, `/pagead/*`, or `/tag/*`; path/query proxy to the configured GPT origins and script content can be rewritten                                          | Upstream status is preserved; successful scripts/assets apply integration cache rules; selected upstream CORS is preserved                                                                    | `curl -i https://edge.example.com/integrations/gpt/script`                                                                                |
| Lockr SDK         | `GET /integrations/lockr/sdk`; no body; fetches and returns the configured SDK as JavaScript                                                                           | Successful SDK uses `cache_ttl_seconds`; upstream/transport failures follow integration mapping; no added CORS policy                                                                         | `curl -i https://edge.example.com/integrations/lockr/sdk`                                                                                 |
| Lockr API         | `GET` or `POST /integrations/lockr/api/*`; path, query, selected headers, and bounded body proxy to `api_endpoint`; publisher credentials are stripped                 | Upstream status/body preserved; no local cache/CORS policy                                                                                                                                    | Payload is Lockr-specific; use the SDK for normal calls                                                                                   |
| Permutive SDK     | `GET /integrations/permutive/sdk`; no body; fetches `{organization_id}.edge.permutive.app/{workspace_id}-web.js`                                                       | Successful SDK is JavaScript cached for `cache_ttl_seconds`; transport failures follow integration mapping                                                                                    | `curl -i https://edge.example.com/integrations/permutive/sdk`                                                                             |
| Permutive proxies | Generated `GET`/`POST` API, secure-signal, events, and sync routes plus `GET` CDN; suffix, query, selected headers, and bounded body proxy to the corresponding origin | Upstream status/body preserved; no added cache/CORS policy                                                                                                                                    | `curl -i https://edge.example.com/integrations/permutive/api/settings`                                                                    |
| Prebid bundle     | `GET /integrations/prebid/bundle.js`; no request body; proxies the configured external bundle                                                                          | Requires `external_bundle_url` and its host in `proxy.allowed_domains`; `v` must satisfy the configured cache mode; response strips unsafe headers and applies the bundle cache contract      | `curl -i 'https://edge.example.com/integrations/prebid/bundle.js?v=<configured-sha256>'`                                                  |
| Prebid blockers   | `GET` each exact configured `script_patterns` path; returns `200` empty JavaScript to suppress the publisher bundle                                                    | No upstream call; empty list registers none; dedicated CORS is not applicable                                                                                                                 | `curl -i https://edge.example.com/prebid.js`                                                                                              |
| Sourcepoint CDN   | `GET`, `HEAD`, `OPTIONS`, or `POST /integrations/sourcepoint/cdn/*`; suffix/query and a bounded body proxy to `cdn_origin`; selected consent cookies can round-trip    | Upstream status preserved; cookie-bearing responses are private; eligible static responses use `cache_ttl_seconds`; redirect locations and eligible JS/HTML content are rewritten first-party | `curl -i https://edge.example.com/integrations/sourcepoint/cdn/unified/wrapperMessagingWithoutDetection.js`                               |
| Testlight auction | `POST /integrations/testlight/auction`; OpenRTB JSON is bounded, parsed, and sent to the configured endpoint with `user.id` populated from the consent-allowed EC ID   | Malformed/oversized input and transport errors use shared mappings; upstream status/body preserved; no added cache/CORS policy and no EC response header                                      | `curl -X POST https://edge.example.com/integrations/testlight/auction -H 'Content-Type: application/json' -d '{"imp":[{"id":"slot-1"}]}'` |

`adserver_mock`, `creative`, `gpt_diagnostics`, `nextjs`, and `osano` have no
HTTP endpoint. Their request schema, statuses, cache/CORS contract, rate limit,
and curl example are therefore not applicable; their browser, ad server,
rewriter, or diagnostic behavior is documented in the integration guides.

### Prebid Integration

#### POST /auction

See [First-Party Endpoints](#post-auction) above.

#### GET /integrations/prebid/bundle.js

Proxies the configured external Prebid bundle through the first-party domain.
The optional `v` query value is the configured SHA-256 cache key.

#### GET `<script_patterns>` (Optional)

Each configured Prebid script pattern registers an endpoint that returns empty
JavaScript, preventing the publisher's original Prebid bundle from loading.
The defaults include `/prebid.js`, `/prebid.min.js`, `/prebidjs.js`, and
`/prebidjs.min.js`; set `script_patterns = []` to disable interception.

---

### Permutive Integration

#### GET /integrations/permutive/sdk

Serves Permutive SDK from first-party domain.

**Response:**

- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** Permutive SDK fetched from `{organization_id}.edge.permutive.app/{workspace_id}-web.js`
- **Cache:** 1 hour (configurable via `cache_ttl_seconds`)

#### GET/POST /integrations/permutive/api/\*

Proxies to `api.permutive.com`.

**Example:**

```bash
curl https://edge.example.com/integrations/permutive/api/settings
# → Proxies to https://api.permutive.com/settings
```

#### GET/POST /integrations/permutive/secure-signal/\*

Proxies to `secure-signals.permutive.app`.

#### GET/POST /integrations/permutive/events/\*

Proxies to `events.permutive.app` for event tracking.

#### GET/POST /integrations/permutive/sync/\*

Proxies to `sync.permutive.com` for ID synchronization.

#### GET /integrations/permutive/cdn/\*

Proxies to `cdn.permutive.com` for static assets.

---

### Testlight Integration

#### POST /integrations/testlight/auction

Testing auction endpoint with EC ID injection.

**Request Body:**

```json
{
  "user": {
    "id": null
  },
  "imp": [{ "id": "slot-1" }]
}
```

**Response:**
Proxies to configured endpoint with `user.id` populated with EC ID.

**Response Headers:**

No EC ID response header is emitted. EC identity is maintained with the `ts-ec` cookie.

---

## Authentication and policy boundaries

### Authentication

Trusted Server asks no request for a password. The EC partner APIs use their
own Bearer-token contract, and signed first-party proxy routes use `tstoken`
instead of HTTP authentication. All other built-in routes are unauthenticated.
A deployment that needs a password on a path of the publisher's own sets it at
the origin or in the hosting platform. An `Authorization` header a reader
sends is passed to the origin with the request, and the response to it is
never stored in a shared cache.

### Rate limiting

`/_ts/api/v1/batch-sync` is the only endpoint in this reference with a
built-in request-rate limiter. Its limit is per partner and configured with
`ec.partners[].batch_rate_limit`. Other limits must be implemented with the
deployment platform or an upstream service; this project does not prescribe
universal numeric limits.

### CORS

CORS is endpoint-specific. `/_ts/api/v1/identify` implements a closed,
publisher-domain policy and an explicit `OPTIONS` handler. Page-bids explicitly
denies preflight. `/_ts/permissions` and `/_ts/permissions.json` answer any
origin, because they show nothing but the asking request's own resolution,
and `/_ts/config` and `/_ts/config.json` answer any origin, because what they
show is the same for every request. `/_ts/data` answers no other origin,
because it shows what is held against one reader. Several integration proxies preserve or synthesize only the
headers described in their contract. `response_headers` is standard response
finalization, not a substitute for registering and validating an `OPTIONS`
route; do not infer cross-origin support from a configured header alone.

### Errors

Error representation is also endpoint-specific. Consult the contract above
and [Error Reference](./error-reference.md). Do not write a client that assumes
every failure is JSON or that every upstream failure has the same status.

---

## Next Steps

- Explore [Integrations Overview](./integrations-overview.md)
- Learn about [Configuration](./configuration.md)
- Review [Error Reference](./error-reference.md)
- Understand [Configuration Reference](./configuration.md)

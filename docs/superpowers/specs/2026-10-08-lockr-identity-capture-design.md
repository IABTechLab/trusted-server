# Proxy-side identity capture for lockr-issued IDs

Status: Design proposed, awaiting engineering review.

Issue: [#1246](https://github.com/IABTechLab/trusted-server/issues/1246).

Related: [2026-07-10 KV EID request snapshot and EC recovery](2026-07-10-kv-eid-request-snapshot-ec-recovery-design.md)
defines the snapshot and post-send primitives this design reuses.

## 1. Purpose

The lockr SDK obtains partner IDs (ID5 today; RampID, UID2, EUID, FirstID,
Yahoo ConnectID, PanoramaID, Criteo and Epsilon by configuration) and hands
them to Prebid through `pbjs.setConfig({ortb2: {user: {ext: {eids}}}})`.
Trusted Server collects auction EIDs from `pbjs.getUserIdsAsEids()`, which only
reports the Prebid User ID module. The lockr IDs therefore reach client-side
bidders and never reach the server-side auction, the `ts-eids` cookie, or the
EC identity graph. This was measured on the production publisher route on
2026-10-06 (Safari) and reconfirmed on 2026-10-08 (Chrome, direct versus TS
matched arm): six captured `/auction` payloads contained no `id5-sync.com`
entry while the same page carried the ID in `ortb2.user.ext.eids`.

The fix proposed in the issue thread (merge configured `ortb2` EIDs into the
JavaScript collector) corrects the symptom but keeps three dependencies this
design removes:

1. The ID still travels through Prebid configuration.
2. The ID still depends on the tsjs prebid module running in the page.
3. The `ts-eids` cookie is written in `bidsBackHandler`, after the first
   auction request has left, and KV ingestion happens on a later eligible
   navigation.

This design captures the IDs where Trusted Server already sees them, in the
first-party proxy response from lockr's API, and writes them to a server-owned
first-party cookie immediately and to KV after the response has been sent.

## 2. Observed lockr flow

All lockr traffic on a TS page goes through `/integrations/lockr/api/*`, proxied
by `handle_api_proxy` in `crates/trusted-server-core/src/integrations/lockr.rs`
to `https://identity.loc.kr`. The trust-server SDK build hard-codes
`host: "/integrations/lockr/api"`, so no client shim is involved in routing.

Measured call sequence after the publisher's consent gate opens:

| Order | Request                                                                 | Carries                                                                                                                                                             |
| ----- | ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1     | `GET /integrations/lockr/sdk`                                           | SDK body, 118,542 bytes                                                                                                                                             |
| 2     | `POST .../publisher/app/v2/identityLockr/settings`                      | Request body `{appID}`. Response: publisher flags, `rampJSClientId`, client IP and geo, `hashedUserAgent`, `vvFlag`                                                 |
| 3     | `POST .../publisher/app/v2/identityLockr/page-view`                     | Request body: `LTID`, existing `tokens` keyed by provider, `noGenerate`, consent strings, URL, referrer. Response on first issue: `aimTokens[]` and `ids.<key>.eid` |
| 4     | `POST .../refresh-tokens`, `.../generate-tokens`, `.../sync-no-hem-ids` | Later token updates, same shapes                                                                                                                                    |
| 5     | `POST .../revoke-consent`                                               | `LTID` and `optOutReason`                                                                                                                                           |

Measured `page-view` response on first issue (identifier values elided):

```json
{
  "success": true,
  "data": true,
  "pvStatus": true,
  "aimTokens": [
    {
      "advertising_token": "%7B%22universal_uid%22%3A%22ID5*...%22%7D",
      "identity_expires": 1791315739717,
      "key_name": "id5id",
      "settings": { "dropCookie": false, "dropLocalStorage": false }
    }
  ],
  "ids": {
    "id5id": {
      "eid": {
        "source": "id5-sync.com",
        "uids": [
          {
            "id": "ID5*...",
            "atype": 1,
            "ext": { "linkType": 1, "pba": "..." }
          }
        ]
      }
    }
  }
}
```

The response is 603 bytes. The SDK then writes cookie and localStorage `id5id`,
and exposes the EID to Prebid with `inserter: "Lockr-For-Publishers"`,
`matcher: ""`, `mm: 2`.

The request arrives at the proxy as a same-origin XHR, so it carries the
`ts-ec`, `__gpp`, `__gpp_sid` and `us_privacy` cookies, and its body carries
lockr's own `gppString`, `consentString` and `ccpaString`. The integration route
already builds an `EcContext` from the request and attaches `EcFinalizeState`
to the response (`dispatch_fallback` in
`crates/trusted-server-adapter-fastly/src/app.rs`). The Fastly entry point
already runs post-send work after `send_edgezero_response` (pull sync in
`crates/trusted-server-adapter-fastly/src/main.rs`).

Nothing reads the proxied response body today. `handle_api_proxy` returns the
upstream response as-is.

## 3. Requirements

1. Lockr-issued IDs reach the server-side auction without the Prebid User ID
   module and without any client-side JavaScript module.
2. The IDs are written to a first-party cookie by the server on the same
   response that delivered them, and to the EC identity graph without a KV
   write before response bytes are sent.
3. The IDs are available to the next auction on the same page load through the
   cookie, and to later navigations through KV, with the cookie as fallback.
4. Token refresh and consent revocation from lockr are reflected without a new
   mechanism.
5. OpenRTB 2.6 EID provenance (`inserter`, `matcher`, `mm`) survives into the
   auction payload.
6. Existing consent gating, size caps and fail-closed KV behavior are
   unchanged.

Out of scope: changing the JavaScript EID collector, server-to-server calls to
lockr, direct proxies for ID5 or LiveRamp, and storing provenance metadata in
KV.

## 4. Design

### 4.1 Capture hook on the integration proxy

Add an optional trait to the integration registry:

```rust
/// Identity tokens an integration proxy observed in an upstream response.
pub struct CapturedEid {
    /// OpenRTB `source` domain, for example `id5-sync.com`.
    pub source: String,
    pub uid: String,
    pub atype: Option<i32>,
    /// Unix seconds; absent when the provider gave no expiry.
    pub expires_at: Option<u64>,
    pub inserter: Option<String>,
    pub matcher: Option<String>,
    pub mm: Option<i32>,
}

/// Outcome of inspecting one proxied response.
pub enum IdentityCapture {
    None,
    Tokens(Vec<CapturedEid>),
    ConsentRevoked,
}

pub trait IntegrationIdentityCapture {
    /// Paths (relative to the integration's `/api` prefix) whose responses
    /// should be buffered and inspected.
    fn capture_paths(&self) -> &'static [&'static str];

    /// Parses a buffered upstream response body.
    fn capture(&self, path: &str, status: StatusCode, body: &[u8]) -> IdentityCapture;
}
```

`IntegrationRegistration::builder` gains `.with_identity_capture()`. The
registry's `handle_proxy` checks whether the matched proxy implements the trait
and whether the request path is in `capture_paths()`. If so it buffers the
upstream response with `collect_response_bounded` at a new
`IDENTITY_CAPTURE_MAX_RESPONSE_BYTES` of 64 KiB, calls `capture`, then rebuilds
the response from the buffered bytes. Every other path streams through
unchanged. Buffering is limited to the listed JSON endpoints; the SDK body and
unlisted paths are never buffered by this hook.

### 4.2 Lockr implementation

`LockrIntegration` implements the trait for `page-view`, `generate-tokens`,
`refresh-tokens`, `sync-no-hem-ids` and `revoke-consent`.

For token endpoints the parser reads `aimTokens[]` and, when present,
`ids.<key_name>.eid`. The `eid` object is preferred because it already carries
`source`, `atype` and provenance. When only `aimTokens[]` is present the
`key_name` is mapped to a source domain with a static table taken from the
SDK's own `tokenMappings` and `tokenSourceMappings`:

| `key_name`                 | `source`                           | `atype` |
| -------------------------- | ---------------------------------- | ------- |
| `id5id`                    | `id5-sync.com`                     | 1       |
| `_lr_env`                  | `liveramp.com`                     | 3       |
| `__uid2_advertising_token` | `uidapi.com`                       | 3       |
| `__euid_advertising_token` | `euid.eu`                          | 3       |
| `firstid`                  | `first-id.fr`                      | 1       |
| `connectId`                | `yahooinc.com`                     | 3       |
| `panoramaId`               | `panorama.com` (verify with lockr) | 1       |
| `cto_bidid`                | `criteo.com`                       | 3       |
| `_publink`                 | `epsilon.com`                      | 3       |

Unknown `key_name` values are logged at debug level and dropped. The ID5
`advertising_token` is URL-encoded JSON `{"universal_uid": "..."}`; the parser
extracts `universal_uid`. Tokens are validated with the existing
`is_valid_eid_uid` rule (non-empty, at most `MAX_UID_LENGTH` of 512 bytes).
`identity_expires` is milliseconds and is converted to seconds.

`revoke-consent` with a 2xx upstream status yields `ConsentRevoked`.

Non-2xx upstream responses and unparseable bodies yield `None` and are passed
through to the browser unchanged.

### 4.3 Consent gate

Capture runs only when all of the following hold:

1. `ec_context.consent()` passes `ec_consent_granted`, evaluated from the
   request's consent cookies exactly as on publisher navigations.
2. The request carries a valid `ts-ec` cookie, so there is an EC ID to attach
   the IDs to. Integration XHRs never generate a new EC ID; that rule is
   unchanged.
3. The lockr request body's `gppString` or `consentString`, when present, does
   not indicate an opt-out. The body is already buffered for POST forwarding by
   `collect_body_bounded`; this adds a parse, not a read.

When the gate fails the response is passed through and nothing is written.

### 4.4 Server-owned first-party cookie

A new cookie, `ts-eids-s`, is set on the proxy response that delivered the
tokens:

- Value: base64 of the structured JSON array already accepted by
  `parse_prebid_eids_cookie`, holding `source` and one `id` per source and
  nothing else. No `atype`, no `ext`, no provenance. Those are re-attached on
  the server from a static per-source table when the auction payload is built
  (section 4.6). Example before encoding:

  ```json
  [{ "source": "id5-sync.com", "uids": [{ "id": "ID5*..." }] }]
  ```

  The cookie rides on every same-origin request, not only auctions, so it is
  kept as small as the ID itself. One ID5 token encodes to 184 bytes
  (measured). All nine lockr-issued sources with representative token lengths
  encode to 1,716 bytes (modeled with the measured ID5, RampID, UID2 and
  Criteo values); UID2 and Criteo tokens account for most of that.

- Attributes: `Path=/; Secure; HttpOnly; SameSite=Lax`, `Domain` as configured
  for `ts-ec`, `Max-Age` = the smallest `expires_at` minus now, capped at 30
  days, defaulting to 30 days when no expiry was given.
- Merge policy: the new value is the union of the existing `ts-eids-s` cookie
  on the request and the captured EIDs, keyed by `source`; captured values
  replace existing values for the same source. Sources whose stored expiry has
  passed are dropped at merge time.
- Size: encoded value capped at 2,048 bytes. When the merged value exceeds
  the cap, sources are dropped oldest-expiry first until it fits; dropping is
  logged. The cap is below the 3,072-byte `ts-eids` cap because this cookie
  carries one ID per source and nothing else, and above the modeled full set
  so no lockr-issued source is dropped in practice.
- `ConsentRevoked` sets `ts-eids-s` with `Max-Age=0`.

The cookie is separate from `ts-eids` for two reasons. The tsjs prebid module
rewrites `ts-eids` on every auction from `getUserIdsAsEids()`, so a server
value stored there would be overwritten within seconds. And a server-set
`HttpOnly` cookie is not subject to the script-written cookie lifetime cap in
WebKit, and cannot be edited by page scripts.

`enforce_set_cookie_cache_privacy` in the Fastly middleware already marks any
response carrying `Set-Cookie` as private and uncacheable, so no cache change
is needed.

### 4.5 Readers of the new cookie

Two readers change:

- `resolve_client_auction_eids` in `crates/trusted-server-core/src/auction/endpoints.rs`
  currently prefers the request body and falls back to `ts-eids`. It will
  parse `ts-eids-s` and merge it with whichever of body or `ts-eids` applied,
  through `merge_auction_eids`. Before merging, each `ts-eids-s` entry is
  expanded with `atype` and provenance from the static table in section 4.6.
  For the same `source`, the `ts-eids-s` entry is ordered first so its values
  win under the existing "first value wins, fill missing fields" merge rule.
- `collect_eid_cookie_updates` in `crates/trusted-server-core/src/ec/prebid_eids.rs`
  takes a third optional cookie value and feeds it through
  `collect_prebid_eid_updates_from_eids`. `EcFinalizeState` and
  `EcRequestState` carry the extra cookie value. Existing partner-registry
  gating applies: a source without an `[[ec.partners]]` entry still reaches the
  auction body but is not written to KV.

The JavaScript collector is not changed by this design. Its `ts-eids` output
continues to cover User ID module providers.

The `x-ts-eids` response header on `/auction` is unchanged. It is emitted
after the merge and consent gating from the final `user.eids`, is on the
`INTERNAL_HEADERS` strip list for inbound requests, and is read by nothing on
the server. It remains the diagnostic view of what was sent to bidders.

### 4.6 EID provenance

`Eid` in `crates/trusted-server-core/src/openrtb.rs` gains three optional
fields with `skip_serializing_if = "Option::is_none"`: `inserter: Option<String>`,
`matcher: Option<String>`, `mm: Option<i32>`. `deny_unknown_fields` stays.
`StructuredCookieEid` and `structured_cookie_eids_to_openrtb` carry the same
three fields. `parse_client_auction_eids` and `merge_auction_eids` preserve
them, with the conflict rule "first non-empty value wins", matching how `atype`
and `ext` are merged today.

Provenance is not stored in the cookie or in KV. A static table in the lockr
integration, keyed by `source`, supplies `atype`, `inserter`
(`Lockr-For-Publishers`), `matcher` (empty) and `mm` (2) when a `ts-eids-s`
entry is expanded for the auction, matching what the SDK sets on
`ortb2.user.ext.eids`. The `atype` column is the one from the mapping table in
section 4.2. Provider `ext` payloads such as ID5's `pba` are not carried; they
are not stored today either and bidders that need them read the provider
cookie directly.

Legacy flattened cookies decode as before. KV entries are unchanged: the
identity graph still stores one UID per partner source and no provenance.

### 4.7 KV write after send

Capture never writes KV before the response is sent. Instead the proxy handler
attaches a response extension:

```rust
pub struct DeferredIdentityUpdates {
    pub updates: Vec<PartnerIdUpdate>,
    pub revoke: bool,
}
```

`PartnerIdUpdate` values are built with the existing partner-registry lookup,
so only configured sources are included. In the Fastly entry point, after
`send_edgezero_response` returns and before pull sync, a new
`run_identity_updates_after_send` applies them:

- `updates` go through `KvIdentityGraph::upsert_partner_ids_from_snapshot`
  with the request's `EcKvSnapshot`, which already skips unchanged UIDs, fails
  closed on a missing or unreadable root row, and refuses to write when the
  stored `consent.ok` is false.
- `revoke` calls `write_withdrawal_tombstone`, the same path the CMP
  withdrawal flow uses.

Both are best-effort and log on failure. The Wasm instance stays alive until
the handler returns, so this work costs nothing on the response path. On a
`page-view` that carries no new token (the common returning-visitor case) the
extension is absent and no KV operation runs.

The Axum, Cloudflare and Spin adapters apply the same extension in their own
post-send positions where one exists; where an adapter has no post-send hook
the updates are applied before send, matching how those adapters already
handle pull sync. The hot-path constraint is a Fastly production constraint.

### 4.8 Data flow summary

```
browser -> POST /integrations/lockr/api/.../page-view (ts-ec, consent cookies, body)
   TS proxy -> identity.loc.kr -> 200 {aimTokens, ids}
   TS: consent gate -> capture -> Set-Cookie ts-eids-s (HttpOnly)
   TS: attach DeferredIdentityUpdates -> send response to browser
   TS: after send -> KV upsert (CAS, skip-unchanged) or tombstone

next /auction on this page -> reads ts-eids-s -> user.eids includes id5-sync.com
next navigation           -> KV snapshot (hot-path read already present)
                             -> ts-eids-s fallback when the row is missing
```

## 5. Freshness

| Event                                      | Cookie                                 | KV                                    | Auction sees it             |
| ------------------------------------------ | -------------------------------------- | ------------------------------------- | --------------------------- |
| First issue (`page-view` with `aimTokens`) | Same response                          | After that response is sent           | Next `/auction` on the page |
| Lockr refresh (`refresh-tokens`)           | Same response, new value and `Max-Age` | After send, only when the UID changed | Next `/auction`             |
| Lockr revoke (`revoke-consent`)            | Cleared on that response               | Tombstone after send                  | Next request                |
| Returning visitor, no new token            | Untouched                              | No operation                          | From KV snapshot            |

Modeled cost per captured response: one JSON parse of at most 64 KiB and one
`Set-Cookie` of at most 2,048 bytes. Modeled cost on every later same-origin
request: the cookie bytes, 184 for ID5 alone and up to 1,716 for all nine
lockr-issued sources. No additional KV read is added; the deferred upsert reuses the
request snapshot and performs its own refresh read only on a CAS retry, as it
does today for `ts-eids` ingestion.

## 6. Configuration

No new lockr options. KV persistence for a lockr-issued source requires the
existing partner registry entry, for example:

```toml
[[ec.partners]]
name = "ID5"
source_domain = "id5-sync.com"
openrtb_atype = 1
bidstream_enabled = true
```

Without this entry the ID still reaches the auction body through the cookie
and is never written to KV, matching current `ts-eids` behavior. The deployed
registry for the affected publisher should be checked for this entry as part of
rollout.

## 7. Error handling

- Upstream non-2xx or malformed JSON: pass-through, no cookie, no KV, debug log.
- Response larger than 64 KiB on a capture path: pass-through with a warning;
  the limit is an order of magnitude above the measured 603-byte response.
- Consent gate failure: pass-through, nothing written.
- Cookie over 2,048 bytes after merge: drop oldest-expiry sources, log.
- KV upsert failure after send: logged, cookie already delivered, next
  navigation retries through normal cookie ingestion.
- Missing `ts-ec`: pass-through, nothing written; the SDK call still succeeds.

## 8. Testing

Rust, alongside the code:

- `integrations/lockr.rs`: parser tests for `aimTokens` only, `ids.*.eid`
  present, URL-encoded ID5 JSON, unknown `key_name`, oversized UID, non-2xx,
  malformed body, `revoke-consent`.
- `integrations/registry.rs`: capture path buffering versus pass-through
  streaming for unlisted paths; response bytes identical after buffering.
- `auction/endpoints.rs`: `ts-eids-s` merged with body, with `ts-eids`, alone;
  expansion from the static table; provenance precedence; caps.
- `ec/prebid_eids.rs`: structured cookie with and without provenance fields;
  legacy cookie unchanged; third cookie input.
- `ec/finalize.rs` and adapter entry point: deferred updates applied after send;
  absent when no tokens; tombstone on revoke; skip-unchanged.
- Consent-denied regression coverage retained for every new path.

JavaScript: no changes, existing suites must still pass.

Live acceptance on the TS matched arm:

1. Fresh profile, accept consent, wait for the `page-view` response.
2. The `page-view` response carries `Set-Cookie: ts-eids-s=...; HttpOnly`,
   and the decoded value holds only `source` and `id`.
3. The next `/auction` request body decodes with `id5-sync.com` in
   `user.eids`, with `inserter` and `mm` present.
4. `GET /_ts/admin/ec` for the session's EC ID shows the ID5 UID under
   `ids["id5-sync.com"]` after one further eligible request.
5. Reload: same UID, no new KV write (admin generation unchanged).
6. Trigger `revoke-consent` from the lockr SDK: cookie cleared, tombstone
   present.

## 9. Rollout

1. Land the `Eid` provenance fields and cookie parser changes first; they are
   additive and safe on their own.
2. Land the capture hook with lockr behind a `capture_identity` boolean on
   `[integrations.lockr]`, default `true`, so a publisher can switch it off
   without a deploy.
3. Add the `id5-sync.com` partner entry on the affected publisher.
4. Verify with the live acceptance steps above.

## 10. Open questions

1. Should `ts-eids-s` also be exposed to client-side auctions, for example
   through `/identify` or a response header, or is the server-side auction the
   only consumer? This design assumes server-side only.
2. The `panoramaId` source domain and `atype` should be confirmed with lockr
   before the mapping table ships.
3. Whether a captured `expires_at` should also bound the KV entry. The identity
   graph has no per-partner expiry today, and this design leaves that as is.

## 11. Future-state options

- Register lockr as a pull-sync partner and resolve tokens server-to-server
  from the `ltid` value, removing the SDK from the identity path entirely.
  Requires an S2S contract with lockr that does not exist today.
- Apply the same capture hook to direct ID5 and LiveRamp proxies once those
  endpoints are proxied first-party.

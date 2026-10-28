# Server-side identity providers

Status: Draft, revision 15. Revised after review rounds 1 to 14. Revision 14
made `ts-eids-s` a client-only cookie, at the spec author's request. Revision 15
removes the `ts-eids-r` removal cookies, runs providers only on writer
navigations, makes capture revoke-only and moves KV to schema version 2,
accepting breaking changes. Section 16 lists the decisions.

Issue: none filed yet. Related:
[#1245](https://github.com/IABTechLab/trusted-server/issues/1245)
(LiveRamp server-to-server),
[#90](https://github.com/IABTechLab/trusted-server/issues/90)
and [#353](https://github.com/IABTechLab/trusted-server/issues/353) (ID5),
[#1246](https://github.com/IABTechLab/trusted-server/issues/1246) (Lockr IDs to
KV), [#777](https://github.com/IABTechLab/trusted-server/issues/777)
(pluggable modules).

Designed against `spec/1246-prebid-eid-fidelity` at `c84a73da7`, which is
`main` at `6ef07ae52` plus the
[Prebid EID fidelity](2026-10-18-prebid-eid-fidelity-design.md) work. Every
`file:line` reference below is to that commit. Core paths are written without
the `crates/trusted-server-core/src/` prefix, and Fastly adapter paths start
with `adapter-fastly/src/`. Prebid.js references are to version 10.26.0, read
from `crates/trusted-server-js/lib/node_modules/prebid.js/modules/`. This spec
depends on the EID fidelity branch for `merge_auction_eids`, the shared
client EID parser and the `ts-eids` cookie carrying the same set the shim
sends. It assumes that work forwards configured ORTB2 EIDs together with
User ID module EIDs, deduplicated, with EID-level provenance preserved
(EID fidelity spec D2, D3, §4.2, §4.4). Lockr writes its ID5 EID only into
`pbjs.getConfig('ortb2').user.ext.eids`, which a collector that reads only
`getUserIdsAsEids()` misses (#1246).

This spec does not depend on PR #1260, and supersedes it: #1260 will not
merge. Its `IdentityCapture` trait, `identity_owner` field and `ts-eids-s`
cookie are not on `main`. Where this spec reuses a name from #1260, it
defines the thing from scratch here, and it keeps `ts-eids` and `x-ts-eids`,
which #1260 retires.

Terms used throughout:

- **EID / UID**: an OpenRTB extended identifier, `{source, uids[]}`, and one
  entry of its `uids`, `{id, atype?, ext?}` (OpenRTB 2.6 §3.2.27, §3.2.28).
- **EID-level fields**: an EID's `inserter`, `matcher`, `mm` and its own
  `ext`, as opposed to a UID's `atype` and `ext`.
- **Entry set**: every EID entry stored for one source, written by one writer
  and replaced or removed whole (section 6.1). A provider may return several
  entries per source, each with several UIDs.
- **Identity provider**: a vendor that turns request signals into an EID,
  such as ID5, LiveRamp ATS, Lockr AIM or UID2.
- **Source**: the EID `source` domain a provider's IDs carry, such as
  `id5-sync.com` or `liveramp.com`. It is also the key of the KV `ids` map. A
  **registered** source has an `[[ec.partners]]` entry.
- **EC / KV row**: the Edge Cookie ID (`ts-ec`) and the KV identity-graph
  entry keyed by it (`ec/kv_types.rs:42`).
- **HEM**: a hashed email, the SHA-256 of a normalized email address. LiveRamp
  ATS, Lockr and UID2 resolve from it. It is personal data, not an anonymous
  value.
- **Page load**: one top-level document navigation and every request the page
  makes because of it: `/auction` calls, SPA view changes reported to
  `/_ts/page-bids`, and integration proxy calls.
- **Writer request**: the one request in a page load allowed to write KV
  (section 5.1).
- **Stager**: every other request. It reads cookies and KV, runs no
  resolving provider and persists no positive ID. Its only KV write is the
  capture removal write, for a revocation a proxied call carries (section
  5.3).
- **Hold mode / after-send mode**: whether a provider is called before the
  auction, with the auction waiting for it, or after the response has been
  sent.
- **Orphan recovery**: what TS does when a visitor's EC cookie has no KV row
  (`ec/finalize.rs:282`).
- **`ts-eids`**: the existing first-party cookie the Prebid shim writes with
  the browser's EIDs. Unchanged by this spec, except that it holds only
  registered sources, merged per source across auctions (section 5.3).
- **`ts-eids-s`**: a new client-only cookie, named `__Host-ts-eids-s`, that
  the shim writes from the servable server set TS returns in `/auction` and
  `/_ts/page-bids` responses. It mirrors server IDs for page scripts. The
  server never opens, parses or trusts it, and the shim never copies it
  into `ts-eids` or the `/auction` body. Defined in section 6.2.

## 1. Problem statement

Trusted Server (TS) collects EIDs three ways today. Browser modules resolve
IDs that the page forwards, partners push UIDs to batch sync, and pull sync
calls partners from the server. Only pull sync calls a provider from the
server, and it speaks one fixed protocol.

### P1. Browser-only collection fails where browsers block vendors

Each provider ships a browser module, and TS forwards what those modules
produce. Safari's tracking protection blocks requests to some vendor hosts.
#1245 reports Prebid's `identityLink` module failing this way on Safari,
leaving no LiveRamp envelope in the auction. Calling providers from the edge
avoids the browser request.

This spec builds that server-side path. In v1 it helps:

- providers that need no hashed email, such as ID5
- providers whose traffic TS already proxies, such as Lockr, for removals
  and opt-outs. A positive capture on a proxied API call is not persisted
  (section 2.3)
- generic pull partners
- any owning provider that can refresh an ID a browser module obtained
  earlier, so the ID survives when the module later fails in that browser

It does not fix the #1245 Safari case by itself. LiveRamp's initial
resolution needs a hashed email, and v1 has no way to obtain one (section
2.2). A Safari visitor whose browser module never obtained an envelope has
nothing to refresh.

### P2. The only server-side path is one fixed protocol

`ec/pull_sync.rs` calls partners after the response is sent. Its protocol is
fixed: `GET <pull_sync_url>?ec_id=<id>` with a bearer token
(`pull_sync.rs:255`, `:368-371`), response `{"uid":"..."}`
(`pull_sync.rs:54-57`). It fills missing UIDs only and never refreshes
(`pull_sync.rs:7-9`).

None of the providers this spec targets speaks that protocol:

- **ID5** takes a POST body with a partner number, device signals, the page
  URL and consent. It returns up to three IDs (`id5-sync.com`, an EUID ID and
  `true-link-id5-sync.com`) and a `signature` the caller sends back on its
  next call (`id5IdSystem.js:40-41`, `:130-170`, `:454-487`).
- **LiveRamp ATS** takes a placement ID, an identifier type and value,
  consent parameters and the end user's IP. It returns one of several
  envelope products, envelopes expire after 15 or 30 days, and refresh is a
  different call that takes the old envelope (#1245 §9, §10).
- **UID2** returns an advertising token for bidders and a refresh token that
  must never reach them.
- **Lockr**'s API traffic already passes through TS's proxy
  (`integrations/lockr.rs:179-237`). Its IDs can be read from the responses,
  with no outbound call.

Each provider needs its own request building, response parsing, error
handling and, for some, private state kept between calls. Without a shared
contract, each integration would repeat consent revalidation, CAS handling,
rate limits and timing. #1245 §6 rules out a private KV writer per vendor.

### P3. A page load writes KV several times

Today a page load can write the KV row once per step:

| Step                                    | Request                                   | Write              | Code                                                        |
| --------------------------------------- | ----------------------------------------- | ------------------ | ----------------------------------------------------------- |
| New visitor gets an EC                  | publisher navigation, during routing      | `create_if_absent` | `adapter-fastly/src/app.rs:845-857`, `ec/mod.rs:424`        |
| New visitor gets an EC                  | integration-proxy navigation              | `create_if_absent` | `integrations/registry.rs:1046-1050`                        |
| Browser EIDs from `ts-eids` or sharedId | navigation, finalize                      | CAS update         | `ec/finalize.rs:83`, `:183`                                 |
| Prebid body EIDs                        | each `/auction`                           | CAS update         | `auction/endpoints.rs:298-303`, `finalize.rs:183`           |
| `ts-eids` on an SPA view                | each `/_ts/page-bids`                     | CAS update         | `publisher.rs:6925`, `finalize.rs:183`                      |
| Pull sync results                       | after send, on requests with a loaded row | CAS update         | `adapter-fastly/src/main.rs:507-517`, `ec/pull_sync.rs:290` |
| Cluster size                            | `/_ts/api/v1/identify`, once per EC       | CAS update         | `ec/identify.rs:107`, `ec/kv.rs:1484`, `:1524-1529`         |

Ingest runs only on requests that set an EID sync source: navigation,
`/auction`, `/_ts/page-bids` and new ECs (`ec/mod.rs:98-111`). Asset routes
skip EC finalize entirely (`adapter-fastly/src/app.rs:834-835`). Pull sync
runs after any response whose request loaded a consenting row
(`pull_sync.rs:86`).

Each of these skips the write when nothing changed, so most are no-ops, but
each can write. A new visitor can cost three writes on the navigation alone,
plus one for each `/auction` and each SPA view. Adding providers naively adds
one more write each. KV writes cost money, Fastly limits how often one key
can be written, and each extra write is another CAS race between tabs.

## 2. Goals and non-goals

### 2.1 Goals

- **G1.** A trait any identity provider implements to be called from the
  server. LiveRamp, Lockr, ID5, UID2 and generic pull partners all fit it,
  including providers that keep private state between calls.
- **G2.** Providers never do HTTP, KV or cookie work themselves. One
  coordinator owns eligibility, consent, transport limits and write-back.
- **G3.** Operators choose per provider between hold mode and after-send
  mode.
- **G4.** At most one browser-driven KV write per page load, and a write
  that fails is not retried. This is strict. The only page-load exceptions
  are a consent withdrawal's tombstone (section 5.1), which also covers a
  capture provider's global revoke (section 8), and the two removal writes:
  the capture removal write a proxied revocation makes (section 5.3), and the removal-only retry of a failed flush (section 5.2,
  phase 4). All are rare and removal-only. Writers outside page loads keep
  their own
  writes: batch sync, the admin endpoints and the deferred deletion
  framework (sections 2.2, 5.5).
- **G5.** Provider-collected IDs are stored in KV and mirrored in a
  first-party cookie (`ts-eids-s`) that page scripts read. KV is the only
  server-side source of provider IDs: eligibility, the auction merge,
  `/identify` and servability read KV, never `ts-eids-s`. Section 15 asks
  whether this meets the product rule that the cookie is read first.
- **G6.** Each source has at most one owning provider, configured by the
  operator. A valid ID the owner wrote wins in the auction over the
  browser's.
- **G7.** For a source with an owning provider that declares its IDs as
  expiring (LiveRamp envelopes), the IDs are refreshed and are never sent to
  bidders or returned by `/_ts/api/v1/identify` after expiry. Unowned
  sources are outside G7 (section 2.3).
- **G8.** Today's pull sync becomes one implementation of the trait. Its
  protocol and fill-missing contract are unchanged. It runs only on writer
  navigations, so it holds for clients that make them (section 2.3). Section 10.4 lists the
  behaviour that does change.
- **G9.** The trait can model a provider that needs a HEM, even though v1
  has no way to obtain one.
- **G10.** For clients that make writer navigations (section 5.1), no
  browser EID that reaches KV today is lost, within one bound.
  Browser ingest runs only on the writer request, from `ts-eids`. The shim
  writes only registered sources into `ts-eids` and merges them per source
  (section 5.3), so the next writer request ingests every registered source
  today's `/auction` ingest writes from the body. A body-only registered ID
  is lost only when the registered sources alone exceed the 3,072-byte trim. What today's
  `/_ts/page-bids` ingest writes comes from `ts-eids` too; only an SPA
  session longer than that cookie's one-day lifetime can lose such an ID.
- **G11.** Writing a provider is easy, and extending the trait does not
  break one. A simple provider is one struct, one descriptor and two short
  methods (section 4.2).
- **G12.** Removals win. Every removal (opt-out, invalidation, withdrawal,
  deletion) is written to KV on the request that carries it, and TS serves
  only from KV, so the ID stops reaching bidders through TS at once. A
  removed provider ID also stops reaching them as a browser value: the
  removing provider's `ps.iv`, `ps.ov` and `ps.o` filter browser candidates
  (section 7.1). The `ts-eids-s` mirror drops it at the next `/auction` or
  `/_ts/page-bids` response. The one bound: a removal write that fails all
  its attempts is lost (section 2.3).
- **G13.** A provider can return complete EIDs, with the same fidelity the
  browser's `/auction` body has: several EID entries per source, several
  UIDs per entry, UID-level `atype` and `ext`, and the EID-level fields. TS
  stores the entry set whole in KV, mirrors it whole in `ts-eids-s` when it
  fits, and sends it to bidders whole (sections 4.5, 6.1, 6.2, 7.1). This is
  additive in behaviour: G13 itself changes none of the paths that predate
  this spec. Browser ingest, `ts-eids`, batch sync, generic pull and
  `/identify` keep the behaviour the rest of this spec gives them (sections
  5, 6.2, 7.2, 10.4), and every record they write is one entry with one
  UID and no EID-level fields. The storage shape changes for every record
  (schema version 2, section 6.1); what bidders receive for these records
  does not.

### 2.2 Non-goals

- **HEM intake.** How a hashed email reaches TS (#1245 §4 options A to D)
  needs its own spec. The trait has a typed slot for it. In v1 the slot is
  always empty and providers that need it are skipped.
- **Concrete vendor implementations.** ID5, LiveRamp and Lockr each get
  their own implementation spec. Section 12 shows how each maps onto the
  trait, to prove the trait fits.
- **Other adapters.** KV identity runs only on Fastly
  (`publisher.rs:6896-6899`). Axum, Cloudflare and Spin get this design when
  EC identity is ported to them (#777 rows 52 to 54).
- **Cross-browser linking.** An ID in one browser's EC row does not appear in
  another browser's row (#1245 §12).
- **LiveRamp ATS Direct segments** and GAM targeting.
- **Batch sync and admin writes.** They keep their own writes. Section 7.2
  changes only how batch sync treats an owned source.
- **Removing `ts-eids` or `x-ts-eids`.** Both stay.
- **Storing the browser's complete EID set.** Browser ingest keeps today's
  rules: registered sources only, one UID per source picked as
  `collect_eid_updates` picks it today (`ec/prebid_eids.rs:146-151`: for a
  repeated source, the first valid UID of its last entry, with the sharedId
  cookie applied last), no EID-level fields, fill-missing
  (`ec/kv.rs:145-155`).
  The browser's complete set still reaches bidders from the `/auction` body,
  as the EID fidelity work defines. G13 applies only to IDs a provider
  writes.

- **Unregistered sources for providers.** A provider still claims only
  sources with an `[[ec.partners]]` entry (section 10.2). Full fidelity
  changes what a provider may return for a source, not which sources.

### 2.3 Accepted tradeoffs

Revisions 14 and 15 make `ts-eids-s` client-only, keep every positive ID
off stagers and drop every server-owned identity cookie. The design accepts
these costs for it:

- **Providers run only on writer navigations.** `/auction` and
  `/_ts/page-bids` call no provider; they serve what KV holds. On a site
  whose auctions all go through `/auction`, hold-mode providers run after
  send on the navigation, and their answers reach the next auction from KV
  (section 4.7, step 7).
- **Positive captures are not used in v1.** Capture is revoke-only: a
  proxied call can remove IDs, opt a provider out and clear that opt-out,
  and nothing else. The browser path (Prebid `ortb2` into `ts-eids`) still
  carries to KV the one UID browser ingest picks for each registered source
  (section 2.2). Full-fidelity sets from capture wait for open question 4
  (sections 4.7,
  12.2, 15).
- **A failed removal write waits in the pending key.** A capture removal
  write is tried before send with the withdrawal path's CAS attempts and
  once more after send (section 5.3). A writer whose flush fails retries its
  removals alone with the same CAS attempts (section 5.2, phase 4). When
  every attempt fails, the removal is appended to the EC ID's pending key
  and counted in `removal_deferred`, and the next writer merges it (section
  5.3, The pending key). Only when that append fails too is it logged at
  error and counted in `removal_lost`.
- **A failed flush has no backup.** Browser ingest is re-derived from
  `ts-eids`, and provider results are resolved again by a later writer
  (section 5.2, phase 4).
- **Every returning writer navigation reads KV twice.** It reads the row
  and the pending key. No navigation is answered from a cookie with zero
  reads (section 5.2).
- **Server-only IDs are readable by page scripts.** UID2 advertising tokens
  and LiveRamp envelopes are in `ts-eids-s`, which is not `HttpOnly`.
  `x-ts-eids` already exposes them (section 9.3).
- **A new visitor's first navigation runs only hold-mode providers that do
  not read the EC ID, and only when it runs an auction.** After-send
  providers and generic pull wait for the second page load, and a
  first-navigation create waits up to `hold_budget_ms` for its providers
  (section 4.7, new visitors).
- **Clients without Fetch Metadata never write.** A writer navigation
  needs `Sec-Fetch-Dest: document` (section 5.1). Safari before 16.4, older
  embedded web views and any client that strips the header are stagers
  forever. For them this is a regression: today's `/auction` ingest writes
  their body EIDs to KV, and under this design nothing does. Pull sync
  never runs for them, and orphan recovery, which moves into the writer's
  phase 1, never runs either. Their consent withdrawals and capture
  removals still apply (sections 5.1, 5.3). A fallback writer built from
  `Accept: text/html` and `Upgrade-Insecure-Requests` was rejected because
  `fetch()` can send both.
- **Rollback below rollout step 0 is unsupported.** Step 1 writes KV
  schema version 2, which builds older than step 0 reject. Step 0 ships
  first as the floor: it reads version 2 rows and never rewrites them
  except to tombstone, so step 1 can roll back to it (sections 6.1, 14).
- **Unowned IDs keep no lifetime.** Expiry is declared by the writing
  provider (`is_expiring`), and browser ingest, batch sync and a provider
  that gives no expiry carry no `exp`. So an unowned `liveramp.com`
  envelope or `uidapi.com` token from one of them is served from KV until
  it is replaced or removed, as it is today, and a browser value equal to
  an owner record TS knows has expired is still forwarded. G7 covers owned
  sources only. A per-source lifetime on `[[ec.partners]]` would close
  this for every writer; it changes what today's writers store, so it is
  left for a later spec.

## 3. Background

### 3.1 Where EIDs come from today

1. **Browser modules.** Prebid's userId modules resolve IDs in the browser.
   The `trustedServer` bid adapter sends them in the `/auction` body and the
   shim copies the same set into `ts-eids` (EID fidelity spec §4.2, §4.3).
2. **Pull sync.** After the response is sent, TS calls each pull-enabled
   `[[ec.partners]]` entry whose UID is missing (`ec/pull_sync.rs:116`).
3. **Batch sync.** Partners push UIDs to `/_ts/api/v1/batch-sync`
   (`ec/batch_sync.rs`).

### 3.2 How the auction builds `user.eids`

`handle_auction` takes the client set from the body, else from `ts-eids`
(`auction/endpoints.rs:290-296`). It keeps the full body set for finalize
because the cookie is size-capped (`endpoints.rs:298-303`). It reads the KV
row once when a partner registry exists (`endpoints.rs:307-311`), resolves
KV IDs for registered sources (`endpoints.rs:444`), and merges the two with
`merge_auction_eids` (`endpoints.rs:505`). The navigation and
`/_ts/page-bids` merge in `publisher.rs:5547-5568`. The merge forwards
browser EIDs as sent and adds a KV EID only for a source the browser did not
send. The result is consent-gated and reported in `x-ts-eids`
(`ec/eids.rs:17`, 4 KB cap).

Browser ingest is fill-missing: a different stored value is never replaced,
because a browser value carries no freshness signal (`ec/kv.rs:146-154`).
A KV record holds one UID per source and no EID-level fields
(`ec/kv_types.rs:102`), and a KV value goes out as one entry with the
partner's `openrtb_atype` (`ec/eids.rs:64-76`).

### 3.3 The existing provider pattern

`AuctionProvider` (`auction/provider.rs:84`) is the house pattern for a
pluggable vendor. The provider builds its request and parses the response,
and the orchestrator owns dispatch, timeouts and response handling. This spec
follows the same split.

### 3.4 Platform and timing facts this design depends on

- **KV identity is Fastly-only.** Axum, Cloudflare and Spin pass no KV graph
  (`publisher.rs:6896-6899`) and run no EC finalize or EC generation.
- **New ECs are created before the cookie goes out.** Generation activates an
  ID only after an Add-confirmed create and re-mints on a collision
  (`ec/mod.rs:407-455`). Finalize refuses a generated cookie without a
  backing row (`ec/finalize.rs:145-149`). This spec keeps that.
- **After-send work starts after the last byte.** `send_edgezero_response`
  streams the whole body before returning
  (`adapter-fastly/src/main.rs:699-702`),
  and pull sync runs after that (`main.rs:510-517`). The response has been
  moved into the send by then, so after-send code cannot see its status
  unless it is captured first (`main.rs:512-517`).
- **State crosses into finalize only through `EcFinalizeState`**
  (`adapter-fastly/src/app.rs:318-332`, inserted at `:535`). Response
  extensions require `Clone + Send + Sync`, which a pending platform request
  is not (`platform/http.rs:178-183`), so an in-flight provider call cannot
  outlive the router.
- **The origin fetch can start before the navigation auction.** When the
  EC preload runs, the request has no template-cache key and the platform
  supports concurrent fan-out and pending streaming responses, the origin
  request is sent with `send_async` (`publisher.rs:4825-4852`), then the
  preload runs (`:4859`) and then `dispatch_auction` (`:4947`). That is
  Fastly's default path for a returning EC in Inline mode. In the other
  modes (a template-cache key, ESI, or no preload) the auction is
  dispatched first and the origin fetch waits for it.
- **Outbound deadlines are soft on Fastly.** The Fastly adapter does not
  offer an enforceable total request deadline
  (`has_enforceable_total_request_deadline()` is false,
  `platform/http.rs:375-383`): first-byte and between-byte timers still let
  connection setup or a byte-trickling response overrun. Dynamic backends
  use a fixed 1-second connect timeout (`adapter-fastly/src/backend.rs:240`)
  today. This spec adds `PlatformBackendSpec::connect_timeout`, `None` by
  default, so only hold-mode identity calls set a shorter one (section
  4.7).
- **Navigation detection.** `is_navigation_request` checks
  `Sec-Fetch-Dest: document`, which excludes iframes, and falls back to
  `Accept: text/html`, which a `fetch()` can also send (`http_util.rs:93-118`).
  `is_prefetch_request` detects prefetch and prerender and is `pub(crate)`
  in core (`publisher.rs:3200`). EC classification must wait until
  pre-route filters allow dispatch (`ec/mod.rs:91-96`).
- **Orphan recovery rotates.** When a cookie's row is proven absent
  (`finalize.rs:371-400`), recovery mints a new EC ID and sets a new cookie
  (`finalize.rs:304`, `:333`), before the response is sent. Only the
  publisher fallback may rotate (`adapter-fastly/src/app.rs:885-890`).
- **Today's navigation preload finds orphans.** Every returning GET
  navigation preloads the row unless a valid pull-complete marker lets it
  skip (`publisher.rs:3110-3117`, `:4819-4831`). This spec removes the
  marker (section 10.4), so every returning navigation preloads.
- **Withdrawal tombstones existing rows only,** with a CAS write of up to five
  attempts and then a completion-marker write, per withdrawn ID
  (`ec/kv.rs:1349-1430`, `:1115-1124`; `finalize.rs:490`, `:509-525`).
- **Cookies make responses private.** Any response with `Set-Cookie` gets
  `Cache-Control: private, max-age=0` and loses its edge-cache headers
  (`response_privacy.rs:122-145`). Returning-visitor navigations set no EC
  cookie today (`finalize.rs:124`), though they can set or expire the
  pull-complete marker (`pull_sync_marker.rs:132-155`).
- **The rate limiter counts in 60-second windows** and increments on every
  check that is under the limit. Any hourly limit below 60 becomes one call
  per minute (`ec/rate_limiter.rs:19-27`,
  `adapter-fastly/src/rate_limiter.rs:41-74`). Pull sync keys it on `ec_hash`
  (`pull_sync.rs:373`), the HMAC of the client IP (`generation.rs:83-93`), so
  everyone behind one address shares one budget.
- **The shim's `ts-eids` is small.** It is trimmed to 3,072 encoded bytes,
  lives one day and has no EID-level `ext` (EID fidelity spec §4.3). The
  server accepts up to 8 KB (`ec/prebid_eids.rs:24`). This spec changes
  only the trim order: registered sources are kept first (section 5.3).
- **Prebid Server receives the browser's cookies.** The Prebid transport
  copies the inbound `Cookie` header into every Prebid Server call and strips
  only consent cookies, only in `OpenrtbOnly` mode
  (`integrations/prebid.rs:2091-2108`,
  `consent_config.rs:137-150`). The publisher proxy forwards the `Cookie`
  header to the origin unchanged (`publisher.rs:3124-3147`).

## 4. Architecture

### 4.1 Overview

```mermaid
flowchart TD
    subgraph Writer["Writer request: a top-level navigation"]
        A[Read ts-ec, consent, ts-eids] --> C[Returning EC:<br/>KV snapshot read]
        C --> D[Pick eligible providers]
        D --> E[Hold-mode providers, if the<br/>navigation runs an auction]
        E --> F[Finalize: queue changes]
        F --> G[Send response]
        G --> H[After-send provider due:<br/>re-read live row]
        H --> I[After-send providers]
        I --> J[Apply IdentityMutation:<br/>one conditional write]
    end
    subgraph Stagers["Stagers: /auction, /_ts/page-bids, integration proxy"]
        K[Read KV where the<br/>route reads it today] --> L[No providers;<br/>revoke-only capture]
        L --> M[Mirror payload in the JSON response;<br/>a captured revocation writes KV]
    end
```

The design has four parts:

1. **`IdentityProvider` trait** (sections 4.2 to 4.6). One per vendor.
2. **`IdentityCoordinator`** (section 4.7). Shared. Chooses providers,
   dispatches them, turns results into changes.
3. **`IdentityMutation`** (section 5.4). A request-scoped accumulator. Every
   KV change in the writer request is added to it, and it is applied with one
   write.
4. **The mirror** (section 6.2). `ts-eids-s` is a client-only cookie the
   shim writes from the servable server set, for page scripts. The server
   sets no identity cookie of its own beyond `ts-ec`.

New code lives in `crates/trusted-server-core/src/ec/identity/`:

| File              | Contents                                                                        |
| ----------------- | ------------------------------------------------------------------------------- |
| `mod.rs`          | Module docs, re-exports                                                         |
| `ids.rs`          | `IdentityProviderId`, `SourceDomain`, `UnixTime`                                |
| `provider.rs`     | The `IdentityProvider` and `IdentityProviderConfig` traits                      |
| `descriptor.rs`   | `ProviderDescriptor`, `Resolves`, `Captures`, `Input`, `ConsentPolicy`          |
| `input.rs`        | `ResolveInput`, `CaptureInput`, `StoredId`, `HashedEmail`                       |
| `exchange.rs`     | `ProviderRequest`, `ProviderResponse`                                           |
| `resolution.rs`   | `Resolution`, `ResolvedEid`, `ResolvedUid`, `IdentityProviderError`, entry sets |
| `policy.rs`       | Consent policy evaluation and the consent fingerprint                           |
| `revision.rs`     | Revisions and the UID hashes of `ps.iv` and `ps.ov`                             |
| `catalog.rs`      | The configured providers, their narrowed sources and source ownership           |
| `servable.rs`     | The servability check of section 7.1                                            |
| `coordinator.rs`  | Eligibility, dispatch, circuit breaker, result-to-change map                    |
| `mutation.rs`     | `IdentityMutation` and its merge into a live `KvEntry`                          |
| `mirror.rs`       | The `ts-eids-s` mirror payload and its size cap                                 |
| `request.rs`      | The writer decision and the request's identity state                            |
| `removal.rs`      | The capture removal write of section 5.3                                        |
| `runtime.rs`      | `IdentityRuntime`: the catalog and keys an adapter builds once                  |
| `flush.rs`        | Phase 4, the single write after send                                            |
| `generic_pull.rs` | `GenericPullProvider`, today's pull sync on the trait                           |
| `config.rs`       | `[ec.identity]` settings and ownership validation                               |
| `test_support.rs` | `ProviderHarness`, a fake HTTP client and a write-counting KV store             |

### 4.2 The trait

The trait follows five rules:

- **One required method.** A provider implements `descriptor`. It adds a
  capability by declaring it in the descriptor and overriding the matching
  methods. Every other method has a default body.
- **Declarative.** What a provider is, may write, needs and must respect
  lives in one `ProviderDescriptor`. It is built from config with no
  secrets, so `ts config validate` and the running coordinator read the same
  data.
- **No I/O.** The coordinator sends requests, buffers, decodes and caps
  responses, and does all KV and cookie work. Providers only build requests
  and read buffered bytes, so the trait is synchronous and needs no
  `async_trait`. `AuctionProvider` parses asynchronously so that it can drain
  streaming bodies. The coordinator buffers instead, because it has to cap
  response bytes anyway.
- **Pure.** `request`, `response` and `capture` have no side effects and no
  hidden inputs. Randomness comes only from `ResolveInput::random_bytes`,
  and anything `response` needs from `request` travels in the call context
  (section 4.5). The coordinator calls `request` at most once per
  dispatch.
- **Safe defaults.** The default consent policy is the strictest one, a
  provider can read only what it declared, and a capability declared but not
  implemented fails loudly in tests (section 13).

```rust
/// A vendor that turns server-side request signals into EIDs.
///
/// Only [`descriptor`](Self::descriptor) is required. A provider adds a
/// capability by declaring it in its descriptor and overriding the matching
/// methods:
///
/// - active resolution: [`Resolves`], plus [`request`](Self::request) and
///   [`response`](Self::response)
/// - passive capture: [`Captures`], plus [`capture`](Self::capture)
///
/// Methods added to this trait later get default bodies, so existing
/// providers keep compiling. No method takes generic parameters, so the
/// trait stays object-safe. Every default body returns the
/// [`IdentityProviderError::not_implemented`] marker, so a capability that is
/// declared but not implemented is caught by the harness.
pub trait IdentityProvider: Send + Sync {
    /// What this provider is, what it may write, what it needs and which
    /// consent it requires.
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Builds the call for this request, or `None` to skip it this time.
    ///
    /// Skipping is not a failure: the coordinator waits `refresh_after_sec`
    /// before asking again (section 8).
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::UndeclaredInput`] when it reads
    /// something its descriptor did not declare, or another
    /// [`IdentityProviderError`] when the request cannot be built.
    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let _ = input;
        Err(IdentityProviderError::not_implemented("request"))
    }

    /// Turns the buffered response to [`request`](Self::request) into a
    /// resolution.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Rejected`] for a refusal and
    /// [`IdentityProviderError::Malformed`] for a body it cannot parse.
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let _ = (response, input);
        Err(IdentityProviderError::not_implemented("response"))
    }

    /// Reads IDs from a response TS proxied for this provider, with no
    /// outbound call. [`Resolution::no_match`] means the response held
    /// nothing to capture, and costs nothing: capture never backs off.
    ///
    /// # Errors
    ///
    /// Returns an [`IdentityProviderError`] only for a provider bug. A body
    /// it does not recognise is `Ok(Resolution::no_match())`, not an error.
    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let _ = input;
        Err(IdentityProviderError::not_implemented("capture"))
    }
}

/// Implemented by each identity integration's config type.
///
/// Builds the provider's descriptor from config alone, with no secrets, so
/// `ts config validate` checks ownership against the same descriptor the
/// running provider uses.
pub trait IdentityProviderConfig: IntegrationConfig {
    /// Describes the provider this config enables.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>>;
}
```

A provider's constructor calls its config's `build_descriptor()` and stores
the result, so the validated descriptor and the running one are the same
value. The two methods have different names so that a type implementing both
traits never hits an ambiguous call. `IntegrationConfig` is the existing
config trait (`settings.rs:235`), so identity configs load like every other
integration config.

**How each change stays additive.** Vendor providers live in
`trusted-server-core` today, and `#[non_exhaustive]` has no effect inside its
defining crate. So additivity rests on construction rules, with
`#[non_exhaustive]` as extra protection for providers that move into their
own crates under #777:

| Change                  | Why existing providers keep compiling                                                                      |
| ----------------------- | ---------------------------------------------------------------------------------------------------------- |
| A new capability        | A new trait method with a default body, and a new descriptor builder method                                |
| A new input             | A new `Input` variant and a new accessor. Providers list inputs; they never match on `Input`               |
| A new field on any type | Every public struct has private fields and is built through constructors or builders                       |
| A new outcome           | A new `Resolution` constructor. Providers build resolutions, never match on them                           |
| A new consent signal    | Providers keep compiling. By design they fail closed: `strict()` blocks the signal until a vendor opts out |
| A new resolve kind      | Kinds are opt-in: a provider is dispatched only for the kinds it lists (default `Initial`, `Refresh`)      |
| A new error             | Errors are built through constructors such as `IdentityProviderError::rejected(status)`                    |

Providers compare kinds with predicates (`input.is_refresh(&source)`) rather
than `match`. Operator settings (`[ec.identity.providers.<id>]`: mode,
timeouts, source narrowing, caps) are applied by the coordinator on top of
the descriptor. Providers never read them.

**Conventions for the listings below.** Every builder method and getter is
`#[must_use]`. `Resolves`, `Captures` and `EidProvenance` implement
`Default`. `Input`, `Access`, `ResolveKind` and `UsOptOut` derive
`Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
derive_more::Display`.
`When` derives `Debug, Clone, Copy, PartialEq, Eq` only, because `Duration`
has no `Display`. `ProviderRequest` derives `Debug, Clone`. `StoredId` and
`HashedEmail` derive `Clone` and have redacting `Debug` implementations
(section 9.3).
`StateChange` derives `Debug, PartialEq, Eq`. `ProviderDescriptor`,
`Resolves`, `Captures`, `ConsentPolicy` and `EidProvenance` derive
`Debug, Clone, PartialEq, Eq`, so tests can compare descriptors. `UnixTime`
derives
`Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`. `ResolvedEid` and
`ResolvedUid` derive `Clone` and have redacting `Debug` implementations
(section 9.3). `IdentityProviderId`
and `SourceDomain` derive `Debug, Clone, PartialEq, Eq, Hash, PartialOrd,
Ord, derive_more::Display` and implement `FromStr` (with
`type Err = Report<IdentityProviderError>`), `Serialize` and `Deserialize`.
Every fallible function documents `# Errors`, and every public item has a
doc comment. The listings abbreviate these.

### 4.3 The descriptor

```rust
let descriptor = ProviderDescriptor::builder("exampleid".parse()?)
    .with_source("exampleid.example.com".parse()?)
    .with_resolves(
        Resolves::new()
            .with_endpoint("https://api.exampleid.example.com/v1/fetch")
            .with_needs([Input::ClientIp, Input::UserAgent])
            .with_wants([Input::PageUrl]),
    )
    .with_consent_policy(
        ConsentPolicy::strict()
            .with_tcf_purposes([4])
            .with_tcf_vendor(9999),
    )
    .build()?;
```

```rust
impl ProviderDescriptor {
    pub fn builder(id: IdentityProviderId) -> ProviderDescriptorBuilder;
    pub fn id(&self) -> &IdentityProviderId;
    pub fn sources(&self) -> &[SourceDomain];
    /// The first source added. Every built descriptor has one.
    pub fn primary_source(&self) -> &SourceDomain;
    /// Finds a declared source by a vendor's spelling of it, normalized.
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain>;
    pub fn resolves(&self) -> Option<&Resolves>;
    /// The endpoint declared with `Resolves::with_endpoint`, if any.
    pub fn endpoint(&self) -> Option<&Url>;
    pub fn captures(&self) -> Option<&Captures>;
    pub fn consent_policy(&self) -> &ConsentPolicy;
    pub fn provenance(&self, source: &SourceDomain) -> Option<&EidProvenance>;
    /// Whether IDs of `source` are declared as expiring.
    pub fn is_expiring(&self, source: &SourceDomain) -> bool;
}

impl ProviderDescriptorBuilder {
    /// Adds a source this provider may write. Repeatable.
    pub fn with_source(self, source: SourceDomain) -> Self;
    /// Declares active resolution.
    pub fn with_resolves(self, resolves: Resolves) -> Self;
    /// Declares passive capture.
    pub fn with_captures(self, captures: Captures) -> Self;
    /// Replaces the default policy, [`ConsentPolicy::strict`].
    pub fn with_consent_policy(self, policy: ConsentPolicy) -> Self;
    /// Marks the IDs of every declared source as expiring (section 6.1).
    pub fn with_expiring_ids(self) -> Self;
    /// Marks the IDs of one declared source as expiring, for a provider
    /// whose sources differ.
    pub fn with_expiring_source(self, source: &SourceDomain) -> Self;
    /// Default EID-level fields for a declared source. At output, the
    /// coordinator applies them to a stored entry of that source, written
    /// by this provider, only when the entry sets none of `inserter`,
    /// `matcher` and `mm`. An entry that sets any of them is the vendor's
    /// complete statement, so vendor and config provenance never mix in one
    /// entry. The defaults are not stored, so a config change applies at
    /// once.
    pub fn with_provenance(self, source: &SourceDomain, provenance: EidProvenance) -> Self;
    /// # Errors
    ///
    /// Returns `Config` when the descriptor has no source, no capability, a
    /// resolver with no endpoint or host, an endpoint that is not an `https`
    /// URL, a host that is not a bare hostname, provenance or an expiring
    /// mark for a source it does not declare, or a provenance `inserter` or
    /// `matcher` that is blank or over 255 bytes, or an `mm` below 0.
    pub fn build(self) -> Result<ProviderDescriptor, Report<IdentityProviderError>>;
}

impl Resolves {
    pub fn new() -> Self;
    /// The endpoint the provider calls, as config spells it. `build()`
    /// parses it and requires `https`. Its host becomes an allowed host.
    pub fn with_endpoint(self, endpoint: &str) -> Self;
    /// Adds another host the provider may call. Repeatable.
    pub fn with_host(self, host: &str) -> Self;
    /// Inputs required for every kind. Without them the provider is skipped.
    pub fn with_needs(self, inputs: impl IntoIterator<Item = Input>) -> Self;
    /// Inputs required for one kind only.
    pub fn with_needs_for(self, kind: ResolveKind, inputs: impl IntoIterator<Item = Input>) -> Self;
    /// Inputs passed when available, never required.
    pub fn with_wants(self, inputs: impl IntoIterator<Item = Input>) -> Self;
    /// The kinds this provider handles. Default: `Initial` and `Refresh`.
    pub fn with_kinds(self, kinds: impl IntoIterator<Item = ResolveKind>) -> Self;
    /// This provider keeps private state in KV (`ps.st`, section 6.1). It
    /// can then read [`ResolveInput::state`] and return a state change.
    pub fn with_private_state(self) -> Self;
    /// Keeps an expired record this provider wrote for a source it owns as
    /// a refresh anchor until `exp` plus `grace` (section 5.4, step 4), for
    /// a provider without private state whose refresh accepts an expired
    /// ID. `build()` rejects it without the `Refresh` kind or with a zero
    /// grace.
    pub fn with_refresh_grace(self, grace: Duration) -> Self;
}

impl Captures {
    pub fn new() -> Self;
    /// Offers proxied responses to exactly this method and path. Repeatable.
    pub fn with_path(self, method: Method, path: &str) -> Self;
    /// Offers proxied responses whose path starts with `prefix`. Repeatable.
    pub fn with_path_prefix(self, prefix: &str) -> Self;
}

/// Default EID-level fields (OpenRTB 2.6 §3.2.27) that bidders read, such as
/// who inserted an ID and how it was matched. A provider whose vendor
/// returns these per response sets them on each `ResolvedEid` instead
/// (section 4.5). An unset field stays unset: OpenRTB 2.6 reads a missing
/// `matcher` as `source`, so a default never needs a blank value.
impl EidProvenance {
    pub fn new() -> Self;
    pub fn with_inserter(self, inserter: &str) -> Self;
    pub fn with_matcher(self, matcher: &str) -> Self;
    pub fn with_mm(self, mm: i32) -> Self;

    pub fn inserter(&self) -> Option<&str>;
    pub fn matcher(&self) -> Option<&str>;
    pub fn mm(&self) -> Option<i32>;
}

/// An input a provider can ask for. Consent strings, the provider's stored
/// IDs, the publisher domain, the time and the declared endpoint are always
/// available. Private state is available to providers declared
/// `with_private_state()`.
#[non_exhaustive]
pub enum Input {
    ClientIp,
    UserAgent,
    /// The top-level page URL without query string or fragment.
    PageUrl,
    /// Country and subdivision only.
    Region,
    HashedEmail,
}

/// Something a provider read without declaring it.
#[non_exhaustive]
pub enum Access {
    Input(Input),
    Endpoint,
    PrivateState,
}

impl ConsentPolicy {
    /// The default. Blocked when any consent signal present cannot be
    /// decoded. Where TCF applies, requires Purpose 1 and the purposes and
    /// vendor the provider declares. Blocked by every US opt-out TS decodes
    /// (sale, sharing, targeted advertising, and the GPC and known-child
    /// fields of GPP sections), by `Sec-GPC: 1` and by `DNT: 1`.
    pub fn strict() -> Self;
    /// Declares the TCF purposes the provider processes under consent, as
    /// its GVL entry lists them. Each is required where TCF applies, beside
    /// Purpose 1, which is always required.
    pub fn with_tcf_purposes(self, purposes: impl IntoIterator<Item = u8>) -> Self;
    /// Requires vendor consent for this IAB Global Vendor List ID.
    pub fn with_tcf_vendor(self, vendor_id: u16) -> Self;
    /// Declares that the provider has no GVL ID, so purpose consent alone
    /// lets it run where TCF applies. Needs a documented legal basis.
    pub fn without_tcf_vendor(self) -> Self;
    /// Relaxations. Each needs a documented legal basis in the vendor spec.
    pub fn allowing_dnt(self) -> Self;
    pub fn allowing_gpc(self) -> Self;
    pub fn allowing_us_opt_out(self, opt_out: UsOptOut) -> Self;
}

#[non_exhaustive]
pub enum UsOptOut {
    Sale,
    Sharing,
    TargetedAdvertising,
}

impl SourceDomain {
    pub fn as_str(&self) -> &str;
    /// Whether a vendor's spelling of a source names this one, after
    /// normalization.
    pub fn matches(&self, name: &str) -> bool;
}
```

**Declared purposes and vendor.** Decided after the round 14 review. A
fixed purpose set fits no vendor: one GVL entry lists purposes another does
not. So `strict()` requires only Purpose 1, and `build()` returns `Config`
for a descriptor with `resolves` whose policy never calls
`with_tcf_purposes`, or names no vendor with `with_tcf_vendor` and does not
call `without_tcf_vendor`. TCF Policies, Chapter III, Policy 14(3) needs a
legal basis for each vendor's processing, so purpose consent alone is
never the default. A capture-only provider needs neither, because it sends
nothing. Generic pull keeps its parity policy (`eid_persistence_only`,
section 10.4), which checks no TCF purpose or vendor, as pull sync does
today.

The full EC ID is not an `Input`. A public variant would let any provider
declare it, and only generic pull may receive it. `Resolves` has a
`pub(in crate::ec::identity)` method, `with_ec_id()`, and `ResolveInput` an
accessor with the same visibility. Only `generic_pull.rs` uses them.

Every capture path and prefix must lie under the registering integration's
own proxy routes (`IntegrationProxy::routes`, `integrations/registry.rs:316`).
`IntegrationRegistration::build()` cannot fail
(`integrations/registry.rs:720-722`),
so the check runs in `IntegrationRegistry::with_plan`
(`integrations/registry.rs:842-865`), which already returns an error and
merges every registration. The error names the integration.

`IdentityProviderId` is separate from `crate::auction::ProviderId`
(`auction/plan.rs:24-77`) but follows the same pattern. `FromStr` accepts
`^[a-z][a-z0-9_-]{0,62}$`, so an ID can be a bare TOML key and can equal an
integration ID that uses `_`. `SourceDomain::from_str` goes through
`normalize_partner_source_domain` (`ec/partner.rs:20`), mapping its `String`
error to `Config`, so `"…".parse()?` composes inside `build_descriptor`.

```rust
impl ProviderConsent {
    pub fn gdpr_applies(&self) -> bool;
    pub fn raw_tc_string(&self) -> Option<&str>;
    pub fn raw_gpp_string(&self) -> Option<&str>;
    pub fn gpp_section_ids(&self) -> Option<&[u16]>;
    pub fn raw_us_privacy(&self) -> Option<&str>;
}
```

### 4.4 Inputs

The coordinator gives a provider only what its descriptor declared. When the
sources in one request have different kinds, the inputs of every kind
present are available, and the provider uses `is_initial` or `is_refresh` to
decide per source. A required input is read with `?` and a wanted one with
`.ok()`:

```rust
let ip = input.client_ip()?;
let page = input.page_url().ok();
```

Accessors fail in two ways:

- **`UndeclaredInput { access }`**: the descriptor never declared the input,
  the endpoint or private state. That is a provider bug. `.ok()` would hide
  it, so every accessor also records an undeclared read on the
  `ResolveInput`. After `request` or `response` returns, the coordinator
  logs any recorded
  read as `Config`, and the harness fails the test (section 13), whatever
  the provider did with the error.
- **`InputUnavailable { input }`**: the input was declared but is missing
  this time, for example a wanted input or one declared only for another
  kind. That is a normal skip, handled like `request` returning `None`.

```rust
impl ResolveInput<'_> {
    /// The sources to resolve this time, each with its own kind.
    pub fn sources(&self) -> impl Iterator<Item = (&SourceDomain, ResolveKind)> + '_;
    pub fn is_initial(&self, source: &SourceDomain) -> bool;
    pub fn is_refresh(&self, source: &SourceDomain) -> bool;
    /// The descriptor's primary source, for single-source providers.
    pub fn primary_source(&self) -> &SourceDomain;
    /// The stored record for one of this provider's sources, valid or
    /// expired.
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId>;
    /// This provider's private state. The raw JSON is loaded once per
    /// request and decoded on each call. `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// `InvalidState` when the stored state does not decode as `T`, for
    /// example after a schema change. The failed decode is recorded on the
    /// `ResolveInput`, like an undeclared read, and the coordinator clears
    /// the stored state whatever the provider returns, so the next call
    /// starts fresh (section 8). Without `with_private_state()` it is
    /// `UndeclaredInput`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>>;
    /// The endpoint declared with `Resolves::with_endpoint`.
    pub fn endpoint(&self) -> Result<&Url, Report<IdentityProviderError>>;
    pub fn consent(&self) -> &ProviderConsent;
    pub fn publisher_domain(&self) -> &str;
    pub fn now(&self) -> UnixTime;
    /// Random bytes from the platform's generator, the one EC generation
    /// already uses (`ec/generation.rs:52-56`). Fixed by the harness, so
    /// tests are deterministic.
    pub fn random_bytes<const N: usize>(&self) -> [u8; N];

    pub fn client_ip(&self) -> Result<IpAddr, Report<IdentityProviderError>>;
    pub fn user_agent(&self) -> Result<&str, Report<IdentityProviderError>>;
    pub fn page_url(&self) -> Result<&Url, Report<IdentityProviderError>>;
    pub fn region(&self) -> Result<&Region, Report<IdentityProviderError>>;
    pub fn hashed_email(&self) -> Result<&HashedEmail, Report<IdentityProviderError>>;
}

impl CaptureInput<'_> {
    /// The proxied request path, without query string.
    pub fn path(&self) -> &str;
    pub fn response(&self) -> &ProviderResponse;
    pub fn primary_source(&self) -> &SourceDomain;
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain>;
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId>;
    /// The request's consent, from its cookies and headers.
    pub fn consent(&self) -> &ProviderConsent;
    pub fn publisher_domain(&self) -> &str;
    pub fn now(&self) -> UnixTime;
}

#[non_exhaustive]
pub enum ResolveKind {
    /// No stored record for the source.
    Initial,
    /// A stored record exists, valid or expired, and is due (section 4.7,
    /// step 4). The provider decides whether it can refresh an expired one;
    /// if not, it returns `None` for that source.
    Refresh,
}

/// A source's stored record: its entry set and the record's metadata.
impl StoredId {
    /// Every stored entry for the source, in stored order, with its UIDs and
    /// EID-level fields. A record that browser ingest, generic pull, batch
    /// sync or a schema version 1 row wrote is one entry with one UID.
    pub fn eids(&self) -> &[Eid];
    /// The first UID of the first entry. What a single-UID refresh, such as
    /// LiveRamp's, sends back.
    pub fn uid(&self) -> &str;
    /// The `atype` of that first UID.
    pub fn atype(&self) -> Option<i32>;
    /// The UID-level ext of that first UID, as bidders read it in
    /// `uids[].ext`.
    pub fn ext(&self) -> Option<&serde_json::Value>;
    /// The provider that wrote it. `None` for browser ingest and version 1
    /// records.
    pub fn writer(&self) -> Option<&IdentityProviderId>;
    pub fn acquired_at(&self) -> Option<UnixTime>;
    pub fn expires_at(&self) -> Option<UnixTime>;
    pub fn refresh_at(&self) -> Option<UnixTime>;
    pub fn is_expired(&self, now: UnixTime) -> bool;
}

impl Region {
    /// ISO 3166-1 alpha-2, upper case, for example `"US"`.
    pub fn country(&self) -> &str;
    /// The ISO 3166-2 subdivision without the country prefix, upper case,
    /// for example `"CA"`. `None` when the platform does not know it.
    pub fn subdivision(&self) -> Option<&str>;
    /// Whether this is `country` and `subdivision`.
    pub fn is(&self, country: &str, subdivision: &str) -> bool;
}

impl HashedEmail {
    pub fn as_bytes(&self) -> &[u8; 32];
    /// Lowercase hex, as LiveRamp takes it.
    pub fn to_hex(&self) -> String;
    /// Standard base64, as UID2 takes it.
    pub fn to_base64(&self) -> String;
}

impl UnixTime {
    pub fn from_secs(secs: u64) -> Self;
    /// For vendors such as Lockr and UID2 that send milliseconds.
    pub fn from_millis(millis: u64) -> Self;
    pub fn as_secs(self) -> u64;
}

impl Add<Duration> for UnixTime {
    type Output = UnixTime;
}
```

`ProviderConsent` is a view of the request's `ConsentContext`
(`consent/types.rs:109-148`) with the raw strings providers forward and
`gdpr_applies`. It leaves out the decoded detail and `jurisdiction`.
Providers never decide consent: the coordinator has already applied the
descriptor's policy. The raw GPP string and its section IDs still show which
US state section applies, so withholding `jurisdiction` hides only TS's
decoded reading of it.

Where each input comes from. Phase 1 captures the user agent, DNT and the
page URL into `EcContext`, because after-send code no longer has the request
(section 3.4):

- **`ClientIp`**: `services.client_info().client_ip`, the platform's address
  (as `publisher.rs:5574-5578` does), never a request header and never the
  normalized EC IP.
- **`PageUrl`**: the navigation URL, the only request providers run on
  (section 4.7, step 7). Always without query string or fragment. A path
  can still carry identifiers on some sites, so `PageUrl` is a wanted input
  for most providers, never needed.
- **`Region`**: from the request's `GeoInfo`
  (`adapter-fastly/src/app.rs:423-432`).
  `GeoInfo.region` is a free-form `Option<String>` (`platform/types.rs:32`),
  so the coordinator normalizes it to the ISO 3166-2 subdivision and upper
  case. City, coordinates, metro and ASN are never passed.

`HashedEmail` holds the 32 raw bytes of a SHA-256 digest. Its `Debug` output
is redacted. No code in v1 constructs one.

The coordinator supplies `now` from `checked_current_timestamp()`
(`ec/mod.rs:736-748`) and skips identity work for the request when the clock
fails, because a zero time would make every expiry check pass.

### 4.5 Requests, responses and results

```rust
impl ProviderRequest {
    pub fn get(url: &Url) -> Self;
    pub fn post_json(url: &Url, body: &impl Serialize) -> Result<Self, Report<IdentityProviderError>>;
    /// A POST with a `text/plain` body, for APIs that send base64 text.
    pub fn post_text(url: &Url, body: &str) -> Self;
    /// Any method, for APIs the helpers above do not fit. `get`, `post_json`
    /// and `post_text` are shorthands for this.
    pub fn new(method: Method, url: &Url) -> Self;
    /// Sets the body and its `Content-Type`.
    ///
    /// # Errors
    ///
    /// Returns `Config` when `content_type` is not a valid header value.
    pub fn with_body(self, content_type: &str, body: Vec<u8>) -> Result<Self, Report<IdentityProviderError>>;
    pub fn with_query(self, name: &str, value: &str) -> Self;
    /// Both header helpers accept any header. A `Cookie` header, from
    /// either, makes the coordinator refuse to send the request, and the
    /// harness's conformance check fails it (section 13).
    pub fn with_header(self, name: HeaderName, value: HeaderValue) -> Self;
    /// For dynamic values. Rejects an invalid name or value.
    pub fn try_with_header(self, name: &str, value: &str) -> Result<Self, Report<IdentityProviderError>>;
    /// Adds `Authorization: Bearer <token>`.
    pub fn with_bearer(self, token: &Redacted<String>) -> Self;
    /// Data `response` needs from this call, such as a nonce or which
    /// endpoint was used. Kept in memory with the call, never sent.
    ///
    /// # Errors
    ///
    /// Returns `Config` when it does not serialize, or is over 1,024 bytes
    /// of JSON.
    pub fn try_with_call_context(self, context: &impl Serialize) -> Result<Self, Report<IdentityProviderError>>;
    pub fn method(&self) -> &Method;
    pub fn url(&self) -> &Url;
    pub fn headers(&self) -> &HeaderMap;
    pub fn body(&self) -> &[u8];
}

impl ProviderResponse {
    pub fn status(&self) -> StatusCode;
    pub fn header(&self, name: &str) -> Option<&str>;
    /// The decoded body. The coordinator undoes `Content-Encoding` first.
    pub fn body(&self) -> &[u8];
    /// Parses the body as JSON, whatever the status. A parse failure is
    /// `Malformed`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>>;
    /// Requires a 2xx status, else `Rejected`, then parses the body as JSON.
    pub fn success_json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>>;
    /// Requires a 2xx status, else `Rejected`, and a UTF-8 body, else
    /// `Malformed`.
    pub fn success_text(&self) -> Result<&str, Report<IdentityProviderError>>;
    /// The context the request carried, decoded. `Ok(None)` when there was
    /// none; a context that does not decode as `T` is `Config`.
    pub fn call_context<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>>;
}

impl Resolution {
    /// One or more EID entries. A source may appear in several entries, in
    /// the order bidders should see them. The entries of one source form its
    /// new entry set. For an owned source it replaces the stored set whole
    /// (section 6.1); for an unowned source it only fills an absent or
    /// expired record (`FillMissing`, section 7.2) and is never replaced by
    /// its writer. A source the result does not name is left as it is. An
    /// empty result becomes `no_match`, and `is_no_match()` then returns
    /// true.
    pub fn found(eids: impl IntoIterator<Item = ResolvedEid>) -> Self;
    /// The provider recognized no one, or a capture held nothing.
    pub fn no_match() -> Self;
    /// The stored ID for `source` is no longer valid.
    pub fn invalidate(source: &SourceDomain) -> Self;
    /// The provider reports the user opted out.
    pub fn opted_out() -> Self;
    /// Also invalidates `source`. Chainable after `found`, so one response
    /// can return some sources and drop others.
    pub fn also_invalidate(self, source: &SourceDomain) -> Self;
    /// The earliest the coordinator may call again. Default: 24 hours after
    /// `no_match`, none otherwise.
    pub fn with_retry_in(self, delay: Duration) -> Self;
    /// Replaces the provider's private state.
    ///
    /// The state is stored as embedded JSON of at most 1,024 bytes. A larger
    /// value rejects the result's positive part whole (section 5.4): no ID,
    /// state or scheduling change is stored, the old state stays, and the
    /// call is handled as `Malformed`. The result's removals still apply.
    /// From a provider not declared
    /// `with_private_state()`, the whole resolution is a `Config` error.
    /// `capture` never changes state: a state change in a capture result is
    /// ignored with a warning, and the harness fails the test.
    pub fn replacing_state(self, state: &impl Serialize) -> Self;
    pub fn clearing_state(self) -> Self;

    pub fn eids(&self) -> &[ResolvedEid];
    pub fn invalidated(&self) -> &[SourceDomain];
    pub fn is_no_match(&self) -> bool;
    pub fn is_opted_out(&self) -> bool;
    pub fn retry_in(&self) -> Option<Duration>;
    pub fn state_change(&self) -> StateChange<'_>;
}

pub enum StateChange<'a> {
    Keep,
    /// The new state as JSON.
    Replace(&'a str),
    Clear,
}

/// One OpenRTB EID entry a provider returns: a source, its UIDs and its
/// EID-level fields.
impl ResolvedEid {
    /// An entry with one UID, the common case.
    pub fn new(source: &SourceDomain, uid: ResolvedUid) -> Self;
    /// Adds another UID to this entry. Repeatable.
    pub fn with_uid(self, uid: ResolvedUid) -> Self;
    /// EID-level fields. An entry that sets any of `inserter`, `matcher`
    /// and `mm` takes no `with_provenance` default (section 4.3).
    pub fn with_inserter(self, inserter: &str) -> Self;
    pub fn with_matcher(self, matcher: &str) -> Self;
    pub fn with_mm(self, mm: i32) -> Self;
    /// EID-level ext. Over 2,048 bytes as compact JSON it is dropped and
    /// the entry kept.
    pub fn with_ext(self, ext: serde_json::Value) -> Self;
    /// Timing applies to the source's whole entry set. When several entries
    /// of one source set it, the earliest time wins (below).
    pub fn with_expires_in(self, delay: Duration) -> Self;
    pub fn with_expires_at(self, at: UnixTime) -> Self;
    pub fn with_refresh_in(self, delay: Duration) -> Self;
    pub fn with_refresh_at(self, at: UnixTime) -> Self;
    /// When this ID stops being refreshable, such as a refresh token's
    /// expiry. Only for a provider declared `with_private_state()`; from any
    /// other it is ignored with a warning. Until then an expired record is
    /// kept as a refresh anchor (section 5.4).
    pub fn with_refresh_until(self, at: UnixTime) -> Self;

    pub fn source(&self) -> &SourceDomain;
    pub fn uids(&self) -> &[ResolvedUid];
    pub fn inserter(&self) -> Option<&str>;
    pub fn matcher(&self) -> Option<&str>;
    pub fn mm(&self) -> Option<i32>;
    pub fn ext(&self) -> Option<&serde_json::Value>;
    pub fn expiry(&self) -> Option<When>;
    pub fn refresh(&self) -> Option<When>;
    pub fn refresh_until(&self) -> Option<UnixTime>;
}

/// One UID within a [`ResolvedEid`].
impl ResolvedUid {
    pub fn new(id: impl Into<String>) -> Self;
    pub fn with_atype(self, atype: i32) -> Self;
    /// UID-level ext. Over 2,048 bytes as compact JSON it is dropped and the
    /// UID kept.
    pub fn with_ext(self, ext: serde_json::Value) -> Self;

    pub fn id(&self) -> &str;
    pub fn atype(&self) -> Option<i32>;
    pub fn ext(&self) -> Option<&serde_json::Value>;
}

/// A time as a provider gave it: relative to `now`, or absolute.
pub enum When {
    In(Duration),
    At(UnixTime),
}

/// A provider failure. Built through the constructors below.
#[derive(Debug, derive_more::Display)]
#[non_exhaustive]
pub enum IdentityProviderError {
    /// Configuration or provider code makes the call impossible.
    #[display("identity provider configuration error: {message}")]
    Config { message: String },
    /// The provider read something its descriptor did not declare.
    #[display("`{access}` was read but not declared")]
    UndeclaredInput { access: Access },
    /// A declared input is not available for this request.
    #[display("input `{input}` is unavailable for this request")]
    InputUnavailable { input: Input },
    /// The provider refused the request.
    #[display("provider rejected the request with status {status}")]
    Rejected { status: StatusCode },
    /// The provider's response could not be parsed.
    #[display("provider response could not be parsed")]
    Malformed,
    /// The stored private state does not decode as the provider's type.
    #[display("stored provider state could not be decoded")]
    InvalidState,
    /// Set by the coordinator only. Providers never see transport failures.
    #[display("provider request failed in transport")]
    Transport,
}

impl IdentityProviderError {
    pub fn config(message: impl Into<String>) -> Report<Self>;
    pub fn rejected(status: StatusCode) -> Report<Self>;
    pub fn malformed() -> Report<Self>;
    /// The default body of a capability method: a `Config` error with a
    /// fixed marker message, so the harness can tell a missing override from
    /// a real failure.
    pub fn not_implemented(method: &'static str) -> Report<Self>;
}

impl core::error::Error for IdentityProviderError {}
```

Relative times (`with_expires_in`, `with_refresh_in`, `with_retry_in`) are
resolved against the request's `now` by the coordinator, so providers rarely
need the clock.

**From entries to an entry set.** The coordinator groups a result's entries
by source, keeping their order, and checks each group with the browser
parser's field rules (EID fidelity spec §4.4), then with per-source caps
the browser parser does not have: 8 entries, 32 UIDs across them, and
`MAX_ENTRY_SET_BYTES`. The limits are counted before the result joins the
mutation, so a stateful result whose set is discarded is handled like an
oversized state: its positive part is rejected whole (section 5.4) and its
removals still apply:

- An entry whose source the provider does not claim (or that the operator
  narrowed away) is discarded.
- A UID whose `id` is empty, blank or over `MAX_UID_LENGTH`
  (`ec/kv_types.rs:35`) is dropped. An entry left with no UID is discarded.
- An `inserter` or `matcher` that is blank or over 255 bytes, a UID-level or
  EID-level `ext` that is not an object or is over 2,048 bytes as compact
  JSON, and an `mm` or `atype` below 0 are dropped as fields, and the entry
  is kept. OpenRTB 2.6 reads a missing `matcher` as `source`, so dropping a
  blank one keeps its meaning, unless the entry is left with no EID-level
  field and a provenance default then applies (section 4.3).
- An entry equal to an earlier kept entry of the same source is dropped.
- A source keeps its first 8 entries and its first 32 UIDs across them.
- A source's entry set over `MAX_ENTRY_SET_BYTES` (4,096 bytes) is
  discarded whole, never trimmed, because a trimmed set is not what the
  vendor returned. The field caps do not guarantee the set cap: a set whose
  legal fields exceed 4,096 bytes is discarded whole. The harness fails a
  fixture that hits this.

`MAX_ENTRY_SET_BYTES` is measured on the stored `eids` array of section 6.1:
entries without `source`, after the field rules above, without provenance
defaults, serialized by `serde_json::to_vec`. The coordinator and
`KvEntry::validate` call the same function, so a set one accepts the other
accepts.

A discarded set leaves the stored set as it is, as if the result did not
name the source. A result all of whose sets are discarded is handled as
`Malformed` (section 8): `f` grows with its backoff, and its removals still
apply.

Each discard and drop is logged with the provider ID, source and field
only. The set's `exp`, `ra` and `ru` are the earliest values its kept
entries set, so a set is never served or refreshed past any of its entries'
times. A dropped entry's times do not count.
A set of an expiring source that carries no expiry keeps the stored `exp`
when its entries are unchanged, and is discarded otherwise, because an
owned expiring ID without `exp` is never served (section 6.1). That
fallback is for a vendor that omits an expiry at runtime; the harness fails
a fixture that does.

`replacing_state` with a value that does not serialize is handled like an
oversized one: the positive part of the result is rejected whole, so a new ID
is never stored beside refresh state that does not belong to it.

### 4.6 A complete provider

A fictional provider, `exampleid`, shows the whole surface: a descriptor
built from config, a required and a wanted input, consent strings, a
provider state round trip, a UID-level `ext` and a refresh hint.

```rust
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct ExampleIdConfig {
    pub enabled: bool,
    #[validate(url)]
    pub endpoint: String,
    pub partner_id: u32,
    pub gvl_id: u16,
}

impl IntegrationConfig for ExampleIdConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for ExampleIdConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(EXAMPLEID_INTEGRATION_ID.parse()?)
            .with_source("exampleid.example.com".parse()?)
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_wants([Input::PageUrl])
                    .with_private_state(),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes([4])
                    .with_tcf_vendor(self.gvl_id),
            )
            .build()
    }
}

pub struct ExampleIdProvider {
    descriptor: ProviderDescriptor,
    partner_id: u32,
}

#[derive(Serialize, Deserialize)]
struct ExampleIdState {
    signature: String,
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    ip: String,
    ua: &'a str,
    page: Option<&'a str>,
    gdpr: bool,
    consent: Option<&'a str>,
    signature: Option<String>,
}

#[derive(Deserialize)]
struct FetchReply {
    uid: Option<String>,
    signature: Option<String>,
    link_type: Option<u8>,
    max_age_sec: Option<u64>,
}

impl ExampleIdProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &ExampleIdConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
        })
    }
}

impl IdentityProvider for ExampleIdProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let body = FetchBody {
            partner: self.partner_id,
            ip: input.client_ip()?.to_string(),
            ua: input.user_agent()?,
            page: input.page_url().ok().map(Url::as_str),
            gdpr: consent.gdpr_applies(),
            consent: consent.raw_tc_string(),
            signature: input.state::<ExampleIdState>()?.map(|state| state.signature),
        };
        ProviderRequest::post_json(input.endpoint()?, &body).map(Some)
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let Some(uid) = reply.uid else {
            return Ok(Resolution::no_match());
        };

        let mut uid = ResolvedUid::new(uid).with_atype(1);
        if let Some(link_type) = reply.link_type {
            uid = uid.with_ext(json!({ "linkType": link_type }));
        }
        let mut eid = ResolvedEid::new(input.primary_source(), uid);
        if let Some(max_age) = reply.max_age_sec {
            eid = eid.with_refresh_in(Duration::from_secs(max_age));
        }

        let resolution = Resolution::found([eid]);
        Ok(match reply.signature {
            Some(signature) => resolution.replacing_state(&ExampleIdState { signature }),
            None => resolution,
        })
    }
}
```

Registration uses the existing builder (`integrations/registry.rs:636`) from
an integration's `register` function, which returns
`Report<TrustedServerError>` (`integrations/lockr.rs:321-323`). error-stack
converts only `C` into `Report<C>` (`error-stack-0.6.0/src/context.rs:101`),
so the builder does that conversion once for every vendor:

```rust
impl IntegrationRegistrationBuilder {
    /// Registers a provider built by a fallible constructor. A failure
    /// becomes `TrustedServerError::Integration`, naming the integration the
    /// builder already holds.
    pub fn try_with_identity_provider<P: IdentityProvider + 'static>(
        self,
        provider: Result<P, Report<IdentityProviderError>>,
    ) -> Result<Self, Report<TrustedServerError>>;
}

Ok(Some(
    IntegrationRegistration::builder(EXAMPLEID_INTEGRATION_ID)
        .try_with_identity_provider(ExampleIdProvider::new(&config))?
        .build(),
))
```

A capture-only provider overrides one method. This fictional one reads
tokens from the API it proxies, and returns `no_match` for any response it
does not recognise:

```rust
impl IdentityProvider for ExampleCaptureProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        Ok(Resolution::found(reply.tokens.into_iter().filter_map(|token| {
            let source = input.source_named(&token.source)?;
            Some(
                ResolvedEid::new(source, ResolvedUid::new(token.value))
                    .with_expires_at(UnixTime::from_secs(token.expires)),
            )
        })))
    }
}
```

Its descriptor declares
`.with_captures(Captures::new().with_path(Method::POST,
"/integrations/examplecapture/api/token"))`
and no `resolves`, so it is never dispatched. In v1 the coordinator uses a
capture's removals and opt-out, and uses its positive IDs only as the sign
that the vendor issued again (section 4.7). The IDs themselves are not
stored. The trait returns them anyway, so a later durable path needs no
trait change (section 15).

### 4.7 The coordinator

`IdentityCoordinator` is built per request from the registered providers and
`[ec.identity]` settings. Its dependencies (settings, registry, KV graph,
rate limiter, runtime services) travel in one `IdentityDeps` struct, because
`handle_auction` and `handle_publisher_request` already take the maximum of
seven arguments (`auction/endpoints.rs:114-122`, `publisher.rs:4433-4441`).

**The eligibility view.** Eligibility reads the KV row the writer read in
phase 1, and nothing else. It never reads `ts-eids-s`. Every removal is
already in KV (section 5.3), so no cookie has to be applied on top.

**Eligibility.** A provider that declares `resolves` is eligible for this
request when all of these hold:

1. The request is a real browser (`device_signals.looks_like_browser()`,
   `adapter-fastly/src/app.rs:410`), the same gate pull sync uses.
2. Global EC consent allows EC and EID persistence (`allows_eid_persistence`,
   as `pull_sync.rs:72-77` does).
3. The descriptor's consent policy is met (section 9.1).
4. At least one claimed source has a kind the provider lists, computed per
   source:
   - **Owned by this provider.** `Initial` when there is no stored record.
     `Refresh` when there is one, valid or expired, and it is due. A stored
     record is due when it is expired, when this provider did not write it,
     when its `refresh_at` has passed, or, if it has none, when
     `acquired_at + refresh_after_sec` has passed or `acquired_at` is
     missing. A browser-seeded value whose UID matches `ps.iv` is never due
     (section 7.2). An expired record is never served (section 7.1), so the
     provider either refreshes it or returns `None`. An expired record kept
     as a refresh anchor (section 5.4) is a stored record, so the kind stays
     `Refresh` while the provider's refresh credentials last.
   - **Unowned.** `Initial` only, and only when KV has no unexpired ID for the
     source. An expired record, or one whose writer is disabled or no longer
     configured, counts as absent. Unowned sources are never refreshed
     (section 7.2).
   - Sources whose kind's needs are missing are left out. If none remain,
     the provider is skipped and the skip is logged at debug.
5. `ps.n` (next eligible time) has passed, and no sticky `ps.o` opt-out is
   set (section 8).
6. The provider's circuit breaker is closed (below).
7. The request is the writer request for a returning EC, or a new
   visitor's writer navigation that runs an auction (below). Stagers run no
   resolving provider, in either mode. Within the writer:
   - Hold-mode providers run before the auction when the navigation runs
     one (matched slots). When it runs none, they run after send instead,
     so a site whose auctions all go through `/auction` still stores their
     answers, and its next `/auction` serves them from KV. Moved after send,
     a hold-mode provider waits `after_send_timeout_ms`, not the hold
     budget, and a miss is an ordinary timeout (section 8).
   - After-send providers run after send, with `after_send_timeout_ms`.
8. The request's backend budget has room. Fastly allows 32 backend
   requests per execution (10 on trial accounts), and after-send calls run
   in the same execution as the origin and the auction. `[ec.identity]
max_backend_requests` (default 32) bounds them together: the adapter's
   HTTP client counts every request it starts
   (`PlatformHttpClient::requests_started`), and a provider call is sent
   only while that count, plus a reserve, stays below the budget. A hold
   call reserves one request for the origin and one per auction provider,
   because both may still be ahead of it; an after-send call reserves
   nothing. A call over budget is not dispatched, changes no `ps` field and
   is never a failure, so it neither backs off the provider nor feeds the
   breaker. An adapter that does not count its requests is not budgeted.
   Pull sync today has no such bound (`ec/pull_sync.rs:184-284`).
9. The per-visitor burst cap is not exceeded. This check comes last, just
   before dispatch, because the Fastly limiter increments on every check under
   the limit (`adapter-fastly/src/rate_limiter.rs:54-66`). The cap is
   `max_calls_per_minute` on the key `identity:<provider>:<visitor>`, where
   `visitor` is an HMAC of the full EC ID with a key derived from
   `ec.passphrase`. It is not `ec_hash`, which everyone behind one IP shares
   (section 3.4). Next to it, a per-IP ceiling,
   `max_calls_per_ip_per_minute` (default 10, `0` off), counts one
   provider's calls from one client address across every visitor behind
   it, on `identity-ip:<provider>:<address>`, where `address` is an HMAC of
   the request's client IP with the same derived key. Minting ECs has no
   limiter, so without it a script that alternates a cookieless and a
   cookied navigation would fire every eligible provider once per minted
   EC. The ceiling is generous because carrier NAT puts many visitors
   behind one address, and a request without a client IP skips it. A due
   provider either cap stops is not dispatched and gets
   `n = max(n, now + 60 s)`, one limiter window, and never a failure
   (section 8).

Pacing per visitor comes from `ps.n` and the refresh rules in step 4, not
from the limiter. The limiter only caps bursts, because its windows are 60
seconds long. It also keeps two overlapping writer navigations from both
dispatching one provider. Every call runs on a writer, so every outcome,
`no_match` and failure backoff included, is stored in `ps` (section 8).

**Pre-disclosure revalidation.** Before any byte goes to a provider for a
returning EC, the writer must have read the live KV row and found it live
and consenting, and computed eligibility steps 4 and 5 from it. That read
is the phase 1 read for a provider dispatched before send, and the phase 4
re-read for one dispatched after send (section 5.2). A new visitor's first
navigation calls only providers that never see the EC ID, before its row
exists (below).

**New visitors.** Decided after the round 14 review (section 15, question
6). When a hold-mode provider that does not read the EC ID
(`Resolves::gets_ec_id`) is configured, a publisher navigation from a new
visitor mints the EC without writing (`EcContext::mint_deferred`). If the
navigation runs an auction, the hold dispatch runs on the row the create
will write, skipping every provider that reads the EC ID, and its answers,
`ps.n` and `ps.f` go into that row. The create then runs, before the
phase 1 preload and the auction, so the create stays the page load's one
write and no ID reaches a bidder before its row exists. A collision fails
the create rather than re-minting, because the auction is about to send the
minted ID. A navigation that never reaches that point (no auction, an
origin error) is created by finalize, before `ts-ec` is set. Without such a
provider the create runs before routing, as before. Only the Fastly adapter
defers; the others create before routing.

**Dispatch.** For each eligible provider, the coordinator builds a
`ResolveInput` holding only the declared inputs and calls `request`. It then
checks the request before sending it:

- The scheme is `https`.
- The host is one of the descriptor's `resolves` hosts.
- No `Cookie` header is present, and every header name and value is valid.

A request that fails a check is dropped and logged as `Config`. The
coordinator sends it through `RuntimeServices::http_client().send_async`, up
to `max_concurrency` at once, with a backend first-byte timeout of the
provider's timeout (`after_send_timeout_ms` after send; before the auction,
`min(timeout_ms, remaining budget)`,
rounded to buckets as `auction/orchestrator.rs:453-473` does). It does not
follow redirects. The dispatch is async. Before the auction it is awaited
inside the request handler, which already runs on an executor, so it never
blocks on a nested executor there; after send, outside any executor, phase 4
blocks on it once. It reads at most `max_response_bytes`, checked against
`Content-Length` first (`pull_sync.rs:412`) and against the buffered body
(`pull_sync.rs:472-474`). It undoes `Content-Encoding` with the same cap
applied to the decoded size, then passes the `ProviderResponse` to
`response`.

**Hold-mode timing.** Hold-mode providers share one wall-clock budget,
`hold_budget_ms` (default 100 ms), measured from the first dispatch. Each
waits for `min(timeout_ms, remaining budget)`. The auction starts when all
have answered or the budget is spent.

Why 100 ms:

- It is a fifth of the navigation auction's recommended 500 ms timeout
  (`auction_timeout_ms`, `trusted-server.example.toml:336-340`). A 300 ms
  budget would take 60% of it.
- Browser-side guidance runs higher, from 100 to 500 ms. Prebid's
  `auctionDelay` defaults to 500 ms (EID fidelity spec D4), and data vendors
  recommend at least 100 or 250 ms (`adlooxRtdProvider.md:86`,
  `51DegreesRtdProvider.md:101`). Those figures include the visitor's device
  connecting to the vendor. An edge call skips that: it goes from the POP,
  often over a connection Fastly already holds open.
- Missing the budget costs that one auction the ID. A call cut off by the
  budget is not a failure: it does not count toward `ps.f` or the breaker.
  It sets `ps.n = now + hold_retry_sec` (default 300 seconds), so the next
  navigation does not wait on the same slow provider straight away.

The coordinator logs each provider's response time. An operator raises the
budget only when a provider's measured p95 latency at the edge exceeds it.

On the navigation, holding has a latency cost, and it depends on the
order of section 3.4:

- **Origin first** (Fastly's default for a returning EC in Inline mode).
  The origin request is already in flight, so the first byte slips by
  `max(0, preload + hold + dispatch - origin TTFB)`. A fast origin pays up
  to the whole hold; a slow one pays nothing.
- **Auction first** (a template-cache key, ESI, or no preload). The origin
  fetch waits for dispatch, so the first byte slips by the whole hold.

`hold_budget_ms` is a soft bound. The Fastly adapter cannot enforce a total
deadline (section 3.4). Each hold-mode call gets a first-byte timeout of
`min(hold_timeout_ms, remaining budget)`, and the coordinator waits for
every dispatched call until the platform settles it
(`ec/identity/coordinator.rs:1140-1162`). The first-byte timer starts only
once the connection is up, so a hold-mode call also sets its connect
timeout to the same call timeout, capped at the adapters' 1-second
default (`PlatformBackendSpec::connect_timeout`). On Fastly a call stalled
in connection setup then holds the navigation for at most twice its call
timeout: the worst case is about `2 × hold_budget_ms`, plus a response that
trickles bytes inside the between-bytes timer. A spec that sets no connect
timeout keeps today's 1 second and today's backend name, so origin,
auction and after-send calls are unchanged. The other adapters have no
per-backend connect timer and ignore the field.

How often a provider is due depends on its refresh interval (step 4). The
default `refresh_after_sec` of one day is a product decision that departs
from vendor guidance: UID2 recommends refreshing hourly, LiveRamp every
30 minutes of active navigation, and ID5 every 2 hours. Following that
guidance means one KV write per active visitor every 30 to 120 minutes for
each owned expiring source, more navigations that hold, and more HTML
responses made private by `Set-Cookie`. Operators who follow a vendor's
guidance set `refresh_after_sec` or the provider's refresh hint to it and
accept those costs.

**Results.** A hold-mode call either answers within its budget or is cut
off; no call outlives the router (section 3.4). An answer feeds that
navigation's auction and joins the writer's mutation. No answer goes into
a cookie. After-send results exist only in phase 4, after headers are
sent, and are dispatched and committed in the same phase 4 flush (section
5.2).

**Circuit breaker.** The rate-limiter trait (`ec/rate_limiter.rs:16-47`)
gains `record(key)` and `count(key)`, backed by Fastly's counter `increment`
and `lookup_count`. Only transport failures, timeouts, 5xx and 429 responses
are recorded, under `identity-fail:<provider>`. While `count` is at or above
`breaker_failures_per_minute`, the provider is skipped for every visitor.
Input-caused 4xx responses and `Malformed` back off only that visitor, so no
visitor can open the breaker for everyone by seeding bad values. Fastly
counters are kept per POP, so the breaker is too.

**Passive capture.** `IntegrationRegistry::handle_proxy`
(`integrations/registry.rs:1031-1060`) holds the `EcContext`.
`IntegrationProxy::handle` itself has no EC context
(`integrations/registry.rs:319-324`).

Capture is revoke-only in v1. Every proxied API call is a stager (section
5.1), and a stager persists no positive ID, so a capture result does three
things only:

- Its removals (`invalidate`, `also_invalidate`) and opt-out (`opted_out`)
  are written to KV.
- A positive result clears the provider's opt-out, under the rules below.
- Its positive IDs are counted in `capture_unpersisted`, by provider ID and
  source, with no value, so the operator can measure what a durable path
  would add (section 15). They are not stored.

For each provider with a capture rule (`with_path` or `with_path_prefix`)
matching the request:

- **After `handle`,** on 2xx responses only, `handle_proxy` buffers the
  response body up to `max_response_bytes` (a larger body is passed through
  uncaptured), decodes a copy under the same cap, builds a `CaptureInput`
  and calls `capture`. It re-wraps the original bytes with
  `EdgeBody::from(bytes)`, so the browser receives the response unchanged.
  The request body is never read.
- **Removals and opt-outs apply whatever the consent,** because a
  revocation arrives after consent is denied. The capture removal write
  stores them on this request (section 5.3).
- **A clear** needs a positive result under the request's consent:
  eligibility steps 1 to 3 pass, and the provider has a sticky `ps.o` that
  only a capture can clear (a capture-only provider without a GVL ID,
  section 8). The same capture removal write stores it as `ps.oc`, set to
  the `at` of the opt-out this request read (its watermark, section 8).
- **The real-browser gate applies to clears only.** Removals, opt-outs and
  global revokes apply whatever the client, as consent withdrawals do: the
  bot gate keeps the write path for revocations so tombstones stay
  authoritative for privacy-extension-heavy clients
  (`adapter-fastly/src/app.rs:595-605`), and `looks_like_browser()` fails
  whenever the JA4 class or the UA platform is missing
  (`ec/device.rs:84-86`), for example under a UA-spoofing extension.
  Dropping their revocations would leave their IDs in KV, served again on
  their next request that passes the check (G12). A script replaying a
  copied `ts-ec` cookie can make TS forget IDs this way, but it can already
  tombstone the row through the withdrawal path with a denying consent
  cookie, and it never discloses or re-enables an ID (section 9.5). A clear
  re-enables IDs, so a client that fails `looks_like_browser()` makes no
  clear. The adapter gives integration proxy dispatch the identity graph
  for every client for this; EC generation on that route still requires a
  real browser.

Capture never touches `ps.n`, `ps.f` or the breaker: a proxied call that
held nothing is not a provider failure. Capture never changes provider
state: a state change in its result is ignored with a warning, and the
harness's conformance check fails a `capture` that returns one (section
13). This holds whether or not the provider declares
`with_private_state()`.

Lockr's API responses are already buffered on Fastly: a non-streaming send
reads the body whole, up to 10 MiB (`adapter-fastly/src/platform.rs:413-429`).
The Lockr proxy forwards the browser's `Accept-Encoding`
(`integrations/lockr.rs:259-265`), so its responses can be compressed. The
coordinator decodes them, as above.

## 5. One KV write per page load

### 5.1 Writer requests and stagers

A request is the **writer request** when all of these hold. The decision is
made once pre-route filters have allowed dispatch, because a challenged or
blocked request must not persist anything (`ec/mod.rs:91-96`). It sits where
routing already decides EC generation (`adapter-fastly/src/app.rs:846-851`,
`integrations/registry.rs:1048`) and is stored on `EcContext`:

- It is an organic route: the publisher fallback, or an integration proxy
  route.
- `Sec-Fetch-Dest` is `document`. The `Accept: text/html` fallback of
  `is_navigation_request` does not count, because `fetch()` can send it
  (`http_util.rs:105-117`).
- It is not a prefetch or prerender. `is_prefetch_request` moves to
  `http_util` as a public function so the adapter can call it.
- The request is a real browser (`adapter-fastly/src/app.rs:410`).
  Integration-proxy EC generation gains the same explicit check: today it
  is gated only by a missing KV graph (`integrations/registry.rs:1048-1049`,
  `adapter-fastly/src/app.rs:458`).

An integration-proxy writer never rotates an EC, as today
(`adapter-fastly/src/app.rs:885-890`).

The adapter captures the response status before `send_edgezero_response`
consumes the response, and passes it to phase 4 (`main.rs:512-517`, `:618`).
A returning EC's writer flushes its whole mutation only when that status is
2xx or 304. For any other status it dispatches no after-send provider and
flushes everything in its mutation except the positive part of the hold-mode
calls it already made before the origin status was known
(`publisher.rs:4887-4894`, `:5151-5175`): their removals and opt-outs, and
the `ps.n` and `ps.f` of every outcome other than `found`, so a `no_match`,
a failure or a cut-off call still backs off. Their positive answers, with
their state and scheduling, are resolved again by a later writer. Browser
ingest is dropped and re-derived at the next navigation. A redirect is
followed by another
navigation, which recomputes everything from KV and the same cookies.

A new visitor's create (section 5.2, phase 1, before routing) is written
whatever the status, as today, because the response sets the EC cookie.

Every other request is a **stager**: `/auction`, `/_ts/page-bids` (except
the SPA writer below), integration proxy calls that are not navigations (including Lockr's API
proxy), prefetches, iframes, clients that send no `Sec-Fetch-Dest`, and every
other route. Stagers read cookies and KV, run no resolving provider, and
write KV only for a captured revocation (section 5.3).

`/_ts/api/v1/identify` stops writing too. The once-per-EC `cluster_size`
evaluation (`ec/identify.rs:107`, `ec/kv.rs:1484`) moves into the writer's
mutation. `/identify` uses a stored `cluster_size` when present and computes
one without writing when not.

**SPA views.** A single-page app changes views with JavaScript, and TS's
script reports each change to `/_ts/page-bids` (`publisher.rs:6890-6894`).
An SPA view is part of the page load that loaded the document, so
`/_ts/page-bids` is a stager. Today it ingests `ts-eids` into KV
(`publisher.rs:6925`).

**The SPA writer.** Decided after the round 14 review (section 15,
question 1). A visitor who stays in an SPA would otherwise wait for their
next full navigation for browser ingest and provider refreshes. So a
`/_ts/page-bids` call is the writer when it comes from a real browser, sends
`Sec-Fetch-Site: same-origin` (the `X-TSJS-Page-Bids` fallback is not
enough), and `ec.identity.spa_writer_interval_sec` (default 300, 0 turns it
off) is above zero. Its phase 4 then:

- writes only when its auction read the row in phase 1, so it adds no row
  read, and only when the row's `wa` is older than the interval. `wa` is
  the time of the row's last writer flush, set by every writer write
- runs browser ingest and after-send providers, never a hold-mode provider,
  and never holds the response
- reads the pending key and writes once, as any writer

So an SPA session writes at most once per interval, and a row whose last
write predates `wa` (none recorded) is due at once.

**Exception: consent withdrawal.** A withdrawal writes its tombstone on
whichever request carries it, before the response is sent, as today. That
includes stagers and clients the bot gate does not treat as real browsers
(`adapter-fastly/src/app.rs:446-452`). Revocation cannot wait for a
navigation that may never come. The exception covers the whole of today's
withdrawal path: a CAS tombstone with up to five attempts, then its
completion-marker write, for each withdrawn EC ID (one or two;
`ec/kv.rs:1349-1430`, `:1115-1124`; `finalize.rs:509-525`). New in this
spec: when those attempts fail before send, the path runs once more after
send, by which time Fastly's per-key limit has usually passed. The same
exception covers a captured opt-out from a provider with
`revoke_scope = "global"` (section 8), from any client: it runs this
path on the request that carries it. It and the two removal writes (sections
5.2, phase 4, and 5.3) are the only page-load exceptions to G4. The
tombstone is not
part of any `IdentityMutation`. A writer whose phase 4 re-read finds the
tombstone drops its mutation (section 5.4, step 1).

### 5.2 Phases of the writer request

**Phase 1, at the routing decision. No KV write.**

- Read `ts-ec`, consent, `ts-eids` and sharedId. `ts-eids-s` is never read.
- Decide whether this is the writer request (section 5.1).
- Capture the user agent, DNT and page URL into `EcContext` (section 4.4).
- For a returning EC, read the KV snapshot. Every returning writer
  navigation reads it once (section 6.3). A missing row starts today's
  orphan recovery: confirm the absence, then rotate before send
  (`ec/finalize.rs:371-400`). The rotated EC's create carries this
  request's browser ingest, as today (`ec/finalize.rs:323`). It is the page
  load's one write (section 5.5): a request that rotates drops the rest of
  its mutation and skips phase 4. If the confirm read finds the row after
  all, no rotation happens and that read is the phase 1 snapshot (today's
  code writes at once there, `ec/finalize.rs:383-386`; here the change joins
  the mutation).
- For a new visitor, `generate_if_needed` creates the row before routing, as
  today (`ec/mod.rs:407-455`), so no unconfirmed ID is ever used. Today that
  create is an empty row and browser ingest is a second write in finalize
  (`ec/mod.rs:412`, `ec/finalize.rs:144`). Here the create also carries
  browser ingest from `ts-eids` and sharedId, which are read before routing,
  so it is the page load's one write.

**Phase 2, routing.**

- The coordinator computes eligible providers from the phase 1 snapshot
  (section 4.7).
- If the navigation runs an auction, due hold-mode providers are dispatched
  before it and awaited within the hold budget. Their answers feed the
  auction. If it runs none, due hold-mode providers join the after-send
  providers (section 4.7, step 7).
- A provider due on the phase 1 snapshot in after-send mode is due for
  dispatch in phase 4, which rebuilds its request from the live row. There
  is no dry run: phase 4 calls `request` once. A `None` then sets `ps.n`
  (section 8).

**Phase 3, finalize, before send.**

- **New visitor.** Nothing to write: the row was created before routing
  (phase 1), or after the hold dispatch (section 4.7, new visitors), or here
  when the navigation never reached that create. `ts-ec` is set because the
  create was `Written`, as today (`ec/finalize.rs:145-149`). Phase 4 does
  nothing for a new visitor.
- **Returning EC.**
  - Browser ingest from `ts-eids` and sharedId adds `FillMissing` changes to
    the mutation, instead of calling `sync_eid_cookie_updates`
    (`ec/finalize.rs:183`). It covers owned sources too, which seeds them
    for their owner (section 7.2).
  - Hold-mode answers join the mutation. A capture on an integration-proxy
    writer navigation goes through the capture removal write, as on a stager
    (section 4.7), and never joins the mutation.
- **Consent withdrawal** runs today's synchronous path (section 5.1).
- **Cookies.** This spec sets no cookie on the writer's response. A
  returning visitor's HTML gets no new `Set-Cookie` from it and stays
  cacheable.

**Phase 4, after send. Returning ECs only. The only write.**

1. If the mutation is empty and no after-send provider is due, stop. No
   read, no write.
2. If phase 1 read the row successfully and no after-send provider is due,
   do not read again (a failed phase 1 read counts as no read, and phase 4
   re-reads): apply the mutation to the phase 1 snapshot and, if anything
   changes, write with that generation. A concurrent change since then,
   such as a capture removal write by another request of the page load
   (section 5.3), fails the CAS, as any conflict does. Otherwise re-read
   the live row. Either way, the snapshot or the live row decides:
   - A tombstone, or `consent.ok = false`: drop the mutation and stop.
   - Missing: drop the mutation and stop. The next writer navigation's
     phase 1 read finds the orphan (section 6.3). This covers an
     integration-proxy writer, which never rotates.
   - Failed: drop the mutation and stop.
3. Recompute eligibility steps 4 and 5 from the live row, build each due
   provider's request, dispatch the after-send providers and turn their
   results into changes. This happens after the re-read, so no EC ID is
   disclosed before consent is confirmed (`ec/pull_sync.rs:100-110` does the
   same today).
4. Apply the mutation to the live entry (section 5.4). If anything changed,
   write it once with `IfGenerationMatch`. The after-send results commit in
   this write; nothing marks them as pending in between.
5. If the write fails, on a precondition failure, Fastly's per-key rate
   limit or any other error, log. The flush is not retried as a whole.
   Fastly limits writes to one key to about one per second, and the limit
   can be stricter, so an immediate retry after a conflict would usually
   fail too. When the mutation holds removals, opt-outs or clears, they are
   then written alone, with the CAS loop of the capture removal write
   (section 5.3): a re-read, the removal rules of section 5.4 and up to five
   attempts. Phase 4 runs after send, so this write first waits one per-key
   window (1 second), because the usual cause of the failed flush is
   another tab's write within the last second. A per-key limit on one of its
   attempts waits another window and counts as an attempt, like a
   generation mismatch. When those fail too, the request's own removals go
   to the pending key (section 5.3), and only when that append fails are
   they lost and counted in `removal_lost`. Nothing else backs the flush,
   so:
   - browser ingest is re-derived from `ts-eids` by the next navigation
   - every positive provider result is lost: hold-mode answers and
     after-send results, with their state changes and `ps.n`/`ps.f`. Each
     provider is still due, and the next writer calls it again, paced by the
     burst cap. Their removals survive through the retry above, unless it
     fails too
   - a stateful provider is called again with the state KV still holds. If
     the vendor has rotated that state (a used UID2 refresh token, for
     example), the call fails, the stored ID ages out, and the browser or a
     later initial call re-seeds it

**Cost per page load.** Writes are at most one, plus the removal-only retry
when a flush that holds removals fails, and a pending-key append when that
fails too. Reads, compared with today, for a writer navigation; each also
reads the pending key once in phase 4 (section 5.3, The pending key), which
the table leaves out:

| Navigation                        | Today                  | This design                                                         |
| --------------------------------- | ---------------------- | ------------------------------------------------------------------- |
| Runs an auction                   | 1 (preload)            | 1 (phase 1). Phase 4 reuses it unless an after-send provider is due |
| No auction, no provider due       | 0 or 1                 | 1 (phase 1). Phase 4 reuses it                                      |
| An after-send provider dispatches | 2 (preload, pull sync) | 2: phase 1 and the phase 4 re-read                                  |

Every returning writer navigation reads the row once in phase 1, and the
pending key once in phase 4. The row read finds
orphans, answers eligibility and filters browser candidates, so no cookie
copy of the row is needed. Each `/auction` and `/_ts/page-bids` keeps its
one existing read. Orphan recovery adds the reads it adds today, in the rare
case it runs.

### 5.3 Stagers

A stager runs no resolving provider and never persists a positive ID. It
reads KV only where its route already does: `/auction` reads the row it
reads today (`auction/endpoints.rs:307-311`), and `/_ts/page-bids` reads it
only when its auction runs (`publisher.rs:7077-7090`). Its auction serves
the server set from that read (section 7.1). It no longer runs today's
ingest write in the returning-visitor branch of `ec_finalize_response`
(`ec/finalize.rs:110-112`).

Earlier revisions let stagers run stateless hold-mode providers in memory.
With no `ps.n` or `ps.f` stored on a stager, a provider that answered
`no_match` or a 4xx was called again once a minute on every `/auction`, for
the whole of an SPA session. Revision 15 removes that.

**The capture removal write.** A captured revocation, from any client
(section 4.7), is written to KV on the proxied request that carries it. It is one of the page-load exceptions to G4 (section 5.1). The write:

- reads the row and applies only the capture's removals (with the `ps.iv` or `ps.ov` hashes they derive), its opt-out and its clear, under the rules of
  section 5.4
- writes with a CAS and the withdrawal path's five attempts, re-reading and
  trying again when the generation no longer matches. Before send, any
  other failure, Fastly's per-key limit included, ends the attempt at once
- runs before the proxied response is sent and, when that attempt fails,
  once more after send. A small proxied JSON response is sent within
  milliseconds, still inside the per-key window, so the after-send attempt
  first waits one window (1 second), and a per-key limit on any of its
  attempts waits another window and is retried as a generation mismatch
  is. Latency is not at stake after send. The adapter reports the limit
  apart from other store errors (`EcKvRateLimited`, from Fastly's
  `TooManyRequests`), and any other error still ends the attempt
- writes nothing when the read shows a tombstone, a missing row or the
  removal already applied
- when both attempts fail, appends the removal to the pending key below and
  increments `removal_deferred`. Only when that append fails too does it log
  at error with the provider ID and source only, and increment
  `removal_lost` (section 2.3)

It runs whatever the request's consent, including when consent is unusable
but not withdrawn, because a revocation can arrive then. A writer
navigation of the same page load that flushes with its phase 1 generation
after this write fails its CAS and is not retried (section 5.2, phase 4);
its browser ingest is re-derived at the next navigation.

**The pending key.** Decided after the round 14 review (section 15,
questions 2 and 4). `pending:<ec_id>` holds what a request could not write
to the row, for the next writer to merge:

- **What goes in.** A removal, opt-out or clear whose row write and
  after-send retry both failed, from a capture or from a writer's flush.
  And a capture's positive change from a request that is not the writer,
  when it passes the consent steps (`passes_consent_steps`), the provider
  is not opted out, and its entry set differs from what the capture's view
  read. A positive change keeps the revision the capture read as its
  `expected`, so a merge never overwrites a record that changed since. When
  the capturing request is itself the writer, its positive changes join its
  own mutation and skip the pending key.
- **The append.** Read the key, drop items older than its one-day TTL,
  append one item `{g, at, ch, ps}` and write with a CAS (`Add` when the key
  is absent), up to five attempts. `g` is the item's generation:
  `max(last g + 1, now * 1000)`, so a key that expired and comes back never
  reuses a generation. A key holds at most 16 items; an append to a full
  key fails. Positive capture changes are appended after send; a failed
  append drops them with a warning, as before this decision.
- **The merge.** Every returning writer reads the pending key in phase 4,
  before deciding it has nothing to do. Items with `g` above the row's `pg`
  merge before the request's own mutation, under the rules of section 5.4:
  a removal applies as in the removal write, and a positive change is
  skipped when its UIDs hash into `ps.iv` or `ps.ov` or its owner is opted
  out. The write stores the newest merged `g` as `pg`. When the write
  fails, the items stay, and only the request's own removals are appended.
- **What it never does.** The key is never deleted; its TTL ends it.
  Auctions and `/identify` never read it, so a deferred positive ID is not
  served until a writer has merged it.

`/auction` and `/_ts/page-bids` return the mirror payload of section 6.2 in
their JSON response, from the KV row they read. The shim writes
`ts-eids-s` from it. These responses are already private, so the mirror
costs no HTML cacheability.

**`ts-eids` holds only registered sources, merged per source.** Today
`/auction` ingests registered sources from its body, which carries more than
the 3,072-byte `ts-eids` cookie. Under this design only the writer ingests,
and only from `ts-eids`. So the shim writes only registered sources into
`ts-eids`; it learns their domains from the configuration TS injects with
its script, and writes no cookie when none is injected. Each `trustedServer`
request's registered entries replace the cookie's entries for the same
source, and the cookie keeps the sources the request lacks, so an auction
that misses an ID module does not remove its ID. A request with no EIDs
leaves the cookie as it is; a TCF block still clears it. The EID fidelity
trim (§4.3) then runs on the merged set. Every registered source today's
`/auction` ingest writes then reaches the next writer's ingest, unless the
registered sources alone exceed the trim (G10).

**Concurrent stagers.** Stagers set no server cookie, so concurrent
responses cannot overwrite each other's state. Each can return a mirror
payload; the last one the shim writes wins, and all of them come from KV.

### 5.4 `IdentityMutation`

```rust
pub(crate) struct IdentityMutation {
    partner_ids: Vec<PartnerIdChange>,
    provider_results: Vec<ProviderResult>,
    provider_state: BTreeMap<IdentityProviderId, ProviderStateChange>,
    cluster_size: Option<u32>,
}

/// The positive part of one stateful provider's result, applied whole or not
/// at all. Its removals and opt-out are not part of it: they go into
/// `partner_ids` and `provider_state` and apply on their own.
pub(crate) struct ProviderResult {
    provider: IdentityProviderId,
    /// `OwnerWrite` and `FillMissing` changes for the sources it returned.
    ids: Vec<PartnerIdChange>,
    /// The state replacement or clear, and the new `n` and `f`.
    state: ProviderStateChange,
    /// Hash of the `ps.st` the call read. `None`: it read no state.
    expected_state: Option<StateRevision>,
}

pub(crate) enum PartnerIdChange {
    /// From browser ingest or an unowned provider.
    /// Applies only if the source is absent or its record is expired.
    FillMissing { source: SourceDomain, record: StoredRecord },
    /// From the owning provider. Applies if the live record's revision equals
    /// `expected`, or if `expected` is `None` and the source has no valid ID
    /// that this provider wrote. A browser-seeded value never blocks the
    /// owner.
    OwnerWrite { source: SourceDomain, record: StoredRecord, expected: Option<Revision> },
    /// From `Resolution::invalidate` or `also_invalidate`. Applies only if
    /// the revision matches. Applying it appends the hash of every UID of the
    /// record it removes to `provider`'s `ps.iv` (section 7.2). `provider` is
    /// the provider that invalidated, which for an unowned source is not an
    /// owner: its `ps.iv` still filters the browser's candidates (section 7.1).
    OwnerRemove { source: SourceDomain, provider: IdentityProviderId, expected: Revision },
    /// From `Resolution::opted_out`. Removes the source's record if this
    /// provider wrote it, at any revision. For a capture-only provider it
    /// also removes a record no provider wrote (browser ingest, schema
    /// version 1 rows), because such a provider never writes one itself
    /// (section 8).
    WriterRemove { source: SourceDomain, writer: IdentityProviderId },
}

/// A `StoredRecord` is a source's whole record: its entry set (section 6.1),
/// `w`, the timing fields and `rv`. Every change below writes or removes a
/// source's set whole. No change edits one entry or one UID, so one `rv`
/// per source covers every entry in it.
///
/// A record's opaque revision: its KV `rv`, 64 random bits from the
/// platform generator, drawn fresh on every write of the record, including a
/// refresh that keeps the UID. Neither the UID nor `acquired_at` can serve:
/// two refreshes of an unchanged UID in one second share both. A record
/// without `rv`, which only a schema version 1 row holds, has the revision
/// `Unset` until its next write gives it one.
pub(crate) enum Revision {
    Unset,
    Set(RecordRevision),
}
```

A new visitor's create is not a mutation: `generate_if_needed` builds the
seed row before routing (section 5.2).

Applying the mutation to a live entry, in order:

1. If the live entry is a tombstone, drop everything.
2. Apply removals first: every `OwnerRemove` and `WriterRemove`. Applying a
   new `ps.o` for a provider also removes every live record that provider
   wrote, owned or unowned, at any revision, and for a capture-only
   provider every record of its sources that no provider wrote. Removals
   are never skipped. Every invalidation that applies appends to the invalidating
   provider's `ps.iv`, and every opt-out removal appends to the provider's `ps.ov`, the
   hash of every UID of the live record it removes, so the hashes always
   come from the record KV held. One that no longer
   applies adds none: the record it named is gone or was replaced.
   A clear carries a watermark: the `at` of the opt-out the clearing
   request read (section 8). Apply clears first: a clear applies when the
   live `ps.o.at` is at or before its watermark and the live `ps.oc` is
   before it, and stores the watermark as `ps.oc`, so the next step sees the
   provider as not opted out. A provider is opted out while `ps.oc` is
   absent or before `ps.o.at`. Then apply opt-outs: one applies when its
   `at` is after the live `ps.o.at` and after the live `ps.oc`. An opt-out
   the clearing request never saw is newer than the watermark, so it stays,
   and an opt-out in the same mutation as a clear still wins. A clear that
   applies also empties the provider's `ps.ov`, so the IDs its opt-out
   removed can come back through the browser path. `ps.iv` is never
   emptied: an invalidation is about the ID, not about consent. A clear
   whose watermark is before the live opt-out, or with no live opt-out to
   clear, is dropped and writes nothing.
3. Apply the positive changes by their own rules. Every change written by
   a provider the live `ps.o` marks opted out is skipped, `OwnerWrite` and
   `FillMissing` alike. A `FillMissing` any of whose UIDs matches the live
   `ps.iv` or `ps.ov` of a provider that claims the source is skipped, owned
   or
   unowned. A browser-ingest `FillMissing` for a source of an opted-out
   capture-only provider is skipped too. For an owned source, every
   positive change is skipped while the owner is opted out, whoever wrote
   it, and an `OwnerWrite` or `FillMissing` any of whose UIDs matches the
   owner's `ps.iv` is skipped, the owner's own write included. These read
   the entry after step 2, so an invalidation or opt-out in the same
   mutation already counts: a browser refill of a UID the same writer just
   invalidated, or of a source whose owner just opted out, never lands.
   Changes that no longer apply are dropped silently: a newer write won.
   Each `ProviderResult` applies atomically. Its IDs, its state change and
   its `n` and `f` apply together, and only when every ID change in it
   would apply on its own and the live `ps.st` still hashes to
   `expected_state`. Otherwise the whole group is dropped. A writer whose
   phase 4 re-read finds that another writer committed a newer ID or state
   since its call read the row therefore stores neither its ID nor its
   state, and no row pairs one call's ID with another call's state.
4. Apply `provider_state` changes (`n`, `f`, `o`, `iv`, `ov`, `st`) and
   `cluster_size`. Truncate each `ps.iv` and `ps.ov` from the front to
   `MAX_INVALIDATED_UIDS` (section 6.1), so step 5 never fails on them. For
   a provider no longer configured, drop every `ps` field except `o`, `oc`
   and `ov`, so a rollback and a roll-forward never forget an opt-out
   (section
   14). Such an entry still counts toward the 32-provider cap, and is the
   first dropped when the cap is exceeded.
   Drop expired records, except **refresh anchors**, an expired record of an
   owned source whose owner is configured and either:
   - still holds `ps.st`, and the record's `ru` has not passed (a stateful
     owner, such as UID2), or
   - wrote the record and declares `with_refresh_grace`, and `exp` plus the
     grace has not passed (a stateless owner, such as LiveRamp, whose
     refresh accepts an expired envelope).

   An anchor is never served (section 7.1), but it keeps the owner's kind
   `Refresh` (section 4.7), so an unrelated mutation, a backoff or an open
   breaker cannot strand refresh credentials that are still valid. A
   stateful anchor is dropped when its `ru` passes or the owner's state is
   cleared (an `InvalidState` included) or missing; a stateless one when its
   grace passes. Either is dropped when the owner is no longer configured. A
   record with neither is dropped at expiry, as before. After a drop the
   owner sees no record and selects `Initial`.

5. Validate the result with `KvEntry::validate`. If it fails, keep only the
   removals and opt-outs and validate again, so a full row can always
   shrink. If that also fails, drop the mutation and log. An invalid row is
   never written.
6. If nothing changed, skip the write.

The capture removal write (section 5.3) applies a mutation that holds only
removals, an opt-out and a clear, through steps 1, 2, 4 (the `o`, `oc`, `iv` and `ov` fields only), 5 and 6.

Because the rules are checked against the live entry, a later writer can
re-apply re-derived changes without losing a concurrent tab's newer data.

### 5.5 Orphans, collisions and deletion

Creating a new visitor's row before its cookie goes out keeps today's three
safety properties, which a deferred create would lose:

- **Withdrawals stick.** A withdrawal can only arrive with a cookie, and the
  cookie is set only after the row exists, so the tombstone always has a row
  to land on.
- **Collisions are caught.** An `AlreadyExists` on the first create re-mints
  the ID before any cookie is set, so no browser adopts another visitor's
  row.
- **Orphans are found.** A row that goes missing later is found by the next
  writer navigation's phase 1 read, which every returning writer navigation
  makes, and recovered by today's rotation, before send (section 5.2,
  phase 1). A stager that sees the row missing stores nothing and tells the
  shim to clear `ts-eids-s`.

**Deletion.** The IAB data-deletion framework, deferred today, would
hard-delete rows (`ec/kv.rs:1551-1563`). When that framework lands, it writes
a tombstone with `TOMBSTONE_TTL` (24 hours, `ec/kv.rs:38-39`) instead, using
its own unconditional writer and today's completion marker. That replaces
one delete with one write, the TTL removes the tombstone with no further
write, and every TS request that reads the row in those 24 hours treats the
identity as withdrawn. A browser that visits within those 24 hours reads
the tombstone at its next writer navigation or `/auction`, and the latter
tells the shim to clear `ts-eids-s`. One that returns later finds the row
missing and rotates. The rotation carries no positive ID, because the
server keeps none outside KV, so a deletion is never undone through a
cookie.

### 5.6 Rules that keep removals winning

- **Removals reach KV on the request that carries them.** A provider's
  removals, opt-outs and clears are written by the writer's own flush, or
  by the capture removal write for a proxied call (section 5.3). They
  apply whatever the consent and whether or not their provider is still
  enabled, and no cookie or config change can strand one. No client gate
  applies to them (section 4.7).
- **A missing row carries no positive ID.** Any KV read that finds the row
  missing (outside a recovery in the same request) treats it like a
  tombstone for the server set: an `/auction` or `/_ts/page-bids` response
  tells the shim to clear `ts-eids-s`. Nothing carries `ps` across an
  orphan rotation, so after one a UID a provider rejected can be re-seeded
  once, and an opted-out provider can be called again until it reports the
  opt-out again.
- **Withdrawn consent never refills the mirror.** A stager whose decoded
  consent denies EID persistence returns a clear for `ts-eids-s`, never a
  value, so a concurrent `/auction` that started before a withdrawal cannot
  bring IDs back.
- **Cluster size rides on a write.** `/_ts/api/v1/identify` authenticates
  the partner with a bearer token and reads the EC from the `ts-ec` cookie,
  with credentialed CORS for the publisher's host and its subdomains
  (`ec/identify.rs:32-75`, `:226-263`). It stops writing (section 5.1).
  When it reads a row without `cluster_size`, it computes the value for its
  response and does not store it. A writer whose phase 1 or phase 4 read
  finds a live row without `cluster_size` adds the evaluation to its
  mutation, run in phase 4 after send, so its KV prefix list
  (`ec/kv.rs:1460-1469`) never delays a page. That is at most one write per
  EC, once (`ec/kv.rs:1497-1529`), and the write is the page load's one
  write. The list cost is new: today the prefix list runs only when a
  partner calls `/identify` for an EC, and under this design it runs once
  for every EC that reaches a writer navigation, including ECs no partner
  ever looks up.
- **EC minting on non-writer navigations.** Today a real browser gets an EC
  on any navigation `is_navigation_request` accepts, including prefetch and
  the `Accept: text/html` fallback (`adapter-fastly/src/app.rs:846-857`).
  That stays: such a request creates the row before routing, as a new
  visitor's writer does, and that create is its page load's one write. It
  runs no providers and writes nothing else.
- **Proxy capture plumbing.** `ProxyDispatchInput` gains `is_real_browser`,
  the writer decision and `IdentityDeps`. Section 4.7 covers how
  `handle_proxy` buffers and decodes a response for `capture`.

## 6. Storage

### 6.1 KV record

`SCHEMA_VERSION` becomes 2 (`ec/kv_types.rs:17`). `KvPartnerId`
(`ec/kv_types.rs:102`) is replaced by one record shape, and `KvEntry`
gains `ps`. KV item metadata, which Fastly caps at 2,000 bytes, holds no IDs
(`ec/kv_types.rs:199-219`) and is unaffected.

**One record shape.** Every record stores its source's entry set in `eids`:
an array of OpenRTB EID entries without `source` (the map key holds it),
each `{uids: [{id, atype?, ext?}], inserter?, matcher?, mm?, ext?}`. A
record that browser ingest, generic pull or batch sync writes is one entry
with one UID and no EID-level fields; only a provider result writes more.
The stored entry types are dedicated (`KvEid`, `KvUid`), carry no `source`
and ignore unknown fields. They are never `openrtb::Eid`, which denies
unknown fields and requires `source` (`openrtb.rs:72`), so a field a later
build adds never makes an older build fail to read the row.

**Reading schema version 1.** Every row on `main` is version 1, whose
records hold one `uid`. The reader accepts both versions. It reads a
version 1 record as one entry holding that `uid`, with no `w`, no `rv` and
no timing beyond `at`, and the next write of the row stores it as version 2.
Today's reader rejects any version but its own (`ec/kv_types.rs:376-381`),
so a build older than rollout step 1 cannot read a row step 1 has written
(section 14).

`KvEntry::validate` rejects, when a row is written, a record whose `eids` is
empty or whose set breaks the limits of section 4.5. It runs on every read
too (`ec/kv.rs:366`), so on read a bad record is dropped from the row and
the violation is logged with the source only. One bad record never makes
the row unreadable. Only `serialize_entry` rejects.

`KvPartnerId` and `KvEntry` are public structs without `Default`, and about
30 `KvPartnerId { .. }` literals in 8 files would stop compiling (for example
`ec/kv.rs:160`, `:208`), as would the constructors at `ec/kv_types.rs:282`,
`:312` and `:344`. So both derive `Default` and gain constructors, and the
literals move to them. `PartnerIdUpdate` (`ec/kv.rs:75-90`) gains
`acquired_at`, so browser ingest records `at`.

```json
{
  "v": 2,
  "ids": {
    "id5-sync.com": {
      "w": "id5",
      "rv": "9c1d04b7e6a83f2a",
      "at": 1760000000,
      "exp": 1767776000,
      "ra": 1760086400,
      "eids": [
        {
          "uids": [
            { "id": "ID5*example", "atype": 1, "ext": { "linkType": 2 } }
          ]
        }
      ]
    },
    "example-ids.example.com": {
      "w": "exampleid",
      "rv": "4be71f0c2d9a6e35",
      "at": 1760000000,
      "eids": [
        {
          "uids": [
            { "id": "example-a", "atype": 1 },
            { "id": "example-b", "atype": 3, "ext": { "stype": "ppuid" } }
          ],
          "inserter": "example.com",
          "mm": 3
        },
        {
          "uids": [{ "id": "example-c", "atype": 1 }],
          "matcher": "partner.example.com"
        }
      ]
    },
    "sharedid.org": {
      "rv": "07a3c9e15b2d48f6",
      "at": 1759990000,
      "eids": [{ "uids": [{ "id": "example-shared-id" }] }]
    }
  },
  "ps": {
    "id5": { "n": 1760003600, "f": 1, "st": "example-opaque-state" }
  }
}
```

| Field   | Meaning                                                                                                                                     |
| ------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `w`     | Writer: a provider ID. Absent for browser ingest and version 1 records                                                                      |
| `rv`    | The record's revision: 16 hex digits, drawn at random on every write of the record (section 5.4)                                            |
| `at`    | Unix seconds when the ID was acquired or last refreshed                                                                                     |
| `exp`   | Unix seconds at or after which the ID must not be sent                                                                                      |
| `ra`    | Unix seconds after which the ID is due for refresh                                                                                          |
| `ru`    | Unix seconds after which the ID can no longer be refreshed. Keeps an expired record as a refresh anchor until then                          |
| `eids`  | The source's entry set, in order, with UID-level and EID-level fields (section 4.5). A missing `atype` uses `[[ec.partners]].openrtb_atype` |
| `ps.n`  | Unix seconds before which the provider must not be called                                                                                   |
| `ps.f`  | Consecutive failures, for backoff                                                                                                           |
| `ps.o`  | Sticky opt-out: `{at, fp}`, when it was reported and the consent bits the provider's policy read then (section 8)                           |
| `ps.oc` | The watermark of the last clear: the `at` of the newest opt-out it lifted. An opt-out at or before it is cleared and never re-applied       |
| `ps.iv` | Hashes of UIDs the owner invalidated, oldest first, at most 128, so they are not re-seeded (section 7.2)                                    |
| `ps.ov` | Hashes of UIDs the provider's opt-out removed, oldest first, at most 128. Emptied when the opt-out clears (section 8)                       |
| `ps.st` | The provider's private state, as embedded JSON (a raw value, not an escaped string) of at most 1,024 bytes. KV only                         |
| `wa`    | Unix seconds of the row's last writer flush, which paces the SPA writer (section 5.1)                                                       |
| `pg`    | The newest pending-key generation merged into the row (section 5.3, The pending key). Items at or below it are never merged again           |

`KvEntry` validation (`ec/kv_types.rs:375`) adds: the entry-set limits of
section 4.5 (UID-level and EID-level `ext` at most 2,048 bytes, 8 entries
and 32 UIDs per source, a set at most 4,096 bytes), `st` at most 1,024
bytes of embedded JSON, `ps` at most 32 providers and `ps.iv` and `ps.ov` each at most `MAX_INVALIDATED_UIDS` (128) hashes. Mutations prune the `ps` entries
of providers no longer configured to their opt-out (section 5.4), and more
than 32 enabled providers is a config error (section 10.2), so the cap is
never reached in practice.

`ps.iv` and `ps.ov` are ordered lists, oldest first, with the same rules.
Each hash is the first 16 hex
digits of the unkeyed SHA-256 of the UID. A new hash is appended, and a
hash already present moves to the end. Mutations truncate it from the front
before validation (section 5.4). 128 hashes hold four whole 32-UID sets, so
one removal never evicts the previous one, and an `also_invalidate` of
several sources keeps all their UIDs. The hash is unkeyed because the same
row stores every live UID in plain text, so a key would protect nothing,
and an unkeyed hash survives an `ec.passphrase` rotation (section 9.3).

Size. A one-UID record is about 120 bytes: today's `uid`, the `eids`
wrapper and the metadata. A record at every cap is at most
`MAX_ENTRY_SET_BYTES` (4,096 bytes) plus its metadata, about 4.2 KB. With
every cap reached, eight owned sources come to about 34 KB, plus 1 KB for
each provider with state, and `ps.iv` and `ps.ov` add up to 2.4 KB each per
provider. A row
that carries two full sets and two stateful providers at every cap is about
15 KB, and typical sets are far smaller. That is above the ~5 KB estimate
in `ec/kv_types.rs:40`, which is a documented estimate rather than an
enforced limit, and the estimate is updated. Fastly's value limit is far
larger. A row that only browser ingest and pull sync write grows by about
60 bytes per source.

The admin EC lookup returns the stored JSON verbatim (`ec/admin.rs:394-397`).
It redacts `ps.st`.

**Serving owned expiring sources.** For an owned source whose provider
marks it as expiring, only records with `w` set to the owner and an
unexpired `exp` are sent to bidders from the server. Any other record for
that source, such as a browser-seeded one, is used only as the input to the
owner's refresh. This fails closed: an ID is re-fetched, never sent past
its real expiry.

### 6.2 The `ts-eids-s` mirror

`ts-eids-s` (`__Host-ts-eids-s`) is client-only. The shim writes it from
the mirror payload TS returns, and page scripts read it. The server never
sets, opens, parses or trusts it. Earlier revisions also had a family of
sealed, server-owned removal cookies, `ts-eids-r`; revision 15 removes them,
because every removal now reaches KV on the request that carries it
(section 5.3).

**Readers.** The mirror is for page scripts that need the server IDs. This
spec names no consumer yet, so the mirror ships off: `/auction` and
`/_ts/page-bids` return no mirror member unless the operator sets
`[ec.identity] mirror_enabled = true` (default `false`). Off, the responses
look as they did before this spec, and the shim neither writes nor clears
the cookie. Section 15 asks for a consumer before the default changes.

**Attributes.** `__Host-ts-eids-s`, with `Path=/; Secure; SameSite=Lax`,
and no `Domain`. It is not `HttpOnly`, because the shim writes it with
`document.cookie` and page scripts read it. Its `Max-Age` comes from the
mirror payload.

**The mirror payload.** `/auction` returns it as `ext.ts_eids_s` in its
response (`ResponseExt`, `openrtb.rs:232`), and `/_ts/page-bids` as a
`ts_eids_s` member of its JSON body. It takes one of two shapes, or is
absent:

```json
{
  "value": "eyJ2IjoxLCJlaWRzIjpbeyJzb3VyY2UiOiJleGFtcGxlLmNvbSIsInVpZHMiOlt7ImlkIjoiZXhhbXBsZS1hIiwiYXR5cGUiOjF9XX1dfQ",
  "max_age": 86400
}
```

```json
{ "clear": true }
```

- **`value`** when the request read a live, consenting row and the servable
  server set is not empty. `max_age` is the smaller of 86,400 seconds, the
  lifetime of `ts-eids`, and the time until the earliest `exp` in the
  mirrored set, so the mirror never outlives an ID.
- **`clear`** when the request's consent denies EID persistence, when its
  KV read finds a tombstone, `consent.ok = false` or a missing row, and
  when the servable server set is empty.
- **Absent** when the request read no KV, such as a `/_ts/page-bids` call
  whose auction does not run. The shim leaves the cookie as it is.

The shim writes `value` as the cookie value with the given `Max-Age`, and
expires the cookie on `clear`. It skips the write when the value is
unchanged.

`value` is the base64url encoding, without padding, of compact JSON:
`{"v": 1, "eids": [...]}`. The example above encodes one entry,
`{"source": "example.com", "uids": [{"id": "example-a", "atype": 1}]}`.
`eids` holds the server-side entries the auction merge would send for this
request (section 7.1), before browser precedence: every KV record that
passes the servability check, for sources with `bidstream_enabled = true`,
after `gate_eids_by_consent`. Each entry is an OpenRTB EID with its
`source`, its UIDs with `atype` and `ext`, and its EID-level fields,
provenance defaults included, exactly as bidders receive it. It never holds
`ps`, `ps.st`, refresh tokens or any other provider state, and no revision,
writer or timing field.

**Size cap.** At most 2,048 bytes for the cookie value, so at most about
1,536 bytes of payload JSON. When the set does not fit, whole sources are
left out, in ascending `cookie_priority` (section 10.1) and then largest
first, and each omission is logged with the provider and source. A source's
entry set is mirrored whole or not at all, never trimmed. A left-out source
still reaches bidders through TS from KV.

**Who writes it.** Only the shim. TS never sets `ts-eids-s`, not even on a
JSON response, so writer HTML responses stay cacheable. A withdrawal or
tombstone met on a writer navigation is cleared from the mirror at the
page's next `/auction` or `/_ts/page-bids`, whose consent check or KV read
returns `clear`. Until then the mirror can hold IDs TS no longer serves.
TS's own bidstream is unaffected, because the server never reads the
mirror.

**No feedback.** The shim never copies mirror IDs into `ts-eids` or the
`/auction` body. A page script that does so anyway, for example by writing
them into Prebid's `ortb2` config, turns them into ordinary browser
candidates. Browser ingest stores them with `FillMissing`, one entry with
one UID, under today's rules (section 2.2), and the provider denials of
section 7.1 filter them before the auction, for owned and unowned sources
alike. So a stale copy of an ID a provider removed is not sent. A mirror ID
never becomes an owner write and never carries its writer, timing or
EID-level fields back into KV.

**Template cache.** `ts-eids-s` joins `ts-ec`, `ts-eids` and sharedId in
the list of identity cookies the template cache refuses as cache keys
(`cookies/template_cache_policy.rs:35`), so no operator can split the
shared template cache per visitor with it.

**Outbound requests.** TS's identity cookies never reach third parties. One
list, `TS_IDENTITY_COOKIE_NAMES` (`ts-ec`, `ts-eids`, `__Host-ts-eids-s`),
is stripped from Prebid Server requests and never added to proxy or
provider requests. The publisher origin is not a third party, but it gets
only the small one:

- **Publisher origin. The mirror stripped.** The publisher proxy removes
  `__Host-ts-eids-s` (`TS_ORIGIN_STRIPPED_COOKIE_NAMES`) from the `Cookie`
  header it forwards (`publisher.rs:3124-3147`), and drops the header when
  nothing is left. `ts-ec` and `ts-eids` still reach the origin, as today,
  because publishers' servers may read them, and nothing else gives an
  origin the browser's IDs: `x-ts-eids` is set only on `/auction`
  responses and never forwarded downstream (`constants.rs:59-69`). So the
  origin's `Cookie` header stays at today's size; only the mirror would add
  to it, up to about 2 KB. An operator whose origin rejects large headers
  (common servers answer 400 or 431 above about 8 KB per header line) sets
  `[ec] strip_eids_cookie_from_origin = true`, which strips `ts-eids` too
  (`TS_ORIGIN_STRIPPED_COOKIE_NAMES_WITH_EIDS`). A metric still reports the
  inbound `Cookie` header size (section 14).
- **Prebid Server.** Stripped in the Prebid transport before the
  consent-forwarding mode is checked (`integrations/prebid.rs:2091-2108`), so
  it applies in every mode, including the early return that forwards the raw
  header. A `Cookie` header that is not valid UTF-8 is dropped, not
  forwarded. The call site passes every `Cookie` value, not one.
- **Integration proxies.** They already use header allowlists without
  `Cookie` (`integrations/lockr.rs:245`, `didomi.rs:258`, `permutive.rs:259`).
- **Identity providers.** The coordinator rejects any request with a `Cookie`
  header (section 4.7).

The strip iterates every `Cookie` field, as `gpt_diagnostics.rs:394-416` does,
and drops the header when nothing is left. It matches cookie names only; no
route opens `ts-eids-s`. The configured source list is computed once per
settings load, not per request.

### 6.3 Read order

The server-side view of provider IDs is the KV row, and nothing else. For a
request with a valid EC that uses identity (the writer request, `/auction`,
`/_ts/page-bids`, or a capture removal write):

1. **The writer's phase 1** reads the KV snapshot on every returning-EC
   navigation. The read finds orphans (section 5.5), feeds eligibility
   (section 4.7), filters browser candidates (section 7.1) and serves the
   withdrawal path when consent is withdrawn.
2. `/_ts/api/v1/identify` reads the row with `kv.get` and never writes it
   (section 5.1). It reads KV only, never a cookie copy, and applies the
   servability check and the browser-context rule of section 7.1.
3. `/auction` and `/_ts/page-bids` keep their existing reads and gain no new
   triggers. A stager navigation that runs an auction, such as a prefetch,
   preloads as today, without the pull-complete marker's skip (section
   10.4).
4. **Any request that read KV** uses the row as the base:
   - A tombstone, or `consent.ok = false`, empties the server set. An
     `/auction` or `/_ts/page-bids` response returns `clear` for the
     mirror.
   - A missing row, unless this request is about to recover it, empties the
     server set and stores nothing (section 5.6).

A request that read no KV, such as a proxied Lockr API call with no
revocation, has no server set. It needs none, because it persists no
positive ID (section 5.3).

## 7. Auction merge and ownership

### 7.1 Merge

`merge_auction_eids` (`auction/endpoints.rs:505`), and the navigation and
page-bids merge (`publisher.rs:5547-5568`), take a third input: the server
set. It holds the KV records the request read, with expired records left
out. It never holds anything from `ts-eids-s`. The server set requires
what today's KV path requires: `ec_allowed()`, and a live consenting row
whenever the request read KV (`endpoints.rs:446-460`). Each server-side ID
must also pass, for this request, the consent policy of the provider that
wrote it (`w`) and, for an owned source, the owner's policy as well. So for a
provider under the strict policy, the sharing and targeted-advertising
opt-outs and GPC stop it even when the generic gates pass. The generic pull
provider keeps today's pull sync policy (section 10.4): `ec_allowed()` and
`allows_eid_persistence`, which these opt-outs do not reach. Putting it
under the strict policy would change behaviour that predates this spec, and
is left to a later change. Also, an unowned source that a strict provider wrote is
not disclosed once that provider's policy fails. A record whose writer is
disabled or no longer configured has no policy left to check, so it is
never served, and it counts as absent for `FillMissing` and for unowned
`Initial`, so the browser or another provider can replace it. A record
without `w` (browser ingest or version 1) needs the owner's policy for an owned
source, and only the generic gates for an unowned one, as today.

For each source, the value sent to bidders is:

| Source kind | Browser sent the source                                                | Browser did not send it                                                       |
| ----------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| Owned       | the owner's valid value, else the browser's unless the owner denies it | the owner's valid value, else a browser-seeded value if the IDs do not expire |
| Unowned     | the browser's value (today's rule)                                     | the server value, if any (today's rule)                                       |

"The owner's valid value" means a record with `w` set to the owner that is
not past its `exp`. A browser-seeded record never beats the browser's
current value. It may be stale, which is the problem the EID fidelity work
fixed (D9). For a provider whose IDs expire, a browser-seeded record is never
sent from the server at all (section 6.1).

**How a server value goes out.** A server value is the source's whole entry
set, one outbound EID per stored entry, in stored order, after the browser
entries (EID fidelity spec §4.5). A one-UID record without EID-level
fields is one entry, exactly as today (`ec/eids.rs:64-76`). For each entry:

- Each UID uses its stored `atype`, else `[[ec.partners]].openrtb_atype`,
  and its stored UID-level `ext`.
- The entry keeps its stored EID-level fields. For a value a provider
  wrote, an entry that sets none of `inserter`, `matcher` and `mm` gets
  that provider's `with_provenance` default for the source (section 4.3).
- An owner's valid value replaces every browser entry for the source, all
  of them when the browser repeated the source. Its set is never mixed
  with browser entries.

A browser value keeps the browser's own entries and EID-level fields, as the
EID fidelity work requires. A browser-seeded or version 1 record is one
entry with one UID, so everything that predates this spec goes out
unchanged.

This needs the output path to carry a set. Today `resolve_partner_ids`
returns `ResolvedPartnerId { uid, openrtb_atype }` and `to_eids` builds one
EID from each (`ec/eids.rs:21-28`, `:64-78`), for the auction
(`auction/endpoints.rs:465-466`) and the admin view
(`ec/admin.rs:485-486`). `ResolvedPartnerId` gains the record's servable
entry set and its writer, and `to_eids` emits one `Eid` per stored entry
under the rules above. `build_eids_header` and both callers use it
otherwise unchanged. For a one-UID record without `w`, the output is
byte-identical to today's.

**Provider denials apply to browser candidates.** Before the precedence
above, the browser's candidates pass the denials of the providers that
claim their source, read from KV:

- A browser UID whose hash is in the `ps.iv` or `ps.ov` of a provider that
  claims the
  source is dropped, for owned and unowned sources alike. Once a provider
  invalidates UID A, or removes it with an opt-out, an A still in the
  `/auction` body or in `ts-eids` is not sent.
- While the owner of a source has a sticky `ps.o`, the browser's value for
  that source is dropped, until the opt-out clears (section 8). The same
  holds for every source of a capture-only provider with a sticky `ps.o`,
  owned or not, because its IDs reach TS only through the browser.

These run before `gate_eids_by_consent`, which knows nothing of
provider-specific rejections. `ps.iv` and `ps.ov` each keep the 128 most recent hashes, so an older rejected UID can be sent again once evicted. A removed set adds a
hash for each of its UIDs (section 7.2), so a browser entry is filtered UID
by UID, and an entry left with no UID is dropped. Every writer navigation,
`/auction` and `/_ts/page-bids` auction reads KV and filters fully.
Unowned sources otherwise keep today's browser-first rule: a provider can
veto only the UIDs it removed itself, never the source.

The browser's candidates include any `ts-eids-s` mirror ID a page script
fed back through Prebid (section 6.2). It is filtered like any other
browser value, so a stale mirror copy of an ID a provider has since removed
is not sent.

Sources with `bidstream_enabled = false` are
left out, as `resolve_partner_ids` does today (`ec/eids.rs:36`).

The merged set is consent-gated (`gate_eids_by_consent`) and reported in
`x-ts-eids` exactly as today, with the same 4 KB cap. A full entry set can
reach that cap sooner, and `x-ts-eids-truncated` is set as today. The
header's truncation stays per entry, as today (`encode_eids_header`,
`ec/eids.rs:106`), because the header also carries the browser's entries,
whose truncation must not change. So the header can hold part of a set;
`user.eids` always holds it whole. `x-ts-eids` therefore shows server-only
IDs, EID-level fields included. The calling script can read it, and so can
any page script that wraps `fetch`. The `ts-eids-s` mirror shows the same
server-side entries to every page script (section 6.2). The header stays
for validation and troubleshooting, and both exposures are accepted
(section 9.3).

One shared servability check covers expiry, the rollback rule, the writer's
and owner's consent policies, and opt-outs: a record whose writer has `ps.o`
set is never served, and neither is any record of a source whose owner has
`ps.o` set, whoever wrote it: an owner's opt-out removes only what the owner
wrote, so a browser-seeded record can remain. No stored record matches an
owner's `ps.iv`, because every invalidation removes its record and step 3
of section 5.4 blocks a refill. `resolve_partner_ids` (`ec/eids.rs:36`), the admin view
(`ec/admin.rs:485`) and `/_ts/api/v1/identify` all use it. `/identify` reads
`entry.ids` directly today (`ec/identify.rs:99-103`), so it switches to the
check. Its response shape does not change: `uid` is the first UID of the
first entry of the source's record, when that record passes the
servability check, and `eid` is built from that UID with the partner's
`openrtb_atype`, as today (`ec/identify.rs:127-135`). A partner calling
`/identify` gets the one UID it gets today, never a provider's full set. The
stale module comment at `ec/eids.rs:1-5` is corrected. So expired IDs are
never returned by any of them (G7).

**Consent for `/identify`.** The policies are checked against the visitor's
consent on this request, never against the row's stored `consent.tcf` and
`consent.gpp`, which are snapshots from creation and authorize nothing.
`/identify` takes its EC only from the `ts-ec` cookie, never from a
header (`ec/mod.rs:325`), and builds its consent from the same request's
cookies, `Sec-GPC` and geolocation (`ec/mod.rs:338-345`). That does not
prove the visitor's browser sent the call: the EC ID is a bearer value
(section 9.5), and a server that holds it can send `Cookie: ts-ec=<id>`
with its own consent cookies. So `/identify` returns a provider-written ID
(a record with `w`) only with browser context: an `Origin` the CORS check
allows and a `Sec-Fetch-Site` of `same-origin` or `same-site`. A call
without either, including one with no `Sec-Fetch-Site` at all, gets the
response it would get if that record were absent. A browser-seeded record
keeps today's behaviour. Within a request:

- An absent consent string is treated as the auction treats it. Where TCF
  applies, a strict policy finds no vendor consent and blocks. Elsewhere
  only the signals present (GPC, GPP, US Privacy) apply.
- The geolocation is the requester's, which for a browser call is the
  visitor's.
- A later vendor denial blocks that provider's IDs even while global EC
  consent still passes, because each ID needs its writer's and owner's
  policy, not only `ec_consent_granted`.

No persisted visitor authorization is added, so `IdentityMutation` carries
no consent update.

### 7.2 Ownership semantics

- **Owned source.** The owner is authoritative. It refreshes when due
  (section 4.7, step 4), and its valid value wins in the auction. Browser
  ingest may only fill the source when it is absent or its record is
  expired. That includes a `ts-eids-s` mirror ID the page fed back
  (section 6.2).
  That seeds it: the owner treats a value it did not write as due, and
  refreshes it at once. No other provider may claim the source (section
  10.2).
- **Removed values are not re-seeded.** When a provider's invalidation or
  opt-out removes a record, the hash of each UID in the removed set goes
  into that provider's `ps.iv` or, for an opt-out, its `ps.ov` (section
  5.4). Browser ingest skips a UID in the `ps.iv` or `ps.ov` of any provider
  that claims the source, so the browser cannot
  re-seed a value a provider just rejected, and no
  invalidate-reseed-refresh loop starts.
- **A provider replaces its own set whole.** For an owned source, a result
  that names it replaces every stored entry for it, so the owner drops one
  entry by returning the set without it. For an unowned source a result
  only fills an absent or expired record (`FillMissing`) and never
  replaces one, so an unowned provider cannot drop an entry. Browser ingest
  never adds an entry to a
  provider's set: it fills a source only when it is absent or expired, as
  today.
- **Unowned source.** Any provider claiming it, and the browser, write with
  `FillMissing` only. Unowned sources are never refreshed. This is exactly
  today's pull-sync contract. The auction is browser-first.
- **Batch sync.** A batch-sync update to an owned source applies only when
  the source is absent (`ec/batch_sync.rs:247`). It never replaces the
  owner's value. Unowned sources keep today's batch-sync behaviour.

## 8. Errors and outcomes

Every resolving provider runs on the writer, so every effect below goes
into the writer's mutation. A capture, the only provider result on a
stager, stores its removals, opt-out and clear through the capture removal
write (section 5.3), and nothing else.

| Result or error                                                                | Stored IDs                                                                                                                                             | `ps`                                                                                   | Log   |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------- | ----- |
| `Resolution::found`                                                            | `OwnerWrite` or `FillMissing` per source                                                                                                               | `f = 0`; `st` replaced or cleared when the resolution says so                          | debug |
| A stateful result whose IDs or state lost to a newer write (section 5.4)       | unchanged; the result's removals still apply                                                                                                           | unchanged                                                                              | debug |
| `replacing_state` oversized or unserializable                                  | no IDs stored; the result's removals still apply                                                                                                       | `st` unchanged; `f` and `n` as for `Malformed`                                         | warn  |
| `Resolution::no_match`                                                         | unchanged                                                                                                                                              | `n = now + retry_in` (default 24 h, max 7 d)                                           | debug |
| `Resolution::invalidate`, `also_invalidate`                                    | `OwnerRemove` of the whole set, with the revision the request read                                                                                     | on apply, a hash per UID of the removed set added to `iv`; `n = now`                   | info  |
| `Resolution::opted_out`                                                        | `WriterRemove` for every record this provider wrote, owned or unowned; for a capture-only provider, also every record of its sources no provider wrote | `o = {at: now, fp}`, `st` cleared; a hash per UID of each removed record added to `ov` | info  |
| `with_retry_in` on any resolution                                              | as above                                                                                                                                               | `n` is at least `now + retry_in`                                                       | debug |
| `request` returns `None`, or `InputUnavailable`                                | unchanged                                                                                                                                              | `n = now + refresh_after_sec`                                                          | debug |
| A recorded `InvalidState`, whatever the provider returned                      | as the result says                                                                                                                                     | `st` cleared, `f` unchanged, `n = now + refresh_after_sec` unless the result sets `n`  | warn  |
| Hold-mode budget spent before the answer                                       | unchanged                                                                                                                                              | `n = now + hold_retry_sec`                                                             | debug |
| `Rejected` with a 4xx other than 429, `Config`, `UndeclaredInput`, `Malformed` | unchanged                                                                                                                                              | `f += 1`, `n = now + min(1 h × 2^(f-1), 24 h)`                                         | warn  |
| `Rejected` with 429 or 5xx, `Transport`, timeout                               | unchanged                                                                                                                                              | `f += 1`, `n = now + min(5 min × 2^(f-1), 1 h)`; breaker `record`                      | info  |
| Breaker open                                                                   | not dispatched                                                                                                                                         | unchanged                                                                              | debug |
| Burst cap exceeded (section 4.7 step 9)                                        | not dispatched                                                                                                                                         | `n = max(n, now + 60 s)`; never a failure                                              | debug |
| Backend budget spent (section 4.7 step 8)                                      | not dispatched                                                                                                                                         | unchanged; never a failure                                                             | debug |
| Capture: any result or error                                                   | removals as above; positive IDs counted in `capture_unpersisted`, never stored                                                                         | only `o`, `oc`, `iv` and `ov`; never `n`, `f` or the breaker                           | debug |
| Captured `opted_out`, `revoke_scope = "global"`, any client                    | the withdrawal tombstone removes the row (section 5.1)                                                                                                 | removed with the row                                                                   | info  |

**Sticky opt-outs.** `ps.o = {at, fp}` records a provider-reported opt-out
and the consent fingerprint at that time. Server-side calls do not carry the
vendor's own opt-out cookie, so `o` is TS's only record of it. Applying it
removes every record the provider wrote, and for a capture-only provider
every record of its sources that no provider wrote (section 5.4). While it
is set no record that provider wrote is served, and the browser's values it
denies are dropped (section 7.1). It is cleared only by an affirmative
re-grant for this provider, and clearing it empties `ps.ov` (section 5.4):

- **A provider with a GVL ID:** `fp` at `at` records a decoded TC string
  that denied the provider's vendor or a purpose its policy requires, and
  the current TC string grants the vendor and every required purpose. Only
  that denied-to-granted transition clears. TCF `Created` and `LastUpdated`
  are day-level and set by the client, so neither is compared with `at`: a
  same-day re-grant clears, and a later CMP re-save under signals that
  already granted the vendor does not. A vendor-reported opt-out recorded
  under full TCF consent is cleared only by the vendor issuing again, for a
  capture-only provider, or by an operator.
- **Any provider, under GPP:** the applicable US section showing no sale,
  sharing or targeted-advertising opt-out for the signals the provider's
  policy reads, where `fp` at `at` recorded one. GPP carries no creation
  time, so the clear is by section values: the opt-out clears when `fp`
  records at least one US opt-out bit (sale, sharing, targeted
  advertising, GPC) and the current consent shows none of the opt-outs
  `fp` records.
- **A capture-only provider without a GVL ID, such as Lockr:** a later 2xx
  capture that returns IDs, under the request's consent. That is the
  vendor issuing again after the visitor re-consented through it. The
  capture removal write stores the clear on that request, with the `at` of
  the opt-out the request read as its watermark,
  under the ordering of section 5.4 (section 5.3). The captured IDs
  themselves are not stored; once the clear is in KV, the browser path
  carries them through browser ingest.
- **An operator,** through the admin endpoints.

A change of jurisdiction, a newly present GPP string, or a privacy signal the
provider relaxes never clears it. A clear is written to KV as `ps.oc`, its
**watermark**: the `at` of the opt-out the clearing request read, not the
time it ran. A slow request that left before a revoke, or a writer whose
phase-1 read predates a revoke staged by another request, therefore names an
older opt-out and cannot dismiss the newer one (section 5.4, step 3). The
operator's clear uses the time it runs, so it lifts every opt-out up to
then. A
consent withdrawal's
tombstone removes everything, provider opt-outs included: after it, the
visitor starts again with a new EC.

**Revoke scope.** A provider that declares `captures` has a `revoke_scope`
(section 10.1), default `"provider"`. With `"provider"`, a captured
`opted_out` is the provider opt-out above. With `"global"`, a captured
`opted_out` runs today's consent-withdrawal tombstone path on that request
instead (section 5.1): the row is tombstoned, the EC cookie is cleared, and
everything is removed, provider opt-outs included. Because the CMP may still
grant, the next navigation would mint a new EC and undo the revoke, so the
response also sets `ts-ec-revoked` (`Path=/; Secure; SameSite=Lax;
HttpOnly`, the EC cookie's domain and one-year `Max-Age`), holding the strict
policy's consent fingerprint for the request (section 8, `fp`, without DNT).
`generate_if_needed` mints no EC while the request carries that marker and
its own fingerprint equals it. Once the consent changes, minting resumes and
the response that sets the new `ts-ec` expires the marker. A CMP re-save with
the same decisions keeps the fingerprint, so it does not lift the revoke. It uses the withdrawal's
existing exception, so it adds no page-load exception to G4, and like a
withdrawal it applies whatever the client (section 4.7). An opt-out from `resolves` is always
provider-scoped.

The **consent fingerprint** `fp` is a bitmask, written as hex, of the
consent signals the provider's policy reads:

- its TCF purpose bits and vendor bit, set when granted, and a TCF bit
  (bit 16) set when TCF applied and a TC string was decoded, so an unset
  purpose or vendor bit records a denial only when the TCF bit is set
- the US sale, sharing and targeted-advertising opt-outs it does not relax
- GPC and DNT, unless relaxed

It is stored in plain form: the same row already holds the raw consent
strings (`KvConsent`, `ec/kv_types.rs:69-80`), so a keyed hash would protect
nothing, and a plain bitmask can be compared bit by bit. It is not the raw
consent string, which changes whenever a CMP re-saves it.

Rules:

- No error ever deletes a still-valid ID. Only `Resolution::invalidate` and
  `also_invalidate` (by revision) and `opted_out` (everything the provider
  wrote) delete.
- An expired ID is not permission to call. Eligibility still applies.
- Errors are logged with provider ID, outcome and status only (section 9.3).
- A provider whose IDs expire and whose refresh keeps failing loses its ID
  at `exp`, not before.

## 9. Security and privacy

### 9.1 Consent

- **Before dispatch:** global EC consent, EID-persistence consent, and the
  descriptor's `ConsentPolicy` (section 4.3). "Where TCF applies" means
  `gdpr_applies` or a jurisdiction of `Gdpr` or `Unknown`, matching
  `consent_allows_server_side_auction` (`consent/mod.rs:298-308`). Each check
  maps to a signal TS decodes:
  - TCF purposes and vendor consents (`consent/tcf.rs:145`), read through
    `effective_tcf` (`consent/mod.rs:283-289`), so a GPP EU TCF section
    counts as a TC string does. Vendor consent uses the existing
    `TcfConsent::has_vendor_consent` (`consent/types.rs:278`). Publisher
    restrictions are not decoded (`consent/types.rs:176`), so a CMP's
    per-vendor "not allowed" restriction is not applied.
  - The US sale opt-out, from GPP US sections (`consent/gpp.rs:74`, `:125`)
    or the US Privacy string (`consent/us_privacy.rs:50-63`).
  - The US sharing and targeted-advertising opt-outs. This work extends the
    GPP decoder to extract them next to `decode_us_sale_opt_out`
    (`consent/gpp.rs:125`). The `iab_gpp` crate already exposes
    `sharing_opt_out` in the US National and California sections, and
    `targeted_advertising_opt_out` in the US National section and the state
    sections that define it.
  - GPP section 6, the US Privacy string inside GPP, which the decoder skips
    today (`consent/gpp.rs:112`), and the US National section's GPC and
    known-child fields. This work decodes them too. A known child without
    consent blocks every provider, except generic pull's parity policy
    (`eid_persistence_only`), which keeps only the global gates so pull sync
    behaves as it does today. A US section or section 6 that is present but
    fails to decode blocks every provider the same way (fail closed).
  - `Sec-GPC` (`consent/extraction.rs:25-50`).
  - DNT, captured in phase 1 (section 4.4). TS reads it today only for the
    auction (`auction/routing.rs:336`).
- **Fail closed.** `strict()` blocks when a raw TC, GPP or US Privacy string
  is present but has no decoded form, for example in proxy mode or after a
  decode failure (`consent/mod.rs:104-127`, `:197-210`). A GPP string that
  decodes but has no applicable section, such as the header-only `DBAA` a
  CMP sends when no section applies, has a decoded form and carries no
  opt-out, so it does not block `strict()` by itself. Where TCF applies, the
  policy still needs TCF consent from a TC string or a GPP TCF section.
- **Before disclosure:** the live-row read and eligibility recomputation
  described in section 4.7.
- **At write:** the CAS on that read's generation.
- **In the mirror payload:** the IDs returned for `ts-eids-s` pass the
  servability check and the generic gate against the request's consent
  (section 6.2). Removals and opt-outs never need consent to apply
  (section 5.6).
- **In the bidstream and `/identify`:** server-side IDs pass their
  writer's policy and, for an owned source, the owner's, against the
  request's visitor consent (section 7.1). Owner denials also filter the
  browser's candidates.
- **On withdrawal:** the tombstone is written synchronously on whichever
  request carries it (section 5.1), and the next `/auction` or
  `/_ts/page-bids` response tells the shim to clear `ts-eids-s` (section
  6.2). A global revoke takes the same path (section 8).

### 9.2 Outbound requests

- HTTPS only. Redirects are not followed.
- The host must be one of the descriptor's `resolves` hosts.
- Providers build headers from scratch. The coordinator rejects any request
  with a `Cookie` header, and TS's identity cookies are stripped from Prebid
  Server requests. The publisher origin receives `ts-ec` and `ts-eids` but
  never the mirror (section 6.2).
- Providers get only the inputs their descriptor declares (section 4.4).
  Only generic pull receives the EC ID. Location is coarse (country and
  subdivision), page URLs carry no query string, and the consent view
  carries no decoded jurisdiction, though the raw GPP string still shows the
  applicable state section (section 4.4). A URL path can still carry
  identifiers, so most providers should want `PageUrl` rather than need it.
- The client IP comes only from the platform's client-IP API. A provider that
  needs it in `X-Forwarded-For` sets it from `ResolveInput::client_ip`.
  Client-supplied forwarding headers are never copied.
- Response bytes are capped before and after decompression.

### 9.3 Logging and data handling

- Never log UIDs, envelopes, HEMs, provider state, consent strings, cookie
  values, or provider URLs with a query string. EC IDs go through `log_id`
  (`ec/mod.rs:63`).
- Provider errors are logged with `current_context()` only, never with
  `{:?}` of the report, whose attachments could carry a URL or body.
- `ProviderRequest`, `ProviderResponse`, `ResolveInput`, `CaptureInput`,
  `StoredId`, `ResolvedEid`, `ResolvedUid`, `ProviderConsent` and
  `HashedEmail` have redacting `Debug` implementations. `ConsentContext`
  gets one too, because it derives `Debug` over the raw consent strings today
  (`consent/types.rs:109`).
- The Lockr proxy logs only the path of the upstream URL, not its query
  (`integrations/lockr.rs:204-205`).
- The rate-limit visitor key is an HMAC of the full EC ID with a key
  derived from `ec.passphrase`, because it leaves the row for the rate
  limiter. Its windows last 60 seconds, so a passphrase rotation costs
  nothing. Nothing stored in the row is keyed: `ps.iv` and `ps.ov` hold unkeyed UID hashes (section 6.1) and the consent fingerprint is a plain bitmask
  (section 8), because the same row already holds the raw UIDs and consent
  strings. So a passphrase rotation breaks nothing stored.
- `CaptureInput` never exposes request bodies or request headers. Proxied
  requests can contain an email or HEM. TS forwards them, and never reads a
  proxied request body for identity.
- Provider state stays in KV. It is never sent to bidders, put in a cookie or
  returned by the admin lookup.
- A provider's EID-level `ext` and UID-level `ext` are vendor data, such as
  segments or match metadata. They are stored in KV and never in `ts-eids`,
  which keeps dropping EID-level `ext` (EID fidelity spec §4.3). They reach
  bidders, `x-ts-eids` and the `ts-eids-s` mirror under the same consent
  gates as the UIDs. A vendor spec that stores such data states what it
  holds, as part of its privacy review.
- **Server-only IDs are readable by page scripts.** `ts-eids-s` is not
  `HttpOnly` and is not sealed, so every script on the page can read the
  servable server set, including UID2 advertising tokens and LiveRamp
  envelopes. `x-ts-eids` already exposes the same IDs to the calling script
  and to any script that wraps `fetch` (section 7.1), so the mirror adds no
  new class of reader. The mirror holds only what bidders receive: never
  `ps.st`, refresh tokens or other provider state (section 6.2). It is
  stripped from origin and Prebid Server requests like the other ID
  cookies. A vendor spec whose IDs must not reach page scripts says so, and
  the operator gives its source `bidstream_enabled = false` or keeps the
  provider off.

### 9.4 Caching

This spec adds no `Set-Cookie` to any response. TS never sets `ts-eids-s`:
the shim writes it from JSON responses that are already private (section
6.2). Returning-visitor HTML keeps today's cacheability, and removing the
pull-complete marker (section 10.4) takes away the one `Set-Cookie` a
returning-visitor navigation could carry today (section 3.4).

### 9.5 The EC ID is a bearer value

The EC ID is the value of the `ts-ec` cookie (`ec/mod.rs:197-203`, `:325`),
and any server can send that cookie. Partners and the origin already hold
raw EC IDs: every `/identify` 200 returns `ec` (`ec/identify.rs:139`), pull
sync appends `ec_id` to the partner URL (`ec/pull_sync.rs:368-371`), and the
origin receives `ts-ec`. A call with no `Origin` header is accepted
(`CorsDecision::NoOrigin`, `ec/identify.rs:52`). Consent comes from the
caller's own cookies and IP geolocation (`ec/mod.rs:338-345`).

So a holder of an EC ID can present its own consent, which skips the
visitor's vendor denials, US opt-outs and GPC. Sealing and CAS prove that
TS wrote a value, never that the visitor's browser carried the request.
This spec designs against that:

- Removals, opt-outs and invalidations only remove, so a replayed EC can
  make TS forget an ID but never disclose one.
- A capture clear (section 8) re-enables a capture-only provider. A holder
  that replays the EC through the vendor's proxied API and gets a 2xx with
  IDs can clear that provider's opt-out. Accepted: the vendor issued again
  for that request, and the IDs reach KV only through the browser path.
- `/identify` fails closed for provider-written IDs without browser
  context (section 7.1). Both headers can be forged by a server, so this
  stops a partner that replays an EC with a plain HTTP client, not one that
  imitates a browser; it costs nothing, and the IDs at stake did not reach
  `/identify` before this spec.
- `x-ts-eids` on `/auction` is accepted as a troubleshooting header
  (section 16): a replayed EC with granting consent reads the server set
  through it, as it reads the browser's own IDs today.

## 10. Configuration

### 10.1 Layers

```toml
# Coordinator policy: vendor-neutral, one place for every timing choice.
[ec.identity]
# Replaces ec.pull_sync_concurrency, which is removed (section 10.2).
max_concurrency = 4
# Shared wall-clock budget for all hold-mode providers, in milliseconds.
# Default 100 (section 4.7 explains why).
hold_budget_ms = 100
# After a call is cut off by the hold budget, wait this long before the
# next. Default 300.
hold_retry_sec = 300
# Per provider response, before and after decoding.
max_response_bytes = 65536
# Backend requests one execution may start: origin, auction and provider
# calls together. Default 32, Fastly's limit; 10 on trial accounts.
max_backend_requests = 32

[ec.identity.providers.id5]
# "hold" or "after_send". Default "after_send". Providers run only on
# writer navigations. Hold mode runs before the navigation's auction, and
# after send when the navigation runs none (section 4.7, step 7).
mode = "hold"
# Hold dispatch before the auction: min(timeout_ms, remaining hold budget).
# Default: hold_budget_ms.
timeout_ms = 80
# Any dispatch after send, including a hold-mode provider on a navigation
# that runs no auction. Default 1000.
after_send_timeout_ms = 1000
# Narrows the sources this provider may write. Default: every source its
# descriptor declares. Narrowed here to the one source registered below.
sources = ["id5-sync.com"]
# Refresh interval when the provider gives no refresh hint, and the wait
# after a skipped call. Default 86400.
refresh_after_sec = 86400
# Burst cap per visitor, enforced in 60-second windows. Default 1.
max_calls_per_minute = 1
# Ceiling per client address across visitors, in 60-second windows.
# Default 10; 0 turns it off.
max_calls_per_ip_per_minute = 10
# Skip the provider for every visitor once this many transport failures,
# timeouts, 429s or 5xx responses happen within one 60-second window.
# Default 50.
breaker_failures_per_minute = 50
# Lower is left out of the ts-eids-s mirror first when it exceeds its cap.
# Default 0.
cookie_priority = 10

[ec.identity.providers.lockr]
# Required for a capture-only provider: its opt-out removes browser-ingested
# records of every source listed here (section 5.4).
sources = ["id5-sync.com"]
# Only for providers that declare `captures`. "provider" or "global".
# Default "provider": a captured opt-out removes this provider's records.
# "global" runs the consent-withdrawal tombstone instead (section 8).
revoke_scope = "provider"

# Source policy: the existing table, with one new field.
[[ec.partners]]
name = "ID5"
source_domain = "id5-sync.com"
openrtb_atype = 1
bidstream_enabled = true
# New. A provider ID that owns this source. Absent: unowned.
owner = "id5"

# Vendor protocol: the integration's own table, defined by its own spec.
[integrations.id5]
enabled = true
partner_id = 1234
```

`[ec.identity]` and each `[ec.identity.providers.<id>]` table are optional.
A provider with no table uses the defaults.

### 10.2 Validation

Deploy-time validation (`validate_settings_for_deploy`, `config.rs:249-262`)
parses integration configs but never builds providers. Each identity
integration gets a line in a new `validate_identity_integration::<T>` list
next to today's per-type list (`config.rs:303-332`). That function parses the
config as `T: IdentityProviderConfig` and calls `build_descriptor`, which
needs no secrets and returns the same descriptor the running provider
stores. A separate pass builds the descriptors of `[[ec.partners]]` pull
providers. The identity check runs next to
`PartnerRegistry::validate_config_for_deploy` (`config.rs:260`) in
`ts config validate` and `ts config push`, and in
`validate_settings_for_runtime` at startup (`config.rs:270-280`). It applies
each provider's `sources` narrowing before checking ownership.

Each of these is an error:

- `owner` names a provider that is not enabled.
- `owner` names a provider whose descriptor does not include that source,
  or a capture-only provider, which never writes a record in v1 (section
  4.7).
- A descriptor fails to build (section 4.3).
- An enabled provider claims a source with no `[[ec.partners]]` entry.
- An enabled provider claims a source owned by a different provider. Use
  `sources` to narrow one of them. For example, when native ID5 owns
  `id5-sync.com`, Lockr is narrowed to the sources no native provider owns.
- `sources` names a source the provider's descriptor does not declare, or
  narrows the provider to no source at all.
- A capture-only provider has no `sources` setting. Its opt-out removes
  browser-ingested records of every source it keeps, including IDs other
  vendors' own modules produced (section 5.4), so the operator must list the
  sources it is the delivery path for; the default of every declared source
  is not allowed.
- Two providers have the same ID (section 10.4).
- A capture rule lies outside the registering integration's proxy routes.
  `IntegrationRegistry::with_plan` reports it at startup (section 4.3).
- `[ec.identity.providers.<id>]` names an unknown provider.
- `mode` is not `hold` or `after_send`.
- More than 32 providers are enabled, generic pull partners included. A KV
  row holds `ps` for at most 32 (section 6.1).
- `timeout_ms` is greater than `hold_budget_ms` for a hold-mode provider.
- `hold_budget_ms` is over 1,000, `max_concurrency` is 0,
  `refresh_after_sec` is 0, or `after_send_timeout_ms` is 0 or over 10,000.
- `revoke_scope` is set for a provider whose descriptor declares no
  `captures`, or is not `provider` or `global`.
- A removed key is set: `ec.pull_sync_concurrency`, or
  `pull_sync_ttl_sec` or `pull_sync_rate_limit` on an `[[ec.partners]]`
  entry. The error names the replacement (sections 10.1, 10.4).

Validation never calls a provider. Live approvals, such as a LiveRamp
placement or an approved origin, stay on the rollout checklist of each vendor
spec.

**Secrets.** A provider's secret fields, such as UID2's API key and client
secret, are `Redacted<String>` config fields. At rest the config holds their
secret-store key names, and they are resolved in memory before runtime
settings are built (`secret_resolution.rs`). Each identity integration adds
its secret paths to `TrustedServerAppConfig::secret_fields()`
(`config.rs:136-175`). `build_descriptor` must never read a secret field,
because deploy validation runs before resolution and would see key names.
The harness's `check_descriptor_ignores_secrets` builds the descriptor from
a config whose secrets are placeholder key names and from one with resolved
secrets, so a descriptor that depends on a secret fails the test (section
13).

### 10.3 Registration

`IntegrationRegistration::builder(ID)` gets
`.with_identity_provider(Arc<dyn IdentityProvider>)`, next to `.with_proxy()`
(`integrations/registry.rs:663`), and
`.try_with_identity_provider(Result<P, Report<IdentityProviderError>>)`,
which wraps a constructor's result for the common case (section 4.6). An
integration can register more than one provider. The registry exposes
`identity_providers()` to build the
coordinator. `handle_proxy` offers a proxied response to every provider whose
descriptor has a capture rule matching it (section 4.7). A compile-time guard,
`const _: Option<&dyn IdentityProvider> = None;`, keeps the trait
object-safe.

### 10.4 Pull sync on the trait

Each `[[ec.partners]]` entry with `pull_sync_enabled = true` registers a
built-in `GenericPullProvider`. Its ID is `pull-` plus the source domain with
dots replaced by `-` (for example `pull-ssp-example-com`). A domain that
already contains `-` also gets `-` and the first 8 hex digits of the SHA-256
of the full domain, so `a-b.example.com` and `a.b.example.com` get different
IDs. An ID that would exceed 63 characters is cut and given the same hash
suffix, so IDs stay unique and valid.
No earlier build stored a generic pull provider ID: before this spec, pull
sync kept no per-provider state and wrote no `w`, so the hash rule renames
nothing that is persisted.

Unchanged:

- The protocol: `GET <pull_sync_url>?ec_id=<id>`, bearer `ts_pull_token`,
  response `{"uid": "..."}`, the 64 KiB cap and the
  `pull_sync_allowed_domains` allowlist.
- Fill-missing only, unless the operator sets `owner`.
- The hourly rotation of partner order, which keeps dispatch fair under
  `max_concurrency` (`pull_sync.rs:143-156`).

Its descriptor, built from the `[[ec.partners]]` entry:

- One source, the entry's `source_domain`. Its IDs do not expire.
- `resolves` with the `pull_sync_allowed_domains` as hosts and the
  crate-private `with_ec_id()`, because the protocol is keyed by the EC ID.
- A consent policy that blocks exactly what pull sync blocks today. The
  migration parity tests (section 13) pin it.
- Default `after_send`, and unowned.
- Optional `owner = "pull-<domain>"` makes it authoritative.
- Its pacing comes from `[ec.identity.providers.pull-<domain>]`, like any
  provider's (section 10.1).

What changes:

- It is dispatched only on the writer request for a returning EC. Today it
  runs after any response whose request loaded a consenting row
  (`pull_sync.rs:86`, `:166-174`), including a new visitor's first
  navigation. New visitors now reach pull partners from their second page
  load.
- Its results go into the single flush. Today it makes its own write.
- Errors back off through `ps` (section 8). Today a failing partner is
  retried on every eligible request, within the rate limit.
- The rate-limit key moves from `pull:<domain>:<ec_hash>`
  (`pull_sync.rs:373`) to the per-visitor key of section 4.7, next to the
  per-IP ceiling of that step. Counters reset at deploy.
- `pull_sync_ttl_sec` and `pull_sync_rate_limit` are removed from
  `[[ec.partners]]` (`settings.rs:430-439`). Their roles move to
  `refresh_after_sec` and `max_calls_per_minute`. Today's hourly limit
  already rounds to a per-minute one (section 3.4), so an operator who set
  `pull_sync_rate_limit = N` sets `max_calls_per_minute = ceil(N / 60)`.
- The pull-complete marker (`pull_sync_marker.rs`) is removed. Every
  returning writer navigation reads KV in phase 1 (section 6.3), so the
  marker saves no read. TS stops setting and reading `ts-ec-pull-complete`;
  a cookie a browser still holds expires within its one-hour `Max-Age`
  (`pull_sync_marker.rs:22`).

`dispatch_pull_sync` and `build_pull_sync_context` are removed.
`run_edgezero_pull_sync_after_send` (`adapter-fastly/src/main.rs:618`)
becomes the coordinator's phase 4 entry point.

### 10.5 Overlap with #777

The module stack in #777 replaces `[integrations.<id>]` with
`[<type>.<id>]` (for example `[identity.lockr]`) and adds `ts config migrate`.
Under that stack only the vendor protocol layer moves.
`[ec.identity.providers.<id>]` is keyed by the same provider ID and is
unaffected. Whichever lands second adapts its config paths. This spec is
written against `main`.

## 11. Adapters

KV identity runs only on Fastly. Axum, Cloudflare and Spin build an
`EcContext` for consent-gated routes, but have no `KvIdentityGraph`, no EC
finalize and no EC generation (`publisher.rs:6896-6899`).

This design changes nothing on them except the Prebid Server and origin
cookie strips, which
apply on every adapter. Providers are never dispatched there, and their
`/auction` and `/_ts/page-bids` keep resolving client EIDs without KV. When
EC identity is ported to them (#777 rows 52 to 54), phase 4 runs after send
where the platform allows it (Cloudflare's `waitUntil`), and before the
response otherwise.

## 12. Vendor examples

These examples were written against the API of sections 4.2 to 4.6 to test
it. They are illustrations of the API, not the vendors' real code, and the
framework does not depend on any detail in them. Endpoints, payloads,
consent parameters, size limits and semantics such as what Lockr's
revocation covers are decided in each vendor's implementation plan, which
replaces any shape marked "assumed" below. `IntegrationConfig` impls are
omitted; they match section 4.6. Endpoints come from operator config, shown
with
`example.com` hosts. Section 12.5 lists what writing them changed in the
API.

### 12.1 LiveRamp ATS (active resolution, expiring IDs)

LiveRamp exercises refresh of an existing ID, expiring IDs, region-dependent
lifetimes, initial resolution that needs a HEM, and headers built from
inputs. The request parameters come from #1245 §9 and LiveRamp's envelope
API documentation; the response shape is assumed. The vendor's own plan
(#1461) is the source of truth for the real protocol.

```rust
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct LiveRampConfig {
    pub enabled: bool,
    /// For example `https://ats.example.com/api/identity/v2/envelope`.
    /// Refreshes go to `<endpoint>/refresh`.
    #[validate(url)]
    pub endpoint: String,
    /// LiveRamp placement ID. An operator identifier, not a secret.
    pub placement_id: String,
    /// Countries the placement is approved for, as ISO 3166-1 alpha-2.
    pub approved_countries: Vec<String>,
}

impl IdentityProviderConfig for LiveRampConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(LIVERAMP_INTEGRATION_ID.parse()?)
            .with_source("liveramp.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::Region])
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes(LIVERAMP_TCF_PURPOSES)
                    .with_tcf_vendor(LIVERAMP_GVL_ID),
            )
            .build()
    }
}

pub struct LiveRampProvider {
    descriptor: ProviderDescriptor,
    placement_id: String,
    approved_countries: Vec<String>,
}

/// Whether the call was an initial lookup or a refresh. Carried from
/// `request` to `response`.
#[derive(Serialize, Deserialize)]
enum LiveRampCall {
    Initial,
    Refresh,
}

/// Assumed shape: V2 returns several envelope products.
#[derive(Deserialize)]
struct EnvelopeReply {
    envelopes: Vec<EnvelopeProduct>,
}

#[derive(Deserialize)]
struct EnvelopeProduct {
    #[serde(rename = "type")]
    product_type: u32,
    source: String,
    value: String,
}

/// The regular ATS envelope (#1245 §9).
const ATS_ENVELOPE_TYPE: u32 = 19;
const ATS_ENVELOPE_SOURCE: &str = "envelopeLiveramp";
/// Identifier types (#1245 §2): 4 is a hashed email, 19 an envelope refresh.
const IT_HASHED_EMAIL: &str = "4";
const IT_ENVELOPE: &str = "19";
/// Consent type 4: `cv` carries a TCF v2 string.
const CT_TCF: &str = "4";

impl IdentityProvider for LiveRampProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let region = input.region()?;
        if !self.approved_countries.iter().any(|country| country == region.country()) {
            return Ok(None);
        }

        let source = input.primary_source();
        let (call, identifier_type, identifier_value) = match input.stored(source) {
            Some(stored) if input.is_refresh(source) => {
                (LiveRampCall::Refresh, IT_ENVELOPE, stored.uid().to_owned())
            }
            _ => (LiveRampCall::Initial, IT_HASHED_EMAIL, input.hashed_email()?.to_hex()),
        };

        let consent = input.consent();
        let endpoint = match call {
            LiveRampCall::Refresh => join(input.endpoint()?, "refresh")?,
            LiveRampCall::Initial => input.endpoint()?.clone(),
        };
        let mut request = ProviderRequest::get(&endpoint)
            .with_query("pid", &self.placement_id)
            .with_query("it", identifier_type)
            .with_query("iv", &identifier_value)
            .with_query("atype", "3")
            .try_with_call_context(&call)?
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))?
            .try_with_header("x-forwarded-for", &input.client_ip()?.to_string())?;
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("ct", CT_TCF).with_query("cv", tc_string);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_sid", &ids.join(","));
        }
        Ok(Some(request))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let source = input.primary_source();
        let call: LiveRampCall = response
            .call_context()?
            .ok_or_else(|| IdentityProviderError::config("missing call context"))?;

        if response.status() == StatusCode::NO_CONTENT {
            return Ok(match call {
                LiveRampCall::Refresh => Resolution::invalidate(source),
                // The user, or the browser through DNT or GPC, opted out.
                LiveRampCall::Initial => Resolution::opted_out(),
            });
        }

        let reply: EnvelopeReply = response.success_json()?;
        let Some(envelope) = reply.envelopes.into_iter().find(|product| {
            product.product_type == ATS_ENVELOPE_TYPE && product.source == ATS_ENVELOPE_SOURCE
        }) else {
            return Ok(Resolution::no_match());
        };

        let lifetime_days = if input.region()?.is("US", "CA") { 15 } else { 30 };
        let eid = ResolvedEid::new(source, ResolvedUid::new(envelope.value).with_atype(3))
            .with_expires_in(Duration::from_secs(lifetime_days * 86_400));
        Ok(Resolution::found([eid]))
    }
}
```

What this shows:

- `request` and `response` both need to know whether the call was an
  initial lookup or a refresh, because a 204 means different things: on a
  refresh LiveRamp no longer accepts the envelope, and on an initial lookup
  the user, or the browser through DNT or GPC, is opted out. The call
  context carries it (section 12.5).
- A refresh goes to its own path, `<endpoint>/refresh`, with `it=19`.
- The example sends the HEM as a query parameter for brevity. LiveRamp
  takes it in a request body, which the vendor plan builds once HEM intake
  exists.
- A `Refresh` is attempted for an expired envelope too (section 4.4,
  `ResolveKind`). LiveRamp answers 204 for one it no longer accepts, and the
  record is invalidated.
- LiveRamp keeps no private state, so it cannot set `refresh_until`. Its
  descriptor declares `with_refresh_grace`, for as long as LiveRamp accepts
  an expired envelope for refresh, so an envelope that expires while
  LiveRamp is backed off or no navigation runs it stays a refresh anchor
  rather than falling back to `Initial`, which needs a HEM v1 does not
  have. The vendor plan (#1461) sets the length.
- `Input::HashedEmail` is declared only for `Initial`, so in v1 an owned
  `liveramp.com` source is resolved only by `Refresh`, of an envelope that
  browser ingest seeded (section 7.2). For an `Initial`, eligibility step 4
  leaves the source out because its needs are missing, so LiveRamp is not
  called at all, and `input.hashed_email()?` is never reached.
- A 451 or 4xx goes through `success_json` as `Rejected` and backs off that
  visitor only (section 8).
- The example sets no refresh hint, so the operator's `refresh_after_sec`
  (default one day) paces refreshes. Each refresh costs a provider call and
  a KV write, so a vendor hint belongs here only when LiveRamp documents
  one.

### 12.2 Lockr (passive capture)

Lockr exercises capture from several exact paths, a revocation path that must
work after consent is denied, an opt-out cleared by the vendor issuing
again, vendor key names mapped to sources, expiry in milliseconds, and a
complete
EID: every UID and the EID-level fields as the vendor returned them, with a
declared default for fields a response omits. The paths and payloads come from
the earlier Lockr capture design (`spec/lockr-identity-capture`, its section
2), measured on a live page.

```rust
const LOCKR_API: &str = "/integrations/lockr/api/publisher/app/v2/identityLockr";

/// Lockr's token key names and the sources they carry. Only confirmed
/// mappings are listed.
const KEY_SOURCES: &[(&str, &str)] = &[
    ("id5id", "id5-sync.com"),
    ("_lr_env", "liveramp.com"),
    ("__uid2_advertising_token", "uidapi.com"),
];

impl IdentityProviderConfig for LockrConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut captures = Captures::new();
        for endpoint in ["page-view", "generate-tokens", "refresh-tokens", "sync-no-hem-ids", "revoke-consent"] {
            captures = captures.with_path(Method::POST, &format!("{LOCKR_API}/{endpoint}"));
        }

        let mut builder = ProviderDescriptor::builder(LOCKR_INTEGRATION_ID.parse()?)
            .with_captures(captures);
        for (_, domain) in KEY_SOURCES {
            builder = builder.with_source(domain.parse()?);
        }
        builder
            .with_provenance(
                &"id5-sync.com".parse()?,
                EidProvenance::new().with_inserter("Lockr-For-Publishers").with_mm(2),
            )
            .build()
    }
}

#[derive(Deserialize)]
struct TokenReply {
    #[serde(rename = "aimTokens", default)]
    aim_tokens: Vec<AimToken>,
    #[serde(default)]
    ids: BTreeMap<String, LockrId>,
}

#[derive(Deserialize)]
struct AimToken {
    key_name: String,
    /// Milliseconds since the epoch.
    identity_expires: Option<u64>,
}

#[derive(Deserialize)]
struct LockrId {
    eid: LockrEid,
}

/// Lockr's own EID shape. Not `openrtb::Eid`, which denies unknown fields
/// (`openrtb.rs:72`, `:124`), so one new Lockr field would fail the whole
/// reply.
#[derive(Deserialize)]
struct LockrEid {
    source: String,
    #[serde(default)]
    uids: Vec<LockrUid>,
    inserter: Option<String>,
    matcher: Option<String>,
    mm: Option<i32>,
    ext: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct LockrUid {
    id: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
}

impl IdentityProvider for LockrProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if input.path().ends_with("/revoke-consent") {
            return Ok(Resolution::opted_out());
        }

        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        Ok(Resolution::found(reply.ids.iter().filter_map(|(key, entry)| {
            let (_, domain) = KEY_SOURCES.iter().find(|(name, _)| name == key)?;
            let source = input.source_named(domain)?;
            if !source.matches(&entry.eid.source) {
                return None;
            }
            let expires = reply
                .aim_tokens
                .iter()
                .find(|token| token.key_name == *key)
                .and_then(|token| token.identity_expires)
                .map(UnixTime::from_millis);
            if expires.is_some_and(|at| at <= input.now()) {
                return None;
            }

            let mut uids = entry.eid.uids.iter().map(resolved_uid);
            let mut eid = ResolvedEid::new(source, uids.next()?);
            for uid in uids {
                eid = eid.with_uid(uid);
            }
            if let Some(inserter) = &entry.eid.inserter {
                eid = eid.with_inserter(inserter);
            }
            if let Some(matcher) = &entry.eid.matcher {
                eid = eid.with_matcher(matcher);
            }
            if let Some(mm) = entry.eid.mm {
                eid = eid.with_mm(mm);
            }
            if let Some(ext) = &entry.eid.ext {
                eid = eid.with_ext(ext.clone());
            }
            if let Some(at) = expires {
                eid = eid.with_expires_at(at);
            }
            Some(eid)
        })))
    }
}

fn resolved_uid(uid: &LockrUid) -> ResolvedUid {
    let mut resolved = ResolvedUid::new(uid.id.clone());
    if let Some(atype) = uid.atype {
        resolved = resolved.with_atype(atype);
    }
    if let Some(ext) = &uid.ext {
        resolved = resolved.with_ext(ext.clone());
    }
    resolved
}
```

What this shows:

- **Exact paths.** A prefix would also capture `settings` and any future
  endpoint. `Captures::with_path` matches one method and path.
- **Revocation outlives consent.** `revoke-consent` arrives after the visitor
  has denied consent, so the request's consent gate would block it. A
  capture's removals must apply whatever the consent (section 12.5). It is
  provider-scoped by default (`revoke_scope = "provider"`): it removes the
  records of Lockr's sources that Lockr or browser ingest wrote, and blocks
  the browser's values for them until Lockr issues again, never the EC.
  Lockr never writes a record in v1, so without the browser-ingest part the
  revoke would remove nothing (section 5.4). An operator who sets
  `revoke_scope = "global"` makes it a full withdrawal that tombstones the
  row, from any client (section 8). Lockr's plan settles which
  scope Lockr's revocation means.
- **Capture sees a token once, and v1 does not keep it.** Lockr returns a
  token only when it first issues one. On a reload its SDK sends the saved
  ID with `noGenerate`, and the response carries no new token (#1246).
  Lockr's API calls are stagers, so the captured IDs are discarded and
  counted in `capture_unpersisted` (section 5.3). The browser path carries
  the UID browser ingest picks for each registered source (section 2.2) to
  KV: Lockr writes the ID into
  Prebid's `ortb2` config, which the EID fidelity work forwards in the
  `/auction` body and `ts-eids`, and the next writer ingests it (section
  2.3).
- **Complete EIDs.** Lockr's ID5 EID carries `inserter`, `matcher` and `mm`,
  which bidders read, and can carry more than one UID. The capture copies
  every UID and every EID-level field the response holds, so a stored set
  is what Lockr issued, under the limits of section 4.5. In v1 capture
  stores no set; the full set matters once a durable path exists (section
  15). The descriptor's
  `with_provenance` stays as the default for a response entry that sets
  none of `inserter`, `matcher` and `mm`; an entry that sets any of them
  is Lockr's complete statement (section 4.3).
- **Milliseconds.** `UnixTime::from_millis` converts Lockr's expiry, and a
  token already expired at capture time is skipped.
- **Optional expiry.** Lockr gives an expiry only for tokens listed in
  `aimTokens`, so it marks no source as expiring. An expiry it does give is
  still enforced: expired IDs are never sent (section 7.1). A provider whose
  sources differ marks only the ones that always expire, with
  `with_expiring_source`.
- **Vendor types.** Lockr's payload is parsed into local, permissive types.
  The repo's `openrtb::Eid` denies unknown fields, so it suits TS's own
  output, not a vendor's input.
- **The opt-out lasts until Lockr issues again.** Lockr has no GVL ID for a
  TC string to re-grant, so its `ps.o` is cleared by a later successful
  token capture under the request's consent, which the capture removal
  write stores (section 8).
- **Ownership and narrowing.** Lockr claims `liveramp.com` and `uidapi.com`
  too. A capture-only provider must list its `sources` (section 10.2), so
  the operator keeps only the sources Lockr delivers on that site. A Lockr
  revoke then removes and blocks only those, not IDs the ID5 or LiveRamp
  modules produced for a source Lockr is not narrowed to.

### 12.3 UID2 (secrets, crypto, private state)

UID2 exercises resolved secrets, encrypted request envelopes that need
random nonces, a nonce that `response` must check against `request`, a large
private state, and a provider-reported opt-out. The API names follow the
UID2 server-side token endpoints. The envelope crypto is elided.

```rust
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct Uid2Config {
    pub enabled: bool,
    /// For example `https://uid2.example.com`.
    #[validate(url)]
    pub base_url: String,
    /// Secret-store key names at rest, resolved values at runtime. Listed in
    /// `secret_fields()` (section 10.2), so `build_descriptor` never reads them.
    pub api_key: Redacted<String>,
    /// The UID2 operator's IAB GVL ID, when it is a TCF vendor. UID2 itself
    /// has none.
    pub tcf_vendor_id: Option<u16>,
    pub client_secret: Redacted<String>,
}

#[derive(Serialize, Deserialize)]
struct Uid2State {
    refresh_token: String,
    refresh_response_key: String,
    /// Milliseconds since the epoch.
    refresh_expires: u64,
}

#[derive(Serialize, Deserialize)]
struct GenerateCall {
    request_nonce: [u8; 8],
}

#[derive(Deserialize)]
struct Uid2Reply {
    status: String,
    body: Option<Uid2Identity>,
}

#[derive(Deserialize)]
struct Uid2Identity {
    advertising_token: String,
    refresh_token: String,
    refresh_response_key: String,
    /// Milliseconds since the epoch, like the two below.
    identity_expires: u64,
    refresh_from: u64,
    refresh_expires: u64,
}

fn join(base: &Url, path: &str) -> Result<Url, Report<IdentityProviderError>> {
    base.join(path)
        .map_err(|_| IdentityProviderError::config("`base_url` cannot take a path"))
}

// Envelope crypto, elided: AES-256-GCM as UID2's API documentation describes.
// fn encrypt_envelope(secret: &Redacted<String>, iv: &[u8; 12], now: UnixTime,
//     nonce: &[u8; 8], payload: &serde_json::Value) -> Result<String, Report<IdentityProviderError>>;
// fn decrypt_generate(secret: &Redacted<String>, body: &str, nonce: &[u8; 8])
//     -> Result<Uid2Reply, Report<IdentityProviderError>>;
// fn decrypt_refresh(key: &str, body: &str) -> Result<Uid2Reply, Report<IdentityProviderError>>;

impl IdentityProviderConfig for Uid2Config {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(UID2_INTEGRATION_ID.parse()?)
            .with_source("uidapi.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.base_url)
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail])
                    .with_private_state(),
            )
            // UID2 has no GVL ID. An operator whose UID2 operator is a TCF
            // vendor names its ID in config.
            .with_consent_policy({
                let policy = ConsentPolicy::strict().with_tcf_purposes(UID2_TCF_PURPOSES);
                match self.tcf_vendor_id {
                    Some(vendor) => policy.with_tcf_vendor(vendor),
                    None => policy.without_tcf_vendor(),
                }
            })
            .build()
    }
}

impl IdentityProvider for Uid2Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let base = input.endpoint()?;
        if input.is_refresh(input.primary_source()) {
            let Some(state) = input.state::<Uid2State>()? else {
                return Ok(None);
            };
            if UnixTime::from_millis(state.refresh_expires) <= input.now() {
                return Ok(None);
            }
            let url = join(base, "v2/token/refresh")?;
            return Ok(Some(
                // The refresh endpoint needs no API key.
                ProviderRequest::post_text(&url, &state.refresh_token),
            ));
        }

        let url = join(base, "v2/token/generate")?;
        let request_nonce = input.random_bytes::<8>();
        let envelope = encrypt_envelope(
            &self.client_secret,
            &input.random_bytes::<12>(),
            input.now(),
            &request_nonce,
            &json!({ "email_hash": input.hashed_email()?.to_base64() }),
        )?;
        Ok(Some(
            ProviderRequest::post_text(&url, &envelope)
                .with_bearer(&self.api_key)
                .try_with_call_context(&GenerateCall { request_nonce })?,
        ))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let body = response.success_text()?;
        let reply: Uid2Reply = match response.call_context::<GenerateCall>()? {
            Some(call) => decrypt_generate(&self.client_secret, body, &call.request_nonce)?,
            None => {
                let state = input
                    .state::<Uid2State>()?
                    .ok_or_else(|| IdentityProviderError::config("refresh without state"))?;
                decrypt_refresh(&state.refresh_response_key, body)?
            }
        };

        match reply.status.as_str() {
            "optout" => Ok(Resolution::opted_out()),
            // A dead refresh token clears the state instead of backing off
            // until `refresh_expires`.
            "expired_token" | "invalid_token" => Err(IdentityProviderError::InvalidState.into()),
            "success" => {
                let identity = reply.body.ok_or_else(IdentityProviderError::malformed)?;
                let uid = ResolvedUid::new(identity.advertising_token).with_atype(3);
                let eid = ResolvedEid::new(input.primary_source(), uid)
                    .with_expires_at(UnixTime::from_millis(identity.identity_expires))
                    .with_refresh_at(UnixTime::from_millis(identity.refresh_from))
                    .with_refresh_until(UnixTime::from_millis(identity.refresh_expires));
                Ok(Resolution::found([eid]).replacing_state(&Uid2State {
                    refresh_token: identity.refresh_token,
                    refresh_response_key: identity.refresh_response_key,
                    refresh_expires: identity.refresh_expires,
                }))
            }
            _ => Err(IdentityProviderError::malformed()),
        }
    }
}
```

What this shows:

- **Secrets.** `api_key` and `client_secret` are `Redacted<String>` fields
  that hold secret-store key names at rest and resolved values at runtime.
  Their paths must be added to `TrustedServerAppConfig::secret_fields()`
  (`config.rs:136-175`), and `build_descriptor` must not read them, because
  deploy validation runs before resolution (section 10.2).
- **Expired tokens refresh.** A `Refresh` runs for an expired advertising
  token too, so a visitor who misses the refresh window still recovers while
  the refresh token lives. `with_refresh_until` keeps the expired record as
  a refresh anchor until `refresh_expires` (section 5.4), and the provider
  still checks `refresh_expires` itself.
- **Randomness.** The envelope needs a random IV and nonce. A provider has no
  safe source of its own in a pure method, so `ResolveInput::random_bytes`
  supplies them, and the harness makes them deterministic.
- **Call context.** `response` must check the nonce `request` generated. The
  call context carries it, as it carries LiveRamp's call kind.
- **Text bodies.** Both endpoints send and return base64 text, not JSON, so
  `ProviderRequest::post_text` and `ProviderResponse::success_text` are
  needed.
- **State size.** A UID2 refresh token is several hundred bytes, so the state
  cap rises from 512 to 1,024 bytes (sections 4.5, 6.1).
- **Opt-out.** `"optout"` maps to `Resolution::opted_out()`, which is sticky
  (section 8).
- The crypto needs an AES-GCM implementation, a new dependency that the UID2
  spec justifies.

### 12.4 ID5, generic pull, and a provider not in v1

- **ID5** has the shape of the section 4.6 example: `ClientIp` and
  `UserAgent` needed, `PageUrl` wanted, the `signature` as private state, and
  `ext.linkType` per UID. It declares three sources (`id5-sync.com`, the
  EUID source its responses name, and `true-link-id5-sync.com`,
  `id5IdSystem.js:40-41`, `:130-170`) and uses `also_invalidate` for a
  declared source a response no longer returns. Each source's EID goes
  through `ResolvedEid` with the UID-level `ext` and EID-level fields the
  response gives, so a response that returns several UIDs or entries for a
  source stores them all.
- **Generic pull** is built in (section 10.4).
- **A local provider**, one that needs no outbound call, does not fit v1. An
  example is a publisher login ID read from a first-party cookie and
  emitted as an EID. It would need a third capability, say `derive`, and an
  input that reads a named first-party cookie. Both are additive under
  section 4.2, which is the point of the check. This spec does not add them.

### 12.5 What the examples changed

Writing the examples found these gaps. Each is now in the API of sections 4
to 6:

| Gap                                                                     | Found by          | Change                                                                                                                                  |
| ----------------------------------------------------------------------- | ----------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `response` cannot know what `request` sent                              | LiveRamp, UID2    | `ProviderRequest::try_with_call_context`, `ProviderResponse::call_context` (4.5)                                                        |
| No safe randomness in a pure method                                     | UID2              | `ResolveInput::random_bytes`, deterministic in the harness (4.2, 4.4, 13)                                                               |
| Secrets have no stated path                                             | UID2              | Secret fields in `secret_fields()`; `build_descriptor` never reads them (10.2)                                                          |
| Text bodies                                                             | UID2              | `post_text`, `success_text` (4.5)                                                                                                       |
| State cap too small for refresh tokens                                  | UID2              | 1,024 bytes (4.5, 6.1)                                                                                                                  |
| Expiry in milliseconds                                                  | Lockr, UID2       | `UnixTime::from_millis` (4.4)                                                                                                           |
| Prefix capture is too broad                                             | Lockr             | `Captures::with_path(method, path)` (4.3)                                                                                               |
| Revocation arrives after consent is denied                              | Lockr             | Capture removals bypass the consent gate (4.7)                                                                                          |
| EID-level `inserter`, `matcher` and `mm` cannot be expressed            | Lockr             | `ProviderDescriptorBuilder::with_provenance`, applied on output (4.3, 7.1)                                                              |
| A source string from a response must match a declared source            | Lockr             | `SourceDomain::matches` normalizes before comparing (4.3)                                                                               |
| An oversized UID-level `ext` discarded the whole ID                     | Lockr (ID5 `pba`) | The `ext` is dropped and the ID kept (4.5)                                                                                              |
| Region tests need country and subdivision                               | LiveRamp          | `Region::country`, `Region::is` (4.4)                                                                                                   |
| A provider could return one UID per source, with fixed EID-level fields | Lockr, ID5        | `ResolvedEid` and `ResolvedUid`: several entries and UIDs per source, EID-level fields per entry, stored and sent whole (4.5, 6.1, 7.1) |
| An expired record stranded refreshable providers                        | UID2, LiveRamp    | `Refresh` covers expired records; the provider decides (4.4, 4.7)                                                                       |
| Every example parsed its endpoint twice                                 | all               | `Resolves::with_endpoint(&str)`, checked by `build()` (4.3)                                                                             |
| Capture had two ways to say "nothing"                                   | Lockr             | `capture` returns `Resolution`; `no_match` means nothing (4.2)                                                                          |

Revision 15 removed two earlier entries. Request-body consent for capture
(`Captures::with_request_consent`) only gated positive captures, which v1
no longer stores. The dry run, and with it `is_dry_run`, saved one KV
re-read in the rare case a stateless provider skipped its call.

## 13. Testing

**Provider harness** (`ec/identity/test_support.rs`). `ProviderHarness`
drives a provider the way the coordinator does, so vendor tests stay short:

```rust
let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
let mut harness = ProviderHarness::new(&provider)
    .with_client_ip("192.0.2.10")
    .with_user_agent("ExampleBrowser/1.0");

let request = harness
    .request()
    .expect("should build a request")
    .expect("should not skip the request");
assert_eq!(
    request.url().host_str(),
    Some("api.exampleid.example.com"),
    "should call the declared host"
);

let resolution = harness
    .respond(StatusCode::OK, &json!({ "uid": "example-uid", "signature": "example-signature" }))
    .expect("should parse the response");
assert_eq!(
    resolution.eids()[0].uids()[0].id(),
    "example-uid",
    "should return the uid"
);
```

`harness.respond_raw(status, headers, bytes)` covers empty bodies such as a
204, and headers. `harness.request_json::<T>()` decodes the built body.

The harness runs the coordinator's steps around the provider, so a test
sees what production would:

- **Kind.** A source with a `with_stored` record is `Refresh` (valid or
  expired), any other `Initial`.
- **Eligibility.** It applies the consent policy (eligibility step 1) and
  step 4: sources whose kind's needs are missing are left out, and with none
  left `request()` returns `Ok(None)` without calling the provider.
- **Provenance.** Applied only by `output_eids`, as the bidstream applies
  it (section 7.1). The `Resolution` getters show what the provider set.

The harness API, in the test-support module, gated by
`#[cfg(any(test, feature = "test-utils"))]`, the crate's existing
test-helper feature. A dependent crate never sees `cfg(test)`, so a
provider in its own crate enables `test-utils` as a dev-dependency feature
of `trusted-server-core`:

```rust
impl<'p> ProviderHarness<'p> {
    pub fn new(provider: &'p dyn IdentityProvider) -> Self;
    /// Replaces every input below at once, for conformance cases.
    pub fn with_setup(self, setup: HarnessSetup) -> Self;
    pub fn with_client_ip(self, ip: &str) -> Self;
    pub fn with_user_agent(self, user_agent: &str) -> Self;
    pub fn with_page_url(self, url: &str) -> Self;
    pub fn with_region(self, country: &str, subdivision: Option<&str>) -> Self;
    pub fn with_hashed_email(self, email: HashedEmail) -> Self;
    /// Raw consent strings. Default: GDPR does not apply and no opt-outs.
    pub fn with_consent(self, consent: TestConsent) -> Self;
    pub fn with_stored(self, stored: StoredId) -> Self;
    pub fn with_state(self, state: &impl Serialize) -> Self;
    /// The fixed clock. Default: a constant test time.
    pub fn with_now(self, now: UnixTime) -> Self;
    pub fn with_random_seed(self, seed: u64) -> Self;

    /// Calls `request` and keeps the built request and its call context.
    pub fn request(&mut self) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>>;
    /// Decodes the last built request's JSON body.
    ///
    /// # Panics
    ///
    /// When no request was built or its body is not `T`.
    pub fn request_json<T: DeserializeOwned>(&self) -> T;
    /// Calls `response` with the kept call context.
    pub fn respond(
        &mut self,
        status: StatusCode,
        body: &serde_json::Value,
    ) -> Result<Resolution, Report<IdentityProviderError>>;
    pub fn respond_raw(
        &mut self,
        status: StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Resolution, Report<IdentityProviderError>>;
    /// Runs a capture as the coordinator does, response decoding included.
    pub fn capture(&self, fixture: &CaptureFixture) -> Result<Resolution, Report<IdentityProviderError>>;
    /// Resolves an expiry or refresh against the fixed clock.
    pub fn resolve(&self, when: When) -> UnixTime;
    /// The replacement state in a result, decoded, for assertions.
    pub fn state_of<T: DeserializeOwned>(&self, resolution: &Resolution) -> Option<T>;
    /// The EIDs bidders would see for a result: the section 4.5 rules, then
    /// the descriptor's provenance defaults (section 7.1).
    pub fn output_eids(&self, resolution: &Resolution) -> Vec<Eid>;

    /// Runs only the cases passed in, never the harness's own inputs. A
    /// provider declared `with_private_state()` fails here with a pointer to
    /// `check_conformance_with_state`.
    pub fn check_conformance(&self, cases: &ConformanceCases);
    pub fn check_conformance_with_state<S: Serialize + DeserializeOwned>(&self, cases: &ConformanceCases);
    /// The same checks, returning the first failure instead of failing the
    /// test. `check_conformance` wraps it. A panic aborts a `wasm32-wasip1`
    /// test run, so the harness's own negative tests use this.
    pub fn try_check_conformance(&self, cases: &ConformanceCases) -> Result<(), ConformanceFailure>;
}

/// Inputs for one harness run. The harness's own `with_*` methods delegate
/// to an inner `HarnessSetup`.
#[derive(Debug, Clone, Default)]
pub struct HarnessSetup { /* private */ }

impl HarnessSetup {
    pub fn with_client_ip(self, ip: &str) -> Self;
    pub fn with_user_agent(self, user_agent: &str) -> Self;
    pub fn with_page_url(self, url: &str) -> Self;
    pub fn with_region(self, country: &str, subdivision: Option<&str>) -> Self;
    pub fn with_hashed_email(self, email: HashedEmail) -> Self;
    pub fn with_consent(self, consent: TestConsent) -> Self;
    pub fn with_stored(self, stored: StoredId) -> Self;
    pub fn with_state(self, state: &impl Serialize) -> Self;
    pub fn with_now(self, now: UnixTime) -> Self;
    pub fn with_random_seed(self, seed: u64) -> Self;
}

/// Raw TCF, GPP and US Privacy strings, as a request would carry them.
#[derive(Debug, Clone, Default)]
pub struct TestConsent { /* private */ }

impl TestConsent {
    pub fn with_gdpr_applies(self, applies: bool) -> Self;
    pub fn with_tc_string(self, tc: &str) -> Self;
    pub fn with_gpp_string(self, gpp: &str, section_ids: &[u16]) -> Self;
    pub fn with_us_privacy(self, us_privacy: &str) -> Self;
    pub fn with_gpc(self) -> Self;
    pub fn with_dnt(self) -> Self;
}

/// One proxied call: the request a capture rule matches, and its response.
#[derive(Debug, Clone)]
pub struct CaptureFixture { /* private */ }

impl CaptureFixture {
    pub fn new(method: Method, path: &str) -> Self;
    pub fn with_response(self, status: StatusCode, body: &serde_json::Value) -> Self;
}

#[derive(Debug, Clone, Default)]
pub struct ConformanceCases { /* private */ }

impl ConformanceCases {
    /// A setup to run `request` under, and the response to answer it with.
    pub fn with_resolve_case(
        self,
        setup: HarnessSetup,
        status: StatusCode,
        response: &serde_json::Value,
    ) -> Self;
    /// For text, binary or empty replies, such as UID2's base64 body or a
    /// LiveRamp 204.
    pub fn with_resolve_case_raw(
        self,
        setup: HarnessSetup,
        status: StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Self;
    pub fn with_capture_case(self, fixture: CaptureFixture) -> Self;
}

/// Test-only constructors, behind the same gate.
impl StoredId {
    pub fn for_test(source: &SourceDomain, uid: &str) -> Self;
    /// A stored full set. `uid()` becomes its first UID.
    pub fn with_eids(self, eids: Vec<Eid>) -> Self;
    pub fn written_by(self, writer: &IdentityProviderId) -> Self;
    pub fn expiring_at(self, at: UnixTime) -> Self;
}

impl HashedEmail {
    /// From the SHA-256 of a normalized address, as 64 hex digits.
    pub fn for_test(sha256_hex: &str) -> Self;
}

pub fn check_descriptor_ignores_secrets<C: IdentityProviderConfig>(placeholder: &C, resolved: &C);
```

`harness.check_conformance(&cases)` runs the checks every provider must
pass. `ConformanceCases` lists the setups to try (inputs, stored IDs, state
and a test-only `HashedEmail`) and one vendor fixture per capture rule, so the
harness never guesses which setup makes a provider call out:

- The harness builds inputs that hold only what the descriptor declares, so
  reading an undeclared input fails the test with `UndeclaredInput`.
- A provider that declares `resolves` returns `Some` from `request` for at
  least one listed setup, and its `response` on a vendor fixture does not
  return the `not_implemented` marker.
- A provider that declares `captures` returns a `Resolution` other than
  `no_match` for at least one listed fixture, and never the
  `not_implemented` marker.
- No undeclared input or state read is recorded.
- A provider without `with_private_state()` never returns state.
- Requests go only to declared hosts, over HTTPS, with no `Cookie` header and
  with valid header names and values.
- Returned entries are only for declared sources, and no fixture's result
  hits a limit of section 4.5: no dropped UID or field, no discarded entry
  and no set over `MAX_ENTRY_SET_BYTES`. A vendor whose real responses can
  exceed a limit says so in its spec; the fixture does not hide it.
- `ResolvedEid::expiry()` and `refresh()` report what the provider set, and
  a fixture set of a source marked expiring carries an expiry.
- `capture` never returns a state change.
- For every case with state, the harness also injects a stored state that
  is a bare JSON string, which no struct decodes, and checks that the
  provider neither panics nor treats it as valid state. A
  `#[serde(default)]` state type that decodes anything fails here. That the
  coordinator then clears it is a coordinator test.

`harness.check_conformance_with_state::<S>(&cases)` also checks that the
state type round-trips through `replacing_state` and `input.state()`.
`check_descriptor_ignores_secrets::<C>(&placeholder, &resolved)` builds the
descriptor from a config with placeholder secret names and from one with
resolved secrets, and fails if the two differ (section 10.2). Vendor tests
add a
fixture response for each outcome the vendor can return, and assert it
through the `Resolution` getters.

The harness also:

- fixes `random_bytes` with `with_random_seed`, so encrypted requests are
  reproducible
- carries the built request's call context into `respond`, as the
  coordinator does
- applies a provider's provenance in `output_eids`, so a test can assert the
  EID-level fields bidders will see

The examples in section 12 are the first harness cases: LiveRamp's initial
and refresh 204s (opted out and invalidated), Lockr's page-view and
revoke-consent, and UID2's success, refresh, opt-out and dead refresh
token.

**Coordinator.**

- An eligibility matrix: consent × ownership × per-source kind × due state ×
  inputs × mode × writer or stager × new or returning EC. No stager
  dispatches a resolving provider in any cell.
- The eligibility view is the phase 1 KV row alone: `ts-eids-s` never
  changes eligibility, whatever it holds.
- On a site whose auctions all go through `/auction`, a hold-mode provider
  runs after send on the next writer navigation that runs no auction, its
  answer reaches KV, and the following `/auction` serves it from KV without calling the provider. Moved after send, it waits `after_send_timeout_ms`, and a miss backs off as a timeout, not as a hold cut-off.
- A `no_match` or 4xx on a writer stores `ps.n`, so a run of `/auction` and
  `/_ts/page-bids` calls in the same SPA session never calls the provider
  again before `ps.n`.
- `sources` narrowing, owner takeover of a browser-seeded record, sticky
  `ps.o` surviving a jurisdiction change and cleared only by a newer grant,
  and `ps.iv` stopping a re-seed.
- Hold budget: a fake HTTP client with controlled latency shows the auction
  starts at the budget, an answer joins the writer's mutation, and a
  cut-off call sets `hold_retry_sec` without counting a failure.
- `request` is called once per dispatch. A `None` or `InputUnavailable`
  waits `refresh_after_sec`.
- After-send results commit in the same phase 4 write. Two overlapping
  writer navigations for one EC do not both dispatch a provider within the
  burst cap's window.
- A navigation answered with a status other than 2xx or 304 dispatches no
  after-send provider, stores no positive hold-mode answer, and flushes
  those answers' removals, opt-outs and backoff, so a `no_match` or 4xx on a
  404 page is not called again before its `ps.n`.
- An `UndeclaredInput` is logged as `Config` and backs off.
- Only transport failures, timeouts, 429 and 5xx feed the breaker. 4xx and
  `Malformed` from many visitors never open it.
- The burst cap is checked after every other eligibility step.
- A provider call that would take the request past `max_backend_requests`
  is not sent and counts as no failure; a hold call leaves room for the
  origin and every auction provider.
- Passive capture runs from `handle_proxy` only for providers with a capture
  rule matching the request, decodes gzip and brotli bodies under the caps,
  never reads the request body, and never touches `n`, `f` or the breaker.
- A capture writes no positive ID itself and increments
  `capture_unpersisted` without logging a value. A positive change that
  passes the consent steps and differs from KV joins the writer's mutation,
  or, on any other request, the pending key after send. Its removals,
  opt-out and any clear are written by the capture removal write on that
  request, before send.
- The capture removal write retries only a CAS conflict, up to five
  attempts, tries once more after send when those fail, and on a second
  failure appends to the pending key and increments `removal_deferred`. It
  increments `removal_lost`, without logging a value, only when that append
  fails too. It
  writes nothing for a tombstone, a missing row or an already applied
  removal, and it runs on the unusable-consent path.
- A writer navigation whose phase 1 snapshot predates a capture removal
  write of the same page load fails its CAS and is not retried; the next
  navigation re-derives its browser ingest.
- A capture from a client that fails `looks_like_browser()` writes its
  removals and opt-out, and its global revoke tombstones the row, as from a
  real browser. Its positive result never clears an opt-out.
- With `revoke_scope = "global"`, a captured opt-out tombstones the row and clears the EC cookie on that request, as a consent
  withdrawal does. With `"provider"`, it removes only that provider's
  records and, for a capture-only provider, the records of its sources no
  provider wrote.
- A Lockr-style `revoke-consent` captured on the proxy removes the
  browser-ingested record of each of Lockr's narrowed sources, adds their
  UIDs to `ps.ov`, and drops the browser's values for those sources. A later
  token capture clears the opt-out and empties `ps.ov`, and the next writer
  navigation ingests the same stable UID again. A source Lockr is not
  narrowed to keeps its browser value throughout.
- A due stateful provider is dispatched in phase 4 with its request built
  from the live state, and a writer navigation without an auction runs it
  after send.
- A provider's `ResolveKind` is `Refresh` for a stored record whether it is
  still valid or expired, and an expired record counts as absent for
  `FillMissing`.
- An expired UID2-style ID with valid refresh state survives an unrelated
  writer's mutation as a refresh anchor, and the next eligibility pass
  selects `Refresh`. Past `ru`, or once the state is cleared, missing or
  fails to decode, the anchor is dropped and the kind is `Initial`.
- A stateful result applies whole. When a phase 4 re-read finds another
  writer's newer ID and state, the call's IDs, state, `n` and `f` are all
  dropped and the row keeps the other writer's ID with that writer's state.
  An oversized or unserializable replacement state stores no ID, and the
  result's removals still apply.

**Consent.**

- The GPP sharing and targeted-advertising opt-outs, GPP section 6, and the
  US National GPC and known-child fields decode from each section that
  defines them. Each one blocks a strict policy.
- A raw consent string that does not decode blocks a strict policy.
- A decodable GPP string with no applicable section, the header-only
  `DBAA` shape, does not block a strict policy where TCF does not apply.
- A targeted-advertising opt-out without a sale opt-out produces no
  server-side IDs in `user.eids` and none in the `ts-eids-s` mirror
  payload.
- The consent fingerprint is a plain bitmask: a GPP opt-out clears exactly
  when `fp` records a US opt-out bit and the current consent shows none of
  the opt-outs `fp` records, whatever else changed.

**One-write invariant.** A new test store counts writes by mode (Add,
`IfGenerationMatch`, Overwrite) and is shared through the `test-utils` feature
so adapter tests can use it. `KvIdentityGraph::counting` counts reads only
(`ec/kv.rs:1596-1605`, `ec/kv_backend.rs:205-221`).

| Scenario                                                            | KV writes                                                                               |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| New visitor, navigation                                             | 1 (create, before send)                                                                 |
| New visitor whose first ID collides                                 | 1 (create with a re-minted ID)                                                          |
| Returning visitor, changes pending                                  | 1                                                                                       |
| Returning visitor, nothing changed                                  | 0                                                                                       |
| 304 revalidation with changes pending                               | 1                                                                                       |
| Returning EC, navigation answered with 3xx, 4xx or 5xx              | 0, or 1 when its hold-mode calls returned removals or backoff                           |
| New visitor, prefetch or prerender navigation                       | 1 (create, before send)                                                                 |
| Returning visitor, prefetch or prerender navigation                 | 0                                                                                       |
| New visitor, client without `Sec-Fetch-Dest`                        | 1 (create, before send)                                                                 |
| Returning visitor, iframe HTML or no `Sec-Fetch-Dest`               | 0                                                                                       |
| Integration-proxy navigation that mints an EC                       | 1 (create)                                                                              |
| `/auction`, `/_ts/page-bids`, `/identify`, assets                   | 0                                                                                       |
| Lockr API proxy, no revocation                                      | 0                                                                                       |
| Captured revocation, from any client                                | CAS removal write (up to 5 attempts), once more after send                              |
| Consent withdrawal, on any request                                  | CAS tombstone (up to 5 attempts) plus marker, per ID; once more after send              |
| Global revoke captured, from any client                             | as a consent withdrawal                                                                 |
| Capture removal, opt-out or global revoke from a non-browser client | 0                                                                                       |
| CAS conflict or per-key limit on the flush                          | 1 attempt; its removals, if any, then written alone after a 1 s wait (up to 5 attempts) |

**Withdrawal and removal.**

- A withdrawal on `/auction` after the navigation's send leaves a tombstone,
  and the navigation's phase 4 drops its mutation and calls no provider.
- A withdrawal tombstone whose attempts fail before send is tried once more
  after send.
- After `opted_out` or `invalidate`, the removed ID stops reaching bidders
  through TS on the next request, and the next `/auction` or
  `/_ts/page-bids` response leaves it out of the mirror payload.
- An `/auction` or `/_ts/page-bids` response whose KV read finds a
  tombstone, a missing row or `consent.ok = false`, or whose consent denies
  EID persistence, returns `clear` for the mirror, and the shim expires
  `ts-eids-s`.
- A deletion tombstone is honoured at the visitor's next writer navigation
  or `/auction`, whatever `ts-eids-s` holds.
- No response sets a server identity cookie other than `ts-ec`.

**Failed flush.** After a refused write, the mutation's removals, opt-outs and clears are written alone with up to five CAS attempts, and a mutation without them makes no second write. When those attempts fail too, the request's own removals go to the pending key, and `removal_lost` is incremented, without logging a value, only when that append fails. The next writer navigation merges the pending key, re-derives browser ingest from `ts-eids` and calls the still-due providers again within the burst cap.

**Reads.** Every returning writer navigation pays one KV read of the row in
phase 1 and one of the pending key in phase 4, plus the phase 4 re-read
when an after-send provider dispatches.
`/auction` and `/_ts/page-bids` pay their existing one read and no more. No
request opens or parses `ts-eids-s`.

**No lost EIDs (G10).**

- The shim writes no unregistered source into `ts-eids`, and keeps a
  registered source a later request lacks.
- A registered source present in the `/auction` body but beyond today's
  trim is kept in `ts-eids` and written by the next navigation.
- When the registered sources alone exceed the trim, only those the trim
  drops are lost, and the test pins which.
- Every source today's `/auction` ingest would write reaches KV whenever
  the registered sources fit the trim.
- An EID present only in Prebid's `ortb2` config, as Lockr writes its ID5,
  reaches `ts-eids` and KV.

**Orphans and collisions (section 5.5).**

- A row deleted out of band is found by the next writer navigation's phase
  1 read and recovered by rotation before send.
- A stager that sees the row missing stores nothing and returns `clear` for
  the mirror, and the next writer recovers.
- A collision on the first create re-mints before any cookie is set.

**Storage.**

- A schema version 1 row reads as version 2, each `uid` as one entry with
  one UID, and its next write stores version 2. A version 2 row and `ps`
  round-trip.
- An owned expiring source's record is served from the server only when
  its `w` is the owner and its `exp` has not passed.
- Revision-checked `OwnerRemove` and `OwnerWrite` lose to a newer write.
  Re-applying changes on a later writer keeps the other tab's change. An
  `OwnerRemove` of a version 1 record matches `Revision::Unset` until the
  record's next write.
- Two refreshes of an unchanged UID in the same second get different `rv`,
  so an invalidation read before the second refresh does not remove it.
- An opt-out and a clear in the same second leave the provider opted out.
  A clear with no opt-out to clear writes nothing, and a clear not later
  than the opt-out is dropped.
- `ps.iv` and `ps.ov` hold unkeyed hashes, and they still match after an
  `ec.passphrase` rotation, as does the consent fingerprint.
- `ts-eids-s`:
  - the mirror payload holds only what bidders would receive from the
    server set: no `ps`, `ps.st`, refresh token, revision, writer or timing
    field, and nothing a consent gate or `bidstream_enabled = false`
    excludes
  - `value` is base64url JSON; `max_age` is at most 86,400 and never past
    the earliest `exp`; an empty set returns `clear`; a request with no KV
    read returns no payload
  - the 2,048-byte cap leaves out whole sources in ascending
    `cookie_priority`, never trims a set, and a left-out source still
    reaches bidders
  - the server never opens it: a forged, stale or malformed `ts-eids-s`
    changes no eligibility, merge, `/identify` answer or KV write
  - no response sets it with `Set-Cookie`
  - the shim writes it as `__Host-ts-eids-s` with `Path=/`, `Secure` and
    `SameSite=Lax`, without `HttpOnly`, skips an unchanged value, expires
    it on `clear`, and never copies it into `ts-eids` or the `/auction`
    body
  - a mirror ID a page script feeds back through Prebid is ingested only as
    a one-UID `FillMissing`, and the `ps.iv`, `ps.ov` and `ps.o` denials of
    section
    7.1 filter it for owned and unowned sources
- The admin lookup redacts `ps.st`.

**Outbound requests.** Requests to Prebid Server, in every
consent-forwarding mode, carry none of the `TS_IDENTITY_COOKIE_NAMES`
cookies, keep every other cookie, and drop the header when nothing is left.
Requests to the publisher origin never carry `__Host-ts-eids-s`, keep
`ts-ec`, `ts-eids` and every other cookie, and drop the header when nothing
is left. With `strip_eids_cookie_from_origin`, they carry no `ts-eids`
either.

**Merge.**

- Owned versus unowned precedence.
- A browser-seeded record never beats the browser's current value.
- Expired IDs are never sent.
- `x-ts-eids` matches what was sent.
- After a provider invalidates UID A, or reports an opt-out that removes
  it, a stale A in the `/auction` body or in `ts-eids` is not sent, for an
  owned or an unowned source.
- An unowned source a strict provider wrote stops reaching bidders and
  `/identify` once that provider's vendor consent is denied or a
  targeted-advertising opt-out applies. A record whose writer is disabled
  is not served, and the browser can refill it.
- `/_ts/api/v1/identify` omits expired IDs and IDs the writer's or owner's
  policy blocks under the request's consent. It never uses the stored
  consent snapshot. A request from another jurisdiction, one with no
  consent string, and a later vendor denial while global EC consent is
  still granted each get that policy's answer for the request.

**Batch sync.** An update to an owned source applies only when absent.

**Complete provider EIDs (G13).**

- A result with several entries for a source, several UIDs per entry and
  every UID-level and EID-level field round-trips through the mutation, KV
  and the merge, and reaches `user.ext.eids`, `x-ts-eids` and the
  `ts-eids-s` mirror in the order the provider returned it.
- Each limit of section 4.5 drops exactly what it names: a blank `inserter`,
  an `ext` over 2,048 bytes, a ninth entry, a 33rd UID, a duplicate entry.
  A set over 4,096 bytes, measured by the shared `MAX_ENTRY_SET_BYTES`
  function, is discarded whole and leaves the stored set as it is; for a
  stateful provider the positive part of the result is rejected with its
  removals kept; a stateless result all of whose sets are discarded is
  handled as `Malformed`.
- For an owned source, a result that names it replaces its whole set; a
  later result without one entry removes it. A source the result does not
  name is left as it is. An unowned provider's set only fills an absent
  record.
- The set's `exp`, `ra` and `ru` are the earliest its kept entries set, and
  an expired set is never served. Expired means `now >= exp`.
- Descriptor provenance applies only to an entry with no EID-level field;
  an entry with any keeps exactly what the provider set.
- `build()` rejects a blank provenance `inserter` or `matcher` and an `mm`
  below 0.
- An owner's valid set replaces every browser entry for the source,
  including repeated browser entries; an unowned set goes out only when the
  browser did not send the source.
- An invalidation of a 32-UID set appends 32 hashes to `ps.iv` from the
  live record it removes, and the filter then holds through `ps.iv`. Two
  such invalidations keep all 64 hashes.
- A full set too large for the `ts-eids-s` mirror is left out of it whole,
  never trimmed, and still reaches bidders from KV.
- `KvEntry::validate` rejects, on write, a record with an empty `eids`. On
  read, a record that breaks a limit is dropped from the row with its
  source logged, and the rest of the row reads normally.
- A stored entry with a field this build does not know reads normally.
- `/identify` returns the same `uid` and `eid` shape for a full set as for
  a one-UID record: the first UID, with the partner's `openrtb_atype`.
- A mutation on a build that no longer configures a provider keeps that
  provider's `ps.o`, `ps.oc` and `ps.ov` and drops its other `ps` fields.

**Behaviour that predates this spec (G13).** Golden tests pin each path that
G13 must not change:

- browser ingest writes a one-UID record holding the UID
  `collect_eid_updates` picks today (for a repeated source, the first valid
  UID of its last entry; the sharedId cookie last), and nothing for an
  unregistered source, whatever EID-level fields or extra UIDs the browser
  sent. The expected values are captured from `main`, not written from
  this sentence
- the `x-ts-eids` header's per-entry truncation
- the `ts-eids` cookie's EID-level `ext` removal and trim order, apart from
  the registered-only merge G10 adds
- `/auction` body forwarding and the merge of browser entries
- a row read from `main`'s schema version 1 goes out to bidders exactly as
  it does on `main`, and the golden requests of the EID fidelity spec
  (§4.7) do not change

**Migration parity.** Today's `ec/pull_sync.rs` tests are ported against
`GenericPullProvider`. Each behaviour change in section 10.4 gets its own
test.

**Config.** Each validation error in section 10.2, `build_descriptor`
running in `ts config validate` without secrets, and acceptance of a
config with no `[ec.identity]` at all.

**Adapters.** Phase 4 runs after send on Fastly. Axum, Cloudflare and Spin
behave as before apart from the Prebid Server and origin cookie strips. The
parity test crate passes
(`cargo test --manifest-path
crates/trusted-server-integration-tests/Cargo.toml --test parity`).

## 14. Rollout

Three steps, each shippable on its own and each able to roll back to the
one before:

0. **Reader first.** A release from `main`, before any of this spec, whose
   only change is to the KV reader. It writes exactly what today's build
   writes, schema version 1, and it ships so that step 1 has a floor to
   roll back to, and so old and new instances can run side by side while
   step 1 deploys. It:
   - accepts a version 2 row (section 6.1) where today's
     `KvEntry::validate` rejects it (`ec/kv_types.rs:376-381`). It reads the
     row into today's in-memory shape: per source, the first UID of the
     first entry, with `w`, `exp` and `at`, and nothing else.
   - serves a version 2 record only when it is unexpired and its row holds
     no provider opt-out (`ps.o` without a later `ps.oc`). It cannot tell
     which sources an opt-out covers, so a row with one serves no stored
     ID. Servability is otherwise today's.
   - never rewrites a version 2 row with a positive change. Browser ingest,
     `/auction` ingest and pull sync skip it and increment
     `v2_row_write_skipped`, so a step 0 instance never downgrades a row
     to version 1 and never drops `ps`, full entry sets or removal hashes.
   - tombstones a version 2 row on a consent withdrawal as it does a
     version 1 row. A tombstone holds no IDs, so its version does not
     matter, and a withdrawal during a rollback is recorded.

   Step 0 has no runtime switch and changes no response.

1. **One write, no new vendors.** This step includes:
   - the outbound cookie strips (origin and Prebid Server) and the
     template cache's identity cookie list. Stripping `ts-ec` and `ts-eids`
     from Prebid Server requests is a behaviour change: both reach it today
     (`integrations/prebid.rs:2088-2108`). The origin keeps both unless the
     operator sets `strip_eids_cookie_from_origin`
   - the writer-request rule and the `IdentityMutation` single flush, with no retry except the removal-only one of section 5.2, phase 4; the new visitor's create stays before routing, as today
   - `/auction`, `/_ts/page-bids` and `/identify` stopping their writes
   - one after-send attempt for a withdrawal tombstone that failed before
     send (section 5.1)
   - the `ts-eids-s` mirror payload in `/auction` and `/_ts/page-bids`
     responses, and the shim code that writes and clears the cookie, behind
     `[ec.identity] mirror_enabled`, off by default until section 15 names
     a consumer
   - the shim's registered-only, per-source merge of `ts-eids`, with the
     registered source list in the injected configuration (section 5.3)
   - a metric for the inbound `Cookie` header size
   - KV schema version 2 (section 6.1): the one record shape, `ps`, with
     `Default` and constructors, and the read of version 1 rows. Full entry
     sets are read, served and validated, though nothing in this step
     writes one, so a rollback from step 2 to step 1 keeps every set a
     provider wrote readable, and keeps each vendor's opt-out (below)
   - `GenericPullProvider` replacing pull sync, which brings the trait, the
     descriptor, the harness and the write-counting test store, and the
     removal of the pull-complete marker and of the legacy pull sync keys
     (section 10.4)
   - the shared servability check for `/identify` and the bidstream

   It changes behaviour on its own: fewer KV writes, `/auction`,
   `/_ts/page-bids` and `/identify` stop writing, every returning writer
   navigation reads the row and the pending key, and new visitors reach pull partners from
   their second page load. Write and read counters per page load show the
   effect. Operators move `pull_sync_ttl_sec`, `pull_sync_rate_limit` and
   `ec.pull_sync_concurrency` to their replacements before deploying it,
   because validation rejects them (section 10.2).

2. **Providers.** `.with_identity_provider` and
   `.try_with_identity_provider` for vendors, ownership and its
   validation, hold mode, revoke-only passive capture with `revoke_scope`
   defaulting to `"provider"`, the capture removal write with its
   `capture_unpersisted` and `removal_lost` counters, the
   real-browser gate on capture clears, provider state, the rate-limiter
   `record` and `count` methods and the circuit breaker, the consent
   decoder extensions of section 9.1, and the first full-set writes, from
   provider results with more than one entry or UID or with EID-level
   fields (G13). Vendor modules follow in their own specs.

**Capture provider checklist.** Before enabling a capture provider on a
live service:

- Each source it declares has an `[[ec.partners]]` entry with
  `bidstream_enabled = true` and the right `openrtb_atype`. Lockr's ID5 uses
  `1` (#1246).
- Its required `sources` list holds only the sources it is the delivery path
  for, because its opt-out removes browser-ingested records of every source
  it keeps, whoever produced them (sections 5.4, 10.2).
- A source moving to an owner is moved off pull sync first. A
  `pull_sync_enabled` entry registers a generic pull provider that claims
  the source, and two providers cannot claim an owned source (section 10.2).
- Live acceptance: after a revocation captured on a live page,
  `/_ts/admin/ec` (`ec/admin.rs:276`) shows the provider's `ps.o` and no
  record for its sources. After the vendor issues again, it shows `ps.oc`,
  and the next writer navigation ingests the source again.

**Rolling back from step 2 to step 1.** Step 1 configures no vendor
providers, so:

- Remove `owner` and the vendor `[ec.identity.providers.*]` tables from the
  config first. Step 1's validation rejects an owner or a provider table it
  does not know (section 10.2).
- Full sets stay readable but are not served while their writer is not
  configured, and browser ingest may replace them (section 7.1).
- A mutation drops each vendor's `ps` fields except `o`, `oc` and `ov`
  (section 5.4), so opt-outs and the IDs they removed survive, and `ps.iv`
  and `ps.st` do not: invalidated values can be re-seeded once, and
  stateful providers start over when rolled forward.

**Rolling back from step 1 to step 0.** Step 0 reads every row step 1
wrote and serves its IDs under the rules above, so a rollback, or a rolling
deploy with both builds live, loses no stored ID and no withdrawal. What it
costs:

- Rows step 1 wrote stop changing until step 1 returns: their ingest and
  pull sync writes are skipped (`v2_row_write_skipped`), and provider
  backoff in `ps` stops advancing because no provider runs.
- A row with a provider opt-out serves no stored ID during the rollback,
  which is stricter than step 1.
- Rows step 0 writes are version 1, and step 1 reads and upgrades them on
  its next write, as for any version 1 row.

Rolling back below step 0 is unsupported: an older build rejects every
version 2 row (`ec/kv_types.rs:376-381`), so old instances serve no stored
ID and fail closed on withdrawals for those rows. Going back that far needs
a KV migration to version 1, which this spec does not provide.

## 15. Open questions

1. **How long single-page apps wait.** Some sites are single-page apps:
   after the first page loads, they change pages with JavaScript instead of
   loading a new document from TS. TS hears about those page changes only
   through `/_ts/page-bids`, which never writes KV and calls no provider
   (section 5.1). So on those sites, browser ingest and provider refreshes
   wait until the visitor's next full page load. After launch, measure how
   long they wait, and revisit if the wait is long. **Decided (round 14):**
   a same-origin `/_ts/page-bids` call from a real browser is the writer
   once `spa_writer_interval_sec` (300) has passed since the row's last
   writer flush, with after-send providers only (section 5.1, The SPA
   writer).
2. **Lost removals.** A removal write that fails all its attempts is lost
   (sections 5.2, 5.3). After launch, watch
   `removal_lost`. If it is not close to zero, should a durable
   fallback come back, and in what form? **Decided (round 14):** the
   pending key (section 5.3) holds it for the next writer;
   `removal_lost` now counts only a removal whose append failed too.
3. **Global revoke and a granting CMP.** A global revoke clears the EC
   cookie, as a withdrawal does, but the CMP's consent may still grant. The
   next navigation then mints a new EC. Should a global revoke also stop
   minting until consent changes? **Decided (round 14):** yes. The global
   scope stays, and the `ts-ec-revoked` marker stops minting until the
   consent fingerprint changes (section 8, Revoke scope).
4. **Durable stager results.** Stagers persist no positive ID (section
   5.3), so a positive Lockr capture, with its full entry set, reaches KV
   only as the one UID browser ingest picks. Should a durable path
   come back for it, for example a capture-driven write within G4, if
   `capture_unpersisted` shows that captures carry IDs or fields the
   browser path does not? **Decided (round 14):** yes, through the pending
   key (section 5.3). A positive capture that differs from KV is merged by
   the next writer within its one write, so G4 holds; auctions do not read
   the key.
5. **Who reads `ts-eids-s`, and does it meet the product rule?** The
   product rule is that provider IDs go to a first-party cookie and to KV,
   with the cookie read first. Since revision 14 the server never reads the
   cookie, and no page script is named as its reader (section 6.2). Before
   `mirror_enabled` defaults to on, name its consumer and confirm with the
   product lead that a client-only mirror meets the rule. Without a
   consumer, the mirror costs about 2 KB on every request to the host and
   exposes server-only IDs to page scripts for nothing. **Decided (round
   14):** hold the mirror. It ships gated off (`mirror_enabled = false`)
   until a consumer is named.
6. **Providers on a new visitor's first navigation.** No provider runs on
   a first navigation (section 4.7, step 7), so a visitor who leaves after
   one page never gets a provider ID. One option keeps one write and never
   uses an unconfirmed ID: mint the EC, run hold-mode providers (which need
   no EC ID; only generic pull does), create the row with their answers,
   then run the auction. It moves the create from before routing into
   routing, and adds the hold budget to every first navigation that runs
   an auction. Is that worth it? **Decided (round 14):** yes, when such a
   provider is configured (section 4.7, new visitors).

## 16. Decisions log

Decisions made while writing this spec, with the spec author (dhruv8sh):

| Decision                         | Choice                                                                                                                                                                                                                                   |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Scope                            | Framework and trait. Vendors get their own specs.                                                                                                                                                                                        |
| HEM input                        | Typed slot, always empty in v1. Intake is a later spec.                                                                                                                                                                                  |
| Browser vs server precedence     | Configured per source through `owner`. The owner's valid value wins.                                                                                                                                                                     |
| Cookie for server IDs            | Separate `ts-eids-s`, a client-only mirror the shim writes and the server never reads. Revision 14 replaced the server-owned, sealed, HttpOnly cookie of revisions 1 to 13. Revision 15 removed the `ts-eids-r` removal cookies as well. |
| Structure                        | Trait plus coordinator, with an optional passive-capture capability.                                                                                                                                                                     |
| Trait shape                      | One required method, `descriptor`. Capabilities are default methods. Synchronous. Additive by construction.                                                                                                                              |
| Inputs                           | Declared in the descriptor and enforced: a provider can read only what it declared.                                                                                                                                                      |
| Page load for the one-write rule | A navigation plus every request it causes. Only the navigation writes and only the navigation calls providers. Strict.                                                                                                                   |
| New-visitor write                | One create, carrying browser EIDs. Before routing, or, with a hold-mode provider that does not read the EC ID, after the hold dispatch of a navigation that runs an auction, carrying its answers. Changed in rounds 3, 4 and 14.        |
| New-visitor recovery             | Today's rotation. The cookie-proof recreation of revision 3 is removed with the deferred create.                                                                                                                                         |
| Failed write                     | Not retried. Changed from one merge-retry once Fastly's per-key limit was confirmed.                                                                                                                                                     |
| Browser EIDs beyond the cookie   | The shim writes only registered sources into `ts-eids`, merged per source, and the next navigation ingests them. Revision 14 removed the KV gap that `/auction` saved in `ts-eids-s`.                                                    |
| Delay on body-less routes        | Accepted: a browser ID first seen in an `/auction` body reaches KV one page view later.                                                                                                                                                  |
| SPA views (`/_ts/page-bids`)     | Stagers. They never write KV and call no provider.                                                                                                                                                                                       |
| Hold budget                      | `hold_budget_ms` defaults to 100 ms, reduced from 300 ms (section 4.7 gives the reasons).                                                                                                                                                |
| Outbound cookies                 | TS's identity cookies are stripped from Prebid Server requests. The origin keeps `ts-ec` and `ts-eids` and loses only `ts-eids-s`; `strip_eids_cookie_from_origin` strips `ts-eids` too. Changed after the round 14 review.              |
| Deletion                         | A tombstone with the 24-hour `TOMBSTONE_TTL` replaces the hard delete. No extra writes.                                                                                                                                                  |
| GPP opt-outs                     | Sharing and targeted-advertising opt-outs are decoded, and the strict policy blocks them.                                                                                                                                                |
| `x-ts-eids`                      | Kept as a full troubleshooting header. Its exposure of server-only IDs to page scripts is accepted.                                                                                                                                      |
| Consent withdrawal               | Synchronous tombstone on any request, a G4 exception with the two removal writes. Revision 15 adds one after-send attempt when it fails. Confirmed by the spec author after the round 14 review.                                         |
| Equal-second ties                | A removal or opt-out wins a tie with an acquisition or clear. An ID re-acquired in that second is resolved again later. Confirmed after round 10.                                                                                        |
| Complete provider EIDs           | A provider can return several EID entries per source, several UIDs each, with UID-level and EID-level fields. The set is stored and sent whole. Added in revision 13.                                                                    |
| Entry-set writer                 | One writer per source. A result replaces the source's set whole, and one `rv` covers it, so ownership, revisions and removals stay per source.                                                                                           |
| Behaviour before this spec       | G13 changes none of it. Browser ingest, `ts-eids`, batch sync, generic pull and `/identify` keep the behaviour the rest of this spec gives them and write one entry with one UID; only provider results write more.                      |
| Removal hashes                   | Derived when a removal applies, from the live record it removes. Unkeyed, ordered, at most 128 per list. Invalidations go to `ps.iv`, kept; opt-out removals go to `ps.ov`, emptied by the clear.                                        |
| Provenance default               | Applied only to an entry that sets no EID-level field, so vendor and config provenance never mix in one entry. Added in round 13.                                                                                                        |
| Mirror lifetime                  | `ts-eids-s` lives at most a day and never past the earliest `exp` it holds.                                                                                                                                                              |

Revision 15 applies these decisions, made by the spec author (dhruv8sh) after the round 14
review, which allowed breaking changes:

| Decision                  | Choice                                                                                                                                                                                                                                                                             |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Stager providers          | None. Resolving providers run only on writer navigations, so every outcome, backoff included, is stored. Stagers serve KV                                                                                                                                                          |
| Removals                  | Written to KV on the request that carries them. `ts-eids-r`, its keyring and secrets, the overflow write and the strips-only rollout step are removed                                                                                                                              |
| Non-browser removals      | Applied, as consent withdrawals are; only clears need a real browser. Changed after the round 14 review from dropping them                                                                                                                                                         |
| Expiry scope              | G7 covers owned expiring sources. A stateless owner keeps its expired record as a refresh anchor until `exp` plus its declared refresh grace. Unowned IDs keep no lifetime (section 2.3). Changed after the round 14 review                                                        |
| Opt-out clears            | A TCF re-grant clears only a denial `fp` recorded (new TCF bit 16), with no `LastUpdated` comparison. A clear stores a watermark, the `at` of the opt-out it read, as `ps.oc`, and clears apply before opt-outs. Changed after the round 14 review                                 |
| Backend budget            | `max_backend_requests` (default 32) bounds origin, auction and provider calls in one execution; a provider call past it is skipped, not a failure, and a hold call reserves the origin and the auction. Added after the round 14 review                                            |
| Per-IP ceiling            | `max_calls_per_ip_per_minute` (default 10) caps one provider's calls per client address across visitors, keyed on an HMAC of the IP, next to the per-visitor cap. Breaker failures are not required to come from distinct visitors. Added after the round 14 review                |
| Reader-first rollout      | Rollout step 0, from `main`, reads version 2 rows, never rewrites them except to tombstone, and serves no stored ID from a row with a provider opt-out, so step 1 can roll back and deploy alongside old instances. Added after the round 14 review                                |
| Global revoke minting     | A global revoke also sets `ts-ec-revoked` with the consent fingerprint, and no EC is minted until the request's fingerprint differs. Decided after the round 14 review                                                                                                             |
| Pending key               | Removals whose writes all failed, and positive captures from a request that is not the writer, go to `pending:<ec_id>` (one-day TTL, CAS append, 16 items). Every writer reads it and merges items above the row's `pg`. Auctions never read it. Decided after the round 14 review |
| SPA writer                | A same-origin `/_ts/page-bids` call from a real browser is the writer once `spa_writer_interval_sec` (300, 0 = off) has passed since the row's `wa`; it writes only on a row its auction read and runs after-send providers only. Decided after the round 14 review                |
| New-visitor providers     | A new visitor's navigation that runs an auction runs hold-mode providers that do not read the EC ID, and the create carries their answers; finalize creates a row its navigation did not. Decided after the round 14 review                                                        |
| Provider consent purposes | `strict()` requires Purpose 1; a resolver declares its purposes and its vendor, or `without_tcf_vendor()`, or `build()` rejects it. The revoke marker fingerprints all ten purposes. Decided after the round 14 review                                                             |
| Capture                   | Revoke-only: removals, opt-out and clear. Positive IDs are counted, never stored. `capture_enabled` and request-body consent are removed                                                                                                                                           |
| Capture-only opt-out      | Also removes the records of its sources that no provider wrote, and blocks the browser's values for them. A capture-only provider cannot own a source and must list its `sources`                                                                                                  |
| Provider denials          | A provider's `ps.iv` and `ps.ov` filter browser candidates for unowned sources too. An opt-out's removals go to `ps.ov`, which its clear empties                                                                                                                                   |
| Mirror feedback           | The shim never copies `ts-eids-s` into `ts-eids` or the `/auction` body                                                                                                                                                                                                            |
| Dry run                   | Removed, with `is_dry_run`                                                                                                                                                                                                                                                         |
| KV schema                 | Version 2 with one record shape. Version 1 rows are read and upgraded on write. Rollout step 0 reads version 2 and never rewrites it, so step 1 can roll back to it                                                                                                                |
| Stored hashes             | `ps.iv` hashes are unkeyed and the consent fingerprint is a plain bitmask, because the row already holds the raw values. No legacy revision hash                                                                                                                                   |
| Pull sync                 | The pull-complete marker and the legacy pull sync config keys are removed                                                                                                                                                                                                          |
| Withdrawal retry          | A tombstone that fails before send gets one more attempt after send                                                                                                                                                                                                                |

# Server-side identity on every adapter: requirements and PR sequence

Status: draft. Read at Trusted Server main `598a85170` and EdgeZero main
`e11ea308` (crates at `v0.0.11`) on 2026-11-11.

## Goals

1. Axum is deployable as a production service on AWS, behind CloudFront and an
   ALB.
2. Server-side identity works on every adapter: Fastly, Axum, Cloudflare and
   Spin.
3. SharedID is read, stored and passed to bidders the way Prebid defines it,
   and shared state is passed through each adapter the right way.
4. Identity is written back as first-party cookies on the HTML document
   response.

The first Axum publisher runs client-side Prebid and wants identity without the
auction orchestrator. Its pages pass through Trusted Server, so cookies can be
set on the document response, which Safari does not cap.

## Where things stand

Two lines of code carry Axum today.

- **Main.** Axum is a dev server. It binds loopback, reads config and secrets
  from env vars only, takes the client IP from the socket peer, has no geo,
  and stops on Ctrl-C only. Only Fastly has server-side identity. Axum,
  Cloudflare and Spin build an `EcContext` for consent alone: no store, no
  `ts-ec` cookie, no partner registry.
- **Axum RC line.** Tag `v0.1.1-rc-axum.202611.2` is the head of draft #1541
  (`7c6e68d94`) and is the source of the deployed demo. It adds production
  startup, the EC record in DynamoDB and celld, CloudFront geo, a container
  image and AWS examples. It branches from main at `2dd968aee` (2026-10-20).
  It carries 74 commits that are not on main and is 29 commits behind it. It
  pins EdgeZero tag `v0.0.12-rc-axum.202611.1`, which is EdgeZero #431 ("DO
  NOT MERGE"). It has none of the provider stack, so there is no server-side
  ID5 or LiveRamp, no SharedID write-back and no mirror cookie.

Capability by adapter on main, and the open PR that adds each capability
elsewhere:

| Capability                          | Fastly         | Axum                                  | Cloudflare                       | Spin                             |
| ----------------------------------- | -------------- | ------------------------------------- | -------------------------------- | -------------------------------- |
| EC minted and `ts-ec` set           | main           | #1536 (RC line)                       | none                             | none                             |
| `EcKvStore` backend for the record  | main (KV, CAS) | #1536 DynamoDB, #1537 celld (RC line) | none                             | none                             |
| Identify                            | main           | #1536 (RC line)                       | none                             | none                             |
| Batch sync, pull sync, admin lookup | main           | admin lookup answers 501 locally      | admin lookup answers 501 locally | admin lookup answers 501 locally |
| Auction EIDs from the record        | main           | #1536 (RC line)                       | none                             | none                             |
| Server-side providers               | #1487          | refused at startup on #1487           | refused at startup on #1487      | refused at startup on #1487      |
| Trusted client IP and geo           | main           | #1528, #1532 (EdgeZero RC only)       | `cf-connecting-ip`, country only | TCP peer, no geo                 |
| After-response work                 | main           | none                                  | none                             | none                             |
| Rate limiting                       | main (ERL)     | none                                  | none                             | none                             |
| First-party cookie write-back       | main           | RC line                               | none                             | none                             |

EdgeZero main provides none of these for Axum: production startup, SIGTERM
drain, trusted client IP, AWS config and secret stores, DynamoDB, geo, or
concurrent outbound HTTP with deadlines. Open PRs #275 through #439 add them
(see the sequence below). No EdgeZero PR adds a portable after-response hook,
conditional KV writes, a portable client-IP accessor, geo on Fastly,
Cloudflare or Spin, or a rate limiter. The `KvStore` contract on EdgeZero main
has get, put, delete, exists and list only. EdgeZero main has no geo module;
`ctx.geo()` exists only on the #439 stack, and only Axum fills it there.

Main has no server-side providers. The single page-load write model, the
navigation permit and the startup refusal of providers off Fastly all exist
only on #1487 and the PRs stacked on it. Axum, Cloudflare and Spin do build a
partner registry, per request, for `/_ts/admin/eids`.

### How the EC record write is split

The record logic is already shared. `KvIdentityGraph` in
`crates/trusted-server-core/src/ec/kv.rs` owns the compare-and-swap retry
loops, consent tombstones, entry validation and the page-load write for every
adapter. An adapter supplies only the narrow `EcKvStore` trait in
`crates/trusted-server-core/src/ec/kv_backend.rs`. It has five operations:

- a lookup that returns a generation marker
- a strongly consistent `key_exists`
- `insert` with `Add`, `Overwrite` or `IfGenerationMatch`
- a strongly consistent prefix listing
- delete

Each backend is written by hand because EdgeZero's `KvStore` gives none of
these guarantees. The Fastly backend calls the Fastly SDK directly
(`crates/trusted-server-adapter-fastly/src/ec_kv.rs`). The RC line adds
DynamoDB and celld backends inside the Axum adapter (`ec_kv.rs`,
`celld_ec.rs`). Cloudflare already reaches EdgeZero's `KvStore` for generic
runtime KV, but that store cannot back the EC record.

The target is one `EcKvStore` implementation in core, written over EdgeZero's
`KvStore` once EdgeZero offers conditional writes and strong reads (EZ-2). The
per-adapter backends are then deleted. The DynamoDB and celld conditional
logic belongs in EdgeZero's store providers, not in the Trusted Server Axum
adapter.

Platform limits to confirm in the EZ-2 design: Cloudflare Workers KV is
eventually consistent and has no compare-and-swap, so a Cloudflare backend
needs a strongly consistent store such as Durable Objects or D1. Spin's
key-value interface needs checking for compare-and-swap.

## Requirements

Each requirement states behavior and how to accept it. The PR sequence below
maps them to work.

### AX: Axum deployable

- **AX-1 Production serving.** The service binds a configured address. It
  exits non-zero before listening when config, secrets or a required store
  fails to load. On SIGTERM it stops accepting, finishes in-flight requests and
  after-response tasks, and exits within a configured grace shorter than the
  ECS stop timeout. Accept: an ECS task stop during a request completes the
  request and its record write, and CI runs that test. The RC line has the
  startup, SIGTERM and drain parts, and the test passes locally
  (`scripts/test-native-ec-image.mjs` from #1536), but CI does not run it.
  Draining after-response tasks needs EZ-1; EdgeZero #406 drains connections
  only.
- **AX-2 Managed config and secrets.** Config and secrets load from AWS stores
  through EdgeZero store providers. Environment overrides use the full
  `TRUSTED_SERVER__` prefix. Main's `TRUSTED_SERVER_CONFIG_*` and
  `TRUSTED_SERVER_SECRET_*` names break this rule, and #1528 removes them. The
  RC line adds `TS_NATIVE_GEO_PROFILE_FILE`, `TS_NATIVE_BINDINGS_TOML` and
  `TS_APPCONFIG_TOKEN`, which must not be ported.
- **AX-3 Trusted IP and geo.** The client IP is taken only from trusted
  CloudFront and ALB hops, for IPv4 and IPv6 ranges. Country and US region come
  from CloudFront headers only when the hop is trusted. The reference
  deployment enables geo, because an unknown country fails closed and issues no
  EC. The code in EdgeZero #409 and #439 and Trusted Server #1532 already
  accepts IPv6. The reference Terraform in #1540 trusts IPv4 ranges only and
  leaves geo off by default.
- **AX-4 Record store fit for production.** EC records support create-if-absent
  and a generation compare-and-swap. Writes do not all hit one item or one
  partition key. Retries are bounded. Accept: a load test at the target
  page-load rate with provider refreshes shows no throttling.
- **AX-5 Published production image.** The published, attested image runs the
  production server, not the dev server.
- **AX-6 Reference infrastructure.** It wires the DynamoDB table, port 443
  egress for vendor APIs, a readiness check that tests dependencies, JSON logs,
  and more than one task. EdgeZero #275 runs Axum on a current-thread runtime,
  so a process uses one core and capacity comes from task count. #1540 already
  has the 443 egress (IPv4 only). Its `/health` is static, logs are plain text,
  it runs one task, and the DynamoDB module is not wired in.
- **AX-7 Ships from main.** The deployed build comes from main. It never pins
  an EdgeZero RC tag (#431, #462) or a revision that exists only on an RC
  branch, such as `456313675`, which #1528 pins today. Main may pin PR #275's
  head, as #1506's maintainer answers allow. Pinning any other unmerged
  EdgeZero revision needs its own decision. The end state is a released
  EdgeZero tag.

### ID: Server-side identity on every adapter

- **ID-1 One EC lifecycle.** On every adapter a consented document navigation
  mints the EC, creates the record and sets `ts-ec` together. A cookie is never
  issued without a record. An existing EC is kept when geo is unknown. Accept:
  one conformance suite runs against all four adapters.
- **ID-2 One shared record store.** The identity logic stays in core
  (`KvIdentityGraph`) and becomes awaitable (#1526). It runs on one
  `EcKvStore` implementation over EdgeZero's `KvStore`, not a backend per
  adapter. Each platform's EdgeZero store provides a generation marker,
  create-if-absent, compare-and-swap, and strongly consistent existence and
  prefix reads (EZ-2). An adapter whose store cannot meet that contract
  rejects identity config at startup with a clear error instead of running
  without a record. Accept: no adapter crate implements `EcKvStore`.
- **ID-3 Partner APIs answered locally.** Identify, batch sync, pull sync and
  admin lookup run on every adapter with a store. On an adapter without one,
  identify and batch sync return a local error and never reach the publisher
  origin. Admin lookup already answers 501 locally, and pull sync is outbound
  only.
- **ID-4 Providers on every adapter with a store.** Lookups start at navigation
  alongside the origin fetch, share one deadline, and are collected before
  finalization. A page load writes the record once. Startup accepts an enabled
  provider on every adapter that supports providers.
- **ID-5 After-response work.** Pull sync and guarded follow-up writes run after
  the response is sent on every adapter: after send on Fastly, a task drained
  on SIGTERM on Axum, `waitUntil` on Cloudflare. Spin has no mechanism; the
  work either runs before the response there or is not offered.
- **ID-6 Rate limiting.** Provider lookups, batch sync, pull sync and record
  creation are rate limited on every adapter. Fastly Edge Rate Limiting is the
  only limiter on any branch, and record creation is limited nowhere. Batch
  sync is keyed per partner. #1487 keys provider lookups on the IP-derived EC
  prefix, so everyone behind one IPv4 address or IPv6 /64 shares one budget.
  Choose that key on purpose. With more than one Axum task the counter must be
  shared.
- **ID-7 Abuse resistance.** Record creation needs real protection on every
  adapter. On Fastly the gate checks only that a JA4 fingerprint exists plus a
  browser User-Agent platform, so any TLS client with a browser User-Agent
  passes. The RC adds a User-Agent heuristic and request filters on Axum.
  Cloudflare and Spin have nothing.
- **ID-8 Body EIDs keep being saved.** EIDs sent in the `/auction` body are
  saved to the record, as on main (Fastly). The provider stack stops saving
  them, and the RC's Axum lifecycle never saves them.

### SH: Shared state passed the right way

- **SH-1 Built once.** Settings, the partner registry, store clients and the
  HTTP client are built once per process or isolate and shared. Today
  Cloudflare and Spin rebuild app state per request, partner registries are
  rebuilt several times per request, and Axum builds an HTTP client per
  request.
- **SH-2 Per-request state in the request.** Per-request values travel in the
  request context. They are never kept in a `thread_local` or in retained app
  state. #1487, #1488 and #1489 keep lookups in a `thread_local`. #1527 and
  #1528 keep a per-request `AtomicBool` in the Fastly `AppState`. Fastly
  serves one request per sandbox, so the risk is a value carried into the next
  request, not a race.
- **SH-3 No executor nesting.** No `block_on` runs inside async code. #1526
  wraps finalize in `block_on` (Fastly `main.rs:550`). #1487 calls `block_on`
  inside finalize (`main.rs:699`). Combined unchanged, they panic. On main,
  core pull sync and Cloudflare and Spin config reads also call `block_on`.
  #1526, #1506 and the RC remove the pull-sync one, and the RC removes Spin's.
  Cloudflare's remains on every branch.
- **SH-4 No first-request capture.** Cloudflare must not keep the first
  request's `Env` in a set-once `thread_local`.

### SID: SharedID handled the way Prebid defines it

- **SID-1 Cookie name from config.** The server reads SharedID from the
  cookie named by the publisher's Prebid `storage.name`, on every adapter.
  Prebid has no default name; `_sharedid` and `_pubcid` are the common ones.
  Trusted Server config already holds the name in
  `managed_user_ids[].storage.name`, so use that rather than a second setting.
  Main reads only a cookie named `sharedId` (`constants.rs:10`), which is
  Prebid's module name, not a cookie name. It does so on Fastly for the
  record, and on every adapter for `/_ts/admin/eids`. #1488 makes the name
  configurable, on Fastly only.
- **SID-2 Opt-out.** A present `_pubcid_optout` cookie stops SharedID reads,
  storage, sending and write-back. Prebid also reads the opt-out from
  localStorage, which the server cannot see. #1488 honors the cookie for its
  own provider only.
- **SID-3 One canonical EID.** SharedID is stored and sent as source
  `pubcid.org` with atype 1. Main gets partway by configuration: a
  `pubcid.org` partner with atype 1 already stores and sends the right EID
  from `ts-eids` and the auction body. The `sharedId` cookie path is
  hard-wired to `sharedid.org` (`ec/prebid_eids.rs:241`), and records written
  from the browser default to atype 3. The outbound merge dedupes only within
  one source. Deduplicate across cookie, `ts-eids`, auction body and record so
  a bidder never sees the same ID twice.
- **SID-4 OpenRTB placement.** EIDs go in the field the declared protocol
  defines: `user.eids` for OpenRTB 2.6. Every builder writes only
  `user.ext.eids`: bidder OpenRTB (`auction/openrtb.rs:372`), Prebid Server
  (`integrations/prebid.rs:2882`) and APS (`integrations/aps.rs:1131`), while
  `openrtb-2.6` is the only accepted protocol. Keep `user.ext.eids` only where
  a named downstream needs it.
- **SID-5 Retention.** The record keeps a SharedID for as long as the cookie is
  meant to last. The stored value wins over a different browser value. Main
  already does this on Fastly (`ec/kv.rs:134-174`). #1488 regresses it: it
  keeps a cookie-read SharedID for 24 hours and lets the cookie replace the
  stored value. That has to change before write-back can work.

### WB: First-party write-back

- **WB-1 Document response, every adapter.** Identity cookies are set on the
  top-level HTML document response on every adapter. Main mints `ts-ec` on
  navigations on Fastly only, and the RC adds Axum. The navigation permit
  exists only in the provider stack, where only the Fastly adapter classifies
  it.
- **WB-2 `ts-ec` lifetime.** `ts-ec` keeps `Domain=.<publisher>; Path=/; Secure;
SameSite=Lax; HttpOnly; Max-Age=31536000`. Today it is set only for a new or
  rotated EC, so an active reader's cookie expires a year after minting.
  Decide whether returning readers get a refresh.
- **WB-3 SharedID cookie.** New; no code on any branch writes it, and #1488
  says so on purpose. Set from the record when the request has no SharedID
  cookie or a different value. Use Prebid's storage name and the Domain Prebid
  writes with. Not HttpOnly, Secure, `SameSite=Lax`, one year. Also keep
  Prebid's `<name>_cst` companion cookie consistent: when it is missing or
  differs, Prebid rewrites the SharedID cookie from script on the next page,
  and Safari caps that copy again. Expire both when consent does not allow the
  EC or `_pubcid_optout` is present. Prebid's `extend` with a `pixelUrl` is
  its own server-write path and must keep working alongside this.
- **WB-4 Mirror cookie.** `__Host-ts-eids-mirror` as in #1489: opt-in per
  partner, dropped per vendor when TCF consent is denied, requires
  `bidstream_enabled` and Purpose 4, 3,584-byte budget, host-only. Every
  mirrored ID has a bounded expiry. In #1489 an ID with no stated expiry never
  expires (`Expiry::Never`), and its pull-sync provider writes such IDs.
  Closed #1461 capped mirrored IDs at 24 hours.
- **WB-5 Cacheable pages.** An unchanged cookie gets no `Set-Cookie`, so
  returning readers' pages stay cacheable. A response that carries
  `Set-Cookie` is not stored in a shared cache; main already enforces this on
  every adapter (`response_privacy.rs:122-148`) and does not re-set `ts-ec`.
  Main does re-set the `ts-ec-pull-complete` marker about hourly, which #1489
  retires. The template cache also bypasses any request with a Cookie header
  unless the publisher configures otherwise, and its deny list names only
  `sharedId`, so it must learn the configured SharedID name.
- **WB-6 Identity-only page script.** A tsjs mode captures Prebid's IDs without
  the auction shim, reads the mirror cookie, and merges the IDs into Prebid.
  Today the `ts-eids` capture runs only inside the shim, which also adds the
  Trusted Server bidder to every ad unit. Nothing reads the mirror.

### EZ: asks of EdgeZero

These are behaviors Trusted Server needs from EdgeZero. No open EdgeZero PR
provides them.

- **EZ-1** An after-response task API on all four adapters, drained on shutdown
  on Axum.
- **EZ-2** `KvStore` gains what the EC record needs: reads that return a
  generation marker, create-if-absent and generation compare-and-swap writes,
  and strongly consistent existence checks and prefix listing. A store that
  cannot meet these returns a capability error; `KvError::Unsupported` already
  exists on main. #463's celld `CelldRecordStore` has all of these semantics on
  a separate API, and #414's DynamoDB provider has strong existence and
  listing but no compare-and-swap. The portable `KvStore` gains none of them
  on any branch, and #410 and #437 rule them out today.
- **EZ-3** A portable trusted client-IP accessor.
- **EZ-4** Geo through `ctx.geo()` on Fastly, Cloudflare and Spin, with region.
- **EZ-5** A rate limiter with Fastly ERL, in-memory and shared-counter
  backends.
- **EZ-6** A released tag containing the Axum production stack (#275 through
  #439).

## Out of scope

- A narrow install with one path rule and no page proxy. It needs an EC minted
  on a request that is not a navigation.
- Conformance with the SharedID Edge specification.
- An Akamai adapter.
- The open #1342 to #1423 PR stack and PRs stacked on it, such as #1448. This
  plan neither depends on it nor sequences it.

## PR sequence

The order lands the async store and the EdgeZero cutover on main first, then
identity on top, then the EdgeZero production stack, then forward-ports of the
RC line, then new work. Each step ends in a state that can be checked.

### Step 0: decisions and cleanup

1. The cutover is #1506 (decided 2026-11-11). Close #1527 once #1506 has
   equivalents of its native-client, response-owner and managed Fastly
   delivery tests. #1528 is stacked on #1527, so it is rebased onto #1506.
2. Close #1310. #1488's ID5 provider supersedes it.
3. Drop the commits of #1545 (closed, unmerged) from #1541.

### Step 1: Trusted Server main

1. #1557 Sourcepoint consent. Already in the RC tag.
2. #1526 retargeted to main, with #1525 in parallel. #1526's own diff applies
   cleanly to main without #1525.
3. #1516, which merges before #1506 as already agreed.
4. #1506, the EdgeZero cutover. Before merge it:
   - finishes Tasks 10 to 12
   - rebases onto main with #1526 and #1516 (#1526 shares 7 of its files)
   - repins to #275's head once #275 absorbs #460 (see the EdgeZero step).
     That restores outbound cache tags, so #1506 can drop its rejection of
     `origin_readthrough_enabled` (`config.rs:317`). It also picks up
     `b97efec1`, which stops Spin sending `Content-Length: 0` on bodiless
     DELETE and OPTIONS
   - updates its description, which still says docs only

   Main then pins #275's head instead of `v0.0.11`.

5. #1487 rebased onto #1526 and #1506 (13 and 23 shared files). Provider
   lookups go through EdgeZero's `HttpClient`. Lookup collection becomes an
   `.await` in async finalize (SH-3), and per-request lookups move into the
   request (SH-2).
6. #1489, then #1488. They are siblings on #1487 and conflict in 11 files; the
   second absorbs them. Vendor calls use EdgeZero's `HttpClient`. Fix the
   mirror expiry (WB-4) and SharedID retention (SID-5) here.
7. New: partner routes answered locally on non-Fastly adapters (ID-3).
8. New: SharedID correctness on Fastly (SID-1 to SID-4). It removes the legacy
   `sharedId` cookie and `sharedid.org` source path in the same change.
9. #1503 `ts ec show` and `ts ec eids`.
10. #1449, then #1543, image publishing.

Landing #1506 before the provider stack means the ID5 and LiveRamp calls are
written against EdgeZero's outbound API from the start. #1506's answer to its
in-flight question allows the other order, but each PR that lands first adds
callers #1506 has to port.

### Step 1, in parallel: EdgeZero

1. Independent PRs, which need nothing else: #444, #466 (rebased on #444),
   #452, #401, #389.
2. #275, after it absorbs the fixes to its own code that sit in later PRs:
   - #460 (`251974d4`, `dc296073`): surrogate keys on `OutboundCachePolicy`
     and a borrowed `poll_ready` on `OutboundPending`. #460 removes `Copy`
     from a #275 public type, so it must land before #275 releases. Bring
     the Cloudflare wasm clippy fix from RC-only `ebf622c2` with it.
   - #406 `9f090e57`: the CLI template emits Fastly-only image code that
     #275 gates behind `native_image_example`.
   - One copy of the CI trigger change, so stacked PRs run CI. It exists
     three times, in #406, #460 and #414.
   - Optional: guard all five egress unwind sites against a panicking logger
     (#409 `0769cbfd` covers three).

   Then fix placement inside the stack:
   - #414 `9a985c99` (a flaky-test fix) moves to #407.
   - #414 `4ea99467` (`run_production_app_with_options_and_initializer`,
     which the RC's native startup calls) moves to #406.
   - #463 rebases onto #422 without #439, #464 onto #439 without #454's
     prototype, and #453 off its stale base.
   - The RC-only merges that fit #389 (`7c07b8e7`) and #401 (`2510ecc9`,
     `501a2dce`) onto #275 have to be redone in those PRs.

3. #406, #407, #409. These give a deployable ECS service.
4. #414.
5. #422, #439, #464, #463. Retarget #463 first; it carries an old copy of
   #439. If celld is not used in production, restack #439 on #414.
6. New: EZ-2 conditional KV on the DynamoDB and celld providers, generalizing
   #463's celld-only record operations into `KvStore`.
7. Release a tag (EZ-6). File EZ-1 and EZ-3 to EZ-5 as issues.
8. #287 last. It shares 27 files with #275.

Every ready EdgeZero PR currently has changes requested, and #275 has 20 of 95
threads open.

### Step 2: Trusted Server main on the EdgeZero production stack

1. #1528 native startup, rebased onto #1506. It needs EdgeZero #406, #409 and
   #414, which are stacked on #275 but are not in #275's head.
2. Forward-port the RC line as new PRs against main, closing each RC unit as
   its port lands:
   1. New: one core `EcKvStore` over EdgeZero's `KvStore` (ID-2). Fastly
      moves onto it and `adapter-fastly/src/ec_kv.rs` is deleted in the same
      change.
   2. #1536 native EC lifecycle and #1537 storage selection, on that shared
      backend. Their adapter-local DynamoDB and celld backends are not
      ported. Spread writes across items and partitions (AX-4). #1537's
      example config breaks startup on Axum, Cloudflare and Spin.
   3. #1532 CloudFront geo. It maps a US request with no region to `Unknown`,
      which fails every US Cloudflare request closed. It does not trust IPv6
      CloudFront ranges.
   4. #1539 and #1538, the image, on one contract with #1543.
   5. #1540 AWS examples.
3. Close #1541, #1424 and #1499 once main reaches parity with the RC tag.

### Step 3: new work

1. Providers on Axum. Remove the startup refusal, register provider kinds,
   classify the navigation permit, add the after-response task and a rate
   limiter (ID-4 to ID-6).
2. SharedID write-back (WB-3).
3. Mirror on Axum. The adapter calls the page-load write (WB-4).
4. Identity-only tsjs mode (WB-6).
5. Partner APIs on Axum (ID-3).
6. Cloudflare and Spin: EdgeZero stores that meet EZ-2. The shared backend and
   lifecycle then run unchanged (ID-1, ID-2).

## Decisions

Decided:

- **Cutover** (2026-11-11). #1506. #1527 closes once its tests are covered.

Open:

1. **Base.** Forward-port to main, or back-port the provider stack to the RC
   line for a faster first publisher. Back-porting meets 25 to 34 conflicts on
   a branch that its own RC says will not merge. Recommendation: forward-port,
   and keep serving the demo from the RC tag until main reaches parity.
2. **EdgeZero pin for the Axum production stack.** Wait for #406, #409 and
   #414 to merge, or extend #1506's "follow #275's head" rule to that stack so
   #1528 can land sooner.
3. **Store path for Axum.** Wait for EZ-2 and use the shared backend, or port
   #1536's adapter-local DynamoDB backend first and delete it when EZ-2 lands.
   Recommendation: wait, since the RC tag already serves the demo.
4. **Cloudflare and Spin store.** Which EdgeZero store meets EZ-2 there, or
   whether identity is declared unsupported.
5. **LiveRamp Custom ID.** The current value is the IP-derived EC prefix,
   shared by everyone behind one IP.
6. **SharedID retention and precedence** (SID-5), and whether the server
   generates a SharedID when none exists.
7. **Mirror partners.** Which partners get `mirror_enabled`. Mirrored IDs are
   readable by every script on the page.
8. **Rate limiter key and backend**, and the number of Axum tasks (ID-6, AX-6).
9. **EC EID source.** Which source name the EC uses, and whether it keeps
   going out as `user.id`.
10. **`ts-ec` refresh** for returning readers (WB-2).

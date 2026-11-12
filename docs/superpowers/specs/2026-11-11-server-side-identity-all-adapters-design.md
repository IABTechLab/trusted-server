# Server-side identity on every adapter: design

Status: draft, in review in #1559. Read at Trusted Server main `598a85170` and
EdgeZero main `e11ea308` (crates at `v0.0.11`) on 2026-11-11 and 2026-11-12.
Epics: #1558 (Trusted Server) and IABTechLab/edgezero#468 (EdgeZero). Plan:
[server-side identity plan](../plans/2026-11-11-server-side-identity-all-adapters.md).

## Goals

1. Axum is deployable from main as a production service on AWS, behind
   CloudFront and a load balancer.
2. Server-side identity works on every adapter whose platform can meet the
   record contract in [Platform support](#platform-support), delivered in the
   order Fastly, Axum, Cloudflare, Spin (DEC-4). Fastly and Axum are the MVP
   (DEC-2) and ship complete without any Cloudflare or Spin work. An adapter that cannot meet the
   contract rejects identity configuration at startup with a clear error and
   never runs identity without a record.
3. SharedID is read, stored and passed to bidders the way Prebid defines it,
   and shared state is passed through each adapter the right way.
4. Identity is written back as first-party cookies on the HTML document
   response.

Target deployment profile: a publisher that runs client-side Prebid and wants
identity without the auction orchestrator. Its pages pass through Trusted
Server, so cookies can be set on the document response, which Safari does not
cap.

## Relationship to other work

- This is a program-level design across Trusted Server and EdgeZero. It is not
  the design that #1236 asks for. The AX requirements feed #1236's tickets.
  #1236's own open choices (key rotation, backup and restore, which Fastly-only
  features stay unsupported on Axum) stay with #1236.
- Each ticket's implementation PR carries its own design notes, and a plan
  when it needs one. This design fixes the requirements, the platform support
  matrix and the order.
- The EdgeZero cutover is #1506 (DEC-1).

## Where things stand

Two lines of code carry Axum today.

- **Main.** Axum is a dev server. It binds loopback, reads config and secrets
  from env vars only, takes the client IP from the socket peer, has no geo,
  and stops on Ctrl-C only. Only Fastly has server-side identity. Axum,
  Cloudflare and Spin build an `EcContext` for consent alone, with no record
  store and no `ts-ec` cookie. They build a partner registry per request, for
  `/_ts/admin/eids` only.
- **Axum RC line.** Tag `v0.1.1-rc-axum.202611.2` (`7c6e68d94`) was the head of
  draft #1541 when read on 2026-11-11; #1541 has moved since. It adds production startup, the EC record in DynamoDB and
  celld, CloudFront geo, a container image and AWS examples. It branches from
  main at `2dd968aee` (2026-10-20), carries 74 commits that are not on main
  and is 29 commits behind it. It pins EdgeZero tag
  `v0.0.12-rc-axum.202611.1`, which is IABTechLab/edgezero#431 (marked DO NOT
  MERGE). It has none of the provider stack: no server-side ID5 or LiveRamp,
  no SharedID write-back and no mirror cookie.

Capability by adapter on main, and the open PR that adds each elsewhere:

| Capability                          | Fastly         | Axum                                  | Cloudflare                       | Spin                             |
| ----------------------------------- | -------------- | ------------------------------------- | -------------------------------- | -------------------------------- |
| EC minted and `ts-ec` set           | main           | #1536 (RC line)                       | none                             | none                             |
| `EcKvStore` backend for the record  | main (KV, CAS) | #1536 DynamoDB, #1537 celld (RC line) | none                             | none                             |
| Identify                            | main           | #1536 (RC line)                       | none                             | none                             |
| Batch sync, pull sync, admin lookup | main           | admin lookup answers 501 locally      | admin lookup answers 501 locally | admin lookup answers 501 locally |
| Auction EIDs from the record        | main           | #1536 (RC line)                       | none                             | none                             |
| Server-side providers               | #1487          | refused at startup on #1487           | refused at startup on #1487      | refused at startup on #1487      |
| Trusted client IP and geo           | main           | #1528, #1532 (EdgeZero RC revisions)  | `cf-connecting-ip`, country only | TCP peer, no geo                 |
| After-response work                 | main           | none                                  | none                             | none                             |
| Rate limiting                       | main (ERL)     | none                                  | none                             | none                             |
| First-party cookie write-back       | main           | RC line                               | none                             | none                             |

EdgeZero main provides none of these for Axum: production startup, SIGTERM
drain, trusted client IP, AWS config and secret stores, DynamoDB, geo, or
concurrent outbound HTTP with deadlines. Open PRs IABTechLab/edgezero#275
through IABTechLab/edgezero#439 add them. No EdgeZero PR adds a portable
after-response hook, conditional KV writes, a portable client-IP accessor, geo
on Fastly, Cloudflare or Spin, or a rate limiter. EdgeZero main has no geo
module; `ctx.geo()` exists only on the IABTechLab/edgezero#439 stack, and only
Axum fills it there.

Main has no server-side providers. The single page-load write, the navigation
permit and the startup refusal of providers off Fastly exist only on #1487 and
the PRs stacked on it.

### How the EC record write is split

The record logic is shared. `KvIdentityGraph` in
`crates/trusted-server-core/src/ec/kv.rs` owns the compare-and-swap retry
loops, consent tombstones and entry validation for every adapter. On #1487 it
also owns the page-load write. An adapter supplies only the `EcKvStore` trait
in `crates/trusted-server-core/src/ec/kv_backend.rs`, which has five
operations:

- a lookup that returns a generation marker
- a strongly consistent `key_exists`
- `insert` with `Add`, `Overwrite` or `IfGenerationMatch`
- a strongly consistent prefix listing
- delete

Each backend is written by hand because EdgeZero's `KvStore` gives none of
these guarantees. The Fastly backend calls the Fastly SDK directly
(`crates/trusted-server-adapter-fastly/src/ec_kv.rs`). The RC line adds DynamoDB
and celld backends inside the Axum adapter. EdgeZero's Fastly store drops what
the record needs: at EdgeZero main and at IABTechLab/edgezero#275's head its
read calls `lookup` without the generation, `exists` is a read, and inserts
carry no mode. So the Fastly backend can move onto EdgeZero only once EZI-2a
covers the Fastly provider.

The target is one `EcKvStore` implementation in core over EdgeZero's
`KvStore` (DEC-3). The per-adapter backends are then deleted, and the DynamoDB
and celld conditional logic lives in EdgeZero's providers.

## Platform support

Identity runs on an adapter only where the record contract and geo are
supported. Elsewhere, startup rejects identity configuration (ID-2). Delivery
follows DEC-4: Fastly, Axum, Cloudflare, Spin.

| Capability                                                                    | Fastly                                                               | Axum                                                                       | Cloudflare                                                                                                                                                          | Spin                                                                                                                                                                                                                    |
| ----------------------------------------------------------------------------- | -------------------------------------------------------------------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Record contract (generation read, create-if-absent, CAS, strong exists, list) | Supported. Fastly KV generations, exposed through EdgeZero by EZI-2a | Supported. DynamoDB or celld through EZI-2b                                | Unsupported today. Workers KV is eventually consistent with no CAS, and EdgeZero has no Durable Objects or D1 store on main, IABTechLab/edgezero#275 or the RC tags | Unsupported today. `spin-sdk` 7.0.0 `key_value` has get, set, delete, exists and get_keys only, with no prefix listing. Its bundled `wasi:keyvalue/atomics` interface defines a CAS resource that the SDK does not wrap |
| After-response work                                                           | Supported (after send)                                               | EZI-1, drained on SIGTERM                                                  | EZI-1 over `waitUntil`                                                                                                                                              | Unsupported. DEC-14 picks before-response or skip                                                                                                                                                                       |
| Rate limiter                                                                  | Supported (Edge Rate Limiting)                                       | EZI-5, in-memory per task, shared counter with more than one task (DEC-11) | EZI-5, backend per DEC-11                                                                                                                                           | EZI-5, backend per DEC-11                                                                                                                                                                                               |
| Geo with region                                                               | Supported                                                            | CloudFront headers (A4, IABTechLab/edgezero#439)                           | Country only today; region needs EZI-4                                                                                                                              | None; needs EZI-4                                                                                                                                                                                                       |
| Trusted client IP                                                             | Supported                                                            | IABTechLab/edgezero#409                                                    | `cf-connecting-ip`; EZI-3 accessor                                                                                                                                  | TCP peer; EZI-3 accessor                                                                                                                                                                                                |

Cloudflare and Spin move to Supported only when EZI-2 and EZI-4 deliver for
them (DEC-8). Until then their startup rejects identity configuration, and
Goal 2 is met for them by that rejection.

## Requirements

Each requirement states behavior, then `Accept:` with the check that proves
it. "Supported adapter" means one where [Platform support](#platform-support)
marks the record contract and geo as supported.

### AX: Axum deployable

- **AX-1 Production serving.** The service binds a configured address, exits
  non-zero before listening when config, secrets or a required store fails to
  load, and on SIGTERM stops accepting, finishes in-flight requests and
  after-response tasks, and exits within a configured grace shorter than the
  ECS stop timeout. Accept: an ECS-style stop during a request with a pending
  record write completes the request and the write, and CI runs that test;
  startup with a missing secret exits non-zero before the port opens. State:
  the RC line has startup, SIGTERM and connection drain, and
  `scripts/test-native-ec-image.mjs` from #1536 passes locally but not in CI.
  Draining after-response tasks needs EZI-1; IABTechLab/edgezero#406 drains
  connections only.
- **AX-2 Managed config and secrets.** Config and secrets load from AWS stores
  through EdgeZero store providers. Environment overrides use the full
  `TRUSTED_SERVER__` prefix only. Accept: an Axum startup test loads config
  and secrets through the providers in IABTechLab/edgezero#414, and no runtime
  code reads a `TS_*` or `TRUSTED_SERVER_CONFIG_*` / `TRUSTED_SERVER_SECRET_*`
  name. State: main reads `TRUSTED_SERVER_CONFIG_*` and
  `TRUSTED_SERVER_SECRET_*`, which #1528 removes; the RC adds
  `TS_NATIVE_GEO_PROFILE_FILE`, `TS_NATIVE_BINDINGS_TOML` and
  `TS_APPCONFIG_TOKEN`.
- **AX-3 Trusted IP and geo.** The client IP comes only from trusted CloudFront
  and load balancer hops, IPv4 and IPv6. Country and US region come from
  CloudFront headers only from a trusted hop. The reference deployment enables
  geo, because an unknown country fails closed and issues no EC. Accept: an
  untrusted peer with forged forwarded and geo headers gets its own IP and no
  geo; a trusted IPv6 CloudFront hop is accepted; the reference deployment
  applies with geo on. State: IABTechLab/edgezero#409, IABTechLab/edgezero#439 and #1532 accept
  IPv6; the reference Terraform in #1540 trusts IPv4 ranges only and leaves
  geo off.
- **AX-4 Record store fit for production.** Writes are spread across partition
  keys, with no item every insert updates, and retries are bounded. Accept: a
  load test at the target page-load rate with provider refreshes (A5 sets the
  rate) shows no throttling. State: #1536 puts every row under one partition
  key and updates one allocator item per insert.
- **AX-5 Published production image.** The published, attested image runs the
  production server. Accept: `gh attestation verify` passes for the image, and
  its entrypoint starts the production server with `STOPSIGNAL SIGTERM`. State:
  #1543 publishes the dev-server image (`STOPSIGNAL SIGINT`); #1539 builds the
  production image without publishing it.
- **AX-6 Reference infrastructure.** The reference stack wires the DynamoDB
  table, port 443 egress for vendor APIs, a readiness check that fails when a
  dependency is down, JSON logs and two or more tasks. IABTechLab/edgezero#275 serves
  Axum on a current-thread runtime, so capacity comes from task count. Accept:
  the stack applies with those properties, and readiness fails with DynamoDB
  unreachable. State: #1540 has the 443 egress (IPv4 only), a static `/health`,
  plain logs, one task and no DynamoDB wiring.
- **AX-7 Ships from main.** Main never pins an EdgeZero RC tag
  (IABTechLab/edgezero#431, IABTechLab/edgezero#462) or a revision that exists only on an RC
  branch, such as `456313675`, which #1528 pins today. Main may pin
  IABTechLab/edgezero#275's head under DEC-1; other unmerged revisions need a
  recorded decision (DEC-5). Accept: `Cargo.lock` on main resolves every
  EdgeZero crate to a released tag or to the revision DEC-1 or DEC-5 allows.

### ID: Server-side identity on every adapter

- **ID-1 One EC lifecycle.** On every supported adapter a consented document
  navigation mints the EC, creates the record and sets `ts-ec` together. A
  cookie is never issued without a record. An existing EC is kept when geo is
  unknown. Accept: the conformance suite (T5) asserts on each supported
  adapter that a consented navigation creates the record and sets `ts-ec` in
  the same response, that a store failure sets no `ts-ec`, and that an
  existing EC survives unknown geo; on an unsupported adapter, startup with
  identity configuration fails.
- **ID-2 One shared record store.** The identity logic stays in core and
  becomes awaitable (#1526). It runs on one `EcKvStore` over EdgeZero's
  `KvStore` (DEC-3). A store that cannot meet the contract makes startup
  reject identity configuration with a clear error. Accept: no adapter crate's
  non-test code implements `EcKvStore`, and a store reporting
  `KvError::Unsupported` for the contract fails startup with identity enabled.
- **ID-3 Partner APIs answered locally.** Identify, batch sync, pull sync and
  admin lookup run on every adapter with a record store. Without one, identify
  and batch sync return a local error and never reach the publisher origin.
  Admin lookup already answers 501 locally; pull sync is outbound only. Accept:
  in each adapter's tests a capturing origin receives no identify or batch-sync
  request; with a store they run, without one they return 501.
- **ID-4 Providers on every supported adapter.** Lookups start at navigation
  alongside the origin fetch, share one deadline, and are awaited before
  finalization, so their IDs reach the record and the cookies of the same
  document response. A page load writes the record once. Accept: a mock
  provider that answers within the deadline has its ID in the record and in
  the mirror cookie of that navigation's response; one that misses the
  deadline is skipped and does not delay the response past it.
- **ID-5 After-response work.** Only pull sync and guarded follow-up writes run
  after the response is sent: after send on Fastly, a task drained on SIGTERM
  on Axum, `waitUntil` on Cloudflare. Spin follows DEC-14. Accept: on Fastly,
  Axum and Cloudflare a follow-up write recorded after send is in the record
  before the next navigation, and on Axum SIGTERM waits for it (A3). On Spin
  the DEC-14 choice holds: either the follow-up write lands before the
  response, or the work is skipped and the response is unchanged.
- **ID-6 Rate limiting.** Provider lookups, batch sync, pull sync and record
  creation are rate limited on every supported adapter. Batch sync is keyed
  per partner; the other keys follow DEC-11. With more than one Axum task the
  counter is shared. Accept: per-adapter tests hit each limit and see the
  limited request refused or skipped, and two Axum tasks share one counter.
  State: Fastly Edge Rate Limiting is the only limiter; record creation is
  limited nowhere; #1487 keys lookups on the IP-derived EC prefix, so everyone
  behind one IPv4 address or IPv6 /64 shares a budget.
- **ID-7 Abuse resistance.** Record creation is refused for clients that are
  not browsers, on every supported adapter. Accept: a scripted client with a
  browser User-Agent and no browser TLS fingerprint creates no record. State:
  Fastly checks only that a JA4 exists plus a browser User-Agent platform; the
  RC's Axum checks only the User-Agent, with request filters; Cloudflare and
  Spin create no records yet.
- **ID-8 Body EIDs keep being saved.** EIDs sent in the `/auction` body are
  saved to the record. Accept: on each supported adapter a body EID is in the
  record after the request. State: main saves them on Fastly; the provider
  stack and the RC's Axum lifecycle do not.

### SH: Shared state passed the right way

- **SH-1 Built once.** Settings, the partner registry, store clients and the
  HTTP client are built once per process or isolate. Accept: an adapter test
  shows settings, the partner registry and store clients built once across two
  requests, and the HTTP client too, except Axum's, which stays per request
  until a measurement shows the Next.js regression gone; after that
  measurement Axum's client is held to the same rule. State: Cloudflare and Spin
  rebuild app state per request; Axum builds the partner registry per request
  (`app.rs:515` at #1506's head) and the HTTP client per request on purpose,
  because a pooled client regressed a Next.js flow (main `platform.rs:571-575`).
  Keep that client choice unless a measurement shows the regression gone.
- **SH-2 Per-request state in the request.** Per-request values travel in the
  request context, never in a `thread_local` or in retained app state. Accept:
  no `thread_local!` or atomic in app state holds a per-request value. State:
  #1487 (and #1488, #1489 on it) keeps lookups in a `thread_local`; #1527 and
  #1528 keep a per-request `AtomicBool` in the Fastly `AppState`.
- **SH-3 No executor nesting.** No `block_on` runs inside async code. Accept:
  the remaining `block_on` calls are only at adapter entry points. State: #1526
  wraps finalize in `block_on` (Fastly `main.rs:550`) and #1487 calls
  `block_on` inside finalize (`main.rs:699`), which panic together. Core pull
  sync and Cloudflare and Spin config reads also call `block_on` on main;
  #1526, #1506 and the RC remove the pull-sync one, RC commit `933de5921`
  removes Spin's, and Cloudflare's remains everywhere.
- **SH-4 No first-request capture.** Cloudflare does not keep the first
  request's `Env` in a set-once `thread_local`. Accept: two requests with
  different `Env` values each see their own.

### SID: SharedID handled the way Prebid defines it

- **SID-1 Cookie name from config.** The server reads SharedID from the cookie
  named by the publisher's Prebid `storage.name`, held in
  `managed_user_ids[].storage.name`. Prebid has no default; `_sharedid` and
  `_pubcid` are common. Accept: with `storage.name = "_pubcid"` a `_pubcid`
  cookie is read on every supported adapter, and a cookie named `sharedId` is
  not. State: main reads only `sharedId` (`constants.rs:10`), Prebid's module
  name, on Fastly for the record and on every adapter for `/_ts/admin/eids`.
- **SID-2 Opt-out.** A `_pubcid_optout` cookie stops SharedID reads, storage,
  sending and write-back. Prebid also reads the opt-out from localStorage,
  which the server cannot see. Accept: with `_pubcid_optout` present nothing
  SharedID-related is read, stored, sent or written.
- **SID-3 One canonical EID.** SharedID is stored and sent as source
  `pubcid.org`, atype 1, once. Accept: a bidder request built from cookie,
  `ts-eids`, body and record that carry the same UUID has exactly one EID
  `{source: "pubcid.org", atype: 1}`. State: a `pubcid.org` partner already
  stores and sends the right EID from `ts-eids` and the body; the `sharedId`
  cookie path is hard-wired to `sharedid.org` (`ec/prebid_eids.rs:241`);
  browser-written records default to atype 3; the outbound merge dedupes only
  within one source.
- **SID-4 OpenRTB placement.** EIDs go in `user.eids` for OpenRTB 2.6, and in
  `user.ext.eids` only for a named downstream that needs it. Accept: the
  bidder, Prebid Server and APS requests carry `user.eids`, and every test
  that asserts EIDs are absent checks both fields. State: every builder writes
  only `user.ext.eids` (`auction/openrtb.rs:372`,
  `integrations/prebid.rs:2882`, `integrations/aps.rs:1131`).
- **SID-5 Retention.** The record keeps a SharedID for as long as the record
  lives, and the stored value wins over a different browser value (DEC-7).
  Accept: a stored SharedID survives a request carrying a different cookie
  value. State: main does this on Fastly (`ec/kv.rs:134-174`); #1488 keeps a
  cookie-read SharedID for 24 hours and lets the cookie replace it.
- **SID-6 EID consent.** Reading SharedID into the record, storing it, sending
  it and writing it back each need EID permission (TCF Purposes 1 and 4,
  through the consent module's EID predicate in `consent/mod.rs`), in
  addition to EC permission and the SID-2 opt-out. EC permission alone
  (`consent_allows_ec`, Purpose 1) is not enough. Where consent is required
  but missing, permission is denied. Accept: on each supported adapter, with a
  populated record, an absent or different browser cookie, Purpose 1 granted
  and Purpose 4 denied, no SharedID is read, sent or written back.

### WB: First-party write-back

- **WB-1 Document response only.** Identity cookies are set on top-level HTML
  document responses, on every supported adapter. Accept: identity
  `Set-Cookie` headers appear on navigation responses and never on asset or
  API responses. State: main mints `ts-ec` on Fastly navigations, and the RC
  adds Axum.
- **WB-2 `ts-ec` lifetime.** `ts-ec` keeps
  `Domain=.<publisher>; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=31536000`.
  Whether returning readers get a refresh is DEC-13. Accept: once DEC-13 is
  decided, its rule holds and a returning reader whose cookie needs no refresh
  gets no `Set-Cookie`. State: set only for a new or rotated EC.
- **WB-3 SharedID cookie.** When SID-2 and SID-6 allow it, the server sets the
  SharedID cookie from the record if the request has none or a different
  value. It uses Prebid's storage name and the Domain Prebid writes with, is
  not HttpOnly, and is `Secure; SameSite=Lax` for one year. It keeps Prebid's
  `<name>_cst` companion consistent, since a missing or different one makes
  Prebid rewrite the cookie from script, which Safari caps. Prebid's `extend`
  with a `pixelUrl` keeps working alongside it. Accept: record present and
  cookie absent gives both cookies with those attributes; an equal cookie
  with a consistent `<name>_cst` gives no `Set-Cookie`; an equal cookie with a
  missing or different `<name>_cst` re-sets only the companion; Purpose 1 denied or `_pubcid_optout` expires both;
  Purpose 4 denied with Purpose 1 granted gives no `Set-Cookie` and leaves the
  browser's cookie alone. State: no branch writes it.
- **WB-4 Mirror cookie.** `__Host-ts-eids-mirror` as in #1489: opt-in per
  partner (DEC-10), dropped per vendor when TCF consent is denied, needs
  `bidstream_enabled` and Purpose 4, 3,584-byte budget, host-only. Every
  mirrored ID expires within 30 days. Accept: an ID written with no stated
  expiry is mirrored with an expiry of at most 30 days. State: in #1489 such an
  ID never expires (`Expiry::Never`); closed #1461 capped mirrored IDs at 24
  hours.
- **WB-5 Cacheable pages.** An unchanged cookie gets no `Set-Cookie`, and a
  response carrying `Set-Cookie` is not stored in a shared cache. Accept: a
  returning reader with unchanged cookies gets no `Set-Cookie`, and the
  template-cache deny list uses the configured SharedID name. State: main
  enforces the cache rule on every adapter (`response_privacy.rs:122-148`) and
  does not re-set `ts-ec`, but re-sets the `ts-ec-pull-complete` marker about
  hourly, which #1489 retires; the deny list names only `sharedId`.
- **WB-6 Identity-only page script.** A tsjs mode captures Prebid's IDs without
  the auction shim, reads the mirror cookie and merges the IDs into Prebid.
  Accept: in that mode Prebid receives the mirrored IDs and no Trusted Server
  bidder is added to ad units. State: the `ts-eids` capture runs only inside
  the shim, and nothing reads the mirror.

### EZI: asks of EdgeZero

Behavior and acceptance only. EZI IDs are distinct from #1506's EZ-n
requirement IDs. Each ask is split by platform, in DEC-4 order: the Fastly
and Axum parts ship in R1 and R2 and never wait on a Cloudflare or Spin part.

- **EZI-1 After-response work.** A task registered during a request runs after
  the response is delivered, bounded in time. Accept: on Fastly and Axum the
  task runs after delivery, and on Axum SIGTERM waits for registered tasks up
  to the grace; later, on Cloudflare the task runs after delivery, and Spin
  returns a capability error.
- **EZI-2a Record contract and the Fastly provider.** `KvStore` gains a read
  that returns a generation marker, writes in `Add`, `Overwrite` and
  `IfGenerationMatch` modes with a precondition-failed outcome, and strongly
  consistent existence and prefix listing, or `KvError::Unsupported`. Accept:
  an EdgeZero contract suite passes against the Fastly provider under Viceroy.
- **EZI-2b DynamoDB and celld providers.** Both meet the EZI-2a contract.
  DynamoDB writes are spread across partition keys with no item every insert
  updates. Accept: the contract suite passes against both. State: IABTechLab/edgezero#463's
  `CelldRecordStore` has the semantics on a separate API; IABTechLab/edgezero#414's DynamoDB
  provider has strong existence and listing but no CAS;
  IABTechLab/edgezero#410 and IABTechLab/edgezero#437 rule out conditional writes today.
- **EZI-2c and EZI-2s Cloudflare and Spin stores.** A store on each platform
  meets the EZI-2a contract or reports `KvError::Unsupported` (DEC-8). Accept:
  the contract suite passes, or startup on that platform rejects identity
  configuration.
- **EZI-3 Trusted client IP.** One accessor returns the trusted client IP on
  every adapter, Fastly and Axum first. Accept: the same accessor works on
  each adapter as its part lands.
- **EZI-4 Geo with region on Cloudflare and Spin.** `ctx.geo()` gives country
  and region there, or a capability error. Fastly keeps its existing geo and
  Axum uses CloudFront geo from IABTechLab/edgezero#439. Accept: a US request
  on Cloudflare reports country and region.
- **EZI-5 Rate limiter.** A limiter with Fastly Edge Rate Limiting and
  in-memory and shared-counter backends for Axum, then Cloudflare and Spin
  backends per DEC-11. Accept: a contract test passes per backend.
- **EZI-6 Releases.** R1 contains IABTechLab/edgezero#275 through
  IABTechLab/edgezero#439 plus EZI-2a and EZI-2b. R2 contains the Fastly and
  Axum parts of EZI-1, EZI-3 and EZI-5. Together R1 and R2 cover Fastly and
  Axum. R3 contains the Cloudflare parts of EZI-1 to EZI-5, and R4 the Spin
  parts. Accept: each tag exists and Trusted Server main pins it; R1 and R2
  contain no Cloudflare or Spin part.

## Configuration

- The SharedID cookie name comes from `managed_user_ids[].storage.name`; no
  new setting (SID-1).
- The existing EC settings enable identity; startup rejects them where
  identity is unsupported (ID-2).
- Provider `rate_limit_per_hour` and `mirror_enabled` from #1487 and #1489
  follow DEC-11 and DEC-10.
- The reference deployment turns geo on (AX-3).
- Environment overrides use the `TRUSTED_SERVER__` prefix only (AX-2).
- Record stores are EdgeZero logical stores in `edgezero.toml`, with no
  adapter-local store setting (ID-2).

## Consent and privacy

- Creating an EC needs TCF Purpose 1 (`consent_allows_ec`). EIDs, SharedID
  included, need Purposes 1 and 4 (SID-6).
- An unknown jurisdiction blocks new ECs (fail closed) and keeps an existing
  one.
- The mirror cookie is readable by page scripts, so it is opt-in per partner
  and filtered per vendor (WB-4, DEC-10).
- The `_pubcid_optout` cookie is honored; a localStorage opt-out is invisible
  to the server (SID-2).

## Failure handling

- A store that fails or keeps failing a precondition after bounded retries
  means no new EC and no `ts-ec` on that response; an existing `ts-ec` stays
  (ID-1).
- A provider that times out, errors or is rate limited is skipped and never
  delays the response past the deadline (ID-4).
- A failed after-response task is logged and retried on the next navigation,
  and never holds shutdown past the grace (AX-1, ID-5).
- An unsupported capability fails startup with an error that names the
  capability and the adapter (ID-2).

## Compatibility and rollout

- Fastly records holding a SharedID under `sharedid.org` are re-filed under
  `pubcid.org`, atype 1, on their next write (T3). There is no bulk migration.
- `user.ext.eids` stays only for the downstreams T2 names.
- `ts-ec` format and attributes do not change.
- Adapters gain identity in the order Fastly, Axum, Cloudflare, Spin. The MVP
  is Fastly and Axum; Cloudflare and Spin follow it. Until an adapter's
  tickets land, its startup rejects identity configuration.
- The RC tag keeps its current use until main reaches parity; RC units close
  as their ports land.

## Out of scope

- A narrow install with one path rule and no page proxy. It needs an EC minted
  on a request that is not a navigation.
- Conformance with the SharedID Edge specification.
- An Akamai adapter.

## Decisions

Decided:

- **DEC-1 Cutover** (user, 2026-11-11). #1506 is the EdgeZero cutover. #1527
  closes once #1506 covers its tests.
- **DEC-2 MVP** (user, 2026-11-12). Fastly and Axum are the MVP: plan waves 0
  to 10. Cloudflare, Spin and the decision-gated work follow it.
- **DEC-3 Shared record store** (user, 2026-11-11). The EC record runs on one
  `EcKvStore` over EdgeZero's `KvStore`, with no per-adapter backends.
- **DEC-4 Platform order** (user, 2026-11-12). Fastly, then Axum, then
  Cloudflare, then Spin. Fastly and Axum (the MVP, DEC-2) must ship complete
  without any Cloudflare or Spin work, so every ticket and release is split by
  platform and no Fastly or Axum item is blocked by a Cloudflare or Spin item.
  Cloudflare and Spin follow the MVP.

Proposed and assumed by the plan until confirmed:

- **DEC-5 EdgeZero pin for the Axum production stack.** Wait for release R1
  (proposed), or follow the stack's head under AX-7. Blocks A1.
- **DEC-6 Base and store path for Axum.** Forward-port the RC line to main and
  wait for EZI-2b (proposed), rather than back-port the provider stack to the
  RC line or port #1536's adapter-local store first. Applying each provider
  stack to the RC tag from its main fork point conflicts in 13 to 23 files.
  Blocks A1, A2, A4, A6, A7.
- **DEC-7 SharedID retention.** The stored value wins and lives as long as the
  record (SID-5, proposed). Open: whether the server generates a SharedID when
  none exists. Blocks T13 and the #1488 retention item.

Open:

- **DEC-8 Cloudflare and Spin store.** Which EdgeZero store meets the record
  contract there, per [Platform support](#platform-support). Blocks T8 and T9.
- **DEC-9 LiveRamp Custom ID.** The current value is the IP-derived EC prefix,
  shared by everyone behind one IP. Blocks enabling LiveRamp in #1488.
- **DEC-10 Mirror partners.** Which partners get `mirror_enabled`. Blocks T14.
- **DEC-11 Rate limiter key and backend, and the Axum task count.** Blocks T10
  and A7.
- **DEC-12 EC EID source.** Which source name the EC uses, and whether it keeps
  going out as `user.id`. No ticket until decided.
- **DEC-13 `ts-ec` refresh** for returning readers. Blocks T16.
- **DEC-14 Spin after-response work.** Run it before the response, or skip it.
  Blocks T9.

## How the claims were checked

Every state and "not implemented" claim comes from reading code at named
revisions, not from PR descriptions:

- Code: `git grep` and `git show` across Trusted Server main, every open PR
  head in both repositories, the RC tags, branches with no open PR, and PRs
  merged or closed since 2026-10-01.
- Overlaps and conflicts: `git diff --name-only` between heads and
  `git merge-tree --write-tree` trial merges.
- PR and issue state: `gh pr view` and `gh api graphql`, read on the dates in
  the status line.
- The full CI gate list ran at this PR's first head, and the docs gates at
  each later head; results are in the review threads.

## Review log

### Round 1, 2026-11-12

Reviews 5871533635 and 5871619954 at `00df85414`, from fresh agent sessions
covering factual claims, consistency and coverage, merge-order feasibility, and
repository policy. 34 findings: 8 blocking, 17 should-fix, 9 nits. This
revision addresses them; each thread holds its disposition.

### Round 2, 2026-11-12

Review 5875159165 at `7cb8933f7`. It accepted and resolved 28 round-1
findings and left six open: the fallback clock (thread 1), the review log and
draft gate (7), SH-1's acceptance against the Axum HTTP client exception (14),
the RC-unit baseline (15), ID-5's acceptance on Spin (18) and decision gates
missing from plan cells (23). It added two should-fix findings: Task 1 must
re-parent the EdgeZero issues it moves (35), and WB-3's no-write case must
require a consistent `<name>_cst` (36). No blocking finding. The revision after
it addresses all eight; round 3 is pending, and the PR stays in draft until a
round finds nothing blocking.

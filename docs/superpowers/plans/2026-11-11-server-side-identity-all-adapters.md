# Server-side identity on every adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** File the missing tickets under #1558, #1236 and
IABTechLab/edgezero#468, and tie every existing PR to them, in an order that
keeps main shippable at each step.

**Architecture:** This plan's tasks are GitHub operations, not code. Tasks 1
to 4 create and link issues in dependency order, EdgeZero first because
Trusted Server tickets depend on it. Tasks 5 and 6 post the per-PR fixes in
Part C to the named PRs, and Task 7 checks coverage. Code lands later in each
ticket's own implementation PR, following the merge order in Part A, with its
own plan when it needs one.

**Tech Stack:** `gh` CLI and GitHub GraphQL (issue types, sub-issues, project
items) against IABTechLab/trusted-server and IABTechLab/edgezero.

**Spec:**
[server-side identity design](../specs/2026-11-11-server-side-identity-all-adapters-design.md)

## Global Constraints

- One-account mode: every write acts as the PR author's login.
- Platform order: Fastly, then Axum, then Cloudflare, then Spin (DEC-4).
- Issue bodies state the requirement, its acceptance and its dependencies,
  and nothing about deployments or their state.
- EdgeZero references are written `IABTechLab/edgezero#N`; a bare `#N` means
  Trusted Server.
- EdgeZero issue bodies state behavior and acceptance only.
- Issue types are set through GraphQL: Feature `IT_kwDOBPEB8s4A35yA`, Task
  `IT_kwDOBPEB8s4A35x7`. New issues join project `PVT_kwDOBPEB8s4BFKrl`.
- An issue Part B names for reuse is reused, never duplicated.
- EdgeZero code is never changed from here; EdgeZero items are requests.
- Trusted Server PRs #1342 to #1423 and #1448 are out of scope (DEC-2).
- Issue and comment text uses no em dashes and no names of people; comments
  start with `CREATOR: `.

## Review Focus

1. A duplicate issue for work an existing issue already tracks. Every create
   step in Tasks 2 to 4 runs a search first.
2. A Part C item posted to a PR whose head has moved. Tasks 5 and 6 compare the
   head with Part C before posting and re-check the item if it moved.
3. A ticket whose dependency has no issue number yet. Tasks run EdgeZero,
   then #1236, then #1558, and Task 7 checks every "Depends on".
4. A bare `#N` meant for EdgeZero inside a Trusted Server issue. Task 7 scans
   the posted bodies.
5. A body that goes beyond its requirement, acceptance and dependencies. Task 7
   reads every body.

---

## Part A: merge order

### Stacking, read 2026-11-12

Trusted Server:

| PR                    | Head        | Base on GitHub             | Fix                                                                                                                  |
| --------------------- | ----------- | -------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| #1557                 | `76e1dd5f6` | main                       | None                                                                                                                 |
| #1525                 | `cd8977cad` | main                       | None                                                                                                                 |
| #1526                 | `e529e5576` | #1525's branch             | Retarget to main; its own diff applies cleanly there                                                                 |
| #1516                 | `214cd4821` | main                       | None                                                                                                                 |
| #1503                 | `1b59bfbe7` | main                       | None; needs neither #1506 nor new EdgeZero                                                                           |
| #1449                 | `27361ec58` | main                       | None; needs neither #1506 nor new EdgeZero                                                                           |
| #1506                 | `851537045` | main                       | Conflicting; rebase after #1557, #1526 and #1516                                                                     |
| #1487                 | `4e7a6e02b` | main                       | Rebase after #1526 and #1506                                                                                         |
| #1489                 | `29de3d446` | #1487's branch             | None                                                                                                                 |
| #1488                 | `25f35720b` | #1487's branch, old commit | Restack onto #1489's branch; conflicting                                                                             |
| #1527                 | `a020daeec` | #1526's branch             | Close once #1506 covers its tests (DEC-1)                                                                            |
| #1528                 | `ea779fb47` | #1527's branch             | Port its two commits onto #1506's outbound API; they modify #1527-only `platform/outbound.rs`; repin off `456313675` |
| #1543                 | `dc1a063d8` | #1449's branch             | Conflicting; rebase onto #1449; ships with A6                                                                        |
| #1424                 | `2423a2ed2` | main                       | Conflicting, head `rc/edgezero-431-demo`; close at parity                                                            |
| #1499                 | `084fdbba9` | #1424's branch             | Close at parity                                                                                                      |
| #1541, #1531 to #1540 | various     | RC baselines               | Forward-port to main as new PRs; close each as its port lands                                                        |

EdgeZero:

| PR                                                                                                                                                   | Head       | Base on GitHub                   | Fix                                                                                                                       |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| IABTechLab/edgezero#275                                                                                                                              | `14a6ca8b` | main                             | Absorb IABTechLab/edgezero#460, IABTechLab/edgezero#406's `9f090e57` and one copy of the CI trigger change before merging |
| IABTechLab/edgezero#460                                                                                                                              | `cec19886` | IABTechLab/edgezero#275's branch | Merge into that branch before IABTechLab/edgezero#275 merges                                                              |
| IABTechLab/edgezero#406, IABTechLab/edgezero#407, IABTechLab/edgezero#409, IABTechLab/edgezero#414, IABTechLab/edgezero#422, IABTechLab/edgezero#439 | various    | linear chain                     | Move IABTechLab/edgezero#414's `4ea99467` to IABTechLab/edgezero#406 and its `9a985c99` to IABTechLab/edgezero#407        |
| IABTechLab/edgezero#463                                                                                                                              | `6f9fa8bc` | IABTechLab/edgezero#422's branch | Drop the IABTechLab/edgezero#439 commits it carries                                                                       |
| IABTechLab/edgezero#464                                                                                                                              | `6fe32531` | IABTechLab/edgezero#439's branch | Drop IABTechLab/edgezero#454's prototype commits                                                                          |
| IABTechLab/edgezero#453                                                                                                                              | `633f0552` | IABTechLab/edgezero#439's branch | Rebase off its stale base                                                                                                 |
| IABTechLab/edgezero#389, IABTechLab/edgezero#401, IABTechLab/edgezero#444, IABTechLab/edgezero#452                                                   | various    | main                             | Land after IABTechLab/edgezero#275 merges; they conflict with it in 7, 6, 4 and 4 files                                   |
| IABTechLab/edgezero#466                                                                                                                              | `cb48c6b3` | main                             | Land after IABTechLab/edgezero#444; they conflict in `edgezero-cli` `config.rs` and `lib.rs`                              |
| IABTechLab/edgezero#287                                                                                                                              | `d590f0eb` | main                             | Last; shares 52 files with IABTechLab/edgezero#275, 27 of them under `crates/`                                            |

### Phases

Phase A runs now and needs no new EdgeZero.

1. T1, partner routes answered locally.
2. #1557.
3. #1526 retargeted to main; #1525 in parallel.
4. #1516.
5. #1503 and #1449.
6. T2, `user.eids` placement, converting every EID-absence assertion.
7. EdgeZero: IABTechLab/edgezero#460, `9f090e57` and one CI trigger change go
   into IABTechLab/edgezero#275's branch; the misplaced commits move; IABTechLab/edgezero#463,
   IABTechLab/edgezero#464 and IABTechLab/edgezero#453 are rebased.

Phase B follows Phase A.

1. #1506, rebased and repinned to IABTechLab/edgezero#275's head after the
   fold-in. Fallback: if the fold-in has not landed two weeks after Phase A
   ends, #1506 merges pinned to IABTechLab/edgezero#275's head as it stands
   (allowed by AX-7) and keeps its readthrough rejection until the repin.
2. #1487, rebased onto #1526 and #1506.
3. #1489, then #1488 restacked onto it.
4. T3 SharedID cleanup, T4 shared state, T5 conformance suite (Fastly first),
   T15 identity-only tsjs.
5. EdgeZero: IABTechLab/edgezero#275 merges. Then IABTechLab/edgezero#389,
   IABTechLab/edgezero#401, IABTechLab/edgezero#444, IABTechLab/edgezero#466 and IABTechLab/edgezero#452 rebase onto main. Then EZI-2a. Then
   IABTechLab/edgezero#406, IABTechLab/edgezero#407, IABTechLab/edgezero#409, IABTechLab/edgezero#414, IABTechLab/edgezero#422, IABTechLab/edgezero#439, IABTechLab/edgezero#464 and IABTechLab/edgezero#463. Then
   EZI-2b and release R1 (EZI-6). Then EZI-1 and EZI-5 for Fastly and Axum,
   and EZI-3, in release R2. IABTechLab/edgezero#287 goes last.

Phase C follows R1 and brings Axum to parity with the RC tag.

1. Main repins to R1.
2. A1 (#1528 port) with A9.
3. T6, one record backend over EdgeZero; Fastly moves first.
4. A4 geo, then A2 EC lifecycle, then A8.
5. A6 image with #1543, then A7.
6. Close #1541, #1424, #1499, #1527 and the RC units as their ports land.

Phase D follows R2, then R3, then R4, one platform at a time (DEC-4).

1. Axum, after R2: A3, T10, T7, T11, T12, T13, T14, then A5.
2. Cloudflare, after R3: T8, then T10, T11, T12, T13 and T14 for Cloudflare.
3. Spin, after R4: T9, then the same tickets for Spin.
4. T16 once DEC-13 is decided.

## Part B: tickets

"Blocked by" names a spec decision. A ticket that spans platforms ships per
platform in DEC-4 order.

### EdgeZero, under IABTechLab/edgezero#468

| ID     | Ticket                                                                                                                                                                                             | Reqs   | New or reuse                                                                                                                                                 | Phase      |
| ------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------- |
| EZI-2a | `KvStore` record contract (generation read, `Add` / `Overwrite` / `IfGenerationMatch`, precondition-failed outcome, strong exists and prefix list, `KvError::Unsupported`) and the Fastly provider | EZI-2a | New; IABTechLab/edgezero#410 and IABTechLab/edgezero#437 rule out conditional writes today                                                                   | B          |
| EZI-2b | DynamoDB and celld providers meet the contract; DynamoDB spreads writes across partition keys with no item every insert updates                                                                    | EZI-2b | New                                                                                                                                                          | B          |
| EZI-1  | Bounded work after the response on Fastly, then Axum (drained on SIGTERM), then Cloudflare; capability error on Spin                                                                               | EZI-1  | New                                                                                                                                                          | B, R2 / R3 |
| EZI-5  | Rate limiter: Edge Rate Limiting on Fastly, in-memory and shared counter on Axum, then Cloudflare and Spin backends per DEC-11                                                                     | EZI-5  | New                                                                                                                                                          | B, R2 / R3 |
| EZI-3  | Trusted client IP through one accessor on every adapter                                                                                                                                            | EZI-3  | Reuse IABTechLab/edgezero#89 with IABTechLab/edgezero#90, IABTechLab/edgezero#91, IABTechLab/edgezero#92 and IABTechLab/edgezero#397; new sub-issue for Spin | B, R2      |
| EZI-4  | Geo with country and region through `ctx.geo()` on Fastly, then Cloudflare, then Spin                                                                                                              | EZI-4  | New; IABTechLab/edgezero#432 and IABTechLab/edgezero#438 cover Axum                                                                                          | R3 / R4    |
| EZI-6  | Releases R1 to R4 as the spec's EZI-6 defines                                                                                                                                                      | EZI-6  | New; R1 under IABTechLab/edgezero#391                                                                                                                        | B onward   |

### Axum deployable, under #1236

| ID  | Ticket                                                                                                                                                       | Reqs                   | Depends on      | Blocked by   | Type    | New or reuse              | Phase |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------- | --------------- | ------------ | ------- | ------------------------- | ----- |
| A1  | Serve Axum in production mode from main: port #1528 onto #1506's outbound API, repin to R1, fail before listening, SIGTERM drain                             | AX-1, AX-2, AX-7       | R1, #1506       | DEC-5, DEC-6 | Feature | New; relates #1141        | C     |
| A2  | Axum EC lifecycle on the shared backend: forward-port #1536 and #1537's mint, record, `ts-ec`, identify, auction and body EIDs, without their adapter stores | ID-1, ID-3, ID-8, WB-1 | T6, A1, A4      | DEC-6        | Feature | New                       | C     |
| A3  | Drain after-response tasks on SIGTERM                                                                                                                        | AX-1, ID-5             | EZI-1 in R2, A1 | none         | Feature | New                       | D     |
| A4  | Trusted client IP and geo behind CloudFront: forward-port #1532 without its core change that maps a US request with no region to `Unknown`                   | AX-3                   | A1              | DEC-6        | Feature | New; relates #1134        | C     |
| A5  | Load-test the record store at the target page-load rate with provider refreshes                                                                              | AX-4                   | A2, T11         | none         | Task    | New                       | D     |
| A6  | Publish and attest the production image; one image contract for #1539 and #1543                                                                              | AX-5                   | A1              | DEC-6        | Feature | New                       | C     |
| A7  | Reference infrastructure: wire DynamoDB, readiness that checks dependencies, JSON logs, two or more tasks, IPv6 CloudFront ranges, geo on by default         | AX-3, AX-6             | A1, A6          | DEC-11       | Feature | Reuse #1142 for readiness | C     |
| A8  | Run the SIGTERM-during-record-write test (`scripts/test-native-ec-image.mjs` from #1536) in CI                                                               | AX-1                   | A2              | none         | Task    | New                       | C     |
| A9  | Use only `TRUSTED_SERVER__` environment names; drop the RC's `TS_NATIVE_GEO_PROFILE_FILE`, `TS_NATIVE_BINDINGS_TOML` and `TS_APPCONFIG_TOKEN`                | AX-2                   | A1              | none         | Task    | New                       | C     |

### Trusted Server, under #1558

| ID  | Ticket                                                                                                                                                                                                                                                                | Reqs                         | Depends on                                    | Blocked by    | Type    | Phase        |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------- | --------------------------------------------- | ------------- | ------- | ------------ |
| T1  | Answer identify and batch sync locally on every adapter: run them with a record store, return 501 without one, never reach the publisher origin                                                                                                                       | ID-3                         | none                                          | none          | Task    | A            |
| T2  | Write EIDs to OpenRTB 2.6 `user.eids` in the bidder, Prebid Server and APS builders; every EID-absence test asserts both `user.eids` and `user.ext.eids` absent; keep `user.ext.eids` only for a named downstream                                                     | SID-4                        | none                                          | none          | Feature | A            |
| T3  | SharedID cleanup: delete the `sharedId` cookie and `sharedid.org` path, dedupe one UUID across cookie, `ts-eids`, body and record, re-file stored `sharedid.org` entries on next write, apply the SID-6 gate, use the configured name in the template-cache deny list | SID-1, SID-3, SID-6, WB-5    | #1488                                         | DEC-7         | Feature | B            |
| T4  | Build settings, partner registry, store clients and HTTP client once per process or isolate; remove Cloudflare's first-request `Env` capture and config `block_on`; port Spin's config `block_on` removal from RC commit `933de5921`                                  | SH-1, SH-3, SH-4             | #1506                                         | none          | Task    | B            |
| T5  | One identity conformance suite with ID-1's cases, run on every supported adapter, and an unsupported-startup test elsewhere                                                                                                                                           | ID-1                         | #1506                                         | none          | Task    | B            |
| T6  | Run the EC record on one `EcKvStore` over EdgeZero's `KvStore`; delete `crates/trusted-server-adapter-fastly/src/ec_kv.rs`                                                                                                                                            | ID-2                         | EZI-2a in R1                                  | none          | Feature | C            |
| T7  | Axum partner APIs on the shared backend: batch sync, pull sync, admin lookup                                                                                                                                                                                          | ID-3, ID-5                   | A2, EZI-1, EZI-5                              | none          | Feature | D            |
| T8  | Cloudflare EC lifecycle: mint, record, `ts-ec`, identify, batch sync, pull sync, admin lookup, auction and body EIDs                                                                                                                                                  | ID-1, ID-2, ID-3, ID-8, WB-1 | T6, EZI-1, EZI-2, EZI-4, EZI-5 for Cloudflare | DEC-8         | Feature | D            |
| T9  | Spin EC lifecycle, the same scope as T8                                                                                                                                                                                                                               | ID-1, ID-2, ID-3, ID-8, WB-1 | T8, EZI-2, EZI-4, EZI-5 for Spin              | DEC-8, DEC-14 | Feature | D            |
| T10 | Rate limits: batch sync per partner, pull sync, provider lookups and record creation; shared counter with more than one Axum task                                                                                                                                     | ID-6                         | EZI-5                                         | DEC-11        | Feature | D            |
| T11 | Providers on every supported adapter: remove the startup refusal, register provider kinds, classify navigations, await lookups before finalization, and run only follow-up writes after the response                                                                  | ID-4, ID-5, WB-1             | A2, T10, EZI-1                                | none          | Feature | D            |
| T12 | Refuse record creation from non-browser clients on every supported adapter                                                                                                                                                                                            | ID-7                         | A2                                            | none          | Feature | D            |
| T13 | SharedID cookie write-back on the document response, with `<name>_cst`, under SID-2 and SID-6                                                                                                                                                                         | WB-3, SID-6                  | T3, #1488 retention fix, A2                   | DEC-7         | Feature | D            |
| T14 | Mirror cookie on Axum, then Cloudflare, then Spin                                                                                                                                                                                                                     | WB-4                         | T11                                           | DEC-10        | Feature | D            |
| T15 | Identity-only tsjs mode                                                                                                                                                                                                                                               | WB-6                         | #1489                                         | none          | Feature | B            |
| T16 | `ts-ec` refresh for returning readers without breaking cacheability                                                                                                                                                                                                   | WB-2, WB-5                   | none                                          | DEC-13        | Task    | after DEC-13 |

Requirements met inside open PRs, not by a ticket: ID-8 on main and SH-2
(#1487), SID-1 to SID-3 and SID-6 for the provider, SID-5 (#1488), WB-4
expiry (#1489), AX-7 (#1528). Part C lists them.

## Part C: fixes to existing PRs

Each item names the head it was checked at. Post it as a comment on that PR
(Tasks 5 and 6); once posted, the PR thread is the source of truth.

### Trusted Server

- **#1506 at `851537045` (AX-7, SID-4).** Rebase after #1557, #1526 and #1516.
  Repin after the fold-in, or under Phase B's fallback; drop the
  `origin_readthrough_enabled` rejection (`config.rs:317`) once cache tags
  return. The repin brings IABTechLab/edgezero `b97efec1`, which stops Spin
  sending `Content-Length: 0` on bodiless DELETE and OPTIONS. Port #1527's
  native-client, response-owner and managed Fastly delivery tests if missing.
  If T2 has landed, make the EID-absence assertions (`endpoints.rs:1708-1711`,
  `prebid.rs:7023`) check both `user.eids` and `user.ext.eids`.
- **#1526 at `e529e5576` (ID-2, SH-3).** Retarget to main. Address its round-1
  review, including the finalize-under-`block_on` test gap.
- **#1525 at `cd8977cad`.** Address its round-1 review.
- **#1487 at `4e7a6e02b` (ID-6, ID-8, SH-2, SH-3, SID-4).** Rebase onto #1526
  and #1506 (13 and 28 shared files). Await lookup collection in the async
  finalize: `main.rs:699` calls `block_on` inside a function #1526 wraps in
  `block_on` (`main.rs:550`). Move per-request lookups out of the
  `thread_local`. Keep saving `/auction` body EIDs; a test at
  `finalize.rs:1434` asserts they are dropped. Send lookups through EdgeZero's
  `HttpClient`. Key the limit per DEC-11. If T2 has landed, convert its EID
  assertions as for #1506.
- **#1489 at `29de3d446` (WB-4).** Cap every mirrored ID's expiry at 30 days.
  `coordinator/validate.rs:132-135` turns an unspecified expiry into
  `Expiry::Never`, and
  `crates/trusted-server-identity-providers/src/pull_sync.rs:128` writes such
  IDs.
- **#1488 at `25f35720b` (SID-1, SID-2, SID-3, SID-5, SID-6).** Restack onto
  #1489; the two overlap in 13 files and conflict in 11. Read the cookie name
  from `managed_user_ids[].storage.name` and default the source to
  `pubcid.org`, atype 1. Gate reads, storage and sending on EID permission
  (SID-6). Keep main's retention (SID-5, DEC-7). Refuse an enabled provider
  kind that is not registered. Settle the LiveRamp Custom ID (DEC-9) and
  response shape. Address its round-1 review.
- **#1528 at `ea779fb47` (AX-7, SH-2).** Port its two commits onto #1506's
  outbound API: a trial merge conflicts in 9 files, two of them modify/delete
  on `platform/outbound.rs` and `platform/outbound_native_tests.rs`, which only
  #1527 has. Repin off `456313675`. Fix the CLI test (`AxumDevServerConfig`
  lacks `ingress`), the type error and the two new high CodeQL alerts. Remove
  the per-request `AtomicBool` from the Fastly `AppState`. Feeds A1.
- **#1543 at `dc1a063d8` (AX-5).** Rebase onto #1449. It publishes the
  dev-server image (`STOPSIGNAL SIGINT`), so it ships with A6's production
  image contract.
- **RC units, fixed in their forward-ports.** #1532: drop the core change that
  maps a US request with no region to `Unknown`, which fails every US
  Cloudflare request closed (AX-3). #1536: do not port the adapter-local
  DynamoDB store (ID-2, AX-4). #1537: its example config breaks startup on
  Axum, Cloudflare and Spin. #1539: fix the failing Axum test. #1540: trust
  IPv6 CloudFront ranges, turn geo on, wire DynamoDB (AX-3, AX-6). All units:
  replace `TS_*` names (AX-2). #1541: drop the commits of closed #1545.
- **Proposed closures.** #1527 once its tests move (DEC-1); #1310, which
  #1488's ID5 provider supersedes; #1424, #1499, #1541 and the RC units at
  parity.

### EdgeZero, requests to the PR authors

- **IABTechLab/edgezero#275 at `14a6ca8b`.** Absorb IABTechLab/edgezero#460,
  `9f090e57` from IABTechLab/edgezero#406 (the CLI template emits Fastly-only
  image code that IABTechLab/edgezero#275 gates) and one copy of the CI trigger
  change, which today sits in IABTechLab/edgezero#406, IABTechLab/edgezero#460 and IABTechLab/edgezero#414 (all
  IABTechLab/edgezero). Optionally guard all five egress unwind sites against
  a panicking logger; `0769cbfd` in IABTechLab/edgezero#409 covers three.
- **IABTechLab/edgezero#460 at `cec19886`.** Validate tags (printable ASCII,
  non-empty) and make Cloudflare reject tags instead of silently using the
  default. Fix the red Cloudflare wasm clippy; the fix exists only in RC
  commit `ebf622c2`. It removes `Copy` from an IABTechLab/edgezero#275 public
  type, so it lands before that release.
- **IABTechLab/edgezero#406 at `6d1dda16`.** Take
  `run_production_app_with_options_and_initializer` from `4ea99467`; the
  Trusted Server RC's native startup calls it.
- **IABTechLab/edgezero#407 at `d6f74308`.** Take the flaky-test fix
  `9a985c99`.
- **IABTechLab/edgezero#414 at `3733482f`.** Drop those two commits once they
  move down.
- **IABTechLab/edgezero#463 at `6f9fa8bc`, IABTechLab/edgezero#464 at `6fe32531`, IABTechLab/edgezero#453 at
  `633f0552`.** Rebase as Part A's table says.
- **IABTechLab/edgezero#389 at `0653e6e3` and IABTechLab/edgezero#401 at
  `1a6ae861`.** Land after IABTechLab/edgezero#275 merges, redoing the RC-only
  merges `7c07b8e7` (IABTechLab/edgezero#389) and `2510ecc9` (IABTechLab/edgezero#401) and the RC-only follow-up
  commit `501a2dce` (IABTechLab/edgezero#401).
- **IABTechLab/edgezero#444 at `217983c0` and IABTechLab/edgezero#466 at
  `cb48c6b3`.** Land IABTechLab/edgezero#444 first, after IABTechLab/edgezero#275.
- **Every stack PR.** Refresh the CI results cited in the descriptions.

### Cutover follow-ups outside these epics

These belong to #1505 and IABTechLab/edgezero#436:

- Cache-tag validation: the IABTechLab/edgezero#460 item above.
- TLS chain-only verification: reuse IABTechLab/edgezero#81, stating that
  verification is never turned off.
- Idle bound for streamed bodies: reuse IABTechLab/edgezero#82.
- Fire-and-forget send and a published outbound test double: new.
- Spin host override: mostly in IABTechLab/edgezero#275; only the probe
  evidence is missing.

---

## Tasks

### Task 1: Align the epics

**Files:** none; GitHub only.

**Interfaces:**

- Produces: #1236 as a sub-issue of #1558. The note on #1236 saying this
  design is not its spec was posted while #1559 was in review.

- [ ] **Step 1: Add #1236 under #1558**

Run: `gh api graphql -f query='mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c}){issue{number}}}' -F p="$(gh issue view 1558 --json id --jq .id)" -F c="$(gh issue view 1236 --json id --jq .id)"`
Expected: `{"data":{"addSubIssue":{"issue":{"number":1558}}}}`

- [ ] **Step 2: Verify**

Run: `gh api graphql -f query='{repository(owner:"IABTechLab",name:"trusted-server"){issue(number:1558){subIssues(first:50){nodes{number}}}}}' --jq '[.data.repository.issue.subIssues.nodes[].number]'`
Expected: a list containing `1236`.

### Task 2: File the EdgeZero tickets under IABTechLab/edgezero#468

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part B's EdgeZero table and the spec's EZI requirements.
- Produces: an issue number per EZI row, recorded in a `CREATOR:` comment on
  IABTechLab/edgezero#468.

- [ ] **Step 1: Search before each row**

For EZI-2a, EZI-2b, EZI-1, EZI-5, EZI-3, EZI-4 and EZI-6 in that order, run
`gh issue list -R IABTechLab/edgezero --state all --search "<two key terms from the row>"`.
Expected: no open issue other than those the row names for reuse.

- [ ] **Step 2: Create or reuse**

For a new row, run
`gh issue create -R IABTechLab/edgezero --title "<Ticket column>" --body-file <file>`,
where the file holds three sections: Behavior (the spec's EZI text), Acceptance
(its Accept line) and Related (the reuse cell and
IABTechLab/trusted-server#1558). For a reuse row, post a `CREATOR:` comment on
the reused issue linking IABTechLab/edgezero#468 and the acceptance.
Expected: one issue URL per new row.

- [ ] **Step 3: Type, parent and project**

For each new issue, set type Feature through `updateIssue`, add it under
IABTechLab/edgezero#468 through `addSubIssue`, and add it to the project
through `addProjectV2ItemById`.
Expected: each mutation returns the issue number.

- [ ] **Step 4: Record the numbers**

Post a `CREATOR:` comment on IABTechLab/edgezero#468 mapping each EZI ID to its
issue.

### Task 3: File the Axum tickets under #1236

Same steps as Task 2, against IABTechLab/trusted-server and #1236, for A1 to
A9 in table order. Each body has Summary (Ticket column), Requirements (Reqs
with a link to the spec section), Acceptance (each requirement's Accept line),
Depends on (issue numbers from Task 2 and this task) and Blocked by. A7 links
#1142; A1 links #1141; A4 links #1134. Record the map in a `CREATOR:` comment
on #1236.

### Task 4: File the Trusted Server tickets under #1558

Same steps as Task 3, for T1 to T16 in table order, under #1558.

### Task 5: Tie out the Trusted Server PRs

- [ ] **Step 1: Check heads**

For each Part C Trusted Server item, run
`gh pr view <N> --json headRefOid --jq .headRefOid`.
Expected: the head Part C names. If it moved, re-check the item against the
new head before posting.

- [ ] **Step 2: Post**

Post one `CREATOR:` comment per PR with its Part C item and the ticket it
serves (`Part of #<ticket>`). For proposed closures, ask the author; do not
close another author's PR.

### Task 6: Tie out the EdgeZero PRs

Same steps as Task 5, for the EdgeZero items in Part C, each linking its EZI
issue from Task 2.

### Task 7: Verify coverage

- [ ] **Step 1: Count sub-issues**

Run the Task 1 Step 3 query for #1558, #1236 and IABTechLab/edgezero#468.
Expected: 16, 9 and 7 entries plus any reused issues, matching Part B.

- [ ] **Step 2: Check dependencies and references**

For every created issue, read its body and confirm three things: each
"Depends on" names an issue number; no Trusted Server body contains a bare
`#N` below 1000 (EdgeZero references use `IABTechLab/edgezero#N`); and the
body holds only its requirement, acceptance and dependencies.
Expected: no gaps.

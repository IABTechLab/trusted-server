# Server-side identity on every adapter: merge order and tickets

Status: draft plan, 2026-11-11. Requirements:
[the spec](../specs/2026-11-11-server-side-identity-all-adapters-requirements.md).
Epics: Trusted Server #1558, EdgeZero IABTechLab/edgezero#468.

Every PR base and state below was read from GitHub on 2026-11-11. Every
"not implemented" claim comes from a five-part audit of both repositories:
main, every open PR head, the RC tags, branches with no PR, and PRs merged or
closed since 2026-10-01. Trusted Server PRs #1342 to #1423 and #1448 are out of
scope by decision.

## Part 1: merge order

### Is it stacked correctly?

Trusted Server:

| PR                                  | Base on GitHub             | Correct for the plan? | Fix                                                                                                                |
| ----------------------------------- | -------------------------- | --------------------- | ------------------------------------------------------------------------------------------------------------------ |
| #1557                               | main                       | Yes                   | None                                                                                                               |
| #1525                               | main                       | Yes                   | None                                                                                                               |
| #1526                               | #1525's branch             | No                    | Retarget to main. Its own diff applies cleanly there, so it does not wait on #1525's changes requested             |
| #1516                               | main                       | Yes                   | None                                                                                                               |
| #1506                               | main                       | Yes, but conflicting  | Rebase after #1557, #1526 and #1516 land                                                                           |
| #1527                               | #1526's branch             | Not merged            | Close once #1506 covers its tests                                                                                  |
| #1528                               | #1527's branch             | No                    | Retarget onto main after #1506. Repin off `456313675`, a revision that exists only on EdgeZero's DO NOT MERGE #462 |
| #1487                               | main                       | Yes, needs rebase     | Rebase after #1526 and #1506                                                                                       |
| #1489                               | #1487's branch             | Yes                   | None                                                                                                               |
| #1488                               | #1487's branch, old commit | No                    | Restack onto #1489's branch so the two form one line. It is conflicting and sits on an old #1487 commit            |
| #1503                               | main                       | Yes                   | None                                                                                                               |
| #1543                               | #1449's branch             | Yes, but conflicting  | Rebase onto #1449                                                                                                  |
| #1541, #1531 to #1540, #1424, #1499 | RC baselines               | Not merged            | Forward-port to main as new PRs; close each as its port lands                                                      |

EdgeZero:

| PR                                      | Base on GitHub | Correct for the plan?      | Fix                                                                                                                                  |
| --------------------------------------- | -------------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| #275                                    | main           | Yes, missing fixes         | Merge #460 into #275's branch first. Add #406's `9f090e57`, one copy of the CI trigger change, and #460's clippy fix from `ebf622c2` |
| #460                                    | #275's branch  | Yes                        | Merge into #275's branch before #275 merges                                                                                          |
| #406 → #407 → #409 → #414 → #422 → #439 | linear chain   | Yes, two commits misplaced | Move #414 `4ea99467` (production startup API) to #406 and #414 `9a985c99` (flaky test fix) to #407                                   |
| #463                                    | #422's branch  | No                         | Drop the #439 commits it carries                                                                                                     |
| #464                                    | #439's branch  | No                         | Drop #454's prototype commits it carries                                                                                             |
| #453                                    | #439's branch  | No                         | Rebase off its stale base                                                                                                            |
| #444, #466, #452, #401, #389, #287      | main           | Yes                        | #389 and #401 must redo the RC-only merges that fit them onto #275 (`7c07b8e7`, `2510ecc9`, `501a2dce`)                              |

### Order

Phase A runs now. Nothing in it needs new EdgeZero.

1. TS T1, partner routes answered locally.
2. TS #1557.
3. TS #1526, retargeted to main. #1525 lands in parallel.
4. TS #1516.
5. EZ: #460 and the other #275 fixes go into #275's branch. The independent
   PRs #444, #466, #452, #401 and #389 land in any order.
6. TS T3, `user.eids` placement. It is independent and can land any time.

Phase B depends on Phase A.

1. TS #1506, rebased, repinned to #275's head after the fold-in.
2. TS #1487, rebased onto #1526 and #1506.
3. TS #1489, then #1488 restacked onto it.
4. TS T2, SharedID correctness, removing the legacy `sharedId` path.
5. TS #1503, then #1449 and #1543.
6. EZ: #275 merges. Then #406, #407, #409, #414, #422 and #439 in order,
   with the misplaced commits moved. Then #464 and #463, both rebased. Then
   E1, conditional KV. Then a release tag, E6. #287 goes last.

Phase C depends on the EdgeZero release.

1. TS repins to the EdgeZero release tag.
2. TS #1528, retargeted and repinned.
3. TS T8, one record backend over EdgeZero, deleting the Fastly backend.
4. TS forward-ports of the RC line: native EC lifecycle (A2), geo (A4),
   image (A6), AWS examples (A7). Close #1541, #1424, #1499, #1527 and the RC
   units.

Phase D is the new feature work: T9 to T14 and A3, A5, A8.

## Part 2: tickets

Each ticket names its requirement IDs from the spec. "Review item" means the
fix belongs in an open PR's review, not in a new issue.

### Trusted Server epic #1558

Make the existing Axum self-hosting epic #1236 a sub-epic of #1558, and put the
AX tickets under it. #1236's item 2 proposes a hand-written Axum `EcKvStore`
on redb or Redis. Point it at T8 instead.

| ID  | Ticket                                                                                                                                                                                                                                                                      | Reqs             | Depends on                  | Type    |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- | --------------------------- | ------- |
| T1  | Answer identify and batch sync locally on Axum, Cloudflare and Spin                                                                                                                                                                                                         | ID-3             | None                        | Task    |
| T2  | Read SharedID from the publisher's Prebid `storage.name` (already in `managed_user_ids`), honor the `_pubcid_optout` cookie, store and send one `pubcid.org` atype 1 EID, dedupe across cookie, `ts-eids`, body and record, and delete the `sharedId` / `sharedid.org` path | SID-1 to SID-3   | #1488                       | Feature |
| T3  | Write EIDs to OpenRTB 2.6 `user.eids` in the bidder, Prebid Server and APS builders (`openrtb.rs:372`, `prebid.rs:2882`, `aps.rs:1131`). Keep `user.ext.eids` only where a named downstream needs it                                                                        | SID-4            | None                        | Feature |
| T8  | Run the EC record on one `EcKvStore` over EdgeZero's `KvStore`, and delete `adapter-fastly/src/ec_kv.rs`                                                                                                                                                                    | ID-2             | E1, E6                      | Feature |
| T9  | Run server-side providers on every adapter with a record store: remove the startup refusal, register provider kinds, classify navigations, collect after the response, rate limit                                                                                           | ID-4 to ID-6     | T8, E2, E3                  | Feature |
| T10 | Run the EC lifecycle on Cloudflare and Spin (mint, record, `ts-ec`, identify, batch sync, pull sync)                                                                                                                                                                        | ID-1, ID-2       | T8, E1 on those platforms   | Feature |
| T11 | Write the SharedID cookie on the document response: Prebid's storage name and Domain, the `<name>_cst` companion so Prebid does not rewrite it from script, expiry on consent denial or opt-out                                                                             | WB-3             | T2, #1488 retention fix     | Feature |
| T12 | Write the mirror cookie on Axum, Cloudflare and Spin                                                                                                                                                                                                                        | WB-4             | T9                          | Feature |
| T13 | Identity-only tsjs mode: capture Prebid's IDs without the auction shim, read the mirror cookie, merge into Prebid                                                                                                                                                           | WB-6             | #1489                       | Feature |
| T14 | Decide and implement `ts-ec` refresh for returning readers without breaking cacheability                                                                                                                                                                                    | WB-2, WB-5       | None                        | Task    |
| T15 | Build settings, partner registry, store clients and the HTTP client once per isolate on Cloudflare and Spin. Remove Cloudflare's first-request `Env` capture and its config `block_on`                                                                                      | SH-1, SH-3, SH-4 | #1506                       | Task    |
| T16 | Protect record creation on every adapter. Off Fastly only the User-Agent is checked. On Fastly a JA4 plus a browser User-Agent passes                                                                                                                                       | ID-7             | None                        | Feature |
| T17 | One identity conformance suite run against all four adapters. Today the EC scenarios run on Fastly only                                                                                                                                                                     | ID-1             | None, grows with T9 and T10 | Task    |

Fixes inside open PRs (ID-8, SH-2, SH-3, SID-5, WB-4, AX-7) are listed in
Part 3, not as tickets.

### Under #1236 (Axum deployable)

Reuse #1142 (health and readiness), #1134 (no-geo behavior), #1141 (config
store) and #1139 (stores) where they fit.

| ID  | Ticket                                                                                                                                                                                                        | Reqs       | Depends on | Type    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | ---------- | ------- |
| A1  | Serve Axum in production mode from main: native startup (#1528) on released EdgeZero, fail before listening, SIGTERM drain                                                                                    | AX-1, AX-7 | E6         | Feature |
| A2  | Forward-port the native EC lifecycle (#1536, #1537) onto the shared backend, without their adapter-local DynamoDB and celld code                                                                              | ID-1, ID-3 | T8, A1     | Feature |
| A3  | Drain after-response tasks on SIGTERM                                                                                                                                                                         | AX-1       | E2, A1     | Feature |
| A4  | Trusted client IP and geo behind CloudFront: forward-port #1532 without its US-no-region change that fails Cloudflare closed, trust IPv6 CloudFront ranges, enable geo by default in the reference deployment | AX-3       | A1         | Feature |
| A5  | Load-test the record store at the target page-load rate with provider refreshes                                                                                                                               | AX-4       | A2, T9     | Task    |
| A6  | Publish and attest the production image, not the dev-server image. One image contract for #1539 and #1543                                                                                                     | AX-5       | A1         | Feature |
| A7  | Reference infrastructure: wire DynamoDB, readiness that checks dependencies (#1142), JSON logs, more than one task                                                                                            | AX-6       | A1, A6     | Feature |
| A8  | Run the SIGTERM-during-record-write test in CI (`scripts/test-native-ec-image.mjs` from #1536)                                                                                                                | AX-1       | A2         | Task    |
| A9  | Use only `TRUSTED_SERVER__` environment names. The RC reads `TS_NATIVE_GEO_PROFILE_FILE`, `TS_NATIVE_BINDINGS_TOML` and `TS_APPCONFIG_TOKEN`                                                                  | AX-2       | A1         | Task    |

### EdgeZero epic #468

Behavior and acceptance criteria only. Reuse existing issues where they fit.

| ID  | Ticket                                                                                                                                                                                                                                                                                                                                                                                               | New or reuse                                               |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- |
| E1  | `KvStore` conditional writes and strong reads: a read that returns a generation marker, create-if-absent, compare-and-swap on the marker, strongly consistent existence and prefix listing, `KvError::Unsupported` where a store cannot. Covers the DynamoDB provider without funnelling writes through one item or partition. #463's celld record store already has the semantics on a separate API | New. #410 and #437 rule out conditional writes today       |
| E2  | Run bounded work after the response is sent on all four adapters, drained on Axum shutdown                                                                                                                                                                                                                                                                                                           | New                                                        |
| E3  | Rate limiter: Edge Rate Limiting on Fastly, in-memory and shared-counter backends elsewhere                                                                                                                                                                                                                                                                                                          | New                                                        |
| E4  | Trusted client IP on every adapter through one accessor                                                                                                                                                                                                                                                                                                                                              | Reuse #89 with #90 to #92 and #397. New sub-issue for Spin |
| E5  | Geo with country and region on Fastly, Cloudflare and Spin through `ctx.geo()`. Main has no geo module; it exists only on the #439 stack, for Axum                                                                                                                                                                                                                                                   | New. Reuse #432 and #438 for Axum                          |
| E6  | Release a tag containing #275 through #439                                                                                                                                                                                                                                                                                                                                                           | New, under #391, linked from #468                          |

## Part 3: fixes to existing PRs

Fixes go to each PR's author as review findings. EdgeZero fixes are requests;
Trusted Server does not push to EdgeZero. Line references are at the PR heads
listed in Part 1.

### Trusted Server

- **#1506 (cutover).**
  - Rebase after #1557, #1526 and #1516. It conflicts with main today.
  - Repin to #275's head after #460 folds in. Then drop the
    `origin_readthrough_enabled` rejection (`config.rs:317`), since cache tags
    return. The repin also brings `b97efec1`, which stops Spin sending
    `Content-Length: 0` on bodiless DELETE and OPTIONS.
  - Port #1527's native-client, response-owner and managed Fastly delivery
    tests if it has no equivalents.
  - Update the description. It still says "Docs only".
- **#1526 (async EC store).** Retarget to main. Address its open review from
  round 1, including the test gap for finalize under `block_on`.
- **#1525.** Address its open review from round 1.
- **#1487 (provider framework).**
  - Rebase onto #1526 and #1506. Make lookup collection an `.await` inside the
    now-async finalize. Today `main.rs:699` calls `block_on` inside a function
    that #1526 wraps in `block_on` (`main.rs:550`), which panics.
  - Move per-request lookups out of the `thread_local` into the request.
  - Keep saving `/auction` body EIDs. A test at `finalize.rs:1434` asserts that
    they are dropped.
  - Send provider lookups through EdgeZero's `HttpClient` after #1506.
  - Decide the rate-limit key. `identity:{provider}:{IP-derived prefix}` gives
    everyone behind one IP one budget.
- **#1489 (mirror and pull sync).** Give every mirrored ID a bounded lifetime.
  An unspecified expiry becomes `Expiry::Never`
  (`coordinator/validate.rs:132-135`), and the pull-sync provider writes such
  IDs (`pull_sync.rs:128`). Closed #1461 capped mirrored IDs at 24 hours.
- **#1488 (vendors).**
  - Restack onto #1489's branch and resolve the conflicts. It sits on an old
    #1487 commit, and the two siblings overlap in 11 files.
  - Keep main's SharedID retention: the stored value wins and lives as long as
    the record. Today a cookie-read SharedID expires in 24 hours and a
    differing cookie replaces it.
  - Read the SharedID cookie name from `managed_user_ids[].storage.name`
    instead of adding a second setting, and default the source to
    `pubcid.org`.
  - After the restack, startup must refuse an enabled provider kind that is not
    registered. The old base silently ignores it.
  - Settle the LiveRamp Custom ID, which everyone behind one IP shares, and
    confirm LiveRamp's response shape. The parser accepts one envelope; one
    LiveRamp page shows a list.
  - Address its open review from round 1.
- **#1528 (native startup).**
  - Retarget onto main after #1506.
  - Repin off `456313675`, which exists only on EdgeZero's DO NOT MERGE #462.
  - Fix the compile failures: `AxumDevServerConfig` is missing `ingress` in a
    CLI test, there is a type error, and there are two new high CodeQL alerts.
  - Remove the per-request `AtomicBool` from the Fastly `AppState`.
- **#1543 (image publishing).** Rebase onto #1449; it conflicts. It publishes
  the dev-server image (`STOPSIGNAL SIGINT`), so agree one production image
  contract with #1539 before it ships as the release image.
- **RC units, fixed during the forward-port, not on the RC:**
  - #1532: drop the core change that maps a US request with no region to
    `Unknown`. It fails every US Cloudflare request closed.
  - #1536: do not port the adapter-local DynamoDB backend. Its rows share one
    partition key, and every insert first updates one allocator item.
  - #1537: its example config breaks startup on Axum, Cloudflare and Spin.
  - #1539: fix the failing Axum test.
  - #1540: trust IPv6 CloudFront ranges, enable geo by default, and wire the
    DynamoDB module.
  - All units: replace `TS_*` environment names.
  - #1541: drop the commits from closed #1545.
- **Close:** #1527 after its tests move, #1310 (superseded by #1488's ID5
  provider), and #1424, #1499 and #1541 once main matches the RC tag.

### EdgeZero (requests to the PR authors)

- **#275.** Absorb #460, #406's `9f090e57` (the CLI template emits
  Fastly-only image code that #275 gates) and one copy of the CI trigger
  change, which today sits in #406, #460 and #414. Optionally guard all five
  egress unwind sites against a panicking logger; #409's `0769cbfd` covers
  three. It also has changes requested, 20 of 95 threads open, and a red
  watchdog fixture test.
- **#460.** Validate tags (printable ASCII, non-empty) and make Cloudflare
  reject tags instead of silently using the default. Fix its red Cloudflare
  wasm clippy; the fix exists only in RC commit `ebf622c2`. It removes `Copy`
  from a #275 public type, so it must land before #275 releases.
- **#406.** Take `run_production_app_with_options_and_initializer` from #414
  (`4ea99467`). The Trusted Server RC's native startup calls it.
- **#407.** Take the flaky-test fix from #414 (`9a985c99`).
- **#414.** Drop those two commits once they move down.
- **#463.** Rebase onto #422 without the #439 commits it carries.
- **#464.** Rebase onto #439 without #454's KV geo prototype.
- **#453.** Rebase off its stale base.
- **#389 and #401.** Redo the RC-only merges that fit them onto #275
  (`7c07b8e7` for #389; `2510ecc9` and `501a2dce` for #401).
- **Every stack PR.** The descriptions cite CI results at heads 4 to 50 commits
  old. Refresh them.

### Outside these epics

The #275 follow-ups belong to the cutover (Trusted Server #1505,
EdgeZero #436), not to identity:

- Cache tags: reuse #459. #460 still lacks tag validation, and Cloudflare
  silently falls back instead of rejecting.
- TLS chain-only verification: reuse #81, stating that verification is never
  turned off.
- Idle bound for streamed bodies: reuse #82.
- Fire-and-forget send and a published outbound test double: new.
- Spin host override: mostly in #275; only the probe evidence is missing.

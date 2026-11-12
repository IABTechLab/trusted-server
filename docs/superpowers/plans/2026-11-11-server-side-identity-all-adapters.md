# Server-side identity on every adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** File the missing tickets, move the existing issues under the new
epics (#1558 and IABTechLab/edgezero#468), link every dependency, and tie
every open PR to its ticket, in the wave order of Part A.

**Architecture:** This plan's tasks are GitHub operations, not code. Task 1
moves existing issues, Tasks 2 to 4 create the new tickets (EdgeZero first,
because Trusted Server tickets depend on it), Task 5 links every blocked-by
dependency, Tasks 6 and 7 post the per-PR fixes in Part C, and Task 8 checks
coverage. Code lands later in each ticket's own implementation PR, in Part A's
wave order, with its own plan when it needs one.

**Tech Stack:** `gh` CLI and GitHub GraphQL (issue types, sub-issues,
blocked-by links, project fields) against IABTechLab/trusted-server and
IABTechLab/edgezero.

**Spec:**
[server-side identity design](../specs/2026-11-11-server-side-identity-all-adapters-design.md)

## Global Constraints

- One-account mode: every write acts as the PR author's login.
- Platform order: Fastly, then Axum, then Cloudflare, then Spin (DEC-4). Fastly
  and Axum ship without Cloudflare or Spin: no Fastly or Axum item is blocked
  by a Cloudflare or Spin item, and Cloudflare and Spin work has its own tickets.
- Only issues move, and they move under the new epics: #1558 for Trusted
  Server and IABTechLab/edgezero#468 for EdgeZero. Existing epics (#1236,
  #1505, IABTechLab/trusted-server#90, IABTechLab/edgezero#88,
  IABTechLab/edgezero#391) are never moved under them and receive no new
  issues; #1236 is linked from #1558's Related section. An issue that leaves
  an old epic moves with `addSubIssue(input:{..., replaceParent: true})`.
- Issue bodies state the requirement, its acceptance and its dependencies,
  and nothing about deployments or their state.
- EdgeZero references are written `IABTechLab/edgezero#N`; a bare `#N` means
  Trusted Server.
- EdgeZero issue bodies state behavior and acceptance only.
- Issue types are set with `updateIssue`: Feature `IT_kwDOBPEB8s4A35yA`, Task
  `IT_kwDOBPEB8s4A35x7`, Bug `IT_kwDOBPEB8s4A35x9`.
- Project `PVT_kwDOBPEB8s4BFKrl`. Priority field `PVTSSF_lADOBPEB8s4BFKrlzg22nQg`
  (P0 `c92c6358`, P1 `dca9fbdd`, P2 `afab7bc8`, P3 `71861fe7`). Size field
  `PVTSSF_lADOBPEB8s4BFKrlzg22m2Q` (XS `771e90ab`, S `01b13689`, M `084dacdb`,
  L `762f39e7`, XL `27016a48`).
- Dependencies use `addBlockedBy(input:{issueId, blockingIssueId})`. GitHub
  links issues only, so a PR or a decision in "Blocked by" goes in the issue
  body.
- An issue Part B names for reuse is reused, never duplicated.
- EdgeZero code is never changed from here; EdgeZero items are requests.
- Trusted Server PRs #1342 to #1423 and #1448 are out of scope (DEC-2).
- Issue and comment text uses no em dashes and no names of people; comments
  start with `CREATOR: `.

## Review Focus

1. A duplicate issue for work an existing issue already tracks. Every create
   step in Tasks 2 to 4 runs a search first.
2. A Part C item posted to a PR whose head has moved. Tasks 6 and 7 compare
   the head with Part C before posting and re-check the item if it moved.
3. A blocked-by link that GitHub refuses across repositories. Task 5 records
   it in the issue body instead and lists it in its readback.
4. A bare `#N` meant for EdgeZero inside a Trusted Server issue. Task 8 scans
   the posted bodies.
5. A body that goes beyond its requirement, acceptance and dependencies. Task
   8 reads every body.

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

### Waves

A wave starts when everything it waits for has landed. Within a wave, rows
run in priority order, then DEC-4's platform order. TS is Trusted Server and
EZ is EdgeZero (`IABTechLab/edgezero#N`). Kinds: New (ticket), Move (existing
issue under its epic), PR fix (changes the PR needs), Merge, Release, Close.

| Wave | Item                                                                                                                             | Pri | Kind               | Action                                                                                                                                  | Waits for                                                                                                                        |
| ---- | -------------------------------------------------------------------------------------------------------------------------------- | --- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| 0    | TS T1                                                                                                                            | P0  | New                | Answer identify and batch sync locally on every adapter                                                                                 | none                                                                                                                             |
| 0    | TS #1557                                                                                                                         | P1  | Merge              | Sourcepoint consent                                                                                                                     | none                                                                                                                             |
| 0    | TS #1526                                                                                                                         | P1  | PR fix, Merge      | Retarget to main, fix its round-1 review; delivers #1522                                                                                | none                                                                                                                             |
| 0    | TS #1525                                                                                                                         | P1  | PR fix             | Fix its round-1 review                                                                                                                  | none                                                                                                                             |
| 0    | TS #1516                                                                                                                         | P1  | Merge              | Already approved                                                                                                                        | none                                                                                                                             |
| 0    | IABTechLab/edgezero#460                                                                                                          | P1  | PR fix             | Part C fixes, then fold into IABTechLab/edgezero#275's branch with `9f090e57` and one CI trigger change                                 | none                                                                                                                             |
| 0    | IABTechLab/edgezero#406, IABTechLab/edgezero#407, IABTechLab/edgezero#414                                                        | P1  | PR fix             | Move `4ea99467` to IABTechLab/edgezero#406 and `9a985c99` to IABTechLab/edgezero#407                                                    | none                                                                                                                             |
| 0    | IABTechLab/edgezero#463, IABTechLab/edgezero#464, IABTechLab/edgezero#453                                                        | P1  | PR fix             | Rebase off the commits they carry                                                                                                       | none                                                                                                                             |
| 0    | TS T2                                                                                                                            | P1  | New                | EIDs in `user.eids`; EID-absence tests check both fields                                                                                | none                                                                                                                             |
| 0    | EZ EZI-5                                                                                                                         | P1  | New                | Rate limiter for Fastly and Axum                                                                                                        | none                                                                                                                             |
| 0    | TS #1189                                                                                                                         | P1  | Move               | EID write path skips Purpose 4 gating                                                                                                   | none                                                                                                                             |
| 0    | TS #1142                                                                                                                         | P1  | Move               | Health and startup contract                                                                                                             | none                                                                                                                             |
| 0    | TS #1503                                                                                                                         | P2  | Merge              | `ts ec` commands; delivers #1432                                                                                                        | none                                                                                                                             |
| 0    | TS #1449                                                                                                                         | P2  | PR fix             | Fix its round-4 review                                                                                                                  | none                                                                                                                             |
| 0    | TS #1141, #1139, #1233                                                                                                           | P2  | Move               | Axum config store; adapter stores; `ts-ec` to creative assets                                                                           | none                                                                                                                             |
| 0    | TS #1192                                                                                                                         | P3  | Move               | Fastly per-request cost                                                                                                                 | none                                                                                                                             |
| 1    | TS #1506                                                                                                                         | P1  | PR fix, Merge      | Rebase; repin after the fold-in, or the fallback below; delivers #1507                                                                  | #1557, #1526, #1516, IABTechLab/edgezero#460 fold-in                                                                             |
| 1    | IABTechLab/edgezero#275                                                                                                          | P1  | PR fix, Merge      | Resolve its review, then merge                                                                                                          | IABTechLab/edgezero#460 fold-in                                                                                                  |
| 1    | TS #1543                                                                                                                         | P2  | PR fix             | Rebase onto #1449; ships with A6                                                                                                        | #1449                                                                                                                            |
| 2    | TS #1487                                                                                                                         | P1  | PR fix, Merge      | Part C fixes                                                                                                                            | #1526, #1506                                                                                                                     |
| 2    | EZ EZI-2a, EZI-1                                                                                                                 | P1  | New                | Record contract with Fastly; after-response work for Fastly and Axum                                                                    | IABTechLab/edgezero#275                                                                                                          |
| 2    | IABTechLab/edgezero#406 to IABTechLab/edgezero#439                                                                               | P1  | PR fix, Merge      | Resolve reviews, retarget to main, merge in order                                                                                       | IABTechLab/edgezero#275                                                                                                          |
| 2    | TS T5, T4                                                                                                                        | P1  | New                | Conformance suite and shared state, Fastly and Axum                                                                                     | #1507                                                                                                                            |
| 2    | IABTechLab/edgezero#389, IABTechLab/edgezero#401, IABTechLab/edgezero#444, IABTechLab/edgezero#452, then IABTechLab/edgezero#466 | P2  | PR fix             | Rebase onto main and redo the RC-only merges                                                                                            | IABTechLab/edgezero#275 (IABTechLab/edgezero#466 also IABTechLab/edgezero#444)                                                   |
| 3    | TS #1489                                                                                                                         | P1  | PR fix, Merge      | Cap mirrored-ID expiry                                                                                                                  | #1487                                                                                                                            |
| 3    | TS #1553, #1549                                                                                                                  | P1  | Move               | After-response CAS retry; no `user.id` for a row-less EC                                                                                | PR #1487                                                                                                                         |
| 3    | IABTechLab/edgezero#463, IABTechLab/edgezero#464                                                                                 | P1  | Merge              | Rebased                                                                                                                                 | IABTechLab/edgezero#422, IABTechLab/edgezero#439                                                                                 |
| 3    | IABTechLab/edgezero#90, IABTechLab/edgezero#92                                                                                   | P1  | Move               | Fastly and Axum ClientInfo (EZI-3)                                                                                                      | none; PR IABTechLab/edgezero#409                                                                                                 |
| 3    | TS #1246                                                                                                                         | P2  | Move               | Lockr IDs to KV                                                                                                                         | PR #1487                                                                                                                         |
| 4    | TS #1488                                                                                                                         | P1  | PR fix, Merge      | Part C fixes                                                                                                                            | #1489                                                                                                                            |
| 4    | EZ EZI-2b                                                                                                                        | P1  | New                | DynamoDB and celld providers                                                                                                            | EZI-2a, IABTechLab/edgezero#463, IABTechLab/edgezero#414                                                                         |
| 4    | TS T15, #1551                                                                                                                    | P2  | New, Move          | Identity-only tsjs; pull sync on a full row                                                                                             | PR #1489                                                                                                                         |
| 5    | EZ R1, R2                                                                                                                        | P1  | Release            | As Part B defines                                                                                                                       | EZI-2a, EZI-2b, IABTechLab/edgezero#406 to IABTechLab/edgezero#439; EZI-1, EZI-5, IABTechLab/edgezero#90, IABTechLab/edgezero#92 |
| 5    | TS T3                                                                                                                            | P1  | New                | SharedID cleanup                                                                                                                        | PR #1488, #1189                                                                                                                  |
| 5    | TS #1555, #1556                                                                                                                  | P2  | Move               | Opt-out EIDs; backend certificate checks                                                                                                | PR #1488 (#1556 also #1507)                                                                                                      |
| 5    | TS #1310, IABTechLab/trusted-server#90, IABTechLab/trusted-server#353                                                            | P2  | Close              | Superseded by #1488's ID5 provider                                                                                                      | #1488 merged                                                                                                                     |
| 6    | TS A1                                                                                                                            | P1  | New, PR fix        | Port #1528, repin to R1                                                                                                                 | R1, #1507, #1141                                                                                                                 |
| 6    | TS T6, T10                                                                                                                       | P1  | New                | Shared record backend, Fastly first; rate limits                                                                                        | R1, #1522, #1139; R2                                                                                                             |
| 7    | TS #1529 (A4), A6 with #1543, A3, A9                                                                                             | P1  | Move, New          | CloudFront IP and geo; production image; SIGTERM drain; env names                                                                       | A1 (A3 also R2)                                                                                                                  |
| 7    | TS #1527, #1523                                                                                                                  | P2  | Close              | Tests moved to #1506                                                                                                                    | #1506, A1                                                                                                                        |
| 8    | TS A2, A7                                                                                                                        | P1  | New                | Axum EC lifecycle; reference infrastructure                                                                                             | T6, A1, #1529; A1, A6, #1142                                                                                                     |
| 9    | TS T11, T7, T13                                                                                                                  | P1  | New                | Providers, Axum partner APIs and SharedID write-back on Fastly and Axum                                                                 | A2 plus their Part B blockers                                                                                                    |
| 9    | TS T12, A8                                                                                                                       | P2  | New                | Abuse protection; SIGTERM test in CI                                                                                                    | A2                                                                                                                               |
| 9    | TS #1541, #1424, #1499, #1531 to #1540                                                                                           | P2  | Close              | The RC line is on main                                                                                                                  | A1, A2, A4, A6, A7                                                                                                               |
| 10   | TS T14, A5                                                                                                                       | P2  | New                | Mirror cookie on Axum; load test                                                                                                        | T11, PR #1489; A2, A7, T11                                                                                                       |
| 11   | EZ EZI-1c, EZI-2c, EZI-4c, EZI-5c and IABTechLab/edgezero#91, then R3; then TS #1518, T8, T17                                    | P2  | New, Move, Release | Cloudflare parts in EdgeZero; Cloudflare release; geo adoption check; Cloudflare EC lifecycle; Cloudflare providers, limits and cookies | Part B blockers, DEC-8                                                                                                           |
| 12   | EZ EZI-1s, EZI-2s, EZI-3s, EZI-4s and EZI-5s, then R4; then TS T9, T18                                                           | P3  | New, Release       | Spin parts in EdgeZero; Spin release; Spin EC lifecycle; Spin providers, limits and cookies                                             | Part B blockers, DEC-8, DEC-14                                                                                                   |
| any  | TS T16, TS #1245, IABTechLab/edgezero#287                                                                                        | P3  | New, Move, Merge   | `ts-ec` refresh; LiveRamp sync spec; IABTechLab/edgezero#287 last                                                                       | DEC-13; DEC-9; IABTechLab/edgezero#275                                                                                           |

Waves 0 to 10 complete Fastly and Axum. None of them waits on a Cloudflare or
Spin item, so Fastly and Axum ship without waves 11 and 12 (DEC-4).

Fallback for wave 1: if the fold-in has not landed two weeks after wave 0
ends, #1506 merges pinned to IABTechLab/edgezero#275's head as it stands
(allowed by AX-7) and keeps its readthrough rejection until the repin.

The two longest chains:

- Axum identity: IABTechLab/edgezero#460, IABTechLab/edgezero#275, IABTechLab/edgezero#406 to IABTechLab/edgezero#439, IABTechLab/edgezero#463, EZI-2b, R1, A1,
  T6, A2, T11, A5.
- SharedID: IABTechLab/edgezero#460, #1506, #1487, #1489, #1488, T3, and T13 once A2 lands.

## Part B: tickets

"Blocked by" is set as a GitHub dependency between issues. A PR or a decision
there goes in the issue body. "Blocks" is the inverse and is not set
separately. No ticket mixes Fastly or Axum work with Cloudflare or Spin work
(DEC-4).

### EdgeZero, under IABTechLab/edgezero#468

New:

| ID     | Ticket                                                                                          | Reqs   | Type    | Size | Pri | Blocked by                                                             | Blocks                     |
| ------ | ----------------------------------------------------------------------------------------------- | ------ | ------- | ---- | --- | ---------------------------------------------------------------------- | -------------------------- |
| EZI-2a | `KvStore` record contract and the Fastly provider                                               | EZI-2a | Feature | L    | P1  | PR IABTechLab/edgezero#275                                             | EZI-2b, EZI-2c, EZI-2s, R1 |
| EZI-2b | DynamoDB and celld providers meet the record contract, with writes spread across partition keys | EZI-2b | Feature | L    | P1  | EZI-2a; PRs IABTechLab/edgezero#463 and IABTechLab/edgezero#414        | R1                         |
| EZI-1  | Bounded work after the response on Fastly and Axum, drained on Axum SIGTERM                     | EZI-1  | Feature | M    | P1  | PR IABTechLab/edgezero#275                                             | EZI-1c, EZI-1s, R2         |
| EZI-5  | Rate limiter: Fastly Edge Rate Limiting, and in-memory and shared-counter backends for Axum     | EZI-5  | Feature | M    | P1  | none                                                                   | EZI-5c, EZI-5s, R2         |
| EZI-1c | After-response work on Cloudflare (`waitUntil`)                                                 | EZI-1  | Feature | S    | P2  | EZI-1                                                                  | R3                         |
| EZI-2c | Cloudflare store meets the record contract, or reports `KvError::Unsupported`                   | EZI-2c | Feature | L    | P2  | EZI-2a; DEC-8                                                          | R3                         |
| EZI-4c | Geo with country and region through `ctx.geo()` on Cloudflare                                   | EZI-4  | Feature | S    | P2  | PR IABTechLab/edgezero#439                                             | R3                         |
| EZI-5c | Rate limiter backend for Cloudflare                                                             | EZI-5  | Feature | S    | P2  | EZI-5; DEC-11                                                          | R3                         |
| EZI-1s | After-response capability error on Spin                                                         | EZI-1  | Task    | XS   | P3  | EZI-1                                                                  | R4                         |
| EZI-2s | Spin store meets the record contract, or reports `KvError::Unsupported`                         | EZI-2s | Feature | M    | P3  | EZI-2a; DEC-8                                                          | R4                         |
| EZI-3s | Spin ClientInfo, the Spin part of EZI-3                                                         | EZI-3  | Task    | S    | P3  | none                                                                   | R4, IABTechLab/edgezero#89 |
| EZI-4s | Geo through `ctx.geo()` on Spin, or a capability error                                          | EZI-4  | Feature | S    | P3  | PR IABTechLab/edgezero#439                                             | R4                         |
| EZI-5s | Rate limiter backend for Spin                                                                   | EZI-5  | Feature | S    | P3  | EZI-5; DEC-11                                                          | R4                         |
| R1     | Release IABTechLab/edgezero#275 through IABTechLab/edgezero#439 plus EZI-2a and EZI-2b          | EZI-6  | Task    | S    | P1  | EZI-2a, EZI-2b; PRs IABTechLab/edgezero#275 to IABTechLab/edgezero#439 | A1, T6                     |
| R2     | Release the Fastly and Axum parts of EZI-1, EZI-3 and EZI-5                                     | EZI-6  | Task    | S    | P1  | EZI-1, EZI-5, IABTechLab/edgezero#90, IABTechLab/edgezero#92           | A3, T7, T10, T11           |
| R3     | Release the Cloudflare parts                                                                    | EZI-6  | Task    | S    | P2  | EZI-1c, EZI-2c, EZI-4c, EZI-5c, IABTechLab/edgezero#91; DEC-8          | R4, T8, T17, #1518         |
| R4     | Release the Spin parts                                                                          | EZI-6  | Task    | S    | P3  | EZI-1s, EZI-2s, EZI-3s, EZI-4s, EZI-5s, R3                             | T9, T18                    |

Existing, moved under IABTechLab/edgezero#468 from IABTechLab/edgezero#88
(these are EZI-3's issues; the IABTechLab/edgezero#88 epic itself stays where
it is):

| Issue                  | Title                                                               | Type (set)   | Size | Pri | Blocked by                                                                     | Blocks                     |
| ---------------------- | ------------------------------------------------------------------- | ------------ | ---- | --- | ------------------------------------------------------------------------------ | -------------------------- |
| IABTechLab/edgezero#89 | As a developer I want to access client metadata in handlers (EZI-3) | Story (keep) | M    | P2  | IABTechLab/edgezero#90, IABTechLab/edgezero#91, IABTechLab/edgezero#92, EZI-3s | none                       |
| IABTechLab/edgezero#90 | Fastly ClientInfo                                                   | Task (keep)  | S    | P1  | none                                                                           | R2, IABTechLab/edgezero#89 |
| IABTechLab/edgezero#92 | Axum ClientInfo                                                     | Task (keep)  | S    | P1  | PR IABTechLab/edgezero#409                                                     | R2, IABTechLab/edgezero#89 |
| IABTechLab/edgezero#91 | Cloudflare ClientInfo                                               | Task (keep)  | S    | P2  | none                                                                           | R3, IABTechLab/edgezero#89 |

EZI-3s (Spin ClientInfo) also blocks IABTechLab/edgezero#89.

Related, left in IABTechLab/edgezero#391 because they belong to that epic's
Axum production scope: IABTechLab/edgezero#432 and IABTechLab/edgezero#438
(EZI-4), IABTechLab/edgezero#410 and IABTechLab/edgezero#437 (EZI-2b), and
IABTechLab/edgezero#397 (EZI-3). R1 is linked from IABTechLab/edgezero#391.

### Axum deployable, under #1558

New:

| ID  | Ticket                                                                                                      | Reqs                   | Type    | Size | Pri | Blocked by       | Blocks                    |
| --- | ----------------------------------------------------------------------------------------------------------- | ---------------------- | ------- | ---- | --- | ---------------- | ------------------------- |
| A1  | Serve Axum in production mode from main: port #1528 onto #1506's outbound API, repin to R1                  | AX-1, AX-2, AX-7       | Feature | L    | P1  | R1, #1507, #1141 | A2, A3, A6, A7, A9, #1529 |
| A2  | Axum EC lifecycle on the shared backend, forward-ported from #1536 and #1537                                | ID-1, ID-3, ID-8, WB-1 | Feature | L    | P1  | T6, A1, #1529    | A5, A8, T7, T11, T12, T13 |
| A3  | Drain after-response tasks on SIGTERM                                                                       | AX-1, ID-5             | Feature | S    | P1  | R2, A1           | none                      |
| A5  | Load-test the record store at the target page-load rate with provider refreshes                             | AX-4                   | Task    | M    | P2  | A2, A7, T11      | none                      |
| A6  | Publish and attest the production image; one contract for #1539 and #1543                                   | AX-5                   | Feature | M    | P1  | A1               | A7                        |
| A7  | Reference infrastructure: DynamoDB, readiness, JSON logs, two or more tasks, IPv6 CloudFront ranges, geo on | AX-3, AX-6             | Feature | L    | P1  | A1, A6, #1142    | A5                        |
| A8  | Run the SIGTERM-during-record-write test in CI                                                              | AX-1                   | Task    | S    | P2  | A2               | none                      |
| A9  | `TRUSTED_SERVER__` environment names only                                                                   | AX-2                   | Task    | S    | P2  | A1               | none                      |

Existing, moved under #1558:

| Issue | Title                                                        | Type (set)  | Size | Pri | Blocked by | Blocks |
| ----- | ------------------------------------------------------------ | ----------- | ---- | --- | ---------- | ------ |
| #1529 | Extract the protected CloudFront native Geo integration (A4) | Task (keep) | M    | P1  | A1         | A2     |
| #1142 | Uniform cross-adapter health and startup-failure contract    | Task (keep) | M    | P1  | none       | A7     |
| #1141 | Make Axum consume the local config store                     | Bug (keep)  | S    | P2  | none       | A1     |
| #1139 | Implement adapter stores or retire dead store manifests      | Bug (keep)  | M    | P2  | none       | T6     |

### Trusted Server, under #1558

New:

| ID  | Ticket                                                                                                                                                                                             | Reqs                                           | Type    | Size | Pri | Blocked by                      | Blocks       |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- | ------- | ---- | --- | ------------------------------- | ------------ |
| T1  | Answer identify and batch sync locally on every adapter: run them with a record store, 501 without one                                                                                             | ID-3                                           | Task    | S    | P0  | none                            | T7, T8       |
| T2  | EIDs in OpenRTB 2.6 `user.eids`; every EID-absence test checks both `user.eids` and `user.ext.eids`                                                                                                | SID-4                                          | Feature | M    | P1  | none                            | none         |
| T3  | SharedID cleanup: legacy path, cross-source dedupe, re-filing, SID-6 gate, template-cache deny list                                                                                                | SID-1, SID-3, SID-6, WB-5                      | Feature | M    | P1  | #1189; PR #1488                 | T13          |
| T4  | Build shared state once per process on Fastly and Axum: settings, partner registry (Axum builds it per request), store clients; keep Axum per-request HTTP client unless measured                  | SH-1, SH-3                                     | Task    | M    | P2  | #1507                           | none         |
| T5  | Identity conformance suite for Fastly and Axum, plus a startup-rejection test where identity is unsupported                                                                                        | ID-1                                           | Task    | M    | P1  | #1507                           | T8           |
| T6  | One record backend over EdgeZero `KvStore`; delete the Fastly `ec_kv.rs`; adapters without a supported store reject identity configuration at startup                                              | ID-2                                           | Feature | L    | P1  | R1, #1522, #1139                | A2, T8       |
| T7  | Axum partner APIs on the shared backend: batch sync, pull sync, admin lookup                                                                                                                       | ID-3, ID-5                                     | Feature | M    | P1  | A2, R2, T1                      | none         |
| T8  | Cloudflare EC lifecycle: mint, record, `ts-ec`, identify, batch sync, pull sync, admin lookup, auction and body EIDs; build state once; drop the first-request `Env` capture and config `block_on` | ID-1, ID-2, ID-3, ID-8, WB-1, SH-1, SH-3, SH-4 | Feature | L    | P2  | T6, R3, T1, T5, #1518           | T9, T17      |
| T9  | Spin EC lifecycle, the same scope as T8; port the config `block_on` removal from RC commit `933de5921`                                                                                             | ID-1, ID-2, ID-3, ID-8, WB-1, SH-1, SH-3       | Feature | L    | P3  | T8, R4; DEC-14                  | T18          |
| T10 | Rate limits on Fastly and Axum: batch sync per partner, pull sync, provider lookups, record creation; shared counter across Axum tasks                                                             | ID-6                                           | Feature | M    | P1  | R2; DEC-11                      | T11, T17     |
| T11 | Providers on Fastly and Axum, awaited before finalization; only follow-up writes after the response                                                                                                | ID-4, ID-5, WB-1                               | Feature | L    | P1  | A2, T10, R2, #1553; PR #1487    | A5, T14, T17 |
| T12 | Refuse record creation from non-browser clients on Fastly and Axum                                                                                                                                 | ID-7                                           | Feature | M    | P2  | A2                              | T17          |
| T13 | SharedID cookie write-back on Fastly and Axum, with `<name>_cst`, under SID-2 and SID-6                                                                                                            | WB-3, SID-6                                    | Feature | M    | P1  | T3, A2; PR #1488; DEC-7         | T17          |
| T14 | Mirror cookie on Axum                                                                                                                                                                              | WB-4                                           | Feature | S    | P2  | T11; PR #1489; DEC-10           | T17          |
| T15 | Identity-only tsjs mode                                                                                                                                                                            | WB-6                                           | Feature | M    | P2  | PR #1489                        | none         |
| T16 | `ts-ec` refresh for returning readers without breaking cacheability                                                                                                                                | WB-2, WB-5                                     | Task    | S    | P3  | DEC-13                          | none         |
| T17 | Cloudflare identity features: providers, rate limits, abuse protection, SharedID write-back and mirror cookie                                                                                      | ID-4, ID-6, ID-7, WB-3, WB-4                   | Feature | L    | P2  | T8, R3, T10, T11, T12, T13, T14 | T18          |
| T18 | Spin identity features, the same scope as T17                                                                                                                                                      | ID-4, ID-6, ID-7, WB-3, WB-4                   | Feature | L    | P3  | T9, R4, T17                     | none         |

Existing, moved under #1558:

| Issue | Title                                                                       | Type (set)  | Size | Pri | Blocked by      | Blocks |
| ----- | --------------------------------------------------------------------------- | ----------- | ---- | --- | --------------- | ------ |
| #1522 | Make shared EC persistence awaitable (delivered by PR #1526)                | Task (keep) | M    | P1  | none            | T6     |
| #1189 | Ungated `ingest_*` EID write path bypasses TCF Purpose 4 gating             | Bug         | M    | P1  | none            | T3     |
| #1549 | Do not send a cookie EC with no row as `user.id`                            | Bug (keep)  | S    | P1  | PR #1487        | none   |
| #1553 | Retry an after-response write that loses its CAS                            | Task (keep) | S    | P1  | PR #1487        | T11    |
| #1551 | Stop pull sync from re-calling a partner for a row that is full             | Bug (keep)  | S    | P2  | PR #1489        | none   |
| #1555 | Client and browser EIDs for provider-owned sources after a provider opt-out | Bug (keep)  | S    | P2  | PR #1488        | none   |
| #1556 | Certificate checks on identity backends                                     | Bug (keep)  | S    | P2  | #1507; PR #1488 | none   |
| #1518 | Verify EdgeZero geolocation adoption in EC and auction consent paths        | Task (keep) | S    | P2  | R3              | T8     |
| #1233 | Stop forwarding `ts-ec` to static creative assets                           | Task (keep) | S    | P2  | none            | none   |
| #1432 | `ts ec show` and `ts ec eids` (delivered by PR #1503)                       | Task (keep) | S    | P2  | none            | none   |
| #1245 | Spec for non-browser LiveRamp sync                                          | Task        | S    | P2  | DEC-9           | none   |
| #1246 | Write Lockr IDs to the KV store                                             | Task (keep) | M    | P2  | PR #1487        | none   |
| #1192 | Reduce Fastly adapter per-request fixed cost                                | Task (keep) | M    | P3  | none            | none   |

Not moved:

- IABTechLab/trusted-server#90 is an epic, so it is linked from #1558, not moved; it and IABTechLab/trusted-server#353 close
  when PR #1488 merges.
- #1507 delivers the cutover and stays under #1505; it blocks A1, T4, T5 and
  #1556.
- Proposed closures: #1523 with #1527; IABTechLab/trusted-server#353 and #1310 once PR #1488 merges.
- Out of these epics: #1484 (auction), #1140 (Cloudflare config), #1164
  (Spin tooling), IABTechLab/trusted-server#518 (cleanup).
- #1134, #1132 and #1125 overlap this work but stay out pending a decision on
  whether DEC-2's exclusion covers issues.

Requirements met inside open PRs, not by a ticket: ID-8 on main and SH-2
(#1487), SID-1 to SID-3 and SID-6 for the provider, SID-5 (#1488), WB-4
expiry (#1489), AX-7 (#1528). Part C lists them.

## Part C: fixes to existing PRs

Each item names the head it was checked at. Post it as a comment on that PR
(Tasks 6 and 7); once posted, the PR thread is the source of truth.

### Trusted Server

- **#1506 at `851537045` (AX-7, SID-4).** Rebase after #1557, #1526 and #1516.
  Repin after the fold-in, or under wave 1's fallback; drop the
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
  change, which today sits in IABTechLab/edgezero#406, IABTechLab/edgezero#460
  and IABTechLab/edgezero#414. Optionally guard all five egress unwind sites
  against a panicking logger; `0769cbfd` in IABTechLab/edgezero#409 covers
  three.
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
- **IABTechLab/edgezero#463 at `6f9fa8bc`, IABTechLab/edgezero#464 at
  `6fe32531`, IABTechLab/edgezero#453 at `633f0552`.** Rebase as Part A's table
  says.
- **IABTechLab/edgezero#389 at `0653e6e3` and IABTechLab/edgezero#401 at
  `1a6ae861`.** Land after IABTechLab/edgezero#275 merges, redoing the RC-only
  merges `7c07b8e7` (IABTechLab/edgezero#389) and `2510ecc9`
  (IABTechLab/edgezero#401) and the RC-only follow-up commit `501a2dce`
  (IABTechLab/edgezero#401).
- **IABTechLab/edgezero#444 at `217983c0` and IABTechLab/edgezero#466 at
  `cb48c6b3`.** Land IABTechLab/edgezero#444 first, after
  IABTechLab/edgezero#275.
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

### Task 1: Move the existing issues under their epics

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part B's "Existing, moved" tables and "Linked, not moved" notes.
- Produces: 17 issues under #1558 and 4 under IABTechLab/edgezero#468, each typed, sized and
  prioritized; link comments on IABTechLab/edgezero#468.

- [ ] **Step 1: Move each issue**

For each row, run
`gh api graphql -f query='mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c}){issue{number}}}' -F p="$(gh issue view <epic> --json id --jq .id)" -F c="$(gh issue view <issue> --json id --jq .id)"`.
Expected: the epic's number. A row whose issue already has a parent is
reported, not moved.

- [ ] **Step 2: Type, Priority and Size**

Set "Type (set)" through `updateIssue` where it is not "keep". Add the issue
to the project through `addProjectV2ItemById` if it is not there, then set
Priority and Size through `updateProjectV2ItemFieldValue` with the IDs in
Global Constraints.
Expected: each mutation returns the item.

- [ ] **Step 3: Link without moving**

Post a `CREATOR:` comment on IABTechLab/edgezero#468 listing the linked
issues and the EZI ID each relates to.

### Task 2: File the EdgeZero tickets under IABTechLab/edgezero#468

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part B's EdgeZero table and the spec's EZI requirements.
- Produces: an issue number per row, recorded in a `CREATOR:` comment on
  IABTechLab/edgezero#468.

- [ ] **Step 1: Search before each row**

For each row in table order, run
`gh issue list -R IABTechLab/edgezero --state all --search "<two key terms from the row>"`.
Expected: no open issue other than those Part B links.

- [ ] **Step 2: Create**

Run `gh issue create -R IABTechLab/edgezero --title "<Ticket>" --body-file <file>`.
The body has Behavior (the spec's EZI text), Acceptance (its Accept line),
Blocked by (every PR and decision in that column, by reference) and Related
(IABTechLab/trusted-server#1558).
Expected: one issue URL per row.

- [ ] **Step 3: Type, parent, Priority and Size**

Set the row's Type through `updateIssue`, add the issue under
IABTechLab/edgezero#468 through `addSubIssue`, add it to the project, and set
Priority and Size.
Expected: each mutation returns the issue or item.

- [ ] **Step 4: Record the numbers**

Post a `CREATOR:` comment on IABTechLab/edgezero#468 mapping each ID to its
issue, and one on IABTechLab/edgezero#391 linking R1.

### Task 3: File the Axum tickets under #1558

Same steps as Task 2, against IABTechLab/trusted-server and #1558, for the
eight new rows in table order. Each body has Summary (Ticket), Requirements
(Reqs, linked to the spec section), Acceptance (each requirement's Accept
line) and Blocked by. Record the map in a `CREATOR:` comment on #1558.

### Task 4: File the Trusted Server tickets under #1558

Same steps as Task 3, for T1 to T18 in table order, under #1558. Record the
map in a `CREATOR:` comment on #1558.

### Task 5: Link the dependencies

**Interfaces:**

- Consumes: every "Blocked by" cell in Part B and the issue numbers from
  Tasks 1 to 4.
- Produces: one GitHub blocked-by link per issue pair.

- [ ] **Step 1: Link each pair**

For every issue named in a "Blocked by" cell, run
`gh api graphql -f query='mutation($i:ID!,$b:ID!){addBlockedBy(input:{issueId:$i,blockingIssueId:$b}){issue{number}}}' -F i=<blocked issue node> -F b=<blocking issue node>`.
Expected: the blocked issue's number. If GitHub refuses a cross-repository
pair, add a "Blocked by IABTechLab/edgezero#N" line to the issue body instead
and list the pair in Step 2's readback.

- [ ] **Step 2: Read back**

For each issue, read its blocked-by list through GraphQL and compare it with
Part B.
Expected: every issue pair present, plus the list of pairs recorded only in
bodies.

### Task 6: Tie out the Trusted Server PRs

- [ ] **Step 1: Check heads**

For each Part C Trusted Server item, run
`gh pr view <N> --json headRefOid --jq .headRefOid`.
Expected: the head Part C names. If it moved, re-check the item against the
new head before posting.

- [ ] **Step 2: Post**

Post one `CREATOR:` comment per PR with its Part C item and the ticket it
serves (`Part of #<ticket>`): #1532 to #1529, #1526 to #1522, #1503 to #1432,
#1506 to #1507, #1528 to A1, #1543 to A6, #1487 to T11, #1488 to T13 and
#1489 to T14 and T15. For proposed closures, ask the author; do not close
another author's PR.

### Task 7: Tie out the EdgeZero PRs

Same steps as Task 6, for the EdgeZero items in Part C, each linking its EZI
issue from Task 2.

### Task 8: Verify coverage

- [ ] **Step 1: Count sub-issues**

Run `gh api graphql -f query='{repository(owner:"IABTechLab",name:"<repo>"){issue(number:<epic>){subIssues(first:100){totalCount}}}}' --jq '.data.repository.issue.subIssues.totalCount'`
for #1558 and IABTechLab/edgezero#468.
Expected: 43 and 21.

- [ ] **Step 3: Check that Fastly and Axum stand alone**

For every issue in waves 0 to 10, read its blocked-by list and confirm that
none of its blockers is a wave 11 or 12 item (a Cloudflare or Spin ticket,
IABTechLab/edgezero#91, R3 or R4).
Expected: no such blocker (DEC-4).

- [ ] **Step 2: Check bodies**

For every created or moved issue, read its body and confirm three things:
each "Blocked by" names an issue, PR or decision; no Trusted Server body
contains a bare `#N` below 1000 (EdgeZero references use
`IABTechLab/edgezero#N`); and a created body holds only its requirement,
acceptance and dependencies.
Expected: no gaps.

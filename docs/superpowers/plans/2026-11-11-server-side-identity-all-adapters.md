# Server-side identity on every adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

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

**Spec:** [server-side identity
design](../specs/2026-11-11-server-side-identity-all-adapters-design.md)

## Global Constraints

- One-account mode: every write acts as the PR author's login.
- Platform order: Fastly, then Axum, then Cloudflare, then Spin (DEC-4). Fastly
  and Axum ship without Cloudflare or Spin: no Fastly or Axum item is blocked by
  a Cloudflare or Spin item, and Cloudflare and Spin work has its own tickets. A
  Fastly or Axum ticket may still make Cloudflare and Spin reject identity
  configuration, answer 501, report a capability as absent or mount the shared
  app (T1, T6, T21, IABTechLab/edgezero#283), because that needs no Cloudflare
  or Spin work.
- Every feature is built once in core and works on every adapter in DEC-4's
  order (DEC-16, SH-5). A capability an adapter lacks becomes an EdgeZero ask;
  no ticket adds identity code to one adapter crate.
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
- Project `PVT_kwDOBPEB8s4BFKrl`. Priority field
  `PVTSSF_lADOBPEB8s4BFKrlzg22nQg` (P0 `c92c6358`, P1 `dca9fbdd`, P2 `afab7bc8`,
  P3 `71861fe7`). Size field `PVTSSF_lADOBPEB8s4BFKrlzg22m2Q` (XS `771e90ab`, S
  `01b13689`, M `084dacdb`, L `762f39e7`, XL `27016a48`).
- Dependencies use `addBlockedBy(input:{issueId, blockingIssueId})`. GitHub
  links issues only, so a PR or a decision in "Blocked by" goes in the issue
  body.
- An issue Part B names for reuse is reused, never duplicated.
- EdgeZero code is never changed from here; EdgeZero items are requests.
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

| PR                  | Head                                  | Base on GitHub                        | Fix                                                                                                                                                                                                                                             |
| ------------------- | ------------------------------------- | ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| #1557               | `76e1dd5f6`                           | main                                  | None                                                                                                                                                                                                                                            |
| #1525               | `cd8977cad`                           | main                                  | None; #1526 does not need it                                                                                                                                                                                                                    |
| #1526               | `e529e5576`                           | #1525's branch                        | Retarget to main; its own diff applies cleanly there                                                                                                                                                                                            |
| #1516               | `8b9c45d5e`                           | main                                  | Merged on main as `1810c763a` on 2026-11-12                                                                                                                                                                                                     |
| #1503               | `5e10c24e4`                           | main                                  | None; needs neither #1506 nor new EdgeZero                                                                                                                                                                                                      |
| #1449               | `fd33f2c74`                           | main                                  | Outside this plan (DEC-19)                                                                                                                                                                                                                      |
| #1506               | `9ec4711d3`                           | main                                  | Rebase after #1557 and #1526; #1516 has merged                                                                                                                                                                                                  |
| #1487               | `1600781eb`                           | main                                  | Rebase after #1526 and #1506: 8 and 17 files conflict                                                                                                                                                                                           |
| #1488               | `7f55776f3`                           | #1487's branch                        | Take #1566's two #1488 commits                                                                                                                                                                                                                  |
| #1489               | `d3780ba13`                           | #1488's branch                        | None                                                                                                                                                                                                                                            |
| #1566               | `117a58e0e`                           | #1488's branch                        | Move its two #1488 commits down; rebuild the Axum part on T21 and EdgeZero (Part C). 1 file conflicts with #1489, 19 with #1506                                                                                                                 |
| #1527               | `a020daeec`                           | #1526's branch                        | Close once #1506 covers its tests (DEC-1)                                                                                                                                                                                                       |
| #1528               | `ea779fb47`                           | #1527's branch                        | Close once #1531 carries A1                                                                                                                                                                                                                     |
| #1543               | `e5e6ea7fa`                           | #1449's branch                        | Outside this plan (DEC-19)                                                                                                                                                                                                                      |
| #1424               | `2423a2ed2`                           | main                                  | Conflicting, head `rc/edgezero-431-demo`; close at parity                                                                                                                                                                                       |
| #1499               | `084fdbba9`                           | #1424's branch                        | Close at parity                                                                                                                                                                                                                                 |
| #1531               | `88a6ba596`                           | #1506's branch                        | Retargeted without a rebase: it forks from main with its own cutover and the RC tag pin, and 15 files conflict with #1506 at `9ec4711d3`. Rebase onto #1506; it then delivers A1                                                                |
| #1532               | `852b875fe`                           | #1531's branch                        | Delivers #1529 (A4); restack after #1531                                                                                                                                                                                                        |
| #1533, #1534        | `82d9a0e66`, `646c39a34`              | #1532's and #1533's branches          | RC-baseline copies of #1525 and #1526: drop from the chain and close                                                                                                                                                                            |
| #1536, #1535, #1537 | `296e05405`, `0666a9b4b`, `4a838272c` | #1534's, #1536's and #1535's branches | Not restacked: mostly Axum-only stores, store selection and wiring that EZI-2b, T6 and T21 replace. Their tests go to A2, A8 and T5 and #1535's code to T19 and T20; each closes when those land                                                |
| #1539, #1538, #1540 | `8d41cb06c`, `3111c2456`, `db1ba337a` | #1537's, #1539's and #1538's branches | Restack on #1531 (A1). #1539 and #1538 apply cleanly; #1540 conflicts in `docs/guide/native-ec-persistence.md` since its `db1ba337a` merged the AWS examples. #1539 and #1543 both add `.dockerignore`, so whichever lands second reconciles it |
| #1541               | `86797e0b4`                           | main                                  | Close at parity                                                                                                                                                                                                                                 |

EdgeZero:

| PR                                                                                                                                                   | Head       | Base on GitHub                   | Fix                                                                                                                       |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| IABTechLab/edgezero#275                                                                                                                              | `14a6ca8b` | main                             | Absorb IABTechLab/edgezero#460, IABTechLab/edgezero#406's `9f090e57` and one copy of the CI trigger change before merging |
| IABTechLab/edgezero#460                                                                                                                              | `cec19886` | IABTechLab/edgezero#275's branch | Merge into that branch before IABTechLab/edgezero#275 merges                                                              |
| IABTechLab/edgezero#406, IABTechLab/edgezero#407, IABTechLab/edgezero#409, IABTechLab/edgezero#414, IABTechLab/edgezero#422, IABTechLab/edgezero#439 | various    | linear chain                     | Move IABTechLab/edgezero#414's `4ea99467` to IABTechLab/edgezero#406 and its `9a985c99` to IABTechLab/edgezero#407        |
| IABTechLab/edgezero#463                                                                                                                              | `6f9fa8bc` | IABTechLab/edgezero#422's branch | Drop the IABTechLab/edgezero#439 commits it carries                                                                       |
| IABTechLab/edgezero#464                                                                                                                              | `6fe32531` | IABTechLab/edgezero#439's branch | Drop IABTechLab/edgezero#454's prototype commits                                                                          |
| IABTechLab/edgezero#389, IABTechLab/edgezero#401, IABTechLab/edgezero#444, IABTechLab/edgezero#452                                                   | various    | main                             | Land after IABTechLab/edgezero#275 merges; they conflict with it in 9, 6, 4 and 6 files, modify/delete included           |
| IABTechLab/edgezero#466                                                                                                                              | `214dd4fa` | main                             | Land after IABTechLab/edgezero#444; they conflict in `edgezero-cli` `config.rs` and `lib.rs`                              |
| IABTechLab/edgezero#469                                                                                                                              | `1d5bf2ff` | main                             | JA4 interface design for IABTechLab/edgezero#283; draft                                                                   |
| IABTechLab/edgezero#287                                                                                                                              | `ce25ef5a` | main                             | Last; shares 51 files with IABTechLab/edgezero#275, 26 of them under `crates/`                                            |

### Waves

A wave starts when everything it waits for has landed. Within a wave, rows
run in priority order, then DEC-4's platform order. TS is Trusted Server and
EZ is EdgeZero (`IABTechLab/edgezero#N`). Kinds: New (ticket), Move (existing
issue under its epic), PR fix (changes the PR needs), Merge, Release (a
revision on EdgeZero's integration branch that Trusted Server pins, DEC-5),
Close.

| Wave | Item                                                                                                                                                   | Pri | Kind             | Action                                                                                                                                                            | Waits for                                                                                                |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | --- | ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| 0    | TS T1                                                                                                                                                  | P0  | New              | Answer identify and batch sync locally on every adapter                                                                                                           | none                                                                                                     |
| 0    | TS #1557                                                                                                                                               | P1  | Merge            | Sourcepoint consent                                                                                                                                               | none                                                                                                     |
| 0    | TS #1526, #1522                                                                                                                                        | P1  | PR fix, Merge    | Retarget to main, fix its round-1 review; delivers #1522                                                                                                          | none                                                                                                     |
| 0    | TS #1516                                                                                                                                               | P1  | Merged           | Merged on main as `1810c763a`                                                                                                                                     | none                                                                                                     |
| 0    | TS #1189                                                                                                                                               | P1  | Close            | Fixed on main by `56a19229f` (#1157), which deleted the ungated `ingest_*` functions; ask its author to close it                                                  | none                                                                                                     |
| 0    | IABTechLab/edgezero#460                                                                                                                                | P1  | PR fix           | Part C fixes, then fold into IABTechLab/edgezero#275's branch with `9f090e57` and one CI trigger change                                                           | none                                                                                                     |
| 0    | IABTechLab/edgezero#406, IABTechLab/edgezero#407, IABTechLab/edgezero#414                                                                              | P1  | PR fix           | Move `4ea99467` to IABTechLab/edgezero#406 and `9a985c99` to IABTechLab/edgezero#407                                                                              | none                                                                                                     |
| 0    | IABTechLab/edgezero#463                                                                                                                                | P1  | PR fix           | Rebase off the commits it carries                                                                                                                                 | none                                                                                                     |
| 0    | TS #1503, #1432                                                                                                                                        | P2  | Merge            | `ts ec` commands; delivers #1432                                                                                                                                  | none                                                                                                     |
| 0    | TS #1233                                                                                                                                               | P2  | Move             | `ts-ec` to creative assets                                                                                                                                        | none                                                                                                     |
| 1    | TS #1506                                                                                                                                               | P1  | PR fix, Merge    | Rebase; repin after the fold-in, or the fallback below; delivers #1507                                                                                            | #1557, #1526, IABTechLab/edgezero#460 fold-in                                                            |
| 1    | IABTechLab/edgezero#275                                                                                                                                | P1  | PR fix, Merge    | Resolve its review, then merge                                                                                                                                    | IABTechLab/edgezero#460 fold-in                                                                          |
| 2    | TS #1487                                                                                                                                               | P1  | PR fix, Merge    | Part C fixes                                                                                                                                                      | #1526, #1506                                                                                             |
| 2    | IABTechLab/edgezero#406 to IABTechLab/edgezero#439; IABTechLab/edgezero#397, IABTechLab/edgezero#410, IABTechLab/edgezero#437, IABTechLab/edgezero#432 | P1  | PR fix, Merge    | Resolve reviews, retarget to main, merge in order; delivers IABTechLab/edgezero#397, IABTechLab/edgezero#410, IABTechLab/edgezero#437 and IABTechLab/edgezero#432 | IABTechLab/edgezero#275                                                                                  |
| 2    | EZ EZI-2a, EZI-3                                                                                                                                       | P1  | New              | Record contract with Fastly; ClientInfo contract                                                                                                                  | IABTechLab/edgezero#275                                                                                  |
| 2    | TS T5                                                                                                                                                  | P1  | New              | Conformance suite for Fastly and Axum                                                                                                                             | #1507                                                                                                    |
| 2    | TS T4, #1192                                                                                                                                           | P2  | New, Move        | Measure Axum's per-request HTTP client (T4); Fastly per-request cost (#1192)                                                                                      | #1507                                                                                                    |
| 2    | TS #1556                                                                                                                                               | P2  | Move             | Regression test that identity backends verify certificates, then close                                                                                            | #1507                                                                                                    |
| 2    | IABTechLab/edgezero#389, IABTechLab/edgezero#401, IABTechLab/edgezero#444, IABTechLab/edgezero#452, then IABTechLab/edgezero#466                       | P2  | PR fix           | Rebase onto main and redo the RC-only merges                                                                                                                      | IABTechLab/edgezero#275 (IABTechLab/edgezero#466 also IABTechLab/edgezero#444)                           |
| 3    | TS #1488                                                                                                                                               | P1  | PR fix, Merge    | Part C fixes                                                                                                                                                      | #1487                                                                                                    |
| 3    | EZ R1                                                                                                                                                  | P1  | Release          | IABTechLab/edgezero#275 through IABTechLab/edgezero#439                                                                                                           | wave 2's EdgeZero chain                                                                                  |
| 3    | EZ EZI-1                                                                                                                                               | P1  | New              | After-response work for Fastly and Axum                                                                                                                           | IABTechLab/edgezero#275, IABTechLab/edgezero#406                                                         |
| 3    | IABTechLab/edgezero#463                                                                                                                                | P1  | Merge            | Rebased                                                                                                                                                           | IABTechLab/edgezero#422, IABTechLab/edgezero#439                                                         |
| 3    | EZ EZI-4a                                                                                                                                              | P1  | New              | Axum geo bound from EdgeZero's manifest                                                                                                                           | PR IABTechLab/edgezero#439                                                                               |
| 3    | IABTechLab/edgezero#90, IABTechLab/edgezero#92, IABTechLab/edgezero#283                                                                                | P1  | Move             | Fastly and Axum ClientInfo; JA4                                                                                                                                   | EZI-3; IABTechLab/edgezero#397                                                                           |
| 3    | TS #1553, #1246                                                                                                                                        | P2  | Move             | After-response CAS retry; Lockr IDs to KV                                                                                                                         | PR #1487                                                                                                 |
| 4    | TS #1489                                                                                                                                               | P1  | PR fix, Merge    | Cap mirrored-ID expiry                                                                                                                                            | #1488                                                                                                    |
| 4    | EZ EZI-2b                                                                                                                                              | P1  | New              | DynamoDB and celld providers on Axum                                                                                                                              | EZI-2a, IABTechLab/edgezero#410, IABTechLab/edgezero#437; PR IABTechLab/edgezero#463                     |
| 4    | EZ R2, R2b                                                                                                                                             | P1  | Release          | R2: EZI-1 for Fastly and Axum; R2b: EZI-3, ClientInfo, JA4 and Axum's geo binding                                                                                 | EZI-1 (R2); EZI-3, IABTechLab/edgezero#90, IABTechLab/edgezero#92, IABTechLab/edgezero#283, EZI-4a (R2b) |
| 4    | TS A1                                                                                                                                                  | P1  | New, PR fix      | Rebase #1531 onto #1506, repin to R1; #1528 closes                                                                                                                | R1, #1507                                                                                                |
| 4    | TS T3                                                                                                                                                  | P1  | New              | SharedID cleanup                                                                                                                                                  | PR #1488                                                                                                 |
| 4    | TS #1555                                                                                                                                               | P2  | Move             | Opt-out EIDs                                                                                                                                                      | PR #1488                                                                                                 |
| 4    | TS #1310, IABTechLab/trusted-server#90, IABTechLab/trusted-server#353                                                                                  | P2  | Close            | Superseded by #1488's ID5 provider                                                                                                                                | #1488 merged                                                                                             |
| 5    | EZ R1b                                                                                                                                                 | P1  | Release          | EZI-2a and EZI-2b                                                                                                                                                 | EZI-2a, EZI-2b                                                                                           |
| 5    | TS #1529 (A4), A6, A3                                                                                                                                  | P1  | Move, New        | CloudFront IP and geo via #1532; production image via #1539 and #1538; SIGTERM drain                                                                              | A1 (A3 also R2; #1529 also R2b)                                                                          |
| 5    | TS T2                                                                                                                                                  | P1  | New              | EIDs in `user.eids`; after the provider stack, so open PRs need no test churn                                                                                     | #1489 merged                                                                                             |
| 5    | TS T21                                                                                                                                                 | P1  | New              | One identity app in core for every adapter                                                                                                                        | T1, #1507, PR #1489                                                                                      |
| 5    | TS T15                                                                                                                                                 | P1  | New              | Identity-only tsjs                                                                                                                                                | PR #1489                                                                                                 |
| 5    | TS A9, #1141                                                                                                                                           | P2  | New, Move        | `TRUSTED_SERVER__` guard; local config handoff                                                                                                                    | A1                                                                                                       |
| 5    | TS #1551                                                                                                                                               | P2  | Move             | Pull sync on a full row                                                                                                                                           | PR #1489                                                                                                 |
| 5    | TS #1527, #1528, #1523, #1533, #1534                                                                                                                   | P2  | Close            | Tests moved to #1506; A1 on #1531; RC copies of #1525 and #1526                                                                                                   | #1506, A1                                                                                                |
| 6    | TS T6                                                                                                                                                  | P1  | New              | Shared record backend, Fastly first                                                                                                                               | R1b, #1522                                                                                               |
| 6    | TS T11, T12, T13                                                                                                                                       | P1  | New              | Providers checked with ID5; abuse protection; SharedID creation and write-back, all in the shared app                                                             | T21, R2 (T11 also PR #1487); R2b (T12); T3, PR #1488 (T13)                                               |
| 7    | TS A2                                                                                                                                                  | P1  | New              | Identity on Axum through the shared app, a new PR with #1536's and #1537's tests                                                                                  | T5, T6, T21, A1, #1529                                                                                   |
| 7    | TS T19                                                                                                                                                 | P1  | New              | EC route in the shared app: mint and write back on a same-origin call; pixel and JSON responses                                                                   | T11, T12, T13, T21                                                                                       |
| 8    | TS A10, A7, A8                                                                                                                                         | P1  | New              | Identity features pass on Axum; reference infrastructure; SIGTERM test in CI                                                                                      | A2 plus their Part B blockers                                                                            |
| 8    | TS T20                                                                                                                                                 | P1  | New              | Injected script calls the EC route on consent and merges the IDs into Prebid                                                                                      | T15, T19                                                                                                 |
| 9    | TS A5, T22                                                                                                                                             | P1  | New              | Load test; LiveRamp per DEC-9                                                                                                                                     | A2, A7, A10 (T22: T11, A10, PR #1488, DEC-9)                                                             |
| 9    | TS #1535, #1536, #1537                                                                                                                                 | P2  | Close            | Their code and tests now in T19, T20, A2 and A8                                                                                                                   | A2, A8, T19, T20                                                                                         |
| 9    | TS #1541, #1424, #1499                                                                                                                                 | P2  | Close            | The RC line is on main                                                                                                                                            | A1, A2, A4, A6, A7                                                                                       |
| 10   | EZ EZI-2p, EZI-1c, EZI-4c; IABTechLab/edgezero#91                                                                                                      | P2  | New, Move        | Portable DynamoDB and celld providers; Cloudflare parts in EdgeZero                                                                                               | Part B blockers                                                                                          |
| 11   | EZ EZI-2c                                                                                                                                              | P2  | New              | Cloudflare record store                                                                                                                                           | EZI-2a, EZI-2p; DEC-8                                                                                    |
| 12   | EZ R3                                                                                                                                                  | P2  | Release          | Cloudflare integration point                                                                                                                                      | waves 10 and 11                                                                                          |
| 13   | TS #1518, T8                                                                                                                                           | P2  | Move, New        | Geo source check; identity on Cloudflare through the shared app                                                                                                   | R3 (T8 also T6, T1, T5, T21; DEC-8)                                                                      |
| 14   | TS T17                                                                                                                                                 | P2  | New              | Identity features pass on Cloudflare                                                                                                                              | T8, R3, T11 to T13, T19                                                                                  |
| 15   | EZ EZI-1s, EZI-2s, EZI-3s, EZI-4s                                                                                                                      | P3  | New              | Spin parts in EdgeZero                                                                                                                                            | Part B blockers; DEC-8 for EZI-2s                                                                        |
| 16   | EZ R4; IABTechLab/edgezero#89                                                                                                                          | P3  | Release, Move    | Spin integration point; ClientInfo on every adapter                                                                                                               | wave 15, R3                                                                                              |
| 17   | TS T9                                                                                                                                                  | P3  | New              | Identity on Spin through the shared app                                                                                                                           | T8, R4; DEC-8, DEC-14                                                                                    |
| 18   | TS T18                                                                                                                                                 | P3  | New              | Identity features pass on Spin                                                                                                                                    | T9, R4, T17                                                                                              |
| any  | TS #1142                                                                                                                                               | P2  | Move             | Health and startup contract on all four adapters; A7 carries Axum's readiness signal                                                                              | none                                                                                                     |
| any  | TS #1549                                                                                                                                               | P2  | Move             | No `user.id` for a row-less EC                                                                                                                                    | DEC-12; PR #1487                                                                                         |
| any  | TS T16, TS #1245, IABTechLab/edgezero#438, IABTechLab/edgezero#287                                                                                     | P3  | New, Move, Merge | `ts-ec` refresh; LiveRamp sync spec; local GeoIP (no open PR); IABTechLab/edgezero#287 last                                                                       | DEC-13; DEC-9; none; IABTechLab/edgezero#275                                                             |
| any  | TS #1139, #1525                                                                                                                                        | P3  | Move, PR fix     | Store inventory on all four adapters; HTTP auction endpoints, which #1526 does not need                                                                           | none                                                                                                     |
| any  | IABTechLab/edgezero#464                                                                                                                                | P3  | PR fix, Merge    | Rebase off the commits it carries, then merge when its review closes; no MVP ticket needs it                                                                      | IABTechLab/edgezero#439                                                                                  |
| any  | EZ EZI-5, EZI-5c, EZI-5s; TS T10                                                                                                                       | P3  | New              | Rate limiting, punted past the MVP                                                                                                                                | DEC-15                                                                                                   |

Waves 0 to 9 are the MVP (DEC-2): they complete Fastly and Axum. None of them
waits on a Cloudflare or Spin item, so the MVP ships without waves 10 to 18
(DEC-4). Waves 10 to 14 (Cloudflare), waves 15 to 18 (Spin) and the `any` rows
follow the MVP; an `any` row starts when its decision or reviewer is ready.

Fallback for wave 1: if the fold-in has not landed two weeks after #1557 and
#1526 have both merged on main (#1516 merged on 2026-11-12), #1506 merges pinned
to IABTechLab/edgezero#275's head as it stands (allowed by AX-7) and keeps its
readthrough rejection until the repin.

The two longest chains:

- Axum identity: IABTechLab/edgezero#460, IABTechLab/edgezero#275,
  IABTechLab/edgezero#406 to IABTechLab/edgezero#439, IABTechLab/edgezero#463,
  EZI-2b, R1b, T6, A2, A10, A5. A1 runs beside it on R1, before the record
  contract lands.
- SharedID: IABTechLab/edgezero#460, #1506, #1487, #1488, T3, T13 on Fastly,
  then A10 on Axum.
- JA4: IABTechLab/edgezero#275, EZI-3, IABTechLab/edgezero#283 (section 13.1 of
  IABTechLab/edgezero#469), R2b, T12, T19, A10.
- LiveRamp: T22 waits on DEC-9 as well as T11 and A10.

## Part B: tickets

Every ticket in waves 0 to 9 is MVP: P1 is required for it, and P2 can slip past
its first release. The Cloudflare and Spin tickets (T8, T9, T17, T18, the EZI
`c` and `s` tickets, R3, R4, IABTechLab/edgezero#91, IABTechLab/edgezero#89 and
#1518) and the `any` rows are post-MVP.

"Blocked by" is set as a GitHub dependency between issues. A PR or a decision
there goes in the issue body. "Blocks" is the inverse and is not set
separately. No ticket mixes Fastly or Axum work with Cloudflare or Spin work
(DEC-4), except the shared app, rejection, 501 and absence paths in Global
Constraints.

### EdgeZero, under IABTechLab/edgezero#468

The integration branch is itself an EdgeZero ask (DEC-5): it carries only
work headed for EdgeZero main, merged in Part A's order. R1 creates it,
protected against force-push and deletion, and R1 to R4 are tags on it that
Trusted Server pins.

New:

| ID     | Ticket                                                                                                                                                                 | Reqs   | Type    | Size | Pri | Blocked by                                                                                                                                                 | Blocks                                                                                                                               |
| ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ | ------- | ---- | --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| EZI-2a | `KvStore` record contract with listing strong within a partition, the Fastly provider and the local persistent store (`PersistentKvStore`)                             | EZI-2a | Feature | L    | P1  | PR IABTechLab/edgezero#275                                                                                                                                 | EZI-2b, EZI-2c, EZI-2s, R1b                                                                                                          |
| EZI-2b | DynamoDB and celld providers on Axum meet the record contract, each key's partition being its DynamoDB partition key                                                   | EZI-2b | Feature | L    | P1  | EZI-2a, IABTechLab/edgezero#410, IABTechLab/edgezero#437; PR IABTechLab/edgezero#463                                                                       | R1b, EZI-2p                                                                                                                          |
| EZI-2p | Portable DynamoDB and celld providers over EdgeZero's outbound HTTP, verified on Fastly, with the WASM credential source and a conditional-write latency budget stated | EZI-2p | Feature | XL   | P2  | EZI-2b; PR IABTechLab/edgezero#275                                                                                                                         | EZI-2c, EZI-2s                                                                                                                       |
| EZI-3  | ClientInfo contract: portable client IP and TLS metadata with explicit absence, the contract part of IABTechLab/edgezero#89                                            | EZI-3  | Feature | M    | P1  | PR IABTechLab/edgezero#275                                                                                                                                 | IABTechLab/edgezero#90, IABTechLab/edgezero#91, IABTechLab/edgezero#92, IABTechLab/edgezero#283, EZI-3s, IABTechLab/edgezero#89, R2b |
| EZI-1  | Bounded work after the response on Fastly and Axum, drained on Axum SIGTERM                                                                                            | EZI-1  | Feature | M    | P1  | PRs IABTechLab/edgezero#275, IABTechLab/edgezero#406                                                                                                       | EZI-1c, EZI-1s, R2                                                                                                                   |
| EZI-5  | Rate limiter: Fastly Edge Rate Limiting, and an in-memory backend for Axum (after the MVP)                                                                             | EZI-5  | Feature | S    | P3  | PR IABTechLab/edgezero#275; DEC-15                                                                                                                         | EZI-5c, EZI-5s, T10                                                                                                                  |
| EZI-1c | After-response work on Cloudflare (`waitUntil`)                                                                                                                        | EZI-1  | Feature | S    | P2  | EZI-1                                                                                                                                                      | R3                                                                                                                                   |
| EZI-2c | On Cloudflare, the portable DynamoDB and celld providers pass the record contract, and Workers KV meets it or reports `KvError::Unsupported`                           | EZI-2c | Feature | L    | P2  | EZI-2a, EZI-2p; DEC-8                                                                                                                                      | R3                                                                                                                                   |
| EZI-4c | Geo with country and region through `ctx.geo()` on Cloudflare                                                                                                          | EZI-4  | Feature | S    | P2  | IABTechLab/edgezero#432                                                                                                                                    | R3                                                                                                                                   |
| EZI-5c | Rate limiter backend for Cloudflare (after the MVP)                                                                                                                    | EZI-5  | Feature | S    | P3  | EZI-5; DEC-15                                                                                                                                              | none                                                                                                                                 |
| EZI-4a | Axum geo bound from EdgeZero's manifest, so `ctx.geo()` needs no application boot profile                                                                              | EZI-4  | Feature | S    | P1  | PR IABTechLab/edgezero#439                                                                                                                                 | R2b                                                                                                                                  |
| EZI-1s | After-response capability error on Spin                                                                                                                                | EZI-1  | Task    | XS   | P3  | EZI-1                                                                                                                                                      | R4                                                                                                                                   |
| EZI-2s | On Spin, the portable DynamoDB and celld providers pass the record contract, and Spin's KV meets it or reports `KvError::Unsupported`                                  | EZI-2s | Feature | M    | P3  | EZI-2a, EZI-2p; DEC-8                                                                                                                                      | R4                                                                                                                                   |
| EZI-3s | Spin ClientInfo, the Spin part of EZI-3                                                                                                                                | EZI-3  | Task    | S    | P3  | EZI-3                                                                                                                                                      | R4, IABTechLab/edgezero#89                                                                                                           |
| EZI-4s | Geo with country and region through `ctx.geo()` on Spin, from a trusted ingress header or a KV- or API-backed provider                                                 | EZI-4  | Feature | S    | P3  | IABTechLab/edgezero#432                                                                                                                                    | R4                                                                                                                                   |
| EZI-5s | Rate limiter backend for Spin (after the MVP)                                                                                                                          | EZI-5  | Feature | S    | P3  | EZI-5; DEC-15                                                                                                                                              | none                                                                                                                                 |
| R1     | Create the protected integration branch and tag IABTechLab/edgezero#275 through IABTechLab/edgezero#439                                                                | EZI-6  | Task    | S    | P1  | IABTechLab/edgezero#397, IABTechLab/edgezero#410, IABTechLab/edgezero#437, IABTechLab/edgezero#432; PRs IABTechLab/edgezero#275 to IABTechLab/edgezero#439 | A1                                                                                                                                   |
| R1b    | Integration-branch tag with the record contract: EZI-2a and EZI-2b                                                                                                     | EZI-6  | Task    | S    | P1  | EZI-2a, EZI-2b                                                                                                                                             | T6                                                                                                                                   |
| R2     | Integration-branch tag with the Fastly and Axum parts of EZI-1                                                                                                         | EZI-6  | Task    | S    | P1  | EZI-1                                                                                                                                                      | A3, A10, T11                                                                                                                         |
| R2b    | Integration-branch tag with EZI-3, the Fastly and Axum ClientInfo, the JA4 accessor and Axum's geo binding                                                             | EZI-6  | Task    | S    | P1  | EZI-3, IABTechLab/edgezero#90, IABTechLab/edgezero#92, IABTechLab/edgezero#283, EZI-4a                                                                     | A10, T12, #1529                                                                                                                      |
| R3     | Integration-branch tag with the Cloudflare parts                                                                                                                       | EZI-6  | Task    | S    | P2  | EZI-1c, EZI-2c, EZI-4c, IABTechLab/edgezero#91; DEC-8                                                                                                      | R4, T8, T17, #1518                                                                                                                   |
| R4     | Integration-branch tag with the Spin parts                                                                                                                             | EZI-6  | Task    | S    | P3  | EZI-1s, EZI-2s, EZI-3s, EZI-4s, R3                                                                                                                         | T9, T18                                                                                                                              |

Existing, moved under IABTechLab/edgezero#468. IABTechLab/edgezero#89 to
IABTechLab/edgezero#92 leave IABTechLab/edgezero#88 and the last six leave
IABTechLab/edgezero#391 or have no parent; both old epics stay where they are:

| Issue                   | Title                                                                                                                                                                                                                                                                                   | Type (set)   | Size | Pri | Blocked by                                                                            | Blocks                                              |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ | ---- | --- | ------------------------------------------------------------------------------------- | --------------------------------------------------- |
| IABTechLab/edgezero#89  | As a developer I want to access client metadata in handlers (EZI-3)                                                                                                                                                                                                                     | Story (keep) | S    | P3  | EZI-3, IABTechLab/edgezero#90, IABTechLab/edgezero#91, IABTechLab/edgezero#92, EZI-3s | none                                                |
| IABTechLab/edgezero#90  | Fastly ClientInfo                                                                                                                                                                                                                                                                       | Task (keep)  | S    | P1  | EZI-3                                                                                 | R2b, IABTechLab/edgezero#89                         |
| IABTechLab/edgezero#92  | Axum ClientInfo                                                                                                                                                                                                                                                                         | Task (keep)  | S    | P1  | EZI-3, IABTechLab/edgezero#397                                                        | R2b, IABTechLab/edgezero#89                         |
| IABTechLab/edgezero#91  | Cloudflare ClientInfo                                                                                                                                                                                                                                                                   | Task (keep)  | S    | P2  | EZI-3                                                                                 | R3, IABTechLab/edgezero#89                          |
| IABTechLab/edgezero#397 | Axum: safe production diagnostics and trusted-proxy handling (delivered by PR IABTechLab/edgezero#409)                                                                                                                                                                                  | Task (keep)  | M    | P1  | none                                                                                  | R1, IABTechLab/edgezero#92, IABTechLab/edgezero#283 |
| IABTechLab/edgezero#410 | Native AWS configuration, secret and KV store providers (PR IABTechLab/edgezero#414)                                                                                                                                                                                                    | Task (keep)  | L    | P1  | none                                                                                  | EZI-2b, R1                                          |
| IABTechLab/edgezero#437 | Native celld configuration and KV store providers (PR IABTechLab/edgezero#422)                                                                                                                                                                                                          | Task (keep)  | M    | P1  | none                                                                                  | EZI-2b, R1                                          |
| IABTechLab/edgezero#432 | Axum: protected CloudFront Geo metadata provider (PR IABTechLab/edgezero#439)                                                                                                                                                                                                           | Story (keep) | M    | P1  | none                                                                                  | R1, EZI-4c, EZI-4s                                  |
| IABTechLab/edgezero#438 | Axum: explicit local GeoIP provider (no open PR)                                                                                                                                                                                                                                        | Story (keep) | M    | P3  | none                                                                                  | none                                                |
| IABTechLab/edgezero#283 | Expose JA4 through `ctx.ja4()` (design in PR IABTechLab/edgezero#469); R2b needs Fastly's runtime JA4 and Axum's JA4 from a trusted proxy header (the design's section 13.1); IABTechLab/edgezero#91 carries Cloudflare's Bot Management JA4 and EZI-3s Spin's absence; closes with R2b | Task (keep)  | M    | P1  | EZI-3, IABTechLab/edgezero#397                                                        | R2b                                                 |

IABTechLab/edgezero#89 stays the umbrella story: EZI-3 carries its contract, and
it completes with the last adapter's mapping. IABTechLab/edgezero#397,
IABTechLab/edgezero#410 and IABTechLab/edgezero#432 leave
IABTechLab/edgezero#391, which stays an epic in place; R1 is linked from it.

### Axum deployable, under #1558

New:

| ID  | Ticket                                                                                                                                                                                                                                                                                                          | Reqs                                              | Type    | Size | Pri | Blocked by                      | Blocks                           |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------- | ------- | ---- | --- | ------------------------------- | -------------------------------- |
| A1  | Serve Axum in production mode from main: rebase #1531 onto #1506 (#1528 closes), with startup, stores and client IP from EdgeZero; repin to R1                                                                                                                                                                  | AX-1, AX-2, AX-7                                  | Feature | L    | P1  | R1, #1507                       | A2, A3, A6, A7, A9, #1529, #1141 |
| A2  | Identity on Axum through T21's shared app: a new PR that takes #1536's and #1537's tests but not their Axum-only stores, store selection or wiring, and switches the production image's store features (`aws-dynamodb-ec`, `celld-ec` in #1539) to EZI-2b's providers; partner APIs included; passes T5's suite | ID-1, ID-2, ID-3, ID-8, WB-1, SH-5                | Feature | M    | P1  | T5, T6, T21, A1, #1529          | A5, A7, A8, A10                  |
| A3  | Drain after-response tasks on SIGTERM                                                                                                                                                                                                                                                                           | AX-1, ID-5                                        | Feature | S    | P1  | R2, A1                          | none                             |
| A5  | Load-test the record store at the target page-load rate with provider refreshes                                                                                                                                                                                                                                 | AX-4                                              | Task    | M    | P1  | A2, A7, A10                     | none                             |
| A6  | Build the production image from source (#1539, #1538): its entrypoint starts the production server with `STOPSIGNAL SIGTERM`; no publishing or attestation                                                                                                                                                      | AX-5                                              | Feature | S    | P1  | A1                              | A7                               |
| A7  | Reference infrastructure: DynamoDB, a readiness signal that reports degraded identity without failing the load balancer health check, JSON logs, two or more tasks, IPv6 CloudFront ranges, geo on, CloudFront's JA4 header forwarded, and an example rate-based rule at the load balancer                      | AX-3, AX-6, ID-6, ID-7                            | Feature | L    | P1  | A1, A2, A6, #1529               | A5                               |
| A8  | Run the SIGTERM-during-record-write test in CI (`scripts/test-native-ec-image.mjs` from #1536)                                                                                                                                                                                                                  | AX-1                                              | Task    | S    | P1  | A2                              | none                             |
| A9  | CI guard: runtime code reads only `TRUSTED_SERVER__` environment names                                                                                                                                                                                                                                          | AX-2                                              | Task    | XS   | P2  | A1                              | none                             |
| A10 | Identity features pass on Axum from the shared app: providers checked with ID5, abuse protection, SharedID creation and write-back, the mirror cookie and the EC route, including Safari's cookie lifetime                                                                                                      | ID-4, ID-7, ID-9, WB-3, WB-4, SID-1, SID-2, SID-6 | Feature | M    | P1  | A2, R2, R2b, T11, T12, T13, T19 | A5, T22                          |

Existing, moved under #1558:

| Issue | Title                                                        | Type (set)  | Size | Pri | Blocked by | Blocks |
| ----- | ------------------------------------------------------------ | ----------- | ---- | --- | ---------- | ------ |
| #1529 | Extract the protected CloudFront native Geo integration (A4) | Task (keep) | M    | P1  | A1, R2b    | A2, A7 |
| #1141 | Make Axum consume the local config store                     | Bug (keep)  | S    | P2  | A1         | none   |
| #1142 | Uniform cross-adapter health and startup-failure contract    | Task (keep) | M    | P2  | none       | none   |
| #1139 | Implement adapter stores or retire dead store manifests      | Bug (keep)  | M    | P3  | none       | none   |

A1 replaces the local configuration bridge: #1528 starts Axum from
`EDGEZERO__ADAPTER__CONFIG_DIR` and never reads the old
`TRUSTED_SERVER_CONFIG_*` names, so #1141 then points `ts config push --local`
and `ts serve` at that directory; #1531 starts Axum the same way. #1142 and
#1139 cover all four adapters, so they sit outside the MVP; A7 carries Axum's
readiness signal. #1566 overlaps A2 and A10; its Axum part is rebuilt on T21
(Part C). T7, T14 and A11 are folded into A2 and A10, because the shared app
(T21) brings partner APIs, the mirror cookie and the EC route to every adapter.

### Trusted Server, under #1558

New:

| ID  | Ticket                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Reqs                                                       | Type    | Size | Pri | Blocked by                 | Blocks                          |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- | ------- | ---- | --- | -------------------------- | ------------------------------- |
| T1  | Answer identify and batch sync locally on every adapter: run them with a record store, 501 without one                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | ID-3                                                       | Task    | S    | P0  | none                       | T8, T21                         |
| T2  | EIDs in OpenRTB 2.6 `user.eids`; every EID-absence test checks both `user.eids` and `user.ext.eids`                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | SID-4                                                      | Feature | M    | P1  | PR #1489                   | none                            |
| T3  | SharedID cleanup: legacy path, cross-source dedupe, re-filing, SID-6 gate, template-cache deny list                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | SID-1, SID-2, SID-3, SID-6, WB-5                           | Feature | M    | P1  | PR #1488                   | T13                             |
| T4  | Measure Axum's per-request HTTP client against a pooled one, and keep it per request unless the measurement shows the Next.js regression gone (SH-1's exception)                                                                                                                                                                                                                                                                                                                                                                                                             | SH-1                                                       | Task    | S    | P2  | #1507                      | none                            |
| T5  | Identity conformance suite that runs against each adapter, Fastly and Axum first                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | ID-1, SH-5                                                 | Task    | M    | P1  | #1507                      | A2, T8                          |
| T6  | One record backend over EdgeZero `KvStore`; delete the Fastly `ec_kv.rs`; adapters without a supported store reject identity configuration at startup, with a test                                                                                                                                                                                                                                                                                                                                                                                                           | ID-2                                                       | Feature | L    | P1  | R1b, #1522                 | A2, T8                          |
| T8  | Identity on Cloudflare through T21's shared app with EZI-2c's store, passing T5's suite; build state once; drop the first-request `Env` capture and config `block_on`                                                                                                                                                                                                                                                                                                                                                                                                        | ID-1, ID-2, ID-3, ID-5, ID-8, WB-1, SH-1, SH-3, SH-4, SH-5 | Feature | L    | P2  | T6, R3, T1, T5, T21; DEC-8 | T9, T17                         |
| T9  | Identity on Spin, the same scope as T8; port the config `block_on` removal from RC commit `933de5921`                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | ID-1, ID-2, ID-3, ID-5, ID-8, WB-1, SH-1, SH-3, SH-5       | Feature | L    | P3  | T8, R4; DEC-8, DEC-14      | T18                             |
| T10 | Rate limits after the MVP: batch sync per partner, pull sync, provider lookups and record creation on Fastly; on Axum off by default and, when turned on, per task divided by the configured task count                                                                                                                                                                                                                                                                                                                                                                      | ID-6                                                       | Feature | M    | P3  | EZI-5, T21; DEC-15         | none                            |
| T11 | Providers in the shared app, awaited before finalization; only follow-up writes after the response; verified on Fastly with ID5                                                                                                                                                                                                                                                                                                                                                                                                                                              | ID-4, ID-5, WB-1                                           | Feature | M    | P1  | T21, R2; PR #1487          | A10, T17, T19, T22              |
| T12 | In the shared app, refuse record creation when a trusted JA4 from `ctx.ja4()` contradicts the User-Agent's browser family, allow an unknown or missing JA4, and own the family rule's data; delete the direct Fastly JA4 read; verified on Fastly                                                                                                                                                                                                                                                                                                                            | ID-7                                                       | Feature | M    | P1  | R2b, T21                   | A10, T17, T19                   |
| T13 | SharedID in the shared app: create one when the reader has none and `storage.type` is `cookie`, and write it back with `<name>_cst`, under SID-2 and SID-6; verified on Fastly                                                                                                                                                                                                                                                                                                                                                                                               | WB-3, SID-2, SID-5, SID-6                                  | Feature | M    | P1  | T3, T21; PR #1488          | A10, T17, T19                   |
| T15 | Identity-only tsjs mode                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | WB-6                                                       | Feature | M    | P1  | PR #1489                   | T20                             |
| T16 | `ts-ec` refresh for returning readers without breaking cacheability                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | WB-2, WB-5                                                 | Task    | S    | P3  | DEC-13                     | none                            |
| T17 | Identity features pass on Cloudflare from the shared app: providers, abuse protection, SharedID creation and write-back, the mirror cookie and the EC route                                                                                                                                                                                                                                                                                                                                                                                                                  | ID-4, ID-7, ID-9, WB-3, WB-4, SID-1, SID-2, SID-6          | Feature | M    | P2  | T8, R3, T11, T12, T13, T19 | T18                             |
| T18 | Identity features pass on Spin, the same scope as T17                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | ID-4, ID-7, ID-9, WB-3, WB-4, SID-1, SID-2, SID-6          | Feature | M    | P3  | T9, R4, T17                | none                            |
| T19 | EC route in the shared app, verified on Fastly: a same-origin `POST` to `/_ts/api/v1/ec` mints, records, runs the providers, takes the posted consent and withdrawals, and answers with the mirror's view as JSON; a `GET` answers Prebid's `pixelUrl` with a 204 and only re-sets an existing SharedID (reconciliation from #1535)                                                                                                                                                                                                                                          | ID-9, WB-1, WB-3, WB-4                                     | Feature | M    | P1  | T11, T12, T13, T21         | A10, T17, T20                   |
| T20 | The injected script calls the EC route with a same-origin `POST` when the CMP reports consent and on every later change, sends consent changes in order with one call in flight, letting a newer change replace a waiting one but never a waiting withdrawal, which later changes wait behind, discards a result overtaken by a newer consent change, removes the IDs it merged on a withdrawal, sends the TCF string and the GPP string (Sourcepoint's capture from #1535), copies a localStorage `_pubcid_optout` into the cookie, and merges the returned IDs into Prebid | ID-9, WB-6, SID-2                                          | Feature | M    | P1  | T15, T19                   | none                            |
| T21 | One identity app in core for every adapter: mounts identify, batch sync, admin lookup and the EC route and runs the EC lifecycle in one router each adapter mounts, building its state once (SH-1); adapter crates supply platform services through EdgeZero where it offers them, and Fastly's limiter and geo stay their existing bridges until their asks land                                                                                                                                                                                                            | SH-5, SH-1, ID-1, ID-3, ID-6                               | Feature | L    | P1  | T1, #1507; PR #1489        | A2, T8, T10, T11, T12, T13, T19 |
| T22 | LiveRamp per DEC-9: set the Custom ID, enable LiveRamp, and pass the provider and mirror checks with it on Fastly and Axum                                                                                                                                                                                                                                                                                                                                                                                                                                                   | ID-4, WB-4                                                 | Feature | S    | P1  | T11, A10; PR #1488; DEC-9  | none                            |

Existing, moved under #1558:

| Issue | Title                                                                       | Type (set)  | Size | Pri | Blocked by       | Blocks |
| ----- | --------------------------------------------------------------------------- | ----------- | ---- | --- | ---------------- | ------ |
| #1522 | Make shared EC persistence awaitable (delivered by PR #1526)                | Task (keep) | M    | P1  | none             | T6     |
| #1549 | Do not send a cookie EC with no row as `user.id`                            | Bug (keep)  | S    | P2  | PR #1487; DEC-12 | none   |
| #1553 | Retry an after-response write that loses its CAS                            | Task (keep) | S    | P2  | PR #1487         | none   |
| #1551 | Stop pull sync from re-calling a partner for a row that is full             | Bug (keep)  | S    | P2  | PR #1489         | none   |
| #1555 | Client and browser EIDs for provider-owned sources after a provider opt-out | Bug (keep)  | S    | P2  | PR #1488         | none   |
| #1556 | Certificate checks on identity backends                                     | Bug (keep)  | XS   | P2  | #1507            | none   |
| #1518 | Verify EdgeZero geolocation adoption in EC and auction consent paths        | Task (keep) | M    | P2  | R3               | none   |
| #1233 | Stop forwarding `ts-ec` to static creative assets                           | Task (keep) | S    | P2  | none             | none   |
| #1432 | `ts ec show` and `ts ec eids` (delivered by PR #1503)                       | Task (keep) | S    | P2  | none             | none   |
| #1245 | Spec for non-browser LiveRamp sync                                          | Task        | S    | P3  | DEC-9            | none   |
| #1246 | Write Lockr IDs to the KV store                                             | Task (keep) | M    | P2  | PR #1487         | none   |
| #1192 | Reduce Fastly adapter per-request fixed cost                                | Task (keep) | M    | P2  | #1507            | none   |

Not moved:

- IABTechLab/trusted-server#90 is an epic, so Task 1 Step 3 links it from #1558
  and it is not moved; it and IABTechLab/trusted-server#353 close when PR #1488
  merges.
- #1507 delivers the cutover and stays under #1505; it blocks A1, T4, T5,
  T21, #1192 and #1556.
- Proposed closures: #1523 with #1527; IABTechLab/trusted-server#353 and #1310
  once PR #1488 merges; #1189, which `56a19229f` (#1157) fixed on main.
- #1556 needs only a regression test: #1506 rejects
  `proxy.certificate_check = false` and drops the setting from pull sync.
- Out of these epics: #1484 (auction), #1140 (Cloudflare config), #1164
  (Spin tooling), IABTechLab/trusted-server#518 (cleanup).
- Outside this plan at lower priority (DEC-19): binary attestation and package
  publishing, #1449, #1463 and #1543.

Requirements met inside open PRs, not by a ticket: ID-8 on main and SH-2
(#1487), SID-1 to SID-3 and SID-6 for the provider, SID-5 (#1488), WB-4
expiry (#1489). Part C lists them.

## Part C: fixes to existing PRs

Each item names the head it was checked at. Post it as a comment on that PR
(Tasks 6 and 7); once posted, the PR thread is the source of truth. Task 6
re-checks each item at the PR's current head before posting.

### Trusted Server

- **#1506 at `9ec4711d3` (AX-7).** Rebase after #1557 and #1526; #1516 has
  merged. Keep EdgeZero pinned to IABTechLab/edgezero#275's head or an
  integration-branch tag (DEC-5): an RC-tag pin pushed to this branch on
  2026-11-12 was reverted. Repin after the fold-in, or under wave 1's fallback;
  drop the `origin_readthrough_enabled` rejection in `config.rs` once cache tags
  return. The current pin already carries IABTechLab/edgezero `b97efec1`, which
  stops Spin sending `Content-Length: 0` on bodiless DELETE and OPTIONS. Port
  #1527's native-client, response-owner and managed Fastly delivery tests if
  missing.
- **#1526 at `e529e5576` (ID-2, SH-3).** Retarget to main. Address its round-1
  review, including the finalize-under-`block_on` test gap.
- **#1525 at `cd8977cad`.** Address its round-1 review.
- **#1487 at `1600781eb` (ID-8, SH-2, SH-3).** Every item from
  `4e7a6e02b` is still open. Rebase onto #1526 and #1506 (8 and 17 files
  conflict). Await lookup collection in the async finalize: `main.rs:699`
  calls `block_on` inside a function #1526 wraps in `block_on`
  (`main.rs:550`, `:577`), and `run_after_response_lookups` does the same at
  `:793` and `:811`. Move per-request lookups out of the `thread_local`
  (`app.rs:414-424`). Keep saving `/auction` body EIDs (ID-8): the test at
  `finalize.rs:1365` asserts they are dropped and the PR's own spec now calls
  that intended, so the comment argues against that spec line. Send lookups
  through EdgeZero's `HttpClient`: #1506 deletes `platform/http.rs`, and its
  `outbound_client().start_batch_until` has the same start-then-gather shape.
- **#1489 at `d3780ba13` (WB-4).** Cap every mirrored ID's expiry at 30 days
  where the mirror is built (`mirror.rs:236-247`), not in validation, because
  SID-5 keeps SharedID `Never` in the record. An unstated expiry still becomes
  `Expiry::Never` (`coordinator/validate.rs:135-138`), pull sync is its only
  production source (`pull_sync.rs:145`), and such IDs reach the cookie with no
  `exp` under a one-year `Max-Age`. The 30-day clamp on stated expiries comes
  from #1488. Answer its open threads, including the one on `any_due` skipping
  cookie-read opt-outs.
- **#1566 at `117a58e0e` (SID-1, SID-3, SID-5, ID-2, SH-5).** Move `0a2a00aaf`
  (SharedID name, source and retention) and `0c13d4272` (drops the blanket
  provider rejection) into #1488. Rebuild the Axum part on T21's shared app and
  EdgeZero capabilities: no Axum-only `EcKvStore` (`ec_kv.rs:87`; ID-2, DEC-3),
  no Trusted Server rate limiter (none on Axum, DEC-11), no User-Agent-only gate
  (EZI-3, ID-7), and no detached `block_on` task (EZI-1). Local development uses
  EdgeZero's local store (EZI-2a). It then serves A2 and A10.
- **#1488 at `7f55776f3` (SID-1, SID-2, SID-3, SID-5, SID-6).** Take #1566's
  `0a2a00aaf`, which reads the SharedID name from `managed_user_ids`, defaults
  the source to `pubcid.org` and keeps the stored value (still open here:
  `sharedid.rs:46-59`, `common.rs:94,110,129-136`). Take `0c13d4272`: the
  blanket `reject_providers_without_a_kind` (`config.rs:348`) makes
  `ts config validate`, `push` and `diff` reject every enabled provider. SID-6
  holds for the provider path. LiveRamp's Custom ID waits on DEC-9, which gates
  enabling LiveRamp, not this merge. Answer its open threads.
- **#1531 at `88a6ba596` (AX-1, AX-2, AX-7), which carries A1 in place of
  #1528.** It now targets #1506's branch but forks from main with its own
  cutover rebuild (`82d7e3be8`) and the RC tag pin, so 15 files conflict with
  #1506 at `9ec4711d3`. Rebase onto #1506's head, drop that rebuild and the pin,
  and repin to R1 (DEC-5). Startup, stores and the client IP come from EdgeZero
  (IABTechLab/edgezero#406, IABTechLab/edgezero#414, IABTechLab/edgezero#409).
  Carry #1528's open items where they apply: the CLI test (`AxumDevServerConfig`
  lacks `ingress`), the type error, the CodeQL alerts and the per-request
  `AtomicBool` in the Fastly `AppState`.
- **RC units, forward-ported (DEC-6).** #1531 goes on #1506 for A1 and #1532 on
  #1531 for #1529. #1539, #1538 and #1540 go on #1531 for A6 and A7: #1539 and
  #1538 apply cleanly, and #1540 at `db1ba337a` conflicts in
  `docs/guide/native-ec-persistence.md`. #1533 and #1534 close as copies of
  #1525 and #1526. #1536 and #1537 are not restacked: their Axum-only stores,
  store selection and wiring give way to EZI-2b, T6 and T21, and their tests
  move into A2, A8 and T5. #1535 (Sourcepoint GPP capture and server consent
  reconciliation) feeds T19 and T20: its reconciliation runs in the EC route and
  its GPP capture in the injected script, in place of a separate `/_ts/consent`
  route wired into each adapter (DEC-16). #1532 takes its geo provider from
  EdgeZero's manifest once IABTechLab/edgezero#439 binds it, dropping
  `TS_NATIVE_GEO_PROFILE_FILE`. The notes below were checked at the RC tag
  `v0.1.1-rc-axum.202611.2` (`7c6e68d94`), which contains every unit. They are
  forward-port notes: each applies to the PR that ports its unit to main, and
  Task 6 posts it there once that PR exists, checked at its head. On the RC unit
  PRs themselves Task 6 posts a note only after re-checking it at the unit's
  head, read on 2026-11-12 as #1532 `852b875fe`, #1536 `296e05405`, #1537
  `4a838272c` and #1539 `8d41cb06c`, and on 2026-11-13 as #1540 `db1ba337a` and
  #1541 `86797e0b4`. #1532: drop the core change that maps a US request with no
  region to `Unknown`, which fails every US Cloudflare request closed (AX-3),
  and show that the same trusted country and region reach EC creation and
  auction consent on Axum. #1536: do not port the adapter-local DynamoDB store
  (ID-2, AX-4). #1537: do not port its Axum celld store or storage selection,
  which EdgeZero's providers and logical stores replace (EZI-2b, DEC-16); its
  example config also breaks startup on Axum, Cloudflare and Spin. #1539: fix
  the failing Axum test. #1540: trust IPv6 CloudFront ranges, keep the geo its
  example inputs select but bind it through EdgeZero's manifest (EZI-4a) instead
  of `TS_NATIVE_GEO_PROFILE_TOML`, key the DynamoDB table by partition instead
  of the one partition key and one allocator item its IAM policy allows (AX-4),
  and add an example rate-based rule at the load balancer (AX-3, AX-6, DEC-11).
  All units: replace `TS_*` names (AX-2). #1541: drop the commits of closed
  #1545.
- **Proposed closures.** #1527 once its tests move (DEC-1); #1528 once #1531
  carries A1; #1533 and #1534, the RC copies of #1525 and #1526; #1535, #1536
  and #1537 once T19, T20, A2 and A8 carry their code and tests; #1310, which
  #1488's ID5 provider supersedes; #1424, #1499 and #1541 at parity.

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
  type, so it lands before R1.
- **IABTechLab/edgezero#406 at `6d1dda16`.** Take
  `run_production_app_with_options_and_initializer` from `4ea99467`; the
  Trusted Server RC's native startup calls it.
- **IABTechLab/edgezero#407 at `d6f74308`.** Take the flaky-test fix
  `9a985c99`.
- **IABTechLab/edgezero#414 at `3733482f`.** Drop those two commits once they
  move down.
- **IABTechLab/edgezero#469 at `1d5bf2ff`.** Bring section 13.1, JA4 from a
  trusted proxy header recorded as `Ja4Source::TrustedProxyHeader`, into the
  work that ships in R2b. Axum's ID-7 needs JA4 from a hop that
  IABTechLab/edgezero#409 or IABTechLab/edgezero#439 checks, such as
  CloudFront's `CloudFront-Viewer-JA4-Fingerprint`; an unchecked peer stays
  `NotExposed` (EZI-3, DEC-18).
- **IABTechLab/edgezero#439 at `3a0e5eb4`.** Bind the geo provider from the
  manifest, so an application reads `ctx.geo()` without its own boot profile
  (EZI-4a, DEC-16); #1532 composes it from a Trusted Server profile today.
- **IABTechLab/edgezero#463 at `6f9fa8bc` and IABTechLab/edgezero#464 at
  `6fe32531`.** Rebase as Part A's table says.
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

- Consumes: Part B's "Existing, moved" tables and "Not moved" notes.
- Produces: 16 issues under #1558 and 10 under IABTechLab/edgezero#468, each
  typed, sized and prioritized; the Related sections of IABTechLab/edgezero#468
  and #1558 updated; comments on IABTechLab/edgezero#391 and
  IABTechLab/edgezero#88.

- [ ] **Step 1: Move each issue**

For each row, first read the issue's current parent with
`gh api graphql -f query='{repository(owner:"<owner>",name:"<repo>"){issue(number:<issue>){parent{number}}}}' --jq '.data.repository.issue.parent.number'`.
The expected parent is IABTechLab/edgezero#88 for IABTechLab/edgezero#89 to
IABTechLab/edgezero#92, IABTechLab/edgezero#391 for IABTechLab/edgezero#397,
IABTechLab/edgezero#410 and IABTechLab/edgezero#432, and none for every other
row. Stop and report a row whose parent differs. Then run
`gh api graphql -f query='mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c,replaceParent:true}){issue{number}}}' -F p="$(gh issue view <epic> --repo <owner>/<repo> --json id --jq .id)" -F c="$(gh issue view <issue> --repo <owner>/<repo> --json id --jq .id)"`,
and read the parent again.
Expected: the new epic's number, and every moved issue's parent is now that
epic.

- [ ] **Step 2: Type, Priority and Size**

Set "Type (set)" through `updateIssue` where it is not "keep". Add the issue
to the project through `addProjectV2ItemById` if it is not there, then set
Priority and Size through `updateProjectV2ItemFieldValue` with the IDs in
Global Constraints.
Expected: each mutation returns the item.

- [ ] **Step 3: Update the EdgeZero epics' text**

Edit IABTechLab/edgezero#468's Related section to say that IABTechLab/edgezero#391 stays
the Axum production epic and that the six issues moved from it or with no
parent are tracked here, and add IABTechLab/trusted-server#90 to #1558's
Related section, each with
`gh issue edit <epic> --repo <owner>/<repo> --body-file <file>`. Post a
`CREATOR:` comment on IABTechLab/edgezero#391 and on IABTechLab/edgezero#88
naming the issues that moved to IABTechLab/edgezero#468, with
`gh issue comment <n> --repo IABTechLab/edgezero --body-file <file>`.
Expected: each command prints the issue or comment URL, and
`gh issue view <epic> --repo <owner>/<repo>` shows the new Related text.

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
issue, and one on IABTechLab/edgezero#391 linking R1, each with
`gh issue comment <n> --repo IABTechLab/edgezero --body-file <file>`.
Expected: two comment URLs.

### Task 3: File the Axum tickets under #1558

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part B's Axum table and the spec's requirements.
- Produces: an issue number per A row, recorded in a `CREATOR:` comment on
  #1558, which Task 5 consumes.

Same steps as Task 2, against IABTechLab/trusted-server and #1558, for the
nine new rows in table order. Each body has Summary (Ticket), Requirements
(Reqs, linked to the spec section), Acceptance (each requirement's Accept
line) and Blocked by. Record the map in a `CREATOR:` comment on #1558.

### Task 4: File the Trusted Server tickets under #1558

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part B's Trusted Server table and the spec's requirements.
- Produces: an issue number per T row, recorded in a `CREATOR:` comment on
  #1558, which Task 5 consumes.

Same steps as Task 3, for the T rows in table order (T1 to T22; T7 and T14
are folded into A2 and A10), under #1558. Record the
map in a `CREATOR:` comment on #1558.

### Task 5: Link the dependencies

**Files:** none; GitHub only.

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

For each issue, run
`gh api graphql -f query='{repository(owner:"<owner>",name:"<repo>"){issue(number:<n>){blockedBy(first:50){nodes{number repository{nameWithOwner}}}}}}' --jq '.data.repository.issue.blockedBy.nodes'`
and compare the list with Part B.
Expected: every issue pair present, plus the list of pairs recorded only in
bodies.

### Task 6: Tie out the Trusted Server PRs

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part C's Trusted Server items and the issue numbers from Tasks 1
  to 4.
- Produces: one `CREATOR:` comment per mapped PR.

- [ ] **Step 1: Check heads**

For each Part C Trusted Server item, run
`gh pr view <N> --repo IABTechLab/trusted-server --json headRefOid --jq .headRefOid`.
Expected: the head Part C names. If it moved, re-check the item against the
new head before posting.

- [ ] **Step 2: Post**

Post one `CREATOR:` comment per PR with its Part C item and its ticket, with
`gh pr comment <N> --repo IABTechLab/trusted-server --body-file <file>`. Use
`Part of #<ticket>` for #1532 to #1529, #1526 to #1522, #1506 to #1507, #1531
to A1, #1536, #1537 and #1566 to A2, #1535 to T19 and T20, #1539 and #1538 to
A6, and #1540 to A7. Use `Blocks #<ticket>` for the provider stack, as
Part B's Blocked by cells say: #1487 blocks T11, #1246, #1549 and #1553;
#1488 blocks T3, T13, T22 and #1555; and #1489 blocks T2, T15, T21 and #1551.
#1525 and #1541 get their Part C item with no ticket; #1503 has no item. For
proposed closures, ask the author; do not close another author's PR.
Expected: one comment URL per PR.

### Task 7: Tie out the EdgeZero PRs

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: Part C's EdgeZero items and the issue numbers from Task 2.
- Produces: one `CREATOR:` comment per mapped PR.

Same steps as Task 6, against IABTechLab/edgezero (`gh pr view <N> --repo
IABTechLab/edgezero` and `gh pr comment <N> --repo IABTechLab/edgezero`), for
the EdgeZero items in Part C. Link IABTechLab/edgezero#406 to EZI-1,
IABTechLab/edgezero#463 to EZI-2b, IABTechLab/edgezero#469 to IABTechLab/edgezero#283,
IABTechLab/edgezero#439 to IABTechLab/edgezero#432 and EZI-4a, and
IABTechLab/edgezero#275, IABTechLab/edgezero#460, IABTechLab/edgezero#407 and
IABTechLab/edgezero#414 to R1. IABTechLab/edgezero#389,
IABTechLab/edgezero#401, IABTechLab/edgezero#444, IABTechLab/edgezero#466 and
IABTechLab/edgezero#464 get their Part C item with no ticket.

### Task 8: Verify coverage

**Files:** none; GitHub only.

**Interfaces:**

- Consumes: the results of Tasks 1 to 5.
- Produces: a pass, or a list of gaps posted as a `CREATOR:` comment on #1558.

- [ ] **Step 1: Count sub-issues**

Run `gh api graphql -f query='{repository(owner:"IABTechLab",name:"<repo>"){issue(number:<epic>){subIssues(first:100){totalCount}}}}' --jq '.data.repository.issue.subIssues.totalCount'`
for #1558 and IABTechLab/edgezero#468.
Expected: 45 and 32.

- [ ] **Step 2: Check that Fastly and Axum stand alone**

For every issue in waves 0 to 9, read its blocked-by list and confirm that none
of its blockers is in wave 10 or later or in an `any` row (a Cloudflare or Spin
ticket, IABTechLab/edgezero#91, R3, R4, EZI-2p, EZI-5, EZI-5c, EZI-5s, T10, T16,
#1139, #1142, #1245, #1549, IABTechLab/edgezero#438 or IABTechLab/edgezero#464), reading the list as Task 5
Step 2 does.
Expected: no such blocker (DEC-4).

- [ ] **Step 3: Check bodies**

For every created or moved issue, read its body with
`gh issue view <n> --repo <owner>/<repo> --json body --jq .body` and confirm
three things:
each "Blocked by" names an issue, PR or decision; no Trusted Server body
contains a bare `#N` below 1000 (EdgeZero references use
`IABTechLab/edgezero#N`); and a created body holds only its requirement,
acceptance and dependencies.
Expected: no gaps.

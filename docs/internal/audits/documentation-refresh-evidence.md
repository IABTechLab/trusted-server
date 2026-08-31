# Documentation Refresh Evidence

- **Schema version:** 1
- **Created:** 2026-08-31
- **Epoch 1 system of record:** this append-only repository ledger
- **Epoch 2 system of record:** canonical c2 tracking issue, URL pending
- **Epoch 3 system of record:** canonical release-handoff issue, URL pending

This ledger records evidence, not plans or inferred outcomes. A pending field
is not proof. Command output may be summarized only when the command, exact
commit, result, and authoritative raw-capture location are also recorded.

## Durable capture contract

Every Epoch 2 or Epoch 3 issue capture, and every Epoch 1 external handoff
import, must be append-only and timestamped in UTC. Each capture includes:

- schema version, actor, operation, and timestamp;
- exact commit and ref, plus PR number and exact head, base, and trusted-tool
  SHAs when a PR or validator is involved;
- workflow run ID, attempt, job name, and job URL when automation is involved;
- redacted request method, endpoint, and body;
- response status and redacted response body;
- dependency snapshot detector, correlator, snapshot ID, ref, and SHA when
  applicable; and
- the applicable dependency-graph, ruleset, branch-protection, merge-queue, or
  branch API JSON.

Tokens, credential-bearing headers, cookies, and unredacted secrets are never
captured. Every request body, response body, and applicable API JSON body
includes its actual redacted content, byte length, and SHA-256. Each issue
comment is at most 60 KiB. A larger capture is split into ordered chunks; each
chunk includes its actual redacted content, index, byte length, and SHA-256,
and the capture records the aggregate byte length and SHA-256. Workflow, PR,
issue, and artifact URLs are navigation aids only; pasted redacted bodies plus
hashes are authoritative. Corrections append a new comment that names the
superseded comment URL and capture ID; existing comments are never edited or
deleted.

### Capture template

```text
Capture ID:
Schema version: 1
Timestamp (UTC):
Actor:
Operation:
Commit SHA:
Ref:
PR number / URL:
PR head SHA:
PR base SHA:
Trusted-tool SHA:
Run ID / attempt / URL:
Job name / URL:
Request method / endpoint:
Redacted request body:
Request-body bytes / SHA-256:
Response status:
Redacted response body:
Response-body bytes / SHA-256:
Snapshot detector / correlator / ID / ref / SHA:
Graph API redacted JSON body / bytes / SHA-256:
Ruleset API redacted JSON body / bytes / SHA-256:
Protection and merge-queue API redacted JSON body / bytes / SHA-256:
Branch API redacted JSON body / bytes / SHA-256:
Ordered chunk index / total:
Ordered chunk redacted content / bytes / SHA-256:
Aggregate bytes / SHA-256:
Navigation URLs:
Supersedes capture/comment:
Result:
```

## Package checkpoint template

Copy this block into the matching Epoch 1 section before each package starts.
One block covers one reviewed package or adjacent evidence-only commit.

```text
Task / package:
Package start HEAD:
Timestamp (UTC):
Actor:
Approved path list:
Failing fixture or pre-change proof:
Focused red command / expected diagnostic:
Minimal change:
Focused green command / result:
Affected regressions / result:
Exact staged name-status from package start HEAD:
Untracked-path check / result:
Unstaged tracked-byte check / result:
Generated command / second-run no-diff proof:
Docs-parity checks / result:
Live smoke or external receipt:
Exception / owner / rationale / expiry:
Evidence-ledger restage and repeated checks:
git diff --cached --check result:
Commit SHA / message:
Post-commit clean-status result:
Correction reference:
```

## Epoch 1: pre-merge implementation evidence

Epoch 1 retains exact evidence while `origin/rc/202608` equals
`07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` and the implementation branch
contains that commit. Any rc advance requires a focused delta audit and an
updated approved baseline before work continues.

### Task 1: decisions and immutable tips

- Capture timestamp: 2026-08-31T22:55:52Z.
- Executor: `OpenAI Codex task agent task1_implementer`.
- Approver: `aram356`.
- Operation: fetch refs, verify exact rc tip and ancestry, record starting
  `main` tip, and establish decision/evidence records.
- Implementation start HEAD:
  `b904b3aeb5af26a536afadcbfb2d70af36bca5a2`.
- `git fetch origin rc/202608 main`: passed.
- `git rev-parse origin/rc/202608`:
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`.
- Ancestry check for `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`
  against `HEAD`: exited 0.
- `git rev-parse origin/main`:
  `d516a9e94249e10cbc36e41beb4269f9255cf407`.
- Approved paths: the design, implementation plan, decision record, and this
  evidence record.
- `cd docs && npm run format`: passed.
- `cd docs && npm run lint`: passed.
- `cd docs && npm run build`: passed; generated `.vitepress/.temp` output was
  removed and not staged.
- `git diff --check` and `git diff --cached --check`: passed after formatting.
- Cached path review: exactly the four approved paths above; no unintended
  untracked file or unstaged tracked byte remained.
- Commit: the enclosing commit uses
  `Approve documentation refresh delivery plan`; its SHA and post-commit clean
  status are recorded in the execution handoff because a commit cannot contain
  its own SHA.

#### Task 1 completion receipt

- Receipt timestamp: 2026-08-31T23:15:37Z.
- Executor: `OpenAI Codex task agent task1_implementer`.
- Approver: `aram356`.
- Primary implementation commit:
  `8588391b9e9d6f02d886c519836eedb84a37abd8`.
- Review-fix commit:
  `349fd46a38fac68d803bc80e1557b9cfddba6ac6`.
- `npx prettier --write` on the four Task 1 records: passed.
- `cd docs && npm run format`: passed.
- `cd docs && npm run lint`: passed.
- `cd docs && npm run build`: passed. VitePress emitted only the known
  non-failing `vcl`-to-plain-text syntax-highlighting warning; generated
  `.vitepress/.temp` output was removed and not staged.
- `git diff --check` and pre-commit `git diff --cached --check`: passed.
- Review-fix staged path set: exactly the spec, plan, decisions record, and
  evidence record; no unintended untracked file or unstaged tracked byte.
- Clean-status observation immediately before this evidence-only mutation:
  `git status --porcelain` printed nothing.

This immediately adjacent evidence-only commit cannot contain its own SHA.
Its full SHA is reported in the execution handoff and independently verified
by the controller and review; no recursive receipt commit follows.

### External delivery URLs and immutable identifiers

| Item                              | PR or issue URL                                        | Target      | Fresh audited base                         | Validated head/tool                                        | Merge SHA | Evidence state                  |
| --------------------------------- | ------------------------------------------------------ | ----------- | ------------------------------------------ | ---------------------------------------------------------- | --------- | ------------------------------- |
| (a) rc implementation PR          | https://github.com/IABTechLab/trusted-server/pull/1049 | `rc/202608` | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` | Remote capture: `b904b3aeb5af26a536afadcbfb2d70af36bca5a2` | Pending   | OPEN, draft; refresh after push |
| (b) containment PR                | Pending                                                | `main`      | Pending                                    | Pending                                                    | Pending   | Pending                         |
| (c) validation-only controller PR | Pending                                                | `main`      | Pending                                    | Pending                                                    | Pending   | Pending                         |
| (d) CNAME deletion PR             | Pending                                                | `main`      | Pending                                    | Pending                                                    | Pending   | Pending                         |
| (c2) activation PR                | Pending                                                | `main`      | Pending                                    | Pending                                                    | Pending   | Epoch 2                         |
| (e) release-handoff PR            | Pending                                                | `main`      | Pending                                    | Pending                                                    | Pending   | Epoch 3                         |

#### PR (a) pre-push metadata capture

- Capture timestamp: 2026-08-31T23:12:46Z.
- URL: https://github.com/IABTechLab/trusted-server/pull/1049.
- State: OPEN, draft.
- Base: ref `rc/202608`, SHA
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`.
- Remote head: ref `spec-docs-refresh`, captured SHA
  `b904b3aeb5af26a536afadcbfb2d70af36bca5a2`.
- Author and assignee: `aram356`.

This is a timestamped pre-push remote capture. It does not assert that Task 1
or its follow-up commits exist on the remote head. Refresh every field after
those commits are pushed and before using PR #1049 as a validation input.

### Cross-worktree handoff: PRs (b), (c), and (d)

While each tightly scoped `main` branch is open, its branch-specific evidence
stays in its PR description or named tracking issue. Do not add rc-only audit
records to that branch. The next named rc checkpoint imports every field below
by both URL and literal value.

```text
Handoff ID / PR label: (b), (c), or (d)
Source PR / tracking issue URL:
Source capture IDs:
Source capture timestamp (UTC):
Source capture actor:
Source worktree path:
Branch name:
Source PR state / draft:
Fresh audited_main_tip:
Exact PR base SHA:
Exact PR head SHA:
Trusted tool / source SHA:
Base-controlled controller ref / SHA:
Merge SHA:
Changed paths and modes:
Local commands and results:
Hosted check names, apps, run IDs, attempts, jobs, and results:
Ruleset / protection / merge-queue capture IDs and body SHA-256 values:
Live receipt or smoke commands, endpoints, expected content, and results:
Rollback owner and tested rollback path:
Imported by rc task / package start HEAD:
Import timestamp / actor:
Imported literal values checked against source:
Import commit SHA:
```

Required import checkpoints:

- PR (b): import into Task 4 after live containment smoke.
- PR (d): import into Task 4 after project-URL smoke.
- PR (c): import in the adjacent Task 11 rc evidence checkpoint.

### Branch-protection, ruleset, and queue snapshots

| Checkpoint               | Capture ID / URL | Exact `main` SHA | Required contexts and apps                           | Strict   | Merge queue          | Bypass policy | Body SHA-256 |
| ------------------------ | ---------------- | ---------------- | ---------------------------------------------------- | -------- | -------------------- | ------------- | ------------ |
| Before PR (c)            | Pending          | Pending          | Pending                                              | Pending  | Must report disabled | Pending       | Pending      |
| Immediately after PR (c) | Pending          | Pending          | Includes `docs/automation-delta` from GitHub Actions | Required | Disabled             | Pending       | Pending      |
| Epoch 1 final reproval   | Pending          | Pending          | Pending exact inventory                              | Required | Disabled             | Pending       | Pending      |

### First-success smokes and public delivery

| Surface                              | Exact commit/ref | Command or operation                                    | Expected oracle                                                                 | Receipt / result |
| ------------------------------------ | ---------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------- | ---------------- |
| Pages containment                    | PR (b) merge     | Live URL matrix                                         | Excluded paths 404; root, Guide, and reference page return expected 200 content | Pending          |
| CNAME deletion                       | PR (d) merge     | Project-path live URL matrix                            | Canonical page and assets resolve under project path; placeholder absent        | Pending          |
| Axum                                 | Pending          | `scripts/smoke-axum.sh`                                 | Non-health publisher response satisfies the documented strong oracle            | Pending          |
| Fastly                               | Pending          | `scripts/smoke-fastly.sh`                               | Local push and required secrets yield a non-health publisher response           | Pending          |
| Cloudflare                           | Pending          | `scripts/smoke-cloudflare.sh`                           | Envelope transfer yields a non-health publisher response                        | Pending          |
| Spin                                 | Pending          | `scripts/smoke-spin.sh` or time-bounded manual contract | Local push and variables yield a non-health publisher response                  | Pending          |
| Controller read-only dispatch        | Pending          | `validate_rc`                                           | Exact trusted rc SHA passes; no writer or attestation job runs                  | Pending          |
| Controller protected-delta rejection | Pending          | Unauthorized protected-file fixture                     | Required status blocks merge                                                    | Pending          |
| Controller unrelated-PR pass         | Pending          | Net-empty protected-delta fixture                       | Required status reports success without privileged PR checkout                  | Pending          |

### Generated-diff proof

Each generator records its command, first-run changed paths, second-run exit
status, and exact clean-diff assertion. “Generated” without a second-run
no-diff proof is incomplete.

| Generator / region               | Source SHA | First-run output | Second-run command | Clean-diff assertion | Result  |
| -------------------------------- | ---------- | ---------------- | ------------------ | -------------------- | ------- |
| Tracked/source classification    | Pending    | Pending          | Pending            | Pending              | Pending |
| Settings reference               | Pending    | Pending          | Pending            | Pending              | Pending |
| Route/API reference              | Pending    | Pending          | Pending            | Pending              | Pending |
| Integration/support matrix       | Pending    | Pending          | Pending            | Pending              | Pending |
| CLI help goldens                 | Pending    | Pending          | Pending            | Pending              | Pending |
| Gate consumers                   | Pending    | Pending          | Pending            | Pending              | Pending |
| c2 and inverse patches           | Pending    | Pending          | Pending            | Pending              | Pending |
| Release retarget/disable patches | Pending    | Pending          | Pending            | Pending              | Pending |

### Exceptions and waivers

Every exception requires an owner, narrow rationale, and review or expiry date.
Expired or ownerless entries fail the checkpoint.

| Type / path                                 | Value classification   | Owner                 | Rationale                                                                                                              | Review or expiry           | State    |
| ------------------------------------------- | ---------------------- | --------------------- | ---------------------------------------------------------------------------------------------------------------------- | -------------------------- | -------- |
| `fastly.toml` `service_id`                  | Service ID             | `aram356`             | Check mode fails at or after expiry; renewal requires a reviewed committed replacement; not the ops migration deadline | `2026-09-30T00:00:00Z`     | Approved |
| Task 15 temporary public-page ownership     | Page/orphan transition | Pending Task 15 owner | Page registered before Task 16 final ownership                                                                         | Expires at Task 16         | Pending  |
| Spin manual smoke, only if CI cannot run it | Manual evidence        | Pending               | Runner capability gap                                                                                                  | Time-bounded date required | Pending  |

### Follow-up issues

Each row requires a deduplicated issue URL or explicit existing-issue
disposition, owner, and labels. Do not replace these rows with umbrella issues.

| Finding                                            | Issue or disposition URL | Owner   | Labels  | State   |
| -------------------------------------------------- | ------------------------ | ------- | ------- | ------- |
| Adapter `Hooks::stores()` and dead store manifests | Pending                  | Pending | Pending | Pending |
| Cloudflare config-store / CLI envelope bridge      | Pending                  | Pending | Pending | Pending |
| Axum local config-store / env bridge               | Pending                  | Pending | Pending | Pending |
| Cross-adapter health and startup-failure contract  | Pending                  | Pending | Pending | Pending |
| `imp_ext` reserved-field protection                | Pending                  | Pending | Pending | Pending |
| Partner pull-token placeholder rejection           | Pending                  | Pending | Pending | Pending |
| Inline `trusted_client_ip.shared_secret`           | Pending                  | Pending | Pending | Pending |
| Deploy-ID constant and set equality                | Pending                  | Pending | Pending | Pending |
| Vendored CLI help internal references              | Pending                  | Pending | Pending | Pending |
| Tinybird telemetry runtime support                 | Pending                  | Pending | Pending | Pending |
| `.env.dev` undeclared `opid_store`                 | Pending                  | Pending | Pending | Pending |
| Fastly staging config-blob selection               | Pending                  | Pending | Pending | Pending |

### Package sections

Every section below receives a completed package checkpoint block. A task with
an isolated `main` PR records its rc import checkpoint separately.

#### Task 2 — PR (b) public-site containment

Pending. Evidence source: cross-worktree handoff; import at Task 4.

#### Task 3 — PR (d) CNAME deletion

Pending. Evidence source: cross-worktree handoff; import at Task 4.

#### Task 4 — WP1 rc import and hygiene

Pending. Must import PRs (b) and (d), prove byte identity, and complete the
Task 1 bootstrap exception checks.

#### Task 5 — docs-parity model and repository scaffolding

Pending. Record each atomic fixture cycle and the bootstrap exception checks.

#### Task 6 — exhaustive classification and sensitive scanner

Pending. Must bootstrap the complete then-current repository and enforce the
approved service-ID exception.

#### Task 7 — Markdown ownership, links, and generated regions

Pending. Record each atomic fixture cycle.

#### Task 8 — settings extraction and template harness

Pending. Record each atomic fixture cycle.

#### Task 9 — integrations, routes, and adapter support

Pending. Record each atomic fixture cycle and any behavior-preserving private
route seam.

#### Task 10 — CLI help, snippets, gates, workflows, and snapshots

Pending. Preserve the planned adjacent source and golden commits and record
both clean checkpoints.

#### Task 11 — PR (c) validation-only controller

Pending. Keep branch evidence external while open, then import exact values in
the adjacent rc evidence commit before Task 12.

#### Task 12 — WP2 truth pass and FAQ archive

Pending. Record full source/disposition equality, scanner results, tombstone
smokes, and the selected archive move.

#### Task 13 — WP3 configuration reference

Pending. Record generated reference equality, compiled probes, and template
round-trip proof.

#### Task 14 — WP4 API contracts

Pending. Record route-set equality, adapter predicates, generated no-diff, and
adapter regression suites for any private seam.

#### Task 15 — deployment guides and first-success smokes

Pending. Record all four smoke contracts, exact tool versions, cleanup, and
any time-bounded Spin exception.

#### Task 16 — WP5 product coverage and navigation

Pending. Record page/orphan ownership, diagram prose equivalents, snippet
checks, and removal of the Task 15 transition exception.

#### Task 17 — WP6 root and crate documentation

Pending. Apply and verify the factual-governance fallback.

#### Task 18 — WP7 rustdoc and JSDoc

Pending. Record the rustdoc matrix, doctests, JSDoc fixtures, and JS checks.

#### Task 19 — WP8b enforcement and release controls

Pending. Record every workflow negative fixture, generated consumer proof,
follow-up issue row, c2 issue URL, release-handoff issue URL, and reviewed
patch/runbook hashes.

#### Task 20 — final Epoch 1 acceptance

Pending. Record the exact final rc baseline, full local and hosted gates,
required-check topology, generated no-diff, smokes, all external handoffs,
clean package shape, final PR head, and implementation-ready approval.

## Epoch 2: activation evidence schema

- **Canonical c2 tracking issue URL:** Pending; Task 19 must populate it before
  activation.
- **Owner:** Pending before c2 opens.

The issue uses the durable capture contract above. Required captures:

1. #1049 merge result, `merged_rc_tip`, current `origin/rc/202608`, and any
   focused delta audit producing `validated_rc_tip`.
2. Read-only `validate_rc` dispatch at `ref: main`, exact `tool_sha`, run and
   job identity, and proof that no writer or attestation ran.
3. c2 PR URL, fresh `audited_main_tip`, exact base/head/tool SHAs, protected
   path modes/blob IDs, cached candidate diff, and trusted validation result.
4. Automatic pending status and exact manual success replacement, including
   context, source app, head/base binding, and just-before-merge reassertion.
5. c2 merge SHA and first `refresh_dependency_snapshot` dispatch.
6. Redacted snapshot request, 201 response, stable detector/correlator,
   snapshot ID, authenticated rc ref/SHA, graph API JSON, alert-triage owner,
   runbook URL, and two-business-day SLA.
7. One genuine scheduled run, including link reader, schedule-only issue
   writer, snapshot reader/writer, reconciliation, timeout/concurrency state,
   and resulting issue/snapshot state.
8. Reverse-c2 patch proof for both protected modes/blobs and, if invoked,
   drain/cancel and same-identity empty-snapshot receipts.
9. Activation decision. It must not claim lifecycle closure.

No Epoch 2 repository evidence-only branch or commit is permitted.

## Epoch 3: release-handoff evidence schema

- **Canonical release-handoff issue URL:** Pending; Task 19 must populate it
  before activation closes.
- **Owner:** Pending before activation closes.

The issue uses the durable capture contract above. Required captures:

1. Selected normal or abandonment path, reviewed patch/runbook hashes, owner,
   exact `validated_rc_tip`, freeze timestamp, bypass policy, and repeated
   freeze checks.
2. On normal release, current-main base versus frozen-rc-head modes/blob IDs
   for both protected paths and the net-empty result; otherwise the separately
   reviewed repair/sync PR evidence.
3. PR (e) URL, fresh `audited_main_tip`, exact head/base/tool SHAs, automatic
   pending status, manual validation success, and just-before-merge binding.
4. PR (e) merge SHA and proof that no further temporary snapshot submission
   path remains.
5. Enumeration of every queued or in-progress same-identity run, wait/cancel
   result, optional separately scoped `actions: write` actor, and proof the
   empty same-identity snapshot is the final submission.
6. Redacted retirement request and 201 response, exact merge SHA/ref,
   detector/correlator/snapshot ID, dependency-graph replacement or
   disappearance, and all body hashes.
7. Final ruleset, required-context, strictness, merge-queue, live Pages/CNAME,
   branch API, and rc-deletion captures.
8. Normal-path full WP8 activation and maintenance allow/reject proofs, or
   abandonment-path removal of the nonreporting context plus the next-PR
   non-stranding proof.
9. Lifecycle-closed decision only after every selected-path gate passes.

No Epoch 3 repository evidence-only branch or commit is permitted.

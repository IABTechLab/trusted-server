# Documentation Refresh Evidence

- **Schema version:** 1
- **Created:** 2026-08-31
- **Implementation system of record:** this append-only repository ledger

This ledger records evidence, not plans or inferred outcomes. A pending field
is not proof. Command output may be summarized only when the command, exact
commit, result, and authoritative raw-capture location are also recorded.

## Durable capture contract

This contract applies only to real external captures. Local builds, fixture
runs, simulations, and mocked API output belong in package checkpoints and
cannot be represented as external receipts. Every external capture must be
append-only and timestamped in UTC. Each capture includes:

- schema version, actor, operation, and timestamp;
- exact commit and ref, plus PR number and exact head, base, and trusted-tool
  SHAs when a PR or validator is involved;
- workflow run ID, attempt, job name, and job URL when automation is involved;
- redacted request method, endpoint, and body;
- response status and redacted response body;
- dependency snapshot detector, correlator, snapshot ID, ref, and SHA when
  applicable; and
- the applicable dependency-graph, ruleset, branch-protection, or branch API
  JSON.

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
Protection API redacted JSON body / bytes / SHA-256:
Branch API redacted JSON body / bytes / SHA-256:
Ordered chunk index / total:
Ordered chunk redacted content / bytes / SHA-256:
Aggregate bytes / SHA-256:
Navigation URLs:
Authoritative capture/comment URL:
Supersedes capture/comment:
Result:
```

## Package checkpoint template

Copy this block into the matching implementation section before each package
starts. One block covers one reviewed package or adjacent evidence-only
commit.

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

## Pre-merge implementation evidence

This ledger retains exact evidence while `origin/rc/202608` equals
`07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` and the implementation branch
contains that commit. Any rc advance requires a focused delta audit and an
updated approved baseline before work continues.

### Prior approval package: decisions and immutable tips

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
- Commit: `8588391b9e9d6f02d886c519836eedb84a37abd8` uses
  `Approve documentation refresh delivery plan`; the review-fix and later
  evidence-only receipt commits are identified below.

#### Prior approval package completion receipt

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

The immediately adjacent evidence-only receipt commit is
`931e53e2cbd94d8a7f9fad9ec9d337d37a0f21ca`. That commit did not record its
own SHA; this later ledger update supplies the exact identifier.

### Task 1: align program records to the single PR

- Capture timestamp: 2026-09-01T07:41:54Z.
- Executor: `OpenAI Codex task agent task1_single_pr_records`.
- Package start HEAD:
  `01bf84a4beb4a1be4f26965478a0211f59392962`.
- Approved paths: the design, decision record, and this evidence record.
- `git fetch origin rc/202608 spec-docs-refresh`: passed; both named refs were
  fetched from `github.com:IABTechLab/trusted-server`.
- `test "$(git rev-parse origin/rc/202608)" = 07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`:
  exited 0.
- `git merge-base --is-ancestor 07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf HEAD`:
  exited 0.
- `git rev-parse origin/rc/202608`:
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`.
- `git rev-parse origin/spec-docs-refresh`:
  `01bf84a4beb4a1be4f26965478a0211f59392962`.
- `gh pr view 1049 --json url,state,isDraft,baseRefName,baseRefOid,headRefName,headRefOid`:
  passed with the exact JSON recorded in the live metadata capture below.
- Minimal change: approved the reviewed spec, made #1049 the only
  implementation row, retained #1104 only as closed reviewed source, removed
  obsolete delivery machinery, and defined durable release-pending rows.
- Initial `npm run format && npm run lint && npm run build`: stopped at
  `npm run format` because the two edited audit files required Prettier.
- `npx prettier --check internal/audits/documentation-refresh-decisions.md internal/audits/documentation-refresh-evidence.md`:
  reproduced the two-file formatting failure.
- `npx prettier --write internal/audits/documentation-refresh-decisions.md internal/audits/documentation-refresh-evidence.md`:
  formatted exactly those two files.
- `npm run format && npm run lint && npm run build`: passed from `docs`.
  VitePress completed in 11.07 seconds with only the known non-failing `vcl`
  syntax-highlighting fallback.
- Generated cleanup: removed only `docs/.vitepress/.temp`.
- `git diff --check`: passed.
- `git ls-files --others --exclude-standard`: printed nothing after generated
  cleanup.
- Enclosing commit message: `Align documentation refresh records to one PR`.
  Its SHA and push result are reported in the execution handoff because the
  commit cannot contain its own SHA.

### Implementation PR and immutable identifiers

PR #1049 is the only implementation row. The captured remote head is evidence
of the named observation only, not a permanent final SHA.

| Implementation PR                                      | Target      | Audited base                               | Captured remote head                       | Capture time         | State       |
| ------------------------------------------------------ | ----------- | ------------------------------------------ | ------------------------------------------ | -------------------- | ----------- |
| https://github.com/IABTechLab/trusted-server/pull/1049 | `rc/202608` | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` | `01bf84a4beb4a1be4f26965478a0211f59392962` | 2026-09-01T07:35:08Z | OPEN, draft |

#### PR #1049 live metadata capture

- Capture timestamp: 2026-09-01T07:35:08Z.
- Command:
  `gh pr view 1049 --json url,state,isDraft,baseRefName,baseRefOid,headRefName,headRefOid`.
- Result:
  `{"baseRefName":"rc/202608","baseRefOid":"07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf","headRefName":"spec-docs-refresh","headRefOid":"01bf84a4beb4a1be4f26965478a0211f59392962","isDraft":true,"state":"OPEN","url":"https://github.com/IABTechLab/trusted-server/pull/1049"}`.

Refresh this capture after package commits are pushed and before using PR #1049
as a hosted validation input.

### Superseded reviewed source PR

- PR: https://github.com/IABTechLab/trusted-server/pull/1104.
- State: closed and superseded.
- Base: `d516a9e94249e10cbc36e41beb4269f9255cf407`.
- Reviewed source commits:
  `34b0613dc603ba6529396dad4dd4b7e68b1e11a9` and
  `e6554f24f58f6122fb806ce25432f66033765c65`.
- Retention: its source branch exists only for later transfer into #1049 in
  Task 2.
- External result: no live merge or deploy receipt exists; none is claimed.

### Release-pending external evidence

Only real post-`main` operations can complete these rows. Each completion uses
the durable capture contract, including the actual redacted bodies, byte
lengths, and SHA-256 hashes. Local builds, CI simulations, fixtures, and mocked
API output cannot complete a row.

| Surface                     | Required real external capture                                                                                                             | Capture owner                 | Canonical capture destination | State                       |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------- | ----------------------------- | --------------------------- |
| Pages and CNAME             | Deployed `main` SHA; live URL/content/header matrix; project-path asset behavior; observed CNAME and canonical-URL behavior                | Pending — Task 17             | Pending — Task 17             | `release-pending`           |
| First scheduled link run    | Default-branch run ID, attempt, jobs, URLs, app identities, bounded artifact, concurrency/timeout result, and issue-reconciliation outcome | Pending — Task 17             | Pending — Task 17             | `release-pending`           |
| Dependency submission/graph | Authenticated `main` SHA; submission request and response; 201 result; detector/correlator; graph API body; triage owner and SLA           | Pending — Task 17             | Pending — Task 17             | `release-pending`           |
| Optional `main` protection  | Only if maintainers opt in: exact contexts/apps, strictness, bypass policy, API bodies, and planted-failure block                          | Pending if selected — Task 17 | Pending if selected — Task 17 | `release-pending`, optional |

Before Task 17 commits, it must replace every applicable pending owner with a
named owner and every applicable pending destination with the authoritative
external capture or comment location. If optional protection is not selected,
Task 17 records that disposition instead of fabricating an owner or URL.

### First-success adapter smokes

| Surface    | Exact commit/ref | Command or operation                                    | Expected oracle                                                       | Receipt / result |
| ---------- | ---------------- | ------------------------------------------------------- | --------------------------------------------------------------------- | ---------------- |
| Axum       | Pending          | `scripts/smoke-axum.sh`                                 | Non-health publisher response satisfies the documented strong oracle  | Pending          |
| Fastly     | Pending          | `scripts/smoke-fastly.sh`                               | Local push and required secrets yield a non-health publisher response | Pending          |
| Cloudflare | Pending          | `scripts/smoke-cloudflare.sh`                           | Envelope transfer yields a non-health publisher response              | Pending          |
| Spin       | Pending          | `scripts/smoke-spin.sh` or time-bounded manual contract | Local push and variables yield a non-health publisher response        | Pending          |

### Generated-diff proof

Each generator records its command, first-run changed paths, second-run exit
status, and exact clean-diff assertion. “Generated” without a second-run
no-diff proof is incomplete.

| Generator / region            | Source SHA | First-run output | Second-run command | Clean-diff assertion | Result  |
| ----------------------------- | ---------- | ---------------- | ------------------ | -------------------- | ------- |
| Tracked/source classification | Pending    | Pending          | Pending            | Pending              | Pending |
| Settings reference            | Pending    | Pending          | Pending            | Pending              | Pending |
| Route/API reference           | Pending    | Pending          | Pending            | Pending              | Pending |
| Integration/support matrix    | Pending    | Pending          | Pending            | Pending              | Pending |
| CLI help goldens              | Pending    | Pending          | Pending            | Pending              | Pending |
| Gate consumers                | Pending    | Pending          | Pending            | Pending              | Pending |

### Exceptions and waivers

Every exception requires an owner, narrow rationale, and review or expiry date.
Expired or ownerless entries fail the checkpoint.

| Type / path                                 | Value classification   | Owner                 | Rationale                                                                                                              | Review or expiry           | State    |
| ------------------------------------------- | ---------------------- | --------------------- | ---------------------------------------------------------------------------------------------------------------------- | -------------------------- | -------- |
| `fastly.toml` `service_id`                  | Service ID             | `aram356`             | Check mode fails at or after expiry; renewal requires a reviewed committed replacement; not the ops migration deadline | `2026-09-30T00:00:00Z`     | Approved |
| Task 13 temporary public-page ownership     | Page/orphan transition | Pending Task 13 owner | Page registered before Task 14 final ownership                                                                         | Expires at Task 14         | Pending  |
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

Every section below receives a completed package checkpoint block. All
implementation stays on `spec-docs-refresh` and PR #1049.

#### Task 2 — Transfer the reviewed containment commits

- Capture timestamp: 2026-09-01T09:24:44Z.
- Executor: `OpenAI Codex task agent task2_transfer_containment`.
- Package start HEAD:
  `43145751bb4c4286802fbc59624844bed8a73dfc`.
- Approved paths: `docs/.vitepress/config.mts`, `docs/guide/index.md`,
  `docs/guide/onboarding.md`, `docs/internal/onboarding.md`, plus this
  evidence-only ledger update.
- `git fetch origin rc/202608 spec-docs-refresh`: exited 0 and fetched both
  named branches from `github.com:IABTechLab/trusted-server`.
- Before mutation,
  `git rev-parse HEAD origin/spec-docs-refresh origin/rc/202608` returned, in
  order, `43145751bb4c4286802fbc59624844bed8a73dfc`,
  `43145751bb4c4286802fbc59624844bed8a73dfc`, and
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`.
- `git merge-base origin/rc/202608 origin/spec-docs-refresh` returned
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`, and
  `git merge-base --is-ancestor origin/rc/202608 origin/spec-docs-refresh`
  exited 0. The immutable rc tip was therefore exact and ancestral to the
  package-start branch.
- Source PR: https://github.com/IABTechLab/trusted-server/pull/1104; state
  `CLOSED`; base branch `main`; base SHA
  `d516a9e94249e10cbc36e41beb4269f9255cf407`; source branch
  `docs-public-containment`; head SHA
  `e6554f24f58f6122fb806ce25432f66033765c65`.
- Exact authentication command:
  `gh pr view 1104 --repo IABTechLab/trusted-server --json url,state,baseRefName,baseRefOid,headRefName,headRefOid,commits`.
  It exited 0 and returned the required two commits in order:
  `34b0613dc603ba6529396dad4dd4b7e68b1e11a9` with subject
  `Contain internal documentation pages`, then
  `e6554f24f58f6122fb806ce25432f66033765c65` with subject
  `Fix internal onboarding links`; the returned base, head, URL, and state
  matched the values above.
- `git cat-file -e '34b0613dc603ba6529396dad4dd4b7e68b1e11a9^{commit}'`
  and
  `git cat-file -e 'e6554f24f58f6122fb806ce25432f66033765c65^{commit}'`
  each exited 0. `git log --reverse --format='%H %s'
d516a9e94249e10cbc36e41beb4269f9255cf407..e6554f24f58f6122fb806ce25432f66033765c65`
  returned exactly those two authenticated SHA/subject pairs and no others.
- `git diff --name-only
d516a9e94249e10cbc36e41beb4269f9255cf407..e6554f24f58f6122fb806ce25432f66033765c65`
  returned exactly the four reviewed source paths:
  `docs/.vitepress/config.mts`, `docs/guide/index.md`,
  `docs/guide/onboarding.md`, and `docs/internal/onboarding.md`.
- `git cherry-pick 34b0613dc603ba6529396dad4dd4b7e68b1e11a9`
  completed without a conflict and produced
  `06d916fcc839a67c7f4bb9fc4445e17ea0a10e56` with the unchanged subject
  `Contain internal documentation pages`.
- `git cherry-pick e6554f24f58f6122fb806ce25432f66033765c65`
  completed without a conflict and produced
  `5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed` with the unchanged subject
  `Fix internal onboarding links`.
- Conflict status: none. No manual conflict resolution was performed.
- `git diff --name-status
43145751bb4c4286802fbc59624844bed8a73dfc..HEAD` returned exactly
  `M docs/.vitepress/config.mts`, `M docs/guide/index.md`,
  `D docs/guide/onboarding.md`, and `A docs/internal/onboarding.md`.
- `git diff --quiet e6554f24f58f6122fb806ce25432f66033765c65
HEAD -- docs/.vitepress/config.mts docs/guide/index.md
docs/guide/onboarding.md docs/internal/onboarding.md` exited 0. The source
  and transferred blobs were byte-identical: config
  `0ec992096fe4b1e3e097269a039f81868c76694c`, guide index
  `8a2c00d049e6a8e54d25cab3183802a74a26c35a`, and internal onboarding
  `7a84844e79c4e13688f5476086e89011946cc74e`; guide onboarding was absent
  from both trees.
- From `docs`, `npm ci` exited 0 and reported `added 346 packages in 3s`;
  `npm run lint` exited 0 after `eslint .`; `npm run format` exited 0 with
  `All matched files use Prettier code style!`; and `npm run build` exited 0
  with `build complete in 4.86s.` VitePress emitted only the known non-failing
  `vcl`-to-`txt` syntax-highlighting fallback.
- The exact built-file assertion, run from `docs`, was:

  ```sh
  test -z "$(find .vitepress/dist -type f | rg '/(superpowers|internal|epics)/|/guide/onboarding[.]html$|/README[.]html$|/business-use-cases[.]html$|/assets/(superpowers_|internal_|epics_|guide_onboarding[.]md[.]|README[.]md[.]|business-use-cases[.]md[.])')"
  ```

  It exited 0 and printed nothing. The exact page-manifest assertion was:

  ```sh
  node -e 'const h=require("./.vitepress/dist/hashmap.json"); const x=Object.keys(h).filter((k)=>/^(superpowers_|internal_|epics_|guide_onboarding[.]md$|README[.]md$|business-use-cases[.]md$)/.test(k)); console.log("forbidden manifest entries="+x.length); if(x.length) process.exit(1)'
  ```

  It exited 0 and printed `forbidden manifest entries=0`. The exact built-href
  assertion was:

  ```sh
  test -z "$(rg -o --no-filename 'href=\"[^\"]+\"' .vitepress/dist --glob '*.html' | rg -i 'href=\"(?:/trusted-server)?/(?:superpowers|internal|epics)(?:[/.?#\"]|$)|href=\"(?:/trusted-server)?/(?:guide/onboarding|README|business-use-cases)(?:[/.?#\"]|$)')"
  ```

  It exited 0 and printed nothing. A supplemental per-family assertion
  recursively enumerated `.vitepress/dist`, read `hashmap.json`, extracted
  every HTML `href`, and failed on any excluded route, page-manifest entry, or
  route/asset file. It exited 0 with these exact results:
  `superpowers/**: sources=135, route/assets=0, manifest=0, hrefs=0`;
  `internal/**: sources=4, route/assets=0, manifest=0, hrefs=0`;
  `epics/**: sources=1, route/assets=0, manifest=0, hrefs=0`;
  `guide/onboarding.md: sources=0, route/assets=0, manifest=0, hrefs=0`;
  `README.md: sources=1, route/assets=0, manifest=0, hrefs=0`; and
  `business-use-cases.md: sources=1, route/assets=0, manifest=0, hrefs=0`.

- The exact required-artifact assertion was:

  ```sh
  test -f .vitepress/dist/index.html && rg -q '<title>Trusted Server</title>' .vitepress/dist/index.html && rg -q 'The New Execution Layer for Publishers' .vitepress/dist/index.html && test -f .vitepress/dist/guide/index.html && rg -q '<title>Guide | Trusted Server</title>' .vitepress/dist/guide/index.html && rg -q '<h1 id="guide"' .vitepress/dist/guide/index.html && test -f .vitepress/dist/guide/api-reference.html && rg -q '<title>API Reference | Trusted Server</title>' .vitepress/dist/guide/api-reference.html && rg -q 'Quick reference for all Trusted Server HTTP endpoints[.]' .vitepress/dist/guide/api-reference.html
  ```

  It exited 0 and printed nothing, proving the Home, Guide, and API artifacts
  exist and contain both required identifying markers.

- The exact onboarding-link assertion was:

  ```sh
  test "$(rg -o '\.\./[^)# ]+' internal/onboarding.md | wc -l | tr -d ' ')" = 9 && while IFS= read -r link; do test -f "internal/$link" && git ls-files --error-unmatch -- ":(top)docs/${link#../}" >/dev/null || exit 1; done < <(rg -o '\.\./[^)# ]+' internal/onboarding.md)
  ```

  It exited 0 and printed nothing. All 9/9 repository-relative links resolved
  to tracked targets; the seven unique targets were
  `docs/guide/what-is-trusted-server.md`, `docs/guide/architecture.md`,
  `docs/guide/getting-started.md`, `docs/guide/configuration.md`,
  `docs/guide/testing.md`, `docs/guide/integrations-overview.md`, and
  `docs/guide/integration-guide.md`.

- Removed only generated `docs/.vitepress/.temp` after the artifact checks.
  `git diff --check` then exited 0, and `git status --porcelain=v1
--untracked-files=all` printed nothing before this evidence-only mutation.
- `npx prettier --write
internal/audits/documentation-refresh-evidence.md` exited 0 and reported
  `internal/audits/documentation-refresh-evidence.md 41ms`; the following
  `npm run format` exited 0 with
  `All matched files use Prettier code style!`.
- This is local build and repository evidence only. It is not a live Pages,
  deployment, or CNAME receipt, and it does not complete any release-pending
  row.
- Evidence-only commit message: `Record documentation containment transfer`.
  Its SHA and push result are reported in the execution handoff because a
  commit cannot record its own identifier.

#### Task 3 — Complete WP1 CNAME and policy hygiene

Pending. Record CNAME deletion, public-site containment, policy updates, and
local artifact proof. Live Pages/CNAME remains release-pending.

#### Task 4 — Scaffold the standalone `docs-parity` crate

Pending. Record each atomic fixture cycle and the bootstrap exception checks.

#### Task 5 — Close tracked-file classification and sensitive-data scanning

Pending. Bootstrap the complete then-current repository and enforce the
approved service-ID exception.

#### Task 6 — Implement generated regions, Markdown ownership, and link checks

Pending. Record each atomic fixture cycle and generated second-run no-diff.

#### Task 7 — Extract settings semantics and execute the example harness

Pending. Record generated reference equality, compiled probes, and template
round-trip proof.

#### Task 8 — Check integration capabilities and adapter routes

Pending. Record each atomic fixture cycle and any behavior-preserving private
route seam.

#### Task 9 — Check CLI help, snippets, gates, and workflow foundations

Pending. Preserve the planned adjacent source and golden commits and record
both clean checkpoints.

#### Task 10 — Complete WP2 truth pass and dispositions

Pending. Record full source/disposition equality, scanner results, tombstone
smokes, and the selected archive move.

#### Task 11 — Complete WP3 configuration reference and template

Pending. Record generated reference equality, compiled probes, template
round-trip proof, and bounded PR-description publication.

#### Task 12 — Complete WP4 generated API contracts

Pending. Record route-set equality, adapter predicates, generated no-diff, and
adapter regression suites for any private seam.

#### Task 13 — Add deployment guides and recurring first-success smokes

Pending. Record all four smoke contracts, exact tool versions, cleanup, and
any time-bounded Spin exception.

#### Task 14 — Complete WP5 product coverage and navigation

Pending. Record page/orphan ownership, diagram prose equivalents, snippet
checks, and removal of the Task 13 transition exception.

#### Task 15 — Complete WP6 root and crate documentation

Pending. Apply and verify the factual-governance fallback.

#### Task 16 — Complete WP7 rustdoc and JSDoc

Pending. Record the rustdoc matrix, doctests, JSDoc fixtures, and JS checks.

#### Task 17 — Activate final CI and release-pending controls

Pending. Record workflow negative fixtures, generated consumer proof,
follow-up issue rows, and the release runbook. Real release receipts remain in
the release-pending table until observed after the result reaches `main`.

#### Task 18 — Close PR #1049 implementation

Pending. Record the exact final rc baseline, full local and hosted gates,
generated no-diff, smokes, follow-up dispositions, clean package shape, final
PR head, and implementation approval without claiming release-pending effects.

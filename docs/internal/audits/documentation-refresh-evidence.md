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
- Retention: its source branch remains only as historical provenance; its
  reviewed commits were transferred into #1049 in Task 2.
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

| Type / path                                          | Value classification   | Owner                 | Rationale                                                                                                                                                                              | Review or expiry           | State    |
| ---------------------------------------------------- | ---------------------- | --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------- | -------- |
| `fastly.toml` `service_id`                           | Service ID             | `aram356`             | Preserve the existing service binding during this refresh; check mode fails at or after expiry; removal is independent                                                                 | `2026-09-30T00:00:00Z`     | Approved |
| Deleted placeholder literal in design and Decision 9 | Historical example     | `aram356`             | Preserve the approved audit's exact record; scope is `docs/superpowers/specs/2026-08-19-documentation-refresh-design.md` and `docs/internal/audits/documentation-refresh-decisions.md` | `2027-08-31T00:00:00Z`     | Approved |
| Task 13 temporary public-page ownership              | Page/orphan transition | Pending Task 13 owner | Page registered before Task 14 final ownership                                                                                                                                         | Expires at Task 14         | Pending  |
| Spin manual smoke, only if CI cannot run it          | Manual evidence        | Pending               | Runner capability gap                                                                                                                                                                  | Time-bounded date required | Pending  |

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
- Containment evidence correction timestamp: 2026-09-01T11:08:07Z. This
  correction supersedes the shell-command-substitution containment and href
  assertions and the unrecorded supplemental assertion from evidence commit
  `0a8e57f5e5aa03423dce29b61817dfb2b7194a1e`; those checks could discard a
  scanner error and are not authoritative evidence.
- Correction package start HEAD:
  `0a8e57f5e5aa03423dce29b61817dfb2b7194a1e`. Before mutation, it equaled
  `origin/spec-docs-refresh`; `origin/rc/202608` remained exactly
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` and was ancestral to HEAD; the
  worktree was clean.
- From `docs`, the fresh correction build ran `npm ci`, `npm run lint`,
  `npm run format`, and `npm run build` in that order. They all exited 0:
  `npm ci` added 346 packages in 3 seconds, lint completed after `eslint .`,
  format reported `All matched files use Prettier code style!`, and VitePress
  reported `build complete in 4.83s.` with only the known non-failing
  `vcl`-to-`txt` fallback.
- The following exact fail-closed command was run from `docs`. The initial
  `git diff --quiet` binds the four containment paths at current HEAD to
  `5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed` before any artifact scan. Git
  invocation errors or nonzero status, directory-walk or file-read errors,
  invalid JSON, malformed URLs, unsupported filesystem entries, missing
  required artifacts, missing identifying markers, or any excluded output
  terminate nonzero. It recursively enumerates all dist files, parses
  `hashmap.json`, reads every HTML file, and inspects double-quoted,
  single-quoted, and unquoted `href` attributes.

  ```sh
  node <<'NODE'
  const fs = require('node:fs')
  const path = require('node:path')
  const { spawnSync } = require('node:child_process')

  const boundContentSha = '5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed'
  const containmentPaths = [
    'docs/.vitepress/config.mts',
    'docs/guide/index.md',
    'docs/guide/onboarding.md',
    'docs/internal/onboarding.md',
  ]
  const docsRoot = process.cwd()
  const repoRoot = path.resolve(docsRoot, '..')
  const distRoot = path.join(docsRoot, '.vitepress', 'dist')

  const binding = spawnSync(
    'git',
    ['diff', '--quiet', boundContentSha, 'HEAD', '--', ...containmentPaths],
    { cwd: repoRoot, encoding: 'utf8' },
  )
  if (binding.error) throw binding.error
  if (binding.status !== 0) {
    throw new Error(
      `containment binding failed with status ${String(binding.status)}: ${binding.stderr}`,
    )
  }

  function walkFiles(root) {
    const files = []
    for (const entry of fs.readdirSync(root, { withFileTypes: true }).sort((a, b) =>
      a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
    )) {
      const absolute = path.join(root, entry.name)
      if (entry.isDirectory()) files.push(...walkFiles(absolute))
      else if (entry.isFile()) files.push(absolute)
      else throw new Error(`unsupported filesystem entry: ${absolute}`)
    }
    return files
  }

  function countMarkdownFiles(root) {
    return walkFiles(root).filter((file) => file.endsWith('.md')).length
  }

  function countOptionalFile(file) {
    try {
      const stat = fs.statSync(file)
      if (!stat.isFile()) throw new Error(`expected a file: ${file}`)
      return 1
    } catch (error) {
      if (error && error.code === 'ENOENT') return 0
      throw error
    }
  }

  function requireFileCount(file) {
    const stat = fs.statSync(file)
    if (!stat.isFile()) throw new Error(`expected a file: ${file}`)
    return 1
  }

  const families = [
    {
      name: 'superpowers/**',
      sourceCount: () => countMarkdownFiles(path.join(docsRoot, 'superpowers')),
      fileMatch: (file) => file.startsWith('superpowers/') || file.startsWith('assets/superpowers_'),
      manifestMatch: (key) => key.startsWith('superpowers_') || key.startsWith('superpowers/'),
      routeMatch: (route) => route === '/superpowers' || route.startsWith('/superpowers/'),
    },
    {
      name: 'internal/**',
      sourceCount: () => countMarkdownFiles(path.join(docsRoot, 'internal')),
      fileMatch: (file) => file.startsWith('internal/') || file.startsWith('assets/internal_'),
      manifestMatch: (key) => key.startsWith('internal_') || key.startsWith('internal/'),
      routeMatch: (route) => route === '/internal' || route.startsWith('/internal/'),
    },
    {
      name: 'epics/**',
      sourceCount: () => countMarkdownFiles(path.join(docsRoot, 'epics')),
      fileMatch: (file) => file.startsWith('epics/') || file.startsWith('assets/epics_'),
      manifestMatch: (key) => key.startsWith('epics_') || key.startsWith('epics/'),
      routeMatch: (route) => route === '/epics' || route.startsWith('/epics/'),
    },
    {
      name: 'guide/onboarding.md',
      sourceCount: () => countOptionalFile(path.join(docsRoot, 'guide', 'onboarding.md')),
      fileMatch: (file) => file === 'guide/onboarding.html' || file.startsWith('guide/onboarding/') || file.startsWith('assets/guide_onboarding.md.'),
      manifestMatch: (key) => key === 'guide_onboarding.md' || key === 'guide/onboarding.md',
      routeMatch: (route) => route === '/guide/onboarding' || route.startsWith('/guide/onboarding/'),
    },
    {
      name: 'README.md',
      sourceCount: () => requireFileCount(path.join(docsRoot, 'README.md')),
      fileMatch: (file) => file === 'readme.html' || file.startsWith('readme/') || file.startsWith('assets/readme.md.'),
      manifestMatch: (key) => key.toLowerCase() === 'readme.md',
      routeMatch: (route) => route === '/readme' || route.startsWith('/readme/'),
    },
    {
      name: 'business-use-cases.md',
      sourceCount: () => requireFileCount(path.join(docsRoot, 'business-use-cases.md')),
      fileMatch: (file) => file === 'business-use-cases.html' || file.startsWith('business-use-cases/') || file.startsWith('assets/business-use-cases.md.'),
      manifestMatch: (key) => key === 'business-use-cases.md',
      routeMatch: (route) => route === '/business-use-cases' || route.startsWith('/business-use-cases/'),
    },
  ]

  const distFiles = walkFiles(distRoot)
  const relativeDistFiles = distFiles.map((file) => path.relative(distRoot, file).split(path.sep).join('/'))
  const htmlFiles = relativeDistFiles.filter((file) => file.endsWith('.html'))
  const hashmap = JSON.parse(fs.readFileSync(path.join(distRoot, 'hashmap.json'), 'utf8'))
  if (hashmap === null || Array.isArray(hashmap) || typeof hashmap !== 'object') {
    throw new Error('hashmap.json must contain an object')
  }
  const manifestKeys = Object.keys(hashmap).sort()
  for (const key of manifestKeys) {
    if (typeof hashmap[key] !== 'string') throw new Error(`non-string hashmap entry: ${key}`)
  }

  function normalizeHref(rawHref, htmlFile) {
    const value = rawHref.replaceAll('&amp;', '&')
    if (value === '' || value.startsWith('#')) return null
    if (/^(?:data|javascript|mailto|tel):/i.test(value)) return null
    const base = new URL(`/trusted-server/${htmlFile}`, 'https://local.invalid')
    const url = new URL(value, base)
    let route = decodeURIComponent(url.pathname)
    route = path.posix.normalize(route)
    if (route === '/trusted-server') route = '/'
    else if (route.startsWith('/trusted-server/')) route = route.slice('/trusted-server'.length)
    route = route.replace(/\.(?:html|md)$/i, '')
    if (route.length > 1) route = route.replace(/\/$/, '')
    return route.toLowerCase()
  }

  const hrefPattern = /(?:^|[\s<])href\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/gimu
  const hrefRecords = []
  for (const htmlFile of htmlFiles) {
    const html = fs.readFileSync(path.join(distRoot, htmlFile), 'utf8')
    for (const match of html.matchAll(hrefPattern)) {
      const href = match[1] ?? match[2] ?? match[3]
      hrefRecords.push({ file: htmlFile, href, route: normalizeHref(href, htmlFile) })
    }
  }

  const violations = []
  const familyResults = {}
  for (const family of families) {
    const routeAssets = relativeDistFiles.filter((file) => family.fileMatch(file.toLowerCase()))
    const manifest = manifestKeys.filter((key) => family.manifestMatch(key))
    const hrefs = hrefRecords.filter((record) => record.route !== null && family.routeMatch(record.route))
    familyResults[family.name] = {
      sourceCount: family.sourceCount(),
      routeAssetCount: routeAssets.length,
      manifestCount: manifest.length,
      hrefCount: hrefs.length,
    }
    for (const file of routeAssets) violations.push(`${family.name} file: ${file}`)
    for (const key of manifest) violations.push(`${family.name} manifest: ${key}`)
    for (const record of hrefs) violations.push(`${family.name} href in ${record.file}: ${record.href}`)
  }

  const artifactSpecifications = [
    {
      name: 'Home',
      file: 'index.html',
      markers: ['<title>Trusted Server</title>', 'The New Execution Layer for Publishers'],
    },
    {
      name: 'Guide',
      file: 'guide/index.html',
      markers: ['<title>Guide | Trusted Server</title>', '<h1 id="guide"'],
    },
    {
      name: 'API',
      file: 'guide/api-reference.html',
      markers: [
        '<title>API Reference | Trusted Server</title>',
        'Quick reference for all Trusted Server HTTP endpoints.',
      ],
    },
  ]
  const requiredArtifacts = {}
  for (const specification of artifactSpecifications) {
    const html = fs.readFileSync(path.join(distRoot, specification.file), 'utf8')
    const missingMarkers = specification.markers.filter((marker) => !html.includes(marker))
    requiredArtifacts[specification.name] = {
      file: specification.file,
      exists: true,
      markerCount: specification.markers.length - missingMarkers.length,
      requiredMarkerCount: specification.markers.length,
    }
    for (const marker of missingMarkers) {
      violations.push(`${specification.name} missing marker: ${marker}`)
    }
  }

  if (violations.length > 0) {
    throw new Error(`containment verification failed:\n${violations.sort().join('\n')}`)
  }

  console.log(
    JSON.stringify(
      {
        boundContentSha,
        families: familyResults,
        htmlFileCount: htmlFiles.length,
        hrefCount: hrefRecords.length,
        requiredArtifacts,
      },
      null,
      2,
    ),
  )
  NODE
  ```

  It exited 0. The following fenced JSON is the authoritative verbatim stdout
  from this local command:

  ```json
  {
    "boundContentSha": "5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed",
    "families": {
      "superpowers/**": {
        "sourceCount": 135,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "internal/**": {
        "sourceCount": 4,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "epics/**": {
        "sourceCount": 1,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "guide/onboarding.md": {
        "sourceCount": 0,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "README.md": {
        "sourceCount": 1,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "business-use-cases.md": {
        "sourceCount": 1,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      }
    },
    "htmlFileCount": 43,
    "hrefCount": 4604,
    "requiredArtifacts": {
      "Home": {
        "file": "index.html",
        "exists": true,
        "markerCount": 2,
        "requiredMarkerCount": 2
      },
      "Guide": {
        "file": "guide/index.html",
        "exists": true,
        "markerCount": 2,
        "requiredMarkerCount": 2
      },
      "API": {
        "file": "guide/api-reference.html",
        "exists": true,
        "markerCount": 2,
        "requiredMarkerCount": 2
      }
    }
  }
  ```

  This earlier raw local result is superseded by the semantic-parser correction
  below and is no longer authoritative. It was never a live Pages, CNAME,
  deployment, or release receipt.

- Semantic containment evidence correction timestamp:
  2026-09-01T13:35:41Z. This correction supersedes both prior href
  implementations: the shell-command-substitution scan from
  `0a8e57f5e5aa03423dce29b61817dfb2b7194a1e` and the regex-over-document Node
  scan from `75fd40671bf5ae6acafc7f5230a39e88b8e70fc6`. Neither prior href result is
  authoritative because it did not apply HTML attribute semantics.
- Semantic correction package start HEAD:
  `75fd40671bf5ae6acafc7f5230a39e88b8e70fc6`. Before mutation, it equaled
  `origin/spec-docs-refresh`; `origin/rc/202608` remained exactly
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` and ancestral to HEAD; the
  worktree was clean.
- From `docs`, the fresh semantic correction build ran `npm ci`,
  `npm run lint`, `npm run format`, and `npm run build` in that order. All
  exited 0: `npm ci` added 346 packages in 4 seconds, lint completed after
  `eslint .`, format reported `All matched files use Prettier code style!`,
  and VitePress reported `build complete in 5.21s.` with only the known
  non-failing `vcl`-to-`txt` fallback.
- The following exact fail-closed command was run from `docs`. It retains the
  four-path Git binding to
  `5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed`, recursive filesystem walk,
  manifest/file checks, six excluded families, and Home/Guide/API markers.
  Python standard-library `html.parser.HTMLParser` collects only literal
  `href` attributes from start and start-end tags and decodes HTML character
  references. Parser, filesystem, decoding, URL-normalization, Git-binding,
  and synthetic-assertion exceptions propagate; none are suppressed.

  ```sh
  python3 <<'PY'
  import json
  import os
  import platform
  import posixpath
  import re
  import stat
  import subprocess
  from html.parser import HTMLParser
  from pathlib import Path
  from urllib.parse import unquote_to_bytes, urljoin, urlsplit

  BOUND_CONTENT_SHA = '5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed'
  CONTAINMENT_PATHS = [
      'docs/.vitepress/config.mts',
      'docs/guide/index.md',
      'docs/guide/onboarding.md',
      'docs/internal/onboarding.md',
  ]
  LOCAL_ORIGIN = 'https://local.invalid'
  LOCAL_ORIGIN_TUPLE = ('https', 'local.invalid', 443)
  DOCS_ROOT = Path.cwd()
  REPO_ROOT = DOCS_ROOT.parent
  DIST_ROOT = DOCS_ROOT / '.vitepress' / 'dist'

  binding = subprocess.run(
      ['git', 'diff', '--quiet', BOUND_CONTENT_SHA, 'HEAD', '--', *CONTAINMENT_PATHS],
      cwd=REPO_ROOT,
      capture_output=True,
      text=True,
      check=False,
  )
  if binding.returncode != 0:
      raise RuntimeError(
          f'containment binding failed with status {binding.returncode}: {binding.stderr}'
      )


  def walk_files(root):
      files = []
      with os.scandir(root) as iterator:
          entries = sorted(iterator, key=lambda entry: entry.name)
      for entry in entries:
          absolute = Path(entry.path)
          if entry.is_dir(follow_symlinks=False):
              files.extend(walk_files(absolute))
          elif entry.is_file(follow_symlinks=False):
              files.append(absolute)
          else:
              raise RuntimeError(f'unsupported filesystem entry: {absolute}')
      return files


  def count_markdown_files(root):
      return sum(file.suffix == '.md' for file in walk_files(root))


  def count_optional_file(file):
      try:
          mode = os.stat(file, follow_symlinks=False).st_mode
      except FileNotFoundError:
          return 0
      if not stat.S_ISREG(mode):
          raise RuntimeError(f'expected a regular file: {file}')
      return 1


  def require_file_count(file):
      mode = os.stat(file, follow_symlinks=False).st_mode
      if not stat.S_ISREG(mode):
          raise RuntimeError(f'expected a regular file: {file}')
      return 1


  class HrefParser(HTMLParser):
      def __init__(self):
          super().__init__(convert_charrefs=True)
          self.hrefs = []

      def collect_hrefs(self, attrs):
          for name, value in attrs:
              if name.lower() == 'href':
                  if value is None:
                      raise ValueError('href attribute must have a literal value')
                  self.hrefs.append(value)

      def handle_starttag(self, tag, attrs):
          self.collect_hrefs(attrs)

      def handle_startendtag(self, tag, attrs):
          self.collect_hrefs(attrs)


  def parse_hrefs(source):
      parser = HrefParser()
      parser.feed(source)
      parser.close()
      return parser.hrefs


  def origin_tuple(parts):
      hostname = parts.hostname
      if hostname is None:
          return (parts.scheme.lower(), None, None)
      port = parts.port
      if port is None:
          if parts.scheme.lower() == 'https':
              port = 443
          elif parts.scheme.lower() == 'http':
              port = 80
      return (parts.scheme.lower(), hostname.lower(), port)


  def normalize_href(raw_href, html_file):
      if raw_href is None:
          raise ValueError(f'{html_file}: href value is missing')
      if any(ord(character) < 0x20 or ord(character) == 0x7F for character in raw_href):
          raise ValueError(f'{html_file}: href contains a control character: {raw_href!r}')
      if '\\' in raw_href:
          raise ValueError(f'{html_file}: href contains an ambiguous backslash: {raw_href!r}')
      if re.search(r'%(?![0-9A-Fa-f]{2})', raw_href):
          raise ValueError(f'{html_file}: href contains invalid percent encoding: {raw_href!r}')
      base_url = f'{LOCAL_ORIGIN}/trusted-server/{html_file}'
      try:
          resolved = urlsplit(urljoin(base_url, raw_href))
          resolved_origin = origin_tuple(resolved)
      except ValueError as error:
          raise ValueError(f'{html_file}: malformed href {raw_href!r}: {error}') from error
      if resolved_origin != LOCAL_ORIGIN_TUPLE:
          return None
      try:
          route = unquote_to_bytes(resolved.path).decode('utf-8', errors='strict')
      except UnicodeDecodeError as error:
          raise ValueError(f'{html_file}: href path is not valid UTF-8: {raw_href!r}') from error
      route = posixpath.normpath(route)
      if not route.startswith('/'):
          raise ValueError(f'{html_file}: normalized local href is not absolute: {raw_href!r}')
      if route == '/trusted-server':
          route = '/'
      elif route.startswith('/trusted-server/'):
          route = route[len('/trusted-server'):]
      route = re.sub(r'\.(?:html|md)$', '', route, flags=re.IGNORECASE)
      if len(route) > 1:
          route = route.rstrip('/')
      return route.lower()


  synthetic_results = {}


  def require_synthetic(name, condition):
      if not condition:
          raise AssertionError(f'synthetic assertion failed: {name}')
      synthetic_results[name] = True


  text_hrefs = parse_hrefs(
      '<pre>href="/trusted-server/internal/pre"</pre>'
      '<code>&lt;a href="/trusted-server/internal/code"&gt;</code>'
      '<p>href=/trusted-server/internal/plain</p>'
  )
  require_synthetic('hrefLookingTextIgnored', text_hrefs == [])
  entity_hrefs = parse_hrefs('<a href="/trusted-server/int&#x65;rnal/x">entity</a>')
  require_synthetic(
      'entityEncodedLocalHrefDecodedAndNormalized',
      entity_hrefs == ['/trusted-server/internal/x']
      and normalize_href(entity_hrefs[0], 'index.html') == '/internal/x',
  )
  require_synthetic(
      'absoluteOffsiteHrefIgnored',
      normalize_href('https://offsite.example/internal/x', 'index.html') is None,
  )
  require_synthetic(
      'protocolRelativeOffsiteHrefIgnored',
      normalize_href('//offsite.example/internal/x', 'index.html') is None,
  )
  quoted_hrefs = parse_hrefs(
      '<a href="/trusted-server/guide/double">double</a>'
      "<a href='/trusted-server/guide/single'>single</a>"
      '<a href=/trusted-server/guide/unquoted>unquoted</a>'
  )
  require_synthetic(
      'doubleQuotedHrefCollected',
      quoted_hrefs[0] == '/trusted-server/guide/double',
  )
  require_synthetic(
      'singleQuotedHrefCollected',
      quoted_hrefs[1] == '/trusted-server/guide/single',
  )
  require_synthetic(
      'unquotedHrefCollected',
      quoted_hrefs[2] == '/trusted-server/guide/unquoted',
  )

  families = [
      {
          'name': 'superpowers/**',
          'source_count': lambda: count_markdown_files(DOCS_ROOT / 'superpowers'),
          'file_match': lambda file: file.startswith('superpowers/')
          or file.startswith('assets/superpowers_'),
          'manifest_match': lambda key: key.startswith('superpowers_')
          or key.startswith('superpowers/'),
          'route_match': lambda route: route == '/superpowers'
          or route.startswith('/superpowers/'),
      },
      {
          'name': 'internal/**',
          'source_count': lambda: count_markdown_files(DOCS_ROOT / 'internal'),
          'file_match': lambda file: file.startswith('internal/')
          or file.startswith('assets/internal_'),
          'manifest_match': lambda key: key.startswith('internal_')
          or key.startswith('internal/'),
          'route_match': lambda route: route == '/internal'
          or route.startswith('/internal/'),
      },
      {
          'name': 'epics/**',
          'source_count': lambda: count_markdown_files(DOCS_ROOT / 'epics'),
          'file_match': lambda file: file.startswith('epics/')
          or file.startswith('assets/epics_'),
          'manifest_match': lambda key: key.startswith('epics_')
          or key.startswith('epics/'),
          'route_match': lambda route: route == '/epics'
          or route.startswith('/epics/'),
      },
      {
          'name': 'guide/onboarding.md',
          'source_count': lambda: count_optional_file(DOCS_ROOT / 'guide' / 'onboarding.md'),
          'file_match': lambda file: file == 'guide/onboarding.html'
          or file.startswith('guide/onboarding/')
          or file.startswith('assets/guide_onboarding.md.'),
          'manifest_match': lambda key: key in ('guide_onboarding.md', 'guide/onboarding.md'),
          'route_match': lambda route: route == '/guide/onboarding'
          or route.startswith('/guide/onboarding/'),
      },
      {
          'name': 'README.md',
          'source_count': lambda: require_file_count(DOCS_ROOT / 'README.md'),
          'file_match': lambda file: file == 'readme.html'
          or file.startswith('readme/')
          or file.startswith('assets/readme.md.'),
          'manifest_match': lambda key: key.lower() == 'readme.md',
          'route_match': lambda route: route == '/readme'
          or route.startswith('/readme/'),
      },
      {
          'name': 'business-use-cases.md',
          'source_count': lambda: require_file_count(DOCS_ROOT / 'business-use-cases.md'),
          'file_match': lambda file: file == 'business-use-cases.html'
          or file.startswith('business-use-cases/')
          or file.startswith('assets/business-use-cases.md.'),
          'manifest_match': lambda key: key == 'business-use-cases.md',
          'route_match': lambda route: route == '/business-use-cases'
          or route.startswith('/business-use-cases/'),
      },
  ]

  dist_files = walk_files(DIST_ROOT)
  relative_dist_files = [file.relative_to(DIST_ROOT).as_posix() for file in dist_files]
  html_files = [file for file in relative_dist_files if file.endswith('.html')]
  hashmap = json.loads((DIST_ROOT / 'hashmap.json').read_text(encoding='utf-8'))
  if not isinstance(hashmap, dict):
      raise TypeError('hashmap.json must contain an object')
  manifest_keys = sorted(hashmap)
  for key in manifest_keys:
      if not isinstance(hashmap[key], str):
          raise TypeError(f'non-string hashmap entry: {key}')

  href_records = []
  for html_file in html_files:
      html = (DIST_ROOT / html_file).read_text(encoding='utf-8')
      for href in parse_hrefs(html):
          href_records.append(
              {
                  'file': html_file,
                  'href': href,
                  'route': normalize_href(href, html_file),
              }
          )

  violations = []
  family_results = {}
  for family in families:
      route_assets = [
          file
          for file in relative_dist_files
          if family['file_match'](file.lower())
      ]
      manifest = [key for key in manifest_keys if family['manifest_match'](key)]
      hrefs = [
          record
          for record in href_records
          if record['route'] is not None and family['route_match'](record['route'])
      ]
      family_results[family['name']] = {
          'sourceCount': family['source_count'](),
          'routeAssetCount': len(route_assets),
          'manifestCount': len(manifest),
          'hrefCount': len(hrefs),
      }
      violations.extend(f"{family['name']} file: {file}" for file in route_assets)
      violations.extend(f"{family['name']} manifest: {key}" for key in manifest)
      violations.extend(
          f"{family['name']} href in {record['file']}: {record['href']}"
          for record in hrefs
      )

  artifact_specifications = [
      {
          'name': 'Home',
          'file': 'index.html',
          'markers': [
              '<title>Trusted Server</title>',
              'The New Execution Layer for Publishers',
          ],
      },
      {
          'name': 'Guide',
          'file': 'guide/index.html',
          'markers': ['<title>Guide | Trusted Server</title>', '<h1 id="guide"'],
      },
      {
          'name': 'API',
          'file': 'guide/api-reference.html',
          'markers': [
              '<title>API Reference | Trusted Server</title>',
              'Quick reference for all Trusted Server HTTP endpoints.',
          ],
      },
  ]
  required_artifacts = {}
  for specification in artifact_specifications:
      html = (DIST_ROOT / specification['file']).read_text(encoding='utf-8')
      missing_markers = [
          marker for marker in specification['markers'] if marker not in html
      ]
      required_artifacts[specification['name']] = {
          'file': specification['file'],
          'exists': True,
          'markerCount': len(specification['markers']) - len(missing_markers),
          'requiredMarkerCount': len(specification['markers']),
      }
      violations.extend(
          f"{specification['name']} missing marker: {marker}"
          for marker in missing_markers
      )

  if violations:
      raise RuntimeError('containment verification failed:\n' + '\n'.join(sorted(violations)))

  result = {
      'parser': {
          'name': 'html.parser.HTMLParser',
          'pythonVersion': platform.python_version(),
          'convertCharRefs': True,
      },
      'syntheticAssertions': {
          'count': len(synthetic_results),
          'results': synthetic_results,
      },
      'boundContentSha': BOUND_CONTENT_SHA,
      'families': family_results,
      'htmlFileCount': len(html_files),
      'hrefCount': len(href_records),
      'requiredArtifacts': required_artifacts,
  }
  print(json.dumps(result, indent=2, ensure_ascii=False))
  PY
  ```

  It exited 0. The following fenced JSON is the authoritative verbatim stdout
  from the semantic local command:

  ```json
  {
    "parser": {
      "name": "html.parser.HTMLParser",
      "pythonVersion": "3.9.6",
      "convertCharRefs": true
    },
    "syntheticAssertions": {
      "count": 7,
      "results": {
        "hrefLookingTextIgnored": true,
        "entityEncodedLocalHrefDecodedAndNormalized": true,
        "absoluteOffsiteHrefIgnored": true,
        "protocolRelativeOffsiteHrefIgnored": true,
        "doubleQuotedHrefCollected": true,
        "singleQuotedHrefCollected": true,
        "unquotedHrefCollected": true
      }
    },
    "boundContentSha": "5dcf84bd0bebf8e6297822d0435e737bb7b4e2ed",
    "families": {
      "superpowers/**": {
        "sourceCount": 135,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "internal/**": {
        "sourceCount": 4,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "epics/**": {
        "sourceCount": 1,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "guide/onboarding.md": {
        "sourceCount": 0,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "README.md": {
        "sourceCount": 1,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      },
      "business-use-cases.md": {
        "sourceCount": 1,
        "routeAssetCount": 0,
        "manifestCount": 0,
        "hrefCount": 0
      }
    },
    "htmlFileCount": 43,
    "hrefCount": 4601,
    "requiredArtifacts": {
      "Home": {
        "file": "index.html",
        "exists": true,
        "markerCount": 2,
        "requiredMarkerCount": 2
      },
      "Guide": {
        "file": "guide/index.html",
        "exists": true,
        "markerCount": 2,
        "requiredMarkerCount": 2
      },
      "API": {
        "file": "guide/api-reference.html",
        "exists": true,
        "markerCount": 2,
        "requiredMarkerCount": 2
      }
    }
  }
  ```

  This fenced JSON is the sole authoritative Task 2 containment and href
  result. It is local build evidence only, not a live Pages, CNAME,
  deployment, or release receipt, and it does not complete a release-pending
  row.

- After the final `npm run format`, `npm run lint`, and `npm run build` pass,
  an independent verifier extracted and reran the exact fenced command and
  compared stdout to the fenced JSON byte-for-byte. The command exited 0;
  both values were 1,831 bytes with SHA-256
  `7d2e9d27e69789149eafb99956a4608c391d5cce71aff71e19ad9942cbd0d44e`, and
  `byteIdentical` was `true`. Only generated `.vitepress/.temp` was removed
  afterward.

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

- Capture timestamp: 2026-09-01T14:02:47Z.
- Executor: `OpenAI Codex task agent task3_wp1_policy`.
- Task start HEAD:
  `e6441a86965735584c8bb31ceee8c1f115c243d5`; it equaled
  `origin/spec-docs-refresh` after fetch. `origin/rc/202608` equaled
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`, and that audited target was an
  ancestor of the task start.
- Approved path list: `docs/public/CNAME`, `docs/business-use-cases.md`,
  `fastly.toml`, `docs/package.json`, `docs/package-lock.json`, `CLAUDE.md`,
  `AGENTS.md`, `.github/pull_request_template.md`,
  `.claude/commands/check-ci.md`, `.claude/commands/review-changes.md`,
  `.claude/commands/test-all.md`, `.claude/commands/test-crate.md`,
  `.claude/commands/verify.md`, this evidence record, and
  `docs/internal/audits/documentation-refresh-decisions.md`.
- Failing pre-change proof: an inline Node policy assertion at the task start
  checked the business-use-case warning, package metadata, Fastly authors and
  fixture comments, removal of the obsolete script reference, canonical gate
  consumers, generated fallback equality, typed exception policy, CNAME
  deletion, and retained project-path base. It exited 1 with 27 failures out of
  28 assertions; only the existing `base: '/trusted-server'` assertion passed.

##### CNAME deletion commit

- Minimal change: deleted only `docs/public/CNAME`; the VitePress base remained
  `/trusted-server`.
- `npm run build`: passed from `docs`; VitePress completed in 4.87 seconds with
  only the known non-failing `vcl` syntax-highlighting fallback.
- Focused green proof: the source and built CNAME were absent; Home contained
  both project-path `href` and `src` values; no built HTML used a root
  `/assets/` URL; and no active, non-historical tracked file contained the
  deleted placeholder domain.
- Exact staged name-status from task start:
  `D docs/public/CNAME`. `git diff --cached --check` passed, the unstaged
  tracked-byte check passed, and the non-ignored untracked-path check passed
  after removing only generated `docs/.vitepress/.temp`.
- Commit: `5a7b389dae483bafdc189dd434f98a92b2305b6a` —
  `Resolve documentation site domain`.

##### Policy and hygiene commit

- Package start HEAD:
  `5a7b389dae483bafdc189dd434f98a92b2305b6a`.
- Minimal change: added the source-level unverified-planning warning without
  rewriting marketing copy; emptied Fastly authors; labeled the four KV stores
  and key material as local test fixtures; removed the obsolete local-script
  reference; retained the service ID only under its owned, expiring decision;
  made the docs package private and Apache-2.0; added the exact approved
  sensitive-data taxonomy; converted gate consumers to the canonical link;
  corrected `tracing` to `log`; and added the marked AGENTS fallback region.
- `npm install --package-lock-only --ignore-scripts`: passed and refreshed the
  root package license plus npm's current peer metadata. npm intentionally does
  not copy the package's `private` field into lockfile v3.
- Generated fallback proof: the inline gate-region generator read the
  `CLAUDE.md#ci-gates` body and replaced only the marked AGENTS region. Two
  successive runs reported `changed: false` and the same SHA-256,
  `b56a60860159e41fbd97ae4a2a8c34a4cccf24227064b8bc53075354cef45e50`.
- Focused green policy assertion: the corrected inline Node assertion exited 0
  with 30 of 30 checks passing. It also compared the generated AGENTS body
  byte-for-byte with the canonical CLAUDE body.
- Tracked-file privacy proof: an inline Node scan read all 689 paths returned by
  `git ls-files -z`. Removed contacts, handles, internal-channel and access
  phrases had zero occurrences. The only controlled values were the service ID
  in `fastly.toml` and the deleted placeholder literal in the approved design
  audit plus its narrow decision row. Both exceptions had an owner, rationale,
  allowed type, exact scope, and future expiry; no broad exception shape was
  present.
- Documentation regression command:
  `cd docs && npm ci && npm run lint && npm run format && npm run build` passed.
  VitePress built 43 HTML files in 5.13 seconds with only the known non-failing
  `vcl` fallback.
- Artifact proof used Python's `HTMLParser` to inspect actual `href` and `src`
  attributes. The corrected assertion found 3,425 local URLs and required all
  of them to use `/trusted-server/`. Home, Guide, and API Reference HTML plus
  two page assets each were present. `superpowers/**`, `internal/**`,
  `epics/**`, `guide/onboarding.md`, `README.md`, and
  `business-use-cases.md` had no route, page asset, or local URL. The source and
  output CNAME were absent. The first artifact run used the guessed marker
  `Trusted Server Guide` and failed only that assertion; source inspection
  showed the approved H1 is `Guide`, and the corrected exact H1 assertion
  passed with no violations.
- Final checkpoint hygiene: the first post-evidence regression invocation
  stopped at lint because the preceding build had recreated the generated
  `docs/.vitepress/.temp` tree. The errors were confined to generated VitePress
  JavaScript. After removing only that generated directory, the exact
  `npm ci && npm run lint && npm run format && npm run build` sequence passed;
  the final full-sequence build completed in 5.41 seconds. The corrected
  artifact proof then repeated with the same counts and no violations.
- Exact staged name-status from policy-package start: 14 `M` rows, exactly the
  policy package's approved paths and no others. `git diff --cached --check`,
  the repository-wide unstaged tracked-byte check, and the non-ignored
  untracked-path check all passed. This evidence mutation was then restaged and
  those checks repeated before commit.
- Active exceptions: service ID, owner `aram356`, rationale recorded above,
  expiry `2026-09-30T00:00:00Z`; historical placeholder example, owner
  `aram356`, rationale restricted to preserving approved audit evidence,
  expiry `2027-08-31T00:00:00Z`.
- Live Pages, deployed canonical URLs, and observed CNAME behavior remain
  `release-pending`. No local result in this package completes that row.
- Enclosing policy commit message: `Clean documentation publishing policy`.
  Its SHA and push result are reported in the execution handoff because a
  commit cannot contain its own identifier.

##### Authoritative Task 3 evidence correction

- Correction timestamp: 2026-09-01T15:05:38Z.
- Correction: the earlier Task 3 policy, privacy, and artifact aggregates are
  non-authoritative because their executable predicates and raw output were not
  retained, and the artifact aggregate omitted 28 relative local references.
  This correction supersedes those aggregates without changing their historical
  record. The command and JSON below are the sole authoritative Task 3 gate
  evidence.
- Binding: the command fails unless all Task 3 policy and documentation source
  paths, excluding this append-only evidence ledger, remain byte-equivalent to
  `b4a99c583f62d9d053ffb1c7073b6dd8e96c36c1` in HEAD, index, and working tree.
- Semantics: actual entity-decoded `href` and `src` attributes are resolved
  against each emitted page at a synthetic local origin. Every local reference
  is checked for project-path containment, excluded-family absence, and an
  existing target artifact. Normalized URL origins use lower-case schemes and
  hostnames plus effective default ports; user information is origin-irrelevant,
  while malformed hosts or ports fail closed. Genuine off-origin HTTP(S) and
  non-web-scheme references are ignored and counted; focused synthetics require
  entity-decoded relative and normalized same-origin exclusion detection,
  project-path escape detection, default-port and user-information handling,
  non-default-port offsite handling, and inert offsite, non-web, and code text.
- Exact executable command:

```sh
python3 - <<'PY'
from __future__ import annotations

import json
import posixpath
import re
import subprocess
from datetime import datetime, timezone
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urljoin, urlsplit


BOUND_SHA = "b4a99c583f62d9d053ffb1c7073b6dd8e96c36c1"
LOCAL_ORIGIN = "https://docs.local.invalid"
PROJECT_PREFIX = "/trusted-server/"
REPOSITORY_ROOT = Path.cwd()
DIST_ROOT = REPOSITORY_ROOT / "docs/.vitepress/dist"
EVIDENCE_PATH = "docs/internal/audits/documentation-refresh-evidence.md"


def run_git(arguments: list[str]) -> bytes:
    return subprocess.run(
        ["git", *arguments],
        check=True,
        cwd=REPOSITORY_ROOT,
        stdout=subprocess.PIPE,
    ).stdout


bound_pathspecs = [
    ".claude/commands/check-ci.md",
    ".claude/commands/review-changes.md",
    ".claude/commands/test-all.md",
    ".claude/commands/test-crate.md",
    ".claude/commands/verify.md",
    ".github/pull_request_template.md",
    "AGENTS.md",
    "CLAUDE.md",
    "fastly.toml",
    "docs",
    f":(exclude){EVIDENCE_PATH}",
]
run_git(["cat-file", "-e", f"{BOUND_SHA}^{{commit}}"])
run_git(["diff", "--quiet", BOUND_SHA, "HEAD", "--", *bound_pathspecs])
run_git(["diff", "--quiet", "--", *bound_pathspecs])
run_git(["diff", "--cached", "--quiet", "--", *bound_pathspecs])


def read_text(path: str) -> str:
    return (REPOSITORY_ROOT / path).read_text(encoding="utf-8")


business = read_text("docs/business-use-cases.md")
package = json.loads(read_text("docs/package.json"))
package_lock = json.loads(read_text("docs/package-lock.json"))
fastly = read_text("fastly.toml")
claude = read_text("CLAUDE.md")
agents = read_text("AGENTS.md")
template = read_text(".github/pull_request_template.md")
decisions = read_text("docs/internal/audits/documentation-refresh-decisions.md")
gate_link = "[canonical CI gate list](/CLAUDE.md#ci-gates)"
canonical = claude.split("## CI Gates\n\n", 1)[1].split("\n\n---", 1)[0].strip()
generated_begin = "<!-- BEGIN GENERATED CI GATES: source CLAUDE.md#ci-gates -->"
generated_end = "<!-- END GENERATED CI GATES -->"
generated = agents.split(generated_begin, 1)[1].split(generated_end, 1)[0].strip()
policy_region = claude.split("## Other guidelines", 1)[1].split(
    "## Git Commit Conventions", 1
)[0]

policy_predicates = {
    "business warning": "**Unverified planning material.**" in business,
    "package private": package.get("private") is True,
    "package license": package.get("license") == "Apache-2.0",
    "lock license": package_lock["packages"][""]["license"] == "Apache-2.0",
    "empty authors": re.search(r"^authors = \[\]$", fastly, re.MULTILINE)
    is not None,
    "counter KV comment": "# Local test fixture for the counter KV store."
    in fastly,
    "creative KV comment": "# Local test fixture for the creative KV store."
    in fastly,
    "EC identity KV comment": "# Local test fixture for the EC identity KV store."
    in fastly,
    "consent KV comment": "# Local test fixture for the consent KV store."
    in fastly,
    "obsolete script absent from fastly": "test-prebid-eids.sh" not in fastly,
    "service owner": "owner `aram356`" in fastly,
    "service expiry": "2026-09-30T00:00:00Z" in fastly,
    "template gate link": gate_link in template,
    "template log terminology": "Uses `log` macros" in template
    and "Uses `tracing` macros" not in template,
    ".claude/commands/check-ci.md gate link": gate_link
    in read_text(".claude/commands/check-ci.md"),
    ".claude/commands/review-changes.md gate link": gate_link
    in read_text(".claude/commands/review-changes.md"),
    ".claude/commands/test-all.md gate link": gate_link
    in read_text(".claude/commands/test-all.md"),
    ".claude/commands/test-crate.md gate link": gate_link
    in read_text(".claude/commands/test-crate.md"),
    ".claude/commands/verify.md gate link": gate_link
    in read_text(".claude/commands/verify.md"),
    "generated markers": generated_begin in agents and generated_end in agents,
    "generated equality": generated == canonical,
    "type vendor URL": "vendor URL" in policy_region,
    "type hash-pinned fake-credential fixture": "hash-pinned fake-credential fixture"
    in policy_region,
    "type historical example": "historical example" in policy_region,
    "type service ID": "service ID" in policy_region,
    "type project-owned public domain": "project-owned public domain"
    in policy_region,
    "exception fields": all(
        field in policy_region.lower()
        for field in ("owner", "rationale", "expiry timestamp")
    ),
    "historical decision": "These are the only two active WP1 sensitive-data exceptions."
    in decisions
    and "2027-08-31T00:00:00Z" in decisions,
    "source CNAME absent": not (REPOSITORY_ROOT / "docs/public/CNAME").exists(),
    "project base": "base: '/trusted-server'"
    in read_text("docs/.vitepress/config.mts"),
}
if len(policy_predicates) != 30:
    raise RuntimeError(
        f"policy predicate count changed: expected 30, got {len(policy_predicates)}"
    )
failed_policy = sorted(name for name, passed in policy_predicates.items() if not passed)
if failed_policy:
    raise RuntimeError(f"policy predicates failed: {failed_policy}")


tracked_paths = run_git(["ls-files", "-z"]).split(b"\0")
tracked_paths = [path.decode("utf-8") for path in tracked_paths if path]
tracked_bytes = {
    path: (REPOSITORY_ROOT / path).read_bytes() for path in tracked_paths
}


def occurrences(value: str) -> list[str]:
    needle = value.encode("utf-8")
    found: list[str] = []
    for path, content in tracked_bytes.items():
        found.extend([path] * content.count(needle))
    return sorted(found)


removed_terms = {
    "personal email": "jason" + "@stackpop.com",
    "project-lead handle": "@jev" + "ansnyc",
    "developer handle": "@Christian" + "Pavilonis",
    "internal channel": "#trusted-server-" + "internal",
    "manager or buddy access direction": "Ask your manager or onboarding "
    + "buddy",
    "GitHub access direction": "Get GitHub " + "access to",
    "project-board access direction": "Get access to the [Trusted Server "
    + "project board]",
    "Slack access direction": "Join the Slack " + "workspace",
    "calendar access direction": "Get calendar invites for Task " + "Force",
    "internal standup reference": "Development Team " + "Standup",
}
privacy_term_results = {
    name: len(occurrences(value)) for name, value in removed_terms.items()
}
privacy_violations = [
    f"{name}: expected zero occurrences, got {count}"
    for name, count in privacy_term_results.items()
    if count != 0
]

historical_value = "your" + "-custom-domain.com"
historical_scope = sorted(
    [
        "docs/internal/audits/documentation-refresh-decisions.md",
        "docs/superpowers/specs/2026-08-19-documentation-refresh-design.md",
    ]
)
historical_occurrences = occurrences(historical_value)
if historical_occurrences != historical_scope:
    privacy_violations.append(
        "historical example scope: "
        f"expected {historical_scope}, got {historical_occurrences}"
    )

service_value = "dysUw6h73Vze" + "omD61eal85"
service_scope = ["fastly.toml"]
service_occurrences = occurrences(service_value)
if service_occurrences != service_scope:
    privacy_violations.append(
        f"service ID scope: expected {service_scope}, got {service_occurrences}"
    )

now = datetime.now(timezone.utc)
for exception_type, expiry in (
    ("service ID", "2026-09-30T00:00:00Z"),
    ("historical example", "2027-08-31T00:00:00Z"),
):
    expiry_time = datetime.fromisoformat(expiry.replace("Z", "+00:00"))
    if expiry not in decisions or now >= expiry_time:
        privacy_violations.append(f"{exception_type}: missing or expired {expiry}")
if "Owner: `aram356`." not in decisions:
    privacy_violations.append("historical example: missing owner")
if "Rationale: preserve the approved audit" not in decisions:
    privacy_violations.append("historical example: missing rationale")
if "These are the only two active WP1 sensitive-data exceptions." not in decisions:
    privacy_violations.append("active WP1 exception set is not exact")
if re.search(r"allowed types[^\n]*\ball\b", decisions, re.IGNORECASE) or re.search(
    r"(?:domain|credential)[^\n]*\*", decisions, re.IGNORECASE
):
    privacy_violations.append("broad exception shape detected")
if privacy_violations:
    raise RuntimeError(f"privacy predicates failed: {sorted(privacy_violations)}")


EXCLUDED_PREFIXES = ("superpowers/", "internal/", "epics/")
EXCLUDED_EXACT = {
    "guide/onboarding",
    "guide/onboarding.html",
    "readme",
    "readme.html",
    "business-use-cases",
    "business-use-cases.html",
}
EXCLUDED_ASSET_MARKERS = (
    "superpowers_",
    "internal_",
    "epics_",
    "guide_onboarding.md",
    "readme.md",
    "business-use-cases.md",
)
REQUIRED_HTML = {
    "index.html": "<title>Trusted Server</title>",
    "guide/index.html": '<h1 id="guide"',
    "guide/api-reference.html": '<h1 id="api-reference"',
}
REQUIRED_ASSETS = {
    "index.md.": 2,
    "guide_index.md.": 2,
    "guide_api-reference.md.": 2,
}


def normalize_local_path(path: str) -> str:
    decoded = unquote(path)
    if "\\" in decoded or "\x00" in decoded:
        raise ValueError(f"unsafe local URL path: {path!r}")
    normalized = posixpath.normpath(decoded)
    if decoded.endswith("/") and not normalized.endswith("/"):
        normalized += "/"
    return normalized


def excluded_route(route: str) -> bool:
    folded = route.casefold()
    return folded in EXCLUDED_EXACT or any(
        folded.startswith(prefix) for prefix in EXCLUDED_PREFIXES
    )


def artifact_candidates(local_path: str) -> list[Path]:
    route = local_path.removeprefix(PROJECT_PREFIX)
    if not route or route.endswith("/"):
        return [DIST_ROOT / route / "index.html"]
    direct = DIST_ROOT / route
    candidates = [direct]
    if direct.suffix == "":
        candidates.extend([direct.with_suffix(".html"), direct / "index.html"])
    return candidates


class AttributeParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.references: list[tuple[str, str, str]] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        for name, value in attrs:
            if name in {"href", "src"} and value is not None:
                self.references.append((tag, name, value))


def normalized_origin(parts) -> tuple[str, str, int]:
    scheme = parts.scheme.lower()
    hostname = parts.hostname
    if hostname is None:
        raise ValueError("web URL has no hostname")
    hostname = hostname.lower()
    port = parts.port
    if port is None:
        port = 443 if scheme == "https" else 80
    return (scheme, hostname, port)


LOCAL_ORIGIN_TUPLE = normalized_origin(urlsplit(LOCAL_ORIGIN))


def inspect_references(
    page_relative: str,
    html: str,
    *,
    require_targets: bool,
) -> dict[str, object]:
    parser = AttributeParser()
    parser.feed(html)
    parser.close()
    page_url = f"{LOCAL_ORIGIN}{PROJECT_PREFIX}{page_relative}"
    counts = {
        "projectAbsolute": 0,
        "relativeArtifact": 0,
        "sameDocument": 0,
        "absoluteSameOrigin": 0,
        "offOrigin": 0,
        "nonWebScheme": 0,
    }
    violations: list[str] = []
    for tag, name, raw_value in parser.references:
        value = raw_value.strip()
        try:
            raw_parts = urlsplit(value)
            resolved = urlsplit(urljoin(page_url, value))
        except ValueError as error:
            violations.append(f"{page_relative}: {tag}[{name}] malformed URL: {error}")
            continue
        if resolved.scheme not in {"http", "https"}:
            counts["nonWebScheme"] += 1
            continue
        try:
            resolved_origin = normalized_origin(resolved)
        except ValueError as error:
            violations.append(f"{page_relative}: {tag}[{name}] malformed origin: {error}")
            continue
        if resolved_origin != LOCAL_ORIGIN_TUPLE:
            counts["offOrigin"] += 1
            continue
        try:
            local_path = normalize_local_path(resolved.path)
        except ValueError as error:
            violations.append(f"{page_relative}: {tag}[{name}] {error}")
            continue
        if not local_path.startswith(PROJECT_PREFIX):
            violations.append(
                f"{page_relative}: {tag}[{name}] escapes project path: {value!r}"
            )
            continue
        route = local_path.removeprefix(PROJECT_PREFIX)
        if excluded_route(route):
            violations.append(
                f"{page_relative}: {tag}[{name}] resolves to excluded route: {route!r}"
            )
        same_document = value == "" or value.startswith(("#", "?"))
        if same_document:
            counts["sameDocument"] += 1
        elif value.startswith("/"):
            counts["projectAbsolute"] += 1
        elif raw_parts.scheme:
            counts["absoluteSameOrigin"] += 1
        else:
            counts["relativeArtifact"] += 1
        if require_targets and not any(
            candidate.is_file() for candidate in artifact_candidates(local_path)
        ):
            violations.append(
                f"{page_relative}: {tag}[{name}] target artifact missing: {value!r}"
            )
    return {"counts": counts, "violations": sorted(set(violations))}


synthetic_cases = {
    "relative excluded decoded": (
        '<a href="../Int&#x65;rnal/private.html">x</a>',
        "excluded route",
    ),
    "relative project escape": (
        '<img src="../../../escape.png">',
        "escapes project path",
    ),
    "default port same-origin excluded decoded": (
        '<a href="HTTPS://DOCS.LOCAL.INVALID:443/trusted-server/int&#x65;rnal/private.html">x</a>',
        "excluded route",
    ),
    "userinfo same-origin excluded": (
        '<a href="https://user:secret@docs.local.invalid/trusted-server/internal/private.html">x</a>',
        "excluded route",
    ),
}
synthetic_results: dict[str, bool] = {}
for name, (html, expected_diagnostic) in synthetic_cases.items():
    result = inspect_references(
        "guide/example.html", html, require_targets=False
    )
    synthetic_results[name] = any(
        expected_diagnostic in violation for violation in result["violations"]
    )
offsite_result = inspect_references(
    "guide/example.html",
    '<code>&lt;a href="../internal/private.html"&gt;</code>'
    '<a href="https://offsite.example/internal/private.html">offsite</a>',
    require_targets=False,
)
synthetic_results["offsite ignored"] = (
    offsite_result["counts"]["offOrigin"] == 1
    and offsite_result["violations"] == []
)
nondefault_result = inspect_references(
    "guide/example.html",
    '<a href="https://docs.local.invalid:444/trusted-server/internal/private.html">x</a>',
    require_targets=False,
)
synthetic_results["non-default port offsite ignored"] = (
    nondefault_result["counts"]["offOrigin"] == 1
    and nondefault_result["violations"] == []
)
nonweb_result = inspect_references(
    "guide/example.html",
    '<code>&lt;a href="../internal/private.html"&gt;</code>'
    '<a href="mailto:reviewer@example.test">mail</a>',
    require_targets=False,
)
synthetic_results["non-web scheme ignored and inert"] = (
    nonweb_result["counts"]["nonWebScheme"] == 1
    and nonweb_result["violations"] == []
)
if not all(synthetic_results.values()):
    raise RuntimeError(f"artifact synthetics failed: {synthetic_results}")

artifact_violations: list[str] = []
artifact_files = sorted(path for path in DIST_ROOT.rglob("*") if path.is_file())
artifact_relatives = [path.relative_to(DIST_ROOT).as_posix() for path in artifact_files]
for relative in artifact_relatives:
    folded_relative = relative.casefold()
    if (
        folded_relative == "cname"
        or any(folded_relative.startswith(prefix) for prefix in EXCLUDED_PREFIXES)
        or folded_relative in EXCLUDED_EXACT
    ):
        artifact_violations.append(f"excluded artifact: {relative}")
    if relative.startswith("assets/") and any(
        marker in folded_relative for marker in EXCLUDED_ASSET_MARKERS
    ):
        artifact_violations.append(f"excluded page asset: {relative}")

required_asset_counts: dict[str, int] = {}
for relative, marker in REQUIRED_HTML.items():
    path = DIST_ROOT / relative
    if not path.is_file() or marker not in path.read_text(encoding="utf-8"):
        artifact_violations.append(f"missing required marker: {relative}: {marker}")
for marker, expected in REQUIRED_ASSETS.items():
    count = sum(
        relative.startswith(f"assets/{marker}") and relative.endswith(".js")
        for relative in artifact_relatives
    )
    required_asset_counts[marker] = count
    if count != expected:
        artifact_violations.append(
            f"required asset count {marker!r}: expected {expected}, got {count}"
        )

aggregate_counts = {
    "projectAbsolute": 0,
    "relativeArtifact": 0,
    "sameDocument": 0,
    "absoluteSameOrigin": 0,
    "offOrigin": 0,
    "nonWebScheme": 0,
}
html_count = 0
for path in artifact_files:
    if path.suffix != ".html":
        continue
    html_count += 1
    relative = path.relative_to(DIST_ROOT).as_posix()
    result = inspect_references(
        relative, path.read_text(encoding="utf-8"), require_targets=True
    )
    for name, count in result["counts"].items():
        aggregate_counts[name] += count
    artifact_violations.extend(result["violations"])

local_artifact_references = (
    aggregate_counts["projectAbsolute"]
    + aggregate_counts["relativeArtifact"]
    + aggregate_counts["absoluteSameOrigin"]
)
expected_counts = {
    "projectAbsolute": 3425,
    "relativeArtifact": 28,
    "absoluteSameOrigin": 0,
    "localArtifactReferences": 3453,
}
actual_counts = {
    "projectAbsolute": aggregate_counts["projectAbsolute"],
    "relativeArtifact": aggregate_counts["relativeArtifact"],
    "absoluteSameOrigin": aggregate_counts["absoluteSameOrigin"],
    "localArtifactReferences": local_artifact_references,
}
if actual_counts != expected_counts:
    artifact_violations.append(
        f"bound artifact counts changed: expected {expected_counts}, got {actual_counts}"
    )
if (REPOSITORY_ROOT / "docs/public/CNAME").exists():
    artifact_violations.append("source CNAME exists")
if artifact_violations:
    raise RuntimeError(
        "artifact predicates failed:\n" + "\n".join(sorted(set(artifact_violations)))
    )

output = {
    "boundSourceSha": BOUND_SHA,
    "policy": {
        "predicateCount": len(policy_predicates),
        "predicates": policy_predicates,
        "violations": failed_policy,
    },
    "privacy": {
        "trackedFilesScanned": len(tracked_paths),
        "searchedTermClasses": privacy_term_results,
        "activeExceptions": [
            {
                "type": "service ID",
                "paths": service_scope,
                "occurrences": len(service_occurrences),
                "owner": "aram356",
                "expiry": "2026-09-30T00:00:00Z",
            },
            {
                "type": "historical example",
                "paths": historical_scope,
                "occurrences": len(historical_occurrences),
                "owner": "aram356",
                "expiry": "2027-08-31T00:00:00Z",
            },
        ],
        "violations": sorted(privacy_violations),
    },
    "artifacts": {
        "htmlFiles": html_count,
        "requiredHtml": sorted(REQUIRED_HTML),
        "requiredAssetCounts": required_asset_counts,
        "excludedFamilies": [
            "superpowers/**",
            "internal/**",
            "epics/**",
            "guide/onboarding.md",
            "README.md",
            "business-use-cases.md",
        ],
        "referenceCounts": {
            **aggregate_counts,
            "localArtifactReferences": local_artifact_references,
        },
        "synthetics": synthetic_results,
        "violations": sorted(set(artifact_violations)),
    },
}
print(json.dumps(output, indent=2, sort_keys=True))
PY
```

- Verbatim stdout:

```text
{
  "artifacts": {
    "excludedFamilies": [
      "superpowers/**",
      "internal/**",
      "epics/**",
      "guide/onboarding.md",
      "README.md",
      "business-use-cases.md"
    ],
    "htmlFiles": 43,
    "referenceCounts": {
      "absoluteSameOrigin": 0,
      "localArtifactReferences": 3453,
      "nonWebScheme": 0,
      "offOrigin": 115,
      "projectAbsolute": 3425,
      "relativeArtifact": 28,
      "sameDocument": 1077
    },
    "requiredAssetCounts": {
      "guide_api-reference.md.": 2,
      "guide_index.md.": 2,
      "index.md.": 2
    },
    "requiredHtml": [
      "guide/api-reference.html",
      "guide/index.html",
      "index.html"
    ],
    "synthetics": {
      "default port same-origin excluded decoded": true,
      "non-default port offsite ignored": true,
      "non-web scheme ignored and inert": true,
      "offsite ignored": true,
      "relative excluded decoded": true,
      "relative project escape": true,
      "userinfo same-origin excluded": true
    },
    "violations": []
  },
  "boundSourceSha": "b4a99c583f62d9d053ffb1c7073b6dd8e96c36c1",
  "policy": {
    "predicateCount": 30,
    "predicates": {
      ".claude/commands/check-ci.md gate link": true,
      ".claude/commands/review-changes.md gate link": true,
      ".claude/commands/test-all.md gate link": true,
      ".claude/commands/test-crate.md gate link": true,
      ".claude/commands/verify.md gate link": true,
      "EC identity KV comment": true,
      "business warning": true,
      "consent KV comment": true,
      "counter KV comment": true,
      "creative KV comment": true,
      "empty authors": true,
      "exception fields": true,
      "generated equality": true,
      "generated markers": true,
      "historical decision": true,
      "lock license": true,
      "obsolete script absent from fastly": true,
      "package license": true,
      "package private": true,
      "project base": true,
      "service expiry": true,
      "service owner": true,
      "source CNAME absent": true,
      "template gate link": true,
      "template log terminology": true,
      "type hash-pinned fake-credential fixture": true,
      "type historical example": true,
      "type project-owned public domain": true,
      "type service ID": true,
      "type vendor URL": true
    },
    "violations": []
  },
  "privacy": {
    "activeExceptions": [
      {
        "expiry": "2026-09-30T00:00:00Z",
        "occurrences": 1,
        "owner": "aram356",
        "paths": [
          "fastly.toml"
        ],
        "type": "service ID"
      },
      {
        "expiry": "2027-08-31T00:00:00Z",
        "occurrences": 2,
        "owner": "aram356",
        "paths": [
          "docs/internal/audits/documentation-refresh-decisions.md",
          "docs/superpowers/specs/2026-08-19-documentation-refresh-design.md"
        ],
        "type": "historical example"
      }
    ],
    "searchedTermClasses": {
      "GitHub access direction": 0,
      "Slack access direction": 0,
      "calendar access direction": 0,
      "developer handle": 0,
      "internal channel": 0,
      "internal standup reference": 0,
      "manager or buddy access direction": 0,
      "personal email": 0,
      "project-board access direction": 0,
      "project-lead handle": 0
    },
    "trackedFilesScanned": 689,
    "violations": []
  }
}
```

- Result: 30 of 30 named policy predicates passed. The byte scan covered all
  689 tracked files, all ten named removed-value classes had zero occurrences,
  and the two active exceptions matched their exact one-path and two-path
  scopes. The semantic artifact scan resolved 3,425 project-absolute and 28
  relative artifact references, for 3,453 local artifact references; it also
  resolved 1,077 same-document references. All required and excluded artifact,
  target-existence, project-path, CNAME, and synthetic predicates passed with
  zero violations. Live Pages and observed CNAME behavior remain
  `release-pending`.

#### Task 4 — Scaffold the standalone `docs-parity` crate

- Package start: `14ea4d99ffb726a75868f1ee1c74622f451f03d7` on
  `spec-docs-refresh`. The package-start fetch reasserted
  `origin/rc/202608` at
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`, confirmed that commit as an
  ancestor, and observed PR #1049 open and draft from `spec-docs-refresh` to
  `rc/202608`.
- Bootstrap RED: before a binary or library target existed,
  `cargo test --manifest-path tools/docs-parity/Cargo.toml --test cli` ran ten
  integration tests and failed all ten because the `docs-parity` binary was
  absent. Exact assertion:
  `should build the docs-parity binary for CLI integration tests`. The tests
  covered deterministic help, unknown
  subcommands, nested repository discovery, check/update exit codes and
  no-write checking, outside and unsafe relative paths, an interrupted atomic
  stage, and stable ordering. Adding the typed-governance contract before the
  library existed produced `E0433` for the absent `docs_parity` crate.
- Atomic and ordering GREEN: after the minimal implementation, the first run
  passed ten cases and exposed one defect on the second stable update: exit 2
  reported that the existing record was treated as an unsafe directory. Parent
  validation was corrected to begin at the containing directory; the focused
  stable-order test and the full then-eleven-test suite passed. The interruption
  fixture pre-created `.tracked-paths.txt.docs-parity.tmp`; update exited 2 and
  preserved the prior complete target bytes.
- Boundary RED/GREEN: `C:outside.txt` initially exited 0 instead of 2, and a
  tracked file below a world-writable intermediate directory initially exited
  0 instead of 2. Focused tests then passed after portable drive-relative path
  rejection and full existing-parent-chain validation were added. A tracked
  symlink to an internal regular file passed; an escaping symlink and unsafe
  tracked/output directory modes failed closed.
- Review correction from `4f7fe471a4945d8d239091b30d07b9667e97239e`:
  a dangling final output symlink initially made `update` exit 0 instead of 2
  because `try_exists` followed its missing target. The focused regression then
  passed after both check and update inspected the directory entry with
  `symlink_metadata`: each exited 2, both symlink entries and link targets were
  unchanged, and no dangling target was created.
- Quality hardening from `048ab3aa45f7deeb142ec3076e886eb3f80c7c4e`:
  the Git-admin RED check returned drift exit 1 instead of safety exit 2 for
  `.git/config`; after the fix, check and update both exited 2 and preserved the
  exact config bytes. The portable-name RED case `line\nbreak.txt` exited 0;
  the closed grammar then rejected controls, Windows-invalid characters,
  backslashes, trailing dots/spaces, case-insensitive `.git` components, and
  all reserved Windows device stems with extensions while continuing to allow
  `.gitignore` and `.gitmodules`. Repository roots ending in a space or carriage
  return failed when more than Git's single LF terminator was removed; stripping
  exactly that LF made both focused cases pass. Expiry probes covered leap day,
  year zero, and invalid month, day, hour, minute, and second values. Finally, a
  standalone Cargo check targeting `wasm32-wasip1` failed with the intended
  `docs-parity supports only Linux and macOS hosts` compile-time diagnostic,
  proving there is no unsupported-host fallback.
- Final device-name correction from
  `91bfa7f71ff554e8e6e71df781c471667a2390e9`: `COM¹.txt` initially exited 0
  instead of 2. The focused portable-name test passed after exact superscript
  `¹`, `²`, and `³` suffixes were recognized for case-insensitive `COM` and
  `LPT` stems, with and without extensions. Longer lookalikes such as
  `COM¹extra.txt` and `lpt³more.log` remained valid.
- Final focused commands:

  ```bash
  cargo test --manifest-path tools/docs-parity/Cargo.toml --test cli
  cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
  cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
  cargo run --manifest-path tools/docs-parity/Cargo.toml -- check
  ```

  Results: the integration command passed 19 tests, formatting and Clippy
  passed, and the README's absolute-manifest invocation passed from
  `docs/guide` without writing repository files.

- Workspace isolation: the standalone tool owns
  `tools/docs-parity/Cargo.lock`; the root `Cargo.lock` SHA-256 was
  `9bb34225c5b8d1da39c75c3a8143d905f4b7d228a8986dc93d7e58a4196b4bba`
  before and after the package. The tool lock SHA-256 was
  `f45722ba1c96ddc8095308407102deb8c1ca33a64d140c1382abf692e111d5e3`.
  No root workspace membership was added. Tasks 1-4 remain the approved
  classification-manifest bootstrap exception.

#### Task 5 — Close tracked-file classification and sensitive-data scanning

- Capture timestamp: 2026-09-01T17:13:39Z.
- Executor: `OpenAI Codex task agent task5_classification_scanner`.
- Package start HEAD:
  `d0e8399dddd87db7816eeba2c363b575c6e552ec`; the local and remote
  `spec-docs-refresh` tips matched at package start.
- Approved paths: the standalone tool manifest and lock, classification and
  scanner modules, their CLI/library/model/repository seams, the four
  governance manifests, classification and scanner integration tests, and
  this evidence section. The root Cargo manifest and lock were excluded.
- Classification RED: the first exhaustive integration run exited 101 because
  `classify` did not exist. The synthesized unknown-text, unknown-binary,
  invalid-UTF-8, oversized-text, Dockerfile, MJS, protobuf, unselected-comment,
  unsupported-grammar, symlink-escape, and unclassified-span cases then drove
  the closed manifest implementation. Separate update RED cases proved that a
  new or moved comment span and an inferred binary kind cannot retain review
  attestation.
- Scanner RED: the first 16-fixture integration run failed because `scan` did
  not exist. Subsequent focused RED runs covered all detector classes, exact
  expiry, stale fingerprints, renamed paths, broad scopes, retired-record
  shape, duplicate occurrences, moved byte scopes, project-host lookalikes,
  governance-field self-amplification, comment-bearing governance, sensitive
  governance prose, reviewed-bootstrap preservation, source-member/domain
  prefix truncation, RFC-reserved hosts, source-expression credentials, and
  URL substrings misread as email addresses. Each focused case passed after
  its minimal implementation change; the scanner integration target then
  passed 33 tests.
- Classification review: `git ls-files -z` produced 705 exact tracked paths:
  704 reviewed text files and one reviewed binary image. All 704 text paths
  have source records: 669 whole-file and 35 extracted-comment sources. The
  maintained set contains 645 explicit includes and 588 explicit typed
  excludes; the latter are 135 historical, 6 machine-data, 49
  non-documentation, 378 source-code, and 20 test-fixture dispositions. All
  564 extracted comment spans have exact reviewed dispositions. Agent and
  skill Markdown is whole-file included.
- Privacy review: the final scanner manifest contains 3,678 exact byte-scoped
  occurrences: 3,345 domains, 286 semantically parsed lockfile fields, 35
  credential-shaped literals, 11 binary strings, and one service ID. There
  are no repository email, encoded-token, media-metadata, or retired-plaintext
  findings. Class counts are 3,618 vendor URLs, 35 hash-pinned fake credential
  fixtures, 13 historical examples, 11 project-owned public domains, and one
  service ID.
- Credential disposition: all 35 exact literals were inspected at their
  source locations. Twenty-five are inside Rust test modules, one is an
  integration-test configuration, four are documentation secret-store key
  examples, three are historical test plans, and two are commented template
  key names. They are dummy literals, placeholder-rejection fixtures, an
  industry-standard published signing example, or key identifiers; none is a
  resolved production credential. Rust expressions and wrapper constructors
  are excluded by regression tests rather than allowlisted.
- Domain/email disposition: scheme URLs and boundary-terminated bare hosts
  were reviewed by exact path and normalized host. RFC-reserved `.example`,
  `.invalid`, `.test`, `.localhost`, and `example.com`/`example.net`/
  `example.org` hosts are synthetic and consume no exception. Member-name and
  suffix-prefix lookalikes fail detector fixtures. URL user-info and `@` image
  filenames do not become email findings. Project hosts use the
  `project_owned_public_domain` class; the deleted-CNAME audit has exactly two
  historical records, both owned by `aram356`, with the approved rationale and
  `2027-08-31T00:00:00Z` expiry.
- Governance: the allowlist supports only the five approved classes. Every
  record has exact path, detector, byte selector, content SHA-256, owner,
  rationale, and expiry. Derived governance fields are structurally parsed
  and not re-scanned as content; comments are rejected outside quoted TOML
  strings, and owner/rationale text is scanned directly and cannot be
  self-allowlisted. Ten reviewed retired identifier/access-phrase records are
  SHA-256-only; the manifest and checked-in tests contain no retired plaintext.
- Service exception: the sole record is scoped to `fastly.toml`, owned by
  `aram356`, and expires exactly at `2026-09-30T00:00:00Z`. A deterministic
  clock seam proves success immediately before and failure exactly at and
  after the boundary.
- Determinism and checks: two classification updates and two scanner
  bootstraps were byte-stable. Bootstrap preserves a complete reviewed exact
  set without writing, drops stale records, and reopens review for any new or
  moved finding. Check mode performs no writes. Focused classification and
  scanner commands, the full standalone suite, format, Clippy with warnings
  denied, `classify --check`, and `scan --check` passed. `git diff --check`,
  approved-path scope, generated no-diff, and tracked/untracked cleanliness
  checks passed before the enclosing commit.
- Workspace isolation: the root `Cargo.lock` SHA-256 remained
  `9bb34225c5b8d1da39c75c3a8143d905f4b7d228a8986dc93d7e58a4196b4bba`.
  The standalone lock contains only the scanner/classifier dependency closure.
- Semantic boundary: these mechanical detectors do not claim completeness for
  human semantic sensitivity. Content outside their named classes still
  requires reviewed human disposition.
- Enclosing commit subject: `Enforce documentation source classification`.
  Its SHA and push/PR receipt are reported in the execution handoff because a
  commit cannot contain its own identifier.

#### Task 5 review correction

- Review RED: two omitted classification-attestation tests failed with exit 0
  rather than 2; the omitted allowlist-attestation and service-ID-as-historical
  tests likewise failed with exit 0 rather than 2. The corrected target-form
  commands are recorded in the plan; the earlier filter-form wording is
  superseded.
- Review GREEN: the classification target passes 21 tests and the scanner
  target passes 37 tests. Comment records now use exact byte spans and content
  fingerprints; quote-aware fixtures cover trailing shell, TOML, YAML,
  JavaScript, and protobuf comments, string literals, and two block comments on
  one line. Lockfiles receive all non-domain detectors plus span-aware
  structured URL-field checks. A structured value after an identical
  description value selects the actual field bytes. Equal media-metadata and
  non-metadata binary values remain separate occurrences.
- The reviewed real manifests contain 704 text sources and 570 extracted
  comment spans. The reviewed scanner manifest contains 3,682 exact findings:
  3,348 domain, 286 structured lockfile field, 36 credential-shape, 11 binary
  string, and one service ID. Classes are 3,621 vendor URL, 36 hash-pinned fake
  credential fixture, 13 historical example, 11 project-owned public domain,
  and one service ID. Human semantic sensitivity remains a reviewed-disposition
  obligation, not a detector-completeness claim.
- Two post-review classification updates preserved maintained-manifest SHA-256
  `76b9912045566a3cf72b02bcdb14bce36fa062e02e8521710d686c8850597fd8`.
  Two post-review scanner bootstraps preserved allowlist SHA-256
  `0ba6ab33c455b0ef6d96eb4feb3de0552a5e8a3ce56196d8474c835faae1fa92`.

#### Task 5 strict re-review correction

- RED evidence: the generalized-domain boundary run passed 39 of 40 scanner
  tests and exposed a false positive in the source-member/boundary fixture.
  Before correction, binary and media service identifiers were surfaced only
  as storage detectors, non-string structured URL fields passed silently,
  retired governance lacked mandatory attestation, and stale comment records
  outside comment-mode sources were not closed over the manifest set.
- GREEN uses the standalone `psl` crate's compiled public-suffix data for
  deterministic offline modern, long, country-code, and punycode suffix
  recognition. Binary/media findings retain the semantic service detector.
  JSON and TOML lock fields reject unsupported shapes and duplicate keys and
  fingerprint the exact selected raw bytes. Grammar-specific fixtures cover
  JavaScript template interpolation, shell escapes, TOML multiline strings,
  and YAML scalar states. The operational contract and review workflow are
  documented in the tool README.
- The reviewed real manifests contain 704 text sources and 570 exact comment
  spans. The scanner manifest contains 4,856 exact findings: 4,520 domain,
  286 structured lockfile field, 36 credential-shape, 12 binary string, one
  email, and one service ID. Exception classes are 4,793 vendor URL, 36
  hash-pinned fake credential fixture, 15 historical example, 11
  project-owned public domain, and one service ID.
- False-positive review sampled Rust and TypeScript member expressions, Rust
  build-path strings, format-template URL fragments, HTML test fixtures,
  integration script literals, maintained guides, historical plans/specs, and
  generated lockfiles. Code-member/path/template candidates were removed;
  complete public-suffix bare domains and syntactically valid URL hosts remain
  exact reviewed occurrences.

#### Task 5 domain and grammar re-review correction

- The preceding 4,856/4,520 totals are retracted: they included path,
  source-member, Markdown code, and invalid URL-template false positives. RED
  samples included repository filenames, relative guide links, dotted Rust
  members, and a format placeholder mistaken for a URL host.
- URL findings now select and fingerprint only exact host bytes. Bare-domain
  recognition combines the compiled public-suffix authority with global
  repository-path, relative-path, Markdown code-span/fence, ignore-file, and
  source lexical context. A representative review covered root/internal/public
  Markdown, shell and ignore files, Rust/TypeScript, configuration examples,
  HTML fixtures, lockfiles, and the three previously missed modern-domain
  occurrences.
- The corrected reviewed inventory contains 3,265 exact findings: 2,929
  domain, 286 structured lockfile field, 36 credential-shape, 12 binary string,
  one email, and one service ID. Classes are 3,205 vendor URL, 36 hash-pinned
  fake credential fixture, 13 historical example, 10 project-owned public
  domain, and one service ID. Maintained classification has 704 sources and
  569 exact comment spans after removing one quote-state false extraction.
- Span-aware TOML tests cover quoted and dotted keys and inline tables; JSON
  and TOML reject unsupported or ambiguous value shapes. Multiline shell/YAML
  quote state, nested JavaScript templates, and unterminated lexical states are
  fail-closed. The plan and README now list the exact targets, full standalone
  suite, review flows, and authorized README staging scope.

#### Task 5 parser re-review correction

- Domain RED: the repository-path/markup fixture expected five findings but
  the prior categorical suffix and Markdown suppression emitted one. The
  persistent source-context fixture expected two literal/comment findings but
  emitted none. A separate documented-member fixture emitted three findings
  instead of the single real host. GREEN replaces those categories with a
  repository-aware context: full path tokens must resolve to tracked,
  repository-rooted, current-relative, basename/suffix, or repository-anchored
  path evidence; source members are derived from persistent Rust/JavaScript
  lexical regions. Markup alone never suppresses a public-suffix host.
- Domain selectors and fingerprints cover the exact original host bytes for
  URL and bare-host findings, including case. The reviewed 3,484-domain set had
  zero selector/fingerprint mismatches. All three pre-existing project-host
  occurrences are present. Representative path/member negatives are absent:
  root README and contribution files, build/configuration/getting-started
  paths, the two named nested settings/member chains, document-body access,
  and result containment calls.
- TOML RED: literal-quoted structural keys produced no finding under the
  handwritten token loop. GREEN removes that loop and traverses immutable
  `toml_edit` syntax trees through decoded tables, arrays of tables, dotted
  keys, and nested inline tables. Allowed fields require string scalars and map
  the syntax-tree value span to exact raw content bytes. Tests cover basic and
  literal quoted keys, dotted/quoted keys, nested inline tables, arrays of
  tables, comments/string decoys, non-string/container and duplicate failures,
  escapes, and an identical earlier description value.
- Grammar RED: an unterminated TOML basic string passed classification, while
  a JavaScript regular-expression character class inside nested template
  interpolation became a false block-comment span. GREEN uses one persistent
  state-machine pass per grammar for both extraction and EOF validation. TOML
  covers basic/literal and multiline forms; JavaScript covers nested
  templates/interpolations, strings, comments, and regular expressions. Shell,
  YAML, Rust raw strings/nested blocks, protobuf C-style comments, and Markdown
  comments likewise fail closed on unsupported or unterminated state.
- Final reviewed inventory before this evidence append contains 3,820 exact
  findings: 3,484 domain, 286 structured lockfile field, 36 credential-shape,
  12 binary string, one email, and one service ID. Classes are 3,757 vendor
  URL, 36 hash-pinned fake credential fixture, 15 historical example, 11
  project-owned public domain, and one service ID. The two additional
  historical records are the exact deleted-CNAME occurrences newly visible
  after categorical Markdown suppression was removed.
- Classification remains 704 sources and 569 exact comment spans; the unified
  lexer changed no reviewed real selector or disposition. Two classification
  updates and two scanner bootstraps were byte-stable at manifest SHA-256
  values `4734c0925182fffe79245344738fe3543d7408b71d16a20b9a45f8f6f984a266`
  and `a1e86dacd141cd7180a6666327ae89536b0ffd49b58f4d17791aa8022b50239f`.
  The root lock remained
  `9bb34225c5b8d1da39c75c3a8143d905f4b7d228a8986dc93d7e58a4196b4bba`;
  `toml_edit` is isolated to the standalone tool lock.
- Fresh pre-evidence gates passed: classification 29, scanner 55, CLI 19, and
  the complete 104-test standalone suite, with zero failures. Format and
  all-target/all-feature Clippy with warnings denied are re-run after this
  evidence is scanned and restaged.

#### Task 5 final independent-review correction

- The independent RED pass added ten reproductions. Lockfiles lost domains in
  non-structural fields; a source member in one file suppressed the same host
  in unrelated prose and source literals; digitless unquoted credentials were
  skipped; five common punctuation boundaries failed retired-token matching;
  invalid binary bytes destroyed retired-token offsets; class checks accepted
  detector-compatible semantic misuse; and valid JavaScript, TOML, shell, and
  YAML grammar forms failed extraction. The focused GREEN suites contain 61
  scanner and 33 classification cases; the additional cross-class matrix brings
  the final scanner count to 62.
- Lockfiles now receive the general text scan and structural traversal. A
  general domain is suppressed only when its exact byte span is contained in
  the exact structural value span that represents it. Non-structural JSON and
  TOML values remain general domain findings, while structural values retain
  one exact lockfile-field record.
- Domain member suppression is occurrence-specific. Rust and JavaScript source
  expressions are rejected only at code offsets outside literal/comment
  regions; repository-wide matched-byte state no longer exists. The two-file
  regression keeps the prose occurrence plus the source string and comment,
  while rejecting only the source expression. In the real inventory, the
  named nested settings token appears only at its two documentation
  occurrences; the corresponding source expression and the named prediction,
  document-body, and result-containment expressions remain absent. The named
  README, build, configuration, contribution, and getting-started path tokens
  remain absent through repository-path evidence.
- Credential grammar no longer requires a digit. Quoted and unquoted
  all-letter and digit-bearing values are detected. Source declarations,
  member assignments, calls, and JSON-like identifier expressions are rejected
  through lexical offset and key/value context rather than value composition.
- Retired matching trims the complete reviewed prose/Markdown boundary set
  while retaining punctuation as a separator between code fragments. Binary
  scanning walks maximal valid UTF-8 regions with explicit original byte
  bases; invalid bytes before and between identical tokens preserve two exact,
  distinct selectors and fingerprints.
- Exception classes are checked against the matched finding, not only its
  detector. Vendor records require a public host or structural URL and cannot
  claim project-owned or deleted-CNAME domains. Fake credentials require a
  test/fixture/documentation/example category or an explicit synthetic marker.
  Historical domains are limited to the two approved path, selector, and
  fingerprint records; historical binary strings are limited to the approved
  image artifact; the historical email remains the exact test fixture.
  Project-owned domains require the exact owned host/subdomain boundary, and
  service path, owner, expiry, detector, selector, and fingerprint controls are
  unchanged.
- The reviewed pre-evidence inventory is 5,829 exact findings: 5,491 domain,
  286 structural lockfile field, 38 credential shape, 12 binary string, one
  email, and one service ID. Classes are 5,764 vendor URL, 38 hash-pinned fake
  credential fixture, 15 historical example, 11 project-owned public domain,
  and one service ID. Relative to the prior reviewed inventory, the net change
  is 2,009: 2,007 domains exposed by removing repository-global suppression
  and two credentials. The exact-key transition is 2,067 new and 58 stale
  records because scanner/test edits also move governed offsets. Independent
  review found zero selector/fingerprint mismatches across all 5,829 records
  and zero class-policy violations.
- Comment classification remains 704 sources and changes from 569 to 568 exact
  spans. The sole removal is the shell parameter-count expansion in the
  profiling script, which was previously misclassified as a comment.

#### Task 5 scanner bypass follow-up

- Three additional RED fixtures closed context and boundary bypasses. Unquoted
  credential values containing `::`, `.`, or `-` remain findings in `.env` and
  prose; expression suppression now runs only for paths with an established
  source lexer and at code occurrences. Existing Rust member and assignment
  negatives remain absent.
- Retired-token matching now trims `*`, `~`, and `|` in addition to the prior
  punctuation matrix. Paired Markdown delimiters match the exact token bytes;
  punctuation within a token still prevents a match.
- Fake-credential semantics no longer accept every `docs/` path or substring
  collisions such as `latest`, `contest`, or `exampled`. Evidence is limited to
  fixture/test paths, example configuration paths, exact synthetic-marker
  tokens, a small exact placeholder vocabulary, and the exact published AWS
  signing example; explicit production/live tokens reject arbitrary values.
  The complete regenerated set of 39 fake
  records was checked against its selected repository bytes and the predicate.
  The intentional `production-admin-password-32-bytes` test value remains
  provable only through its exact size marker, not through `production`.
- Before final evidence regeneration the inventory was 5,832 findings: 5,493
  domain, 286 lockfile field, 39 credential shape, 12 binary string, one email,
  and one service ID. Classes were 5,766 vendor URL, 39 fake credential, 15
  historical example, 11 project-owned public domain, and one service ID. The
  three-record increase from the preceding audit is two domain occurrences and
  one credential occurrence introduced by the new regression fixtures.

#### Task 5 consolidated governance edge-hardening checkpoint

- The preceding bypass checkpoint correctly records 65 scanner tests before
  this quality pass. The final focused suites now contain 67 scanner and 35
  classification tests.
- Classification update validates existing manifest versions, size bounds,
  paths, record shapes, uniqueness, comment selectors, fingerprints, and
  source/comment relationships before constructing preservation maps. Invalid
  reviewed input remains byte-unchanged. GitHub operational files use only the
  explicit YAML, shell, TOML, JavaScript, protobuf, and Dockerfile mappings;
  unknown formats fail update for review.
- URL overlap suppression is limited to the parsed authority host bytes. URL
  query and fragment candidates use the ordinary domain context and boundary
  checks; path-like components are not promoted to findings. Credential
  assignment capture is bounded to one line,
  respects quoted escapes and unquoted comment/whitespace terminators, and
  accepts punctuation without weakening occurrence-specific source lexing.
- PNG chunks now require a valid signature, bounded lengths, CRCs, IHDR-first
  and IEND termination. tEXt and uncompressed iTXt remain scanned; zTXt and
  compressed iTXt fail closed deterministically pending bounded decompression,
  so compressed sensitive metadata cannot pass silently. JSON lock span
  association now uses one forward lexical pass with an object stack rather
  than rescanning from byte zero for each field.
- The final reviewed inventory contains 5,847 exact findings: 5,505 domain,
  286 lockfile field, 42 credential shape, 12 binary string, one email, and one
  service ID. Classes are 5,778 vendor URL, 42 fake credential, 15 historical,
  11 project-owned, and one service ID. Relative to the preceding 5,832-record
  checkpoint, the 15-record increase is 12 validated query/fragment domains
  and three punctuation-bearing credentials. The earlier +767 URL-tail claim
  is retracted: those raw path-substring findings were false positives. Exact
  selector/fingerprint and class-policy validation passed
  for all records; the 42 fake records were also reviewed against their selected
  repository bytes and narrowed evidence predicate.

#### Task 5 final domain-context and PNG-keyword checkpoint

- The prior consolidated checkpoint contained 68 scanner tests; the final
  focused scanner suite contains 71 tests. CRC-valid PNG fixtures now prove
  that tEXt, zTXt, and iTXt keywords accept only 1–79 printable Latin-1 bytes,
  reject C0/C1 controls, and reject leading, trailing, or consecutive ASCII
  spaces. Missing separators continue to fail closed.
- The regenerated reviewed inventory contains 5,315 exact findings: 4,970
  domain, 286 lockfile field, 42 credential shape, 12 binary string, four
  email, and one service ID. Classes are 5,243 vendor URL, 42 fake credential,
  18 historical example, 11 project-owned public domain, and one service ID.
- Every domain selector was resolved to its exact repository bytes and the 338
  unique values were aggregated by frequency and audited by URL, fixture,
  internal-test, source-member, repository-path, and lockfile context. The most
  frequent retained values were `registry.npmjs.org` (1,172), `github.com`
  (916), `www.test-publisher.com` (867), `cms.theprospectagroup.net` (254), and
  `js.datadome.co` (121). Mixed-case, source-extension, and low-frequency
  buckets received a separate semantic pass; retained ambiguous-looking bytes
  were public-host or deliberate scanner fixtures, not path/member findings.
- The large reduction from 5,847 is the removal of repository basenames,
  structured JSON/TOML keys, and documented source-member occurrences that had
  been classified as vendor domains. Exact selector assertions now keep all
  requested representative path/member values at zero, while focused fixtures
  retain real hosts in prose, Markdown code, source strings/comments, and URL
  query/fragment values. The real-manifest regression resolves selectors back
  to bytes so representative false domains cannot silently return.
- Markdown prose punctuation, English predicates, assignment punctuation, and
  capitalization never independently suppress hosts. Documented member
  suppression now requires both established harvested evidence and explicit
  code/member syntax at the occurrence. Positive prose/port/assignment
  fixtures remain findings; repeated harvested inline and fenced member
  expressions remain absent. The final regeneration retained all requested
  host positives and kept every audited representative path/member selector at
  zero.

#### Task 6 — Implement generated regions, Markdown ownership, and link checks

- Package start on 2026-09-01 was
  `53a5a9e1d42d4dc78e4583d1b5d40ebdcc9c242a`, equal to the then-current
  `origin/spec-docs-refresh`. Work remained in the existing
  `spec-docs-refresh` worktree and PR #1049.
- The first exact focused commands,
  `cargo test --manifest-path tools/docs-parity/Cargo.toml --test markdown`
  and the corresponding `--test links` command, both failed with `E0432`
  because `docs_parity::markdown` did not exist. The generated-command leaf
  then failed with exit 2 for the missing `generate` subcommand rather than
  the expected drift exit 1. Later focused red fixtures reproduced
  repository-root link misresolution, VitePress punctuation-slug mismatch,
  and a missing Setext anchor before their individual implementations.
- The strict-review RED matrix then failed all seven challenged surfaces:
  relative links to classified exclusions passed; public headings used the
  repository slug contract; multiline CommonMark links were missed and a
  residual `%252D` encoding survived; no injectable production-command seam
  compiled; an invalid `Xxx` HTTP weekday was honored; `pages.toml` did not
  accept typed live/tombstone records; and `mermaid-extra`, arbitrary HTML
  prose anchors, and oversized rendered output were accepted. The combined
  challenged link run contained 13 passes and 14 failures; the generated
  Markdown run contained 9 passes and one failure before implementation.
- Generated-region fixtures cover duplicate, missing, mismatched, nested,
  unknown, and non-standalone markers; unknown records; duplicate row keys;
  wrong cell counts; deterministic row sorting; hand drift; exact CRLF and
  outside-byte preservation; exact owner identity; check-mode no-write;
  same-directory stale-stage interruption; symlink, unsafe-mode, traversal,
  and oversized-target rejection; and byte-identical second update. Input
  manifests, source documents, and rendered output are each bounded to 4 MiB.
  The final focused Markdown target contains 10 passing tests.
- Semantic Markdown fixtures use event offsets and cover one dead relative link
  in each active source set; multiline inline/reference/HTML destinations;
  uppercase and non-anchor HTML tags; `href`, `id`, and `name`; autolinks and
  images; HTML comments; indented, fenced, and inline code exclusion;
  repository-root paths; VitePress routes; queries; invalid UTF-8 and residual
  percent encodings; ATX and Setext headings; entities, formatting, inline
  code, Unicode normalization, leading digits, duplicate slugs, and validated
  explicit IDs. Public headings implement the pinned VitePress 1.6.4 shared
  slug contract; repository and maintained-internal headings use GitHub slug
  semantics without VitePress IDs. Classified exclusions are checked before
  known tracked paths for relative, root, and route spellings; included
  repository Markdown and binary targets retain their distinct behavior.
- The live check covers 77 maintained Markdown sources: 42 public pages,
  4 maintained-internal sources, and 31 other repository sources. The corrected
  public table-of-contents fragment is `#build-deployment-errors`; the corrected
  auction diagram prose selector is `system-flow-prebid-aps`.
- `pages.toml` is exact for the 42 currently built VitePress Markdown pages
  and 38 distinct local navigation routes parsed from the checked config.
  Reachability leaves one typed manual exception,
  `docs/guide/integrations/google_tag_manager.md`, owned by
  `documentation-maintainers` and expiring 2027-03-01. `diagrams.toml` is exact
  for 13 public Mermaid blocks; every record names an existing prose heading
  anchor and owner. Page records are explicitly typed as live or tombstone;
  the page and orphan tombstone `(route, replacement)` sets must be equal,
  tombstone routes cannot collide with live routes, and replacements must name
  live routes. Manual-orphan equality is checked separately. The real inventory
  contains zero tombstones.
- External fixtures use injected transport, clock, and sleeper seams. They
  cover all three source sets, HTTPS and credential rejection, HEAD-to-GET
  fallback only for unsupported HEAD, final status, five-redirect depth,
  loops, relative redirects, three total 429/5xx attempts, 1-second and
  2-second local delays, valid bounded delta and HTTP-date `Retry-After`,
  malformed and over-30-second fallback, request time/body bounds, retry
  exhaustion, exact owned/reasoned expiry at the boundary, final non-success,
  relative redirects, exact non-redirecting HTTPS-only curl arguments,
  malformed command output/status, bounded headers and bodies, header
  count/name/value/line/total limits, and production stdout termination on
  overflow. The injected and production transports enter the same
  redirect/retry/final-status state machine. IMF-fixdate parsing requires the
  exact grammar plus a calendar-consistent weekday. The production curl
  transport is reachable only through explicit
  `links --external --check`; no external network check was run or represented
  as a pull-request gate.
- The final focused links target contains 32 passing tests. A fresh standalone
  full suite contained 169 passing tests: 2 library unit, 35 classification,
  19 CLI, 32 links, 10 Markdown, and 71 scanner tests. Fresh local
  `links --local --check`, `generate --check`, `classify --check`, and
  `scan --check` commands exited 0. Formatting and all-target clippy with
  warnings denied also exited 0.
- Only the standalone dependency graph changed. Direct dependencies add the
  CommonMark event parser, HTML fragment parser, GitHub slugger, and Unicode
  normalization support; their resolved graph adds 45 packages to
  `tools/docs-parity/Cargo.lock`, whose SHA-256 is
  `9c071a95ca2abeff79d198296b698ae19b85e27c794d63184e748b01f6a309c0`.
  Root `Cargo.lock`
  remained byte-identical with SHA-256
  `9bb34225c5b8d1da39c75c3a8143d905f4b7d228a8986dc93d7e58a4196b4bba`.
- The reviewed sensitive inventory contains 5,388 exact occurrences, 45 more
  than the pre-review Task 6 inventory. All 45 additions are the exact registry
  host in source fields introduced by the standalone parser dependency graph;
  every one is classed `vendor_url`. Five unchanged vendor references in the
  corrected public page moved back one byte, and pre-existing standalone-lock
  records moved to their regenerated exact offsets. Synthetic plain `.md` link
  inputs are assembled from split literals, so repository filenames are not
  misclassified as vendor domains. No prior semantic finding was removed.
  Two classification updates retained identical tracked/source hashes
  (`9d907be9...` and `d30a510e...`), and repeated scanner bootstraps retained
  sensitive-manifest SHA-256
  `1aa5de36eb9457be69873d1a060a18f2926b5241482d4638cbee752b2c7e6e08`
  with `reviewed = true`. Two generated updates were byte-stable; subsequent
  generated, scanner, classification, and local-link check commands all
  exited 0.

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

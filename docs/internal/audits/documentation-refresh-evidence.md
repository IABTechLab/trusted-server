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

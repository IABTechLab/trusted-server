# Documentation Refresh (Full Surface) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refresh every maintained documentation surface, make reader-facing inventories derive from checked records, and activate enforcement without exposing write credentials to pull-request-controlled code.

**Architecture:** One rc implementation PR supplies eight package checkpoints and the canonical `docs-parity` tooling. Four small `main` PRs contain the public site, install a base-controlled validation controller, resolve CNAME, and activate scheduled automation after the rc merge; a separately owned release handoff closes the temporary branch lifecycle. Checked manifests connect code inventories, generated Markdown, source classification, examples, and CI so each fact has one source of truth.

**Tech Stack:** Rust 1.95 (`syn`, Serde, `error-stack`, Cargo), VitePress/Node 24, ESLint/JSDoc, GitHub Actions and REST APIs, shell smoke scripts, Fastly Viceroy, Wrangler, Spin, Axum

**Revised:** 2026-08-31 after full spec/plan review (round 21)

---

## Execution gate

**Gate status:** Satisfied by `aram356` on 2026-08-31. The approval covers the
five-PR-through-activation delivery shape, temporary `main`
`docs/automation-delta` required-status and strict/up-to-date protection
change, merge queues disabled on `main` through PR (e), and the external
dependency-snapshot retirement call under the specified runbook and controls.
Task 2 may begin only after Task 1 commits this approval.

The narrower owner gates are also resolved:

- `aram356` owns the temporary `fastly.toml` `service_id` allowlist exception;
  it expires at `2026-09-30T00:00:00Z`, and check mode fails at or after that
  instant. Renewal requires a reviewed, committed replacement before expiry.
  This is not the ops migration deadline.
- Task 3 deletes `docs/public/CNAME` and retains the project-path base. PR (d)
  must merge before Task 4 imports the live publishing deltas into rc.
- Task 12 archives `FAQ_POC.md` at
  `docs/superpowers/archive/FAQ_POC.md`.
- Task 17 uses the factual-governance fallback; no governance owner was named.
- Questions 5 and 8 remain explicitly non-blocking; question 4 remains closed.

## File map

### Program records and release controls

| File                                                                | Responsibility                                                                               |
| ------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `docs/superpowers/specs/2026-08-19-documentation-refresh-design.md` | Approved design, immutable baseline contract, owner decisions, and epoch definitions.        |
| `docs/superpowers/plans/2026-08-30-documentation-refresh.md`        | This execution plan and package checkpoints.                                                 |
| `docs/internal/audits/documentation-refresh-decisions.md`           | Owners, dates, selected open-question branches, audited tips, ruleset snapshot, and PR URLs. |
| `docs/internal/audits/documentation-refresh-inventory.toml`         | Per-file or per-region WP2 dispositions and source anchors.                                  |
| `docs/internal/audits/documentation-refresh-evidence.md`            | Epoch 1 commands/proofs/smokes plus schemas and issue links for post-merge evidence.         |
| `docs/internal/runbooks/documentation-automation-release.md`        | Normal and abandonment release sequencing, snapshot retirement, and branch deletion gate.    |
| `docs/internal/runbooks/documentation-automation-rollback.md`       | Controller, c2, Pages, and CNAME rollback procedures.                                        |
| `docs/internal/runbooks/patches/docs-links-c2.patch`                | Reviewed activation delta from validation-only controller to the rc-final workflow.          |
| `docs/internal/runbooks/patches/docs-links-rollback-c2.patch`       | Exact inverse of c2 without overwriting unrelated base changes.                              |
| `docs/internal/runbooks/patches/docs-links-release-retarget.patch`  | Normal release retarget/removal template.                                                    |
| `docs/internal/runbooks/patches/docs-links-release-disable.patch`   | Abandonment removal template.                                                                |

### `docs-parity` crate and checked records

`tools/docs-parity` is a standalone Cargo workspace with its own committed lockfile; it is not added to the repository workspace members.

| File                                           | Responsibility                                                                                                   |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `tools/docs-parity/Cargo.toml`                 | Standalone binary/library metadata, `[workspace]`, dependencies, and lint policy.                                |
| `tools/docs-parity/Cargo.lock`                 | Reproducible host-tool dependency graph.                                                                         |
| `tools/docs-parity/README.md`                  | Subcommands, manifest ownership, update/check flow, and failure semantics.                                       |
| `tools/docs-parity/src/main.rs`                | Thin CLI parsing and exit-code mapping.                                                                          |
| `tools/docs-parity/src/lib.rs`                 | Subcommand dispatch and shared `Report<DocsParityError>` API.                                                    |
| `tools/docs-parity/src/model.rs`               | Checked record schemas, ownership/expiry types, and generated-region markers.                                    |
| `tools/docs-parity/src/repository.rs`          | Repository-root discovery, tracked-file enumeration, safe paths, exact Git object reads, and atomic writes.      |
| `tools/docs-parity/src/classification.rs`      | Text/binary classification and exhaustive candidate/span closure.                                                |
| `tools/docs-parity/src/scanner.rs`             | Domain, email, credential, identifier, encoded-token, lockfile, binary-string, and media-metadata scanners.      |
| `tools/docs-parity/src/markdown.rs`            | Link/anchor parsing, fence inventory, ownership markers, orphan/tombstone checks, and generated regions.         |
| `tools/docs-parity/src/settings.rs`            | Serde-aware settings extractor, companion semantics, compiled probes, and template harness.                      |
| `tools/docs-parity/src/integrations.rs`        | Integration/provider inventory and capability-record checks.                                                     |
| `tools/docs-parity/src/routes.rs`              | Route record checks, Cloudflare fail-closed parser, and adapter-support rendering.                               |
| `tools/docs-parity/src/cli_help.rs`            | Linux/macOS help capture, annotated union, overrides, and golden comparison.                                     |
| `tools/docs-parity/src/snippets.rs`            | Fence manifest, diagnostic matching, isolated execution, and waiver expiry.                                      |
| `tools/docs-parity/src/gates.rs`               | Canonical gate manifest and link-only/generated consumer checks.                                                 |
| `tools/docs-parity/src/workflow.rs`            | YAML AST policy, dispatch/diff authentication fixtures, and PR-status state machine.                             |
| `tools/docs-parity/src/dependency_snapshot.rs` | Schema-validated Cargo dependency snapshot generation only; submission stays in the no-checkout workflow writer. |

Checked records live under `tools/docs-parity/manifests/`: `tracked-files.toml`, `maintained-sources.toml`, `sensitive-allowlist.toml`, `retired-identifiers.toml`, `snippets.toml`, `settings-companions.toml`, `routes.toml`, `integrations.toml`, `adapter-support.toml`, `cli-overrides.toml`, `gates.toml`, `pages.toml`, `diagrams.toml`, and `orphans.toml`. CLI goldens live at `tools/docs-parity/goldens/cli-linux.txt` and `tools/docs-parity/goldens/cli-macos.txt`. Synthetic fixtures live under `tools/docs-parity/tests/fixtures/`; never add a live secret, internal contact, or real customer value as a fixture.

### Existing surfaces with known edits

- Publishing/policy: `docs/.vitepress/config.mts`, `docs/guide/index.md`, `docs/guide/onboarding.md`, `docs/internal/onboarding.md`, `docs/business-use-cases.md`, `docs/public/CNAME`, `docs/package.json`, `docs/package-lock.json`, `fastly.toml`, `CLAUDE.md`, `AGENTS.md`, `.github/pull_request_template.md`, and `.claude/commands/{check-ci,review-changes,test-all,test-crate,verify}.md`.
- Truth pass: the active sets defined by the spec, with named repairs in `docs/guide/{ad-serving,architecture,configuration,creative-processing,error-reference,integration-guide,roadmap}.md`, `docs/guide/integrations/{gam,kargo}.md`, `crates/trusted-server-core/src/auction/README.md`, `TESTING.md`, `FAQ_POC.md`, `CHANGELOG.md`, `.env.example`, `.env.dev`, `.claude/agents/{code-architect,issue-creator}.md`, `crates/trusted-server-openrtb/generate.sh`, and the human-facing workflow/script comments recorded in the inventory.
- Configuration/API: `trusted-server.example.toml`, `docs/guide/configuration.md`, `docs/guide/api-reference.md`, and generated/check seams in `crates/trusted-server-core/src/{config,settings,auction_config_types}.rs`, `crates/trusted-server-core/src/auction/{plan,profile}.rs`, `crates/trusted-server-core/src/integrations/*.rs`, the four adapter `src/app.rs` files, and their route tests.
- New coverage: `docs/guide/{auction-testing,axum-dev,cloudflare,edgezero,fastly,spin,telemetry,tsjs}.md`, `docs/guide/integrations/{adserver_mock,gpt,testlight}.md`, `tinybird/README.md`, `scripts/smoke-{axum,fastly,cloudflare,spin}.sh`, and `.github/workflows/integration-tests.yml`.
- README/rustdoc/JSDoc: the seven missing crate READMEs named in Task 17, their Cargo manifests, `scripts/README.md`, the WP7 Rust worklist, `crates/trusted-server-js/lib/eslint.config.js`, and the scoped TypeScript/MJS files named in the spec.
- Automation: `.github/workflows/{codeql,deploy-docs,docs-links,format,integration-tests,test}.yml`, `.github/dependabot.yml`, `.tool-versions`, and `crates/trusted-server-openrtb-codegen/Cargo.toml`.

## Package checkpoint rule

After each rc package task:

1. Before editing, record `package_start_head="$(git rev-parse HEAD)"`. Make the
   package edits, generate candidate outputs, and create the package's evidence
   section.
2. Fully stage every intended add/modify/delete with the task's exact
   pathspecs before classification or parity checks. Never use `git add -N`:
   intent-to-add has no candidate blob. Review
   `git diff --cached --name-status "$package_start_head"` and reject every
   changed path outside the task's file list. Require
   `git ls-files --others --exclude-standard` to print nothing, and require
   `git diff --quiet` to exit 0 so no unstaged tracked byte anywhere can affect
   a repository-wide check. Ignored dependency/build output remains unstaged.
3. Run the focused tests and package acceptance commands against that fully
   staged universe. Regenerate checked outputs, restage only their exact paths,
   run `docs-parity check`, and require a clean generated diff.
   Bootstrap exception: Tasks 1, 4, and 5 run every staged-universe check above
   but cannot regenerate `tracked-files.toml` or `maintained-sources.toml`
   because Task 6 creates them. Task 6's initial bootstrap must classify the
   complete then-current repository, including every path those tasks added,
   moved, or deleted. From the Task 6 commit onward, every package that creates,
   moves, or deletes a tracked path must regenerate and stage both manifests;
   public-page changes must also regenerate the applicable page/orphan records.
4. Record commands/results in
   `docs/internal/audits/documentation-refresh-evidence.md` and stage that
   exact file. Because that mutation changes the candidate universe, repeat any
   classification/scanner check that consumes the ledger, restage any generated
   output, and again require global `git diff --quiet` so every tracked
   working-tree byte equals the index/HEAD candidate. Run
   `git diff --cached --check` and review the cached
   name/status and content diff from `package_start_head` against only this
   package.
5. Commit with the exact imperative message listed in the task. If recording
   final commit/run identifiers requires a follow-up, make one immediately
   adjacent evidence-only commit before starting the next package. Never let a
   later directory-wide `git add` absorb earlier evidence, and do not squash
   package or evidence commits.
6. Require `git status --porcelain` to be empty except for explicitly named,
   reviewed state before advancing to the next package.

### Atomic execution rule

This is the master program plan. Composite implementation checkpoints in
Tasks 5-10, 15, and 19 are not single coding actions. Before changing a
component, copy its next fixture from the task's enumerated negative matrix
into the evidence checklist and execute one leaf cycle at a time:

1. add one named failing fixture/test;
2. run its exact focused command and record the expected diagnostic;
3. implement the smallest production/tool change for that fixture;
4. rerun the focused command and its immediately affected regression set;
5. mark that leaf complete, then continue to the next named fixture.

Do not batch multiple parser classes, workflow rejection classes, scanners, or
adapter seams into one unreviewed edit. The package checkpoint commit happens
only after every enumerated leaf is green; the evidence ledger is the resumable
leaf-task list.

### Task 1: Record decisions and revalidate immutable tips

**Files:**

- Modify: `docs/superpowers/specs/2026-08-19-documentation-refresh-design.md`
- Modify: `docs/superpowers/plans/2026-08-30-documentation-refresh.md`
- Create: `docs/internal/audits/documentation-refresh-decisions.md`
- Create: `docs/internal/audits/documentation-refresh-evidence.md`

- [x] **Step 1: Fetch and verify the rc baseline**

Run:

```bash
git fetch origin rc/202608 main
git rev-parse origin/rc/202608
git merge-base --is-ancestor 07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf HEAD
```

Expected: the first command succeeds, `origin/rc/202608` prints exactly `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`, and the ancestry command exits 0. Stop and re-audit every new rc commit if either assertion changes.

- [x] **Step 2: Record the current default-branch tip**

Run `git rev-parse origin/main` and record the full SHA as the starting `audited_main_tip`; do not reuse it for a later `main` PR after `main` advances.

- [x] **Step 3: Resolve the owner gates**

Record owner/date/answer for questions 1, 2, 3, and 7. Record whether question 6 has an owner or will take the deterministic fallback. Leave questions 5 and 8 explicitly non-blocking and preserve question 4 as closed.

- [x] **Step 4: Make the spec state executable**

Change the spec status only after question 7 is explicit. Replace resolved open-question prose with the selected branch plus owner/date; do not erase the rejected alternatives or rollback requirements.

- [x] **Step 5: Establish evidence templates**

Add the complete Epoch 1/package evidence sections, PR/issue URLs for (a)-(e),
ruleset snapshots, first-success smokes, generated-diff proof, follow-up
issues, and exceptions with owner/expiry. For Epochs 2 and 3, record the
required evidence schema and canonical c2/release-handoff issue URLs. The
schema must require append-only timestamped captures of actor/operation, exact
commit/ref and PR head/base/tool SHAs, run IDs/attempts/jobs, redacted request
method/endpoint/body, response status/body, snapshot identity, and applicable
graph/ruleset/protection/branch API JSON. Hash every body with SHA-256; split
captures over 60 KiB into ordered hashed chunks; make links navigational rather
than authoritative; append corrections that name the superseded comment.
Actual post-merge receipts are captured under that schema in those issues
rather than committed later.

Also define the cross-worktree handoff template for PRs (b), (c), and (d):
their branch-specific tips, checks, live receipts, and ruleset snapshots stay in
the PR description or named tracking issue while the tightly scoped `main`
branch is open. The next named rc checkpoint imports those captures by URL and
value into the rc evidence/decision records; the records never ride in a
scope-limited `main` PR.

- [x] **Step 6: Verify and commit the approved handoff**

Run `cd docs && npm run format`, then `git diff --check`.

Expected: both commands pass and only the spec, plan, and new audit records are in this checkpoint.

```bash
git add docs/superpowers/specs/2026-08-19-documentation-refresh-design.md docs/superpowers/plans/2026-08-30-documentation-refresh.md docs/internal/audits/documentation-refresh-decisions.md docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Approve documentation refresh delivery plan"
```

### Task 2: Ship the public-site containment PR (b)

**Files:**

- Modify: `docs/.vitepress/config.mts`
- Modify: `docs/guide/index.md`
- Move/Modify: `docs/guide/onboarding.md` → `docs/internal/onboarding.md`

- [ ] **Step 1: Create an isolated branch from the fresh `origin/main` tip**

Use `@superpowers:using-git-worktrees`. Record that PR's new
`audited_main_tip` in its PR-description handoff block; the containment branch
must contain exactly the four containment concerns below. Do not add the rc-only
decision/evidence records to this branch.

- [ ] **Step 2: Prove the current build leaks excluded pages**

Run `cd docs && npm ci && npm run build`, then assert that at least one `superpowers/**`, `internal/**`, `epics/**`, `guide/onboarding.html`, `README.html`, or `business-use-cases.html` artifact exists.

Expected: the assertion demonstrates the pre-change leak. Save the exact artifact path as failing evidence.

- [ ] **Step 3: Add the minimal containment configuration**

Set `srcExclude` to `superpowers/**`, `internal/**`, `epics/**`, `guide/onboarding.md`, `README.md`, and `business-use-cases.md`. Fill `docs/guide/index.md`, point the Guide nav item at `/guide/`, remove Business Value navigation, move/scrub onboarding, and remove every built-page link to an excluded source. Do not include CNAME, package metadata, marketing-copy edits, or unrelated navigation work.

- [ ] **Step 4: Rebuild and prove the boundary**

Run `cd docs && npm run lint && npm run format && npm run build`.

Expected: all commands pass; the six excluded path families produce no output; `/index.html`, `/guide/index.html`, and `/guide/api-reference.html` exist and contain their expected headings.

- [ ] **Step 5: Review and commit the XS diff**

```bash
git add docs/.vitepress/config.mts docs/guide/index.md docs/guide/onboarding.md docs/internal/onboarding.md
git diff --cached --check
git diff --cached --name-status "$AUDITED_MAIN_TIP"
git diff --cached "$AUDITED_MAIN_TIP" -- docs/.vitepress/config.mts docs/guide/index.md docs/guide/onboarding.md docs/internal/onboarding.md
git commit -m "Contain internal documentation pages"
```

Reject any cached path beyond those four. Require `git diff --quiet` before the
commit so the reviewed index is the complete candidate.

- [ ] **Step 6: Merge and smoke the live Pages deployment**

Immediately before merge, refetch `main` and require the PR base to equal its
`audited_main_tip`; otherwise rebase, re-review, and record the new tip. After
merge, set a task-specific `DOCS_BASE_URL` from the selected project URL and
assert excluded URLs return 404 while site root, Guide, and API reference return
200 with expected text. Put response headers, deployment SHA, URLs, merge SHA,
and authenticated PR base in the external handoff block for Task 4 to import.

### Task 3: Resolve CNAME in independent PR (d)

**Files:**

- Delete or Modify: `docs/public/CNAME`
- Modify only on custom-domain path: `docs/.vitepress/config.mts`, `README.md`, and every hard-coded Pages URL found by the checked inventory

- [ ] **Step 1: Cut a new isolated branch from the then-current `origin/main`**

Record a fresh `audited_main_tip` in the PR-description handoff block; never
stack this on containment or automation PRs and do not add rc-only audit records.

- [ ] **Step 2: Execute exactly the selected branch**

The selected branch is **delete**. The custom-domain instructions remain
rejected/reference-only unless Task 1's CNAME decision is formally reopened
and this plan is amended and re-approved.

Delete path: remove `docs/public/CNAME` and keep the project-path `base`.
Custom-domain path: first run
`git grep -l -F 'https://iabtechlab.github.io/trusted-server'`; at the audited
baseline the exact URL-bearing path list is `README.md`. Record that list in the
external handoff block. If it differs, stop and amend this task's exact allowlist
and staging command before editing. Replace the placeholder with the approved
project-owned public domain, set `base: '/'`, update that exact URL set, and
attach owner/DNS/TLS evidence.

- [ ] **Step 3: Build and test locally**

Run `cd docs && npm ci && npm run lint && npm run format && npm run build`.

Expected: the build is green and assets resolve under the selected base.

- [ ] **Step 4: Commit, merge, and run branch-specific smokes**

```bash
# Delete path:
git add -A -- docs/public/CNAME

# Custom-domain path instead:
git add -A -- docs/public/CNAME docs/.vitepress/config.mts README.md

git diff --cached --check
git diff --cached --name-status "$AUDITED_MAIN_TIP"
git commit -m "Resolve documentation site domain"
```

Run the selected delete staging branch. The delete path's cached set is exactly
CNAME; the rejected custom path's reference set is exactly CNAME, config, and
the recorded URL path. Review
the full cached content and require `git diff --quiet` before committing.

Immediately before merge, assert the exact recorded base. After deploy, the
delete path re-smokes project URLs; the custom path records DNS, TLS, canonical
page, asset, and former hard-coded URL results. Put the merge SHA, base, and all
receipts in the external handoff block for Task 4. Never restore the placeholder
during rollback.

### Task 4: Complete WP1 hygiene on rc

**Files:**

- Import exactly from PR (b): `docs/.vitepress/config.mts`,
  `docs/guide/index.md`, `docs/guide/onboarding.md` →
  `docs/internal/onboarding.md`
- Import exactly from selected PR (d): `docs/public/CNAME` plus, only on the
  custom-domain path, `docs/.vitepress/config.mts`, `README.md`, and each
  checked URL path in that PR
- Modify: `docs/business-use-cases.md`
- Modify: `fastly.toml`
- Modify: `docs/package.json`
- Modify: `docs/package-lock.json`
- Modify: `CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `.github/pull_request_template.md`
- Modify: `.claude/commands/check-ci.md`
- Modify: `.claude/commands/review-changes.md`
- Modify: `.claude/commands/test-all.md`
- Modify: `.claude/commands/test-crate.md`
- Modify: `.claude/commands/verify.md`
- Modify: `docs/internal/audits/documentation-refresh-evidence.md`
- Modify: `docs/internal/audits/documentation-refresh-decisions.md`

- [ ] **Step 1: Import the live publishing deltas into rc**

Fetch `main`, authenticate the recorded merge commits for (b) and (d), and
authenticate each PR's recorded base SHA. Do not merge a moving `main`
wholesale. First require each authenticated base-to-merge name/status to match
its PR allowlist. Then import the final modes/blobs for only those paths directly
from the merge tree and commit the two path sets separately. This intentionally
handles an rc path whose unrelated bytes diverged from `main`; later named rc
packages, not the import, reapply any intended content. Stop on any tree or
allowlist mismatch:

```bash
b_import_start="$(git rev-parse HEAD)"
git diff --name-status "$B_BASE_SHA" "$B_MERGE_SHA" -- docs/.vitepress/config.mts docs/guide/index.md docs/guide/onboarding.md docs/internal/onboarding.md
git restore --source="$B_MERGE_SHA" --staged --worktree -- docs/.vitepress/config.mts docs/guide/index.md docs/guide/onboarding.md docs/internal/onboarding.md
git diff --cached --name-status "$b_import_start"
git diff --cached --check
git diff --quiet
git diff --quiet "$B_MERGE_SHA" -- docs/.vitepress/config.mts docs/guide/index.md docs/guide/onboarding.md docs/internal/onboarding.md
git commit -m "Import public documentation containment"

```

Then run exactly one d block. Delete path:

```bash
d_import_start="$(git rev-parse HEAD)"
git diff --name-status "$D_BASE_SHA" "$D_MERGE_SHA" -- docs/public/CNAME
git restore --source="$D_MERGE_SHA" --staged --worktree -- docs/public/CNAME
git diff --cached --name-status "$d_import_start"
git diff --cached --check
git diff --quiet
git diff --quiet "$D_MERGE_SHA" -- docs/public/CNAME
git commit -m "Import documentation site domain"
git status --porcelain
```

Custom-domain path (the authenticated scan must still have exactly this path
set; otherwise update the plan before applying):

```bash
d_import_start="$(git rev-parse HEAD)"
git diff --name-status "$D_BASE_SHA" "$D_MERGE_SHA" -- docs/public/CNAME docs/.vitepress/config.mts README.md
git restore --source="$D_MERGE_SHA" --staged --worktree -- docs/public/CNAME docs/.vitepress/config.mts README.md
git diff --cached --name-status "$d_import_start"
git diff --cached --check
git diff --quiet
git diff --quiet "$D_MERGE_SHA" -- docs/public/CNAME docs/.vitepress/config.mts README.md
git commit -m "Import documentation site domain"
git status --porcelain
```

Expected: each cached name/status is exactly its authenticated PR delta; every
imported existing path has the merge commit's mode/blob and every imported
deletion is absent; final status is empty. On the custom path, this direct
README import replaces any divergent rc bytes; Task 17 performs the later WP6
README rewrite from that imported state. Record
both source base/merge pairs and resulting rc commit SHAs. Only after these two
clean import commits set the Task 4 `package_start_head` and begin hygiene edits.

- [ ] **Step 2: Add assertions for the policy state**

Use temporary `rg` assertions to show the banner, package privacy/license, empty authors, fixture labels, KV comments, canonical gate link, generated AGENTS gate region, and exception taxonomy are absent or stale before editing.

- [ ] **Step 3: Apply the policy and hygiene edits**

Add the unverified marketing banner; scrub `fastly.toml` as specified while preserving the time-bounded service-ID entry; set the docs package private/Apache-2.0 and refresh its lockfile metadata; add the exception taxonomy to `CLAUDE.md`; make command files and the PR template link-only gate consumers; generate the AGENTS fallback region.

- [ ] **Step 4: Prove contacts/access guidance are absent**

Search all tracked files for every removed onboarding contact, handle, channel, and access phrase. Expected: no matches outside an explicit typed exception in the decision record.

- [ ] **Step 5: Verify and checkpoint WP1**

Run:

```bash
cd docs && npm ci && npm run lint && npm run format && npm run build
git diff --check
```

Expected: all commands pass and the containment/CNAME live evidence is linked from the rc evidence record.

Import the complete external handoff blocks for (b) and (d), including their
audited bases, merge SHAs, live receipts, and URLs, into the rc evidence and
decision records before staging this checkpoint.

```bash
git add docs/business-use-cases.md fastly.toml docs/package.json docs/package-lock.json CLAUDE.md AGENTS.md .github/pull_request_template.md .claude/commands/check-ci.md .claude/commands/review-changes.md .claude/commands/test-all.md .claude/commands/test-crate.md .claude/commands/verify.md docs/internal/audits/documentation-refresh-evidence.md docs/internal/audits/documentation-refresh-decisions.md
git commit -m "Clean documentation publishing policy"
```

### Task 5: Scaffold the standalone `docs-parity` crate

**Files:**

- Create: `tools/docs-parity/Cargo.toml`
- Create: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/README.md`
- Create: `tools/docs-parity/src/main.rs`
- Create: `tools/docs-parity/src/lib.rs`
- Create: `tools/docs-parity/src/model.rs`
- Create: `tools/docs-parity/src/repository.rs`
- Create/Test: `tools/docs-parity/tests/cli.rs`

- [ ] **Step 1: Write the failing CLI contract tests**

Cover repository-root discovery from nested directories, `--help`, unknown subcommands, check-vs-update exit codes, paths outside the repository, unsafe relative paths, atomic-update interruption, and stable ordering. Expect the binary to be absent.

- [ ] **Step 2: Create the independent Cargo root**

Add `[workspace]`, package metadata, the repository's lint policy, `error-stack` error flow, and only the dependencies required by the checked formats. Do not add the tool to root `workspace.members`.

- [ ] **Step 3: Implement the minimal shared model and repository boundary**

The model must make ownership and expiry structurally mandatory where the spec requires them. Repository APIs accept normalized relative paths, reject symlink escapes/unsafe modes, enumerate Git-tracked paths, and write generated files atomically.

- [ ] **Step 4: Run the focused tests**

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml --test cli
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
```

Expected: all pass, `tools/docs-parity/Cargo.lock` exists, and root `Cargo.lock` is unchanged.

- [ ] **Step 5: Document the update/check contract and commit the foundation**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/README.md tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/tests/cli.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Add documentation parity tool foundation"
```

### Task 6: Close tracked-file classification and sensitive-data scanning

**Files:**

- Modify as dependencies are introduced: `tools/docs-parity/Cargo.toml`
- Modify as dependencies are introduced: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/src/classification.rs`
- Create: `tools/docs-parity/src/scanner.rs`
- Modify: `tools/docs-parity/src/main.rs`
- Modify: `tools/docs-parity/src/lib.rs`
- Modify: `tools/docs-parity/src/model.rs`
- Modify: `tools/docs-parity/src/repository.rs`
- Create: `tools/docs-parity/manifests/tracked-files.toml`
- Create: `tools/docs-parity/manifests/maintained-sources.toml`
- Create: `tools/docs-parity/manifests/sensitive-allowlist.toml`
- Create: `tools/docs-parity/manifests/retired-identifiers.toml`
- Create/Test: `tools/docs-parity/tests/classification.rs`
- Create/Test: `tools/docs-parity/tests/scanner.rs`

- [ ] **Step 1: Write exhaustive-classification failures**

Add synthesized repositories proving each of these fails: an unknown text extension, an unknown binary, invalid UTF-8 in an expected-text file, oversized expected text, a new Dockerfile, a `.mjs` file, a `.proto` file, a human-facing comment outside an existing selector, a comment syntax without an extractor, a symlink escape, and an unclassified extracted comment span.

- [ ] **Step 2: Implement the checked classification contract**

Start from `git ls-files -z`; classify every path as text or binary without treating content sniffing as the authority. Require each text path to have a whole-file include/exclude or comment-region selector and each extracted comment span to have a disposition. Fail closed on new paths, selectors, or comment syntaxes.

- [ ] **Step 3: Write scanner detector and allowlist tests**

For domain, email, credential shape, service ID, encoded token, binary strings, lockfile structured fields, media metadata, and identifier/access-phrase denylist, add both a positive fixture and an owner/rationale/expiry allowlisted fixture. Prove expired entries, stale hashes, renamed files, and broad domain exemptions fail. Encode the `fastly.toml` exception expiry as `2026-09-30T00:00:00Z`; check mode must fail at or after that instant. Renewal requires a reviewed, committed replacement before expiry and is independent of the ops migration deadline.

- [ ] **Step 4: Implement deterministic scanning**

Scan all tracked files. Parse lockfile source/registry/URL fields structurally, inspect binary strings and media metadata, and support only the five typed exception classes approved in WP1. Report semantic sensitivity outside detector classes as a required human disposition, not as a scanner guarantee.

- [ ] **Step 5: Bootstrap and review the real manifests**

Generate candidate entries, then manually disposition every path and comment span. Seed the identifier denylist from the WP1/WP2 removals. Record the `fastly.toml` exception owner/date from Task 1; do not enable check mode if that entry is incomplete.

- [ ] **Step 6: Run the negative matrix and real scan**

Add each scanner/classification dependency only in the standalone manifest and
regenerate its lockfile. Require `git diff --quiet -- Cargo.lock` so the root
workspace lockfile is unchanged.

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml classification
cargo test --manifest-path tools/docs-parity/Cargo.toml scanner
cargo run --manifest-path tools/docs-parity/Cargo.toml -- classify --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- scan --check
```

Expected: synthesized violations fail for the intended diagnostic; the repository scan passes only with typed, unexpired entries.

- [ ] **Step 7: Commit the closed universe**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/src/classification.rs tools/docs-parity/src/scanner.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/sensitive-allowlist.toml tools/docs-parity/manifests/retired-identifiers.toml tools/docs-parity/tests/classification.rs tools/docs-parity/tests/scanner.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Enforce documentation source classification"
```

### Task 7: Implement generated regions, Markdown ownership, and link checks

**Files:**

- Modify as dependencies are introduced: `tools/docs-parity/Cargo.toml`
- Modify as dependencies are introduced: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/src/markdown.rs`
- Modify: `tools/docs-parity/src/main.rs`
- Modify: `tools/docs-parity/src/lib.rs`
- Modify: `tools/docs-parity/src/model.rs`
- Modify: `tools/docs-parity/src/repository.rs`
- Create: `tools/docs-parity/manifests/pages.toml`
- Create: `tools/docs-parity/manifests/diagrams.toml`
- Create: `tools/docs-parity/manifests/orphans.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Create/Test: `tools/docs-parity/tests/markdown.rs`
- Create/Test: `tools/docs-parity/tests/links.rs`

- [ ] **Step 1: Write generated-region failure tests**

Cover duplicate/missing markers, unknown record names, hand-edited output, unstable ordering, update mode changing bytes outside markers, interrupted writes, and a second update producing a diff.

- [ ] **Step 2: Implement deterministic region updates**

Require named start/end markers, render from typed records, update atomically, and make `generate --check` fail on any byte drift. Manual endpoint prose must carry ownership markers that are separately checked.

- [ ] **Step 3: Write set-specific Markdown tests**

Add one dead-link fixture for each active set; include missing relative files, missing anchors, duplicate headings, percent-encoded fragments, tombstone routes, an unlisted orphan, and a built page that links to an excluded source.

- [ ] **Step 4: Implement local and external link contracts**

Local checks cover active repo/maintained-internal path and anchor links.
External checks cover all active sets with final HTTPS/status validation, at
most five redirects, HEAD→GET fallback, and at most three total attempts for
429/5xx with 1-second then 2-second delays. Honor `Retry-After` only up to 30
seconds; otherwise use the bounded local delay. Exact-URL exceptions require
owner/reason/expiry. Add fixtures for allowlisted URL, expiry, redirect loop,
redirect-depth overflow, malformed/oversized `Retry-After`, retry exhaustion,
and credentials accidentally embedded in a URL.

- [ ] **Step 5: Check page/nav/orphan/diagram records**

Make `pages.toml` the intended VitePress publication/nav inventory, `orphans.toml` carry only typed tombstone/manual exceptions, and `diagrams.toml` require a prose equivalent plus owner for every diagram.

- [ ] **Step 6: Verify**

Add each Markdown/link dependency only in the standalone manifest, regenerate
its lockfile, and require `git diff --quiet -- Cargo.lock` before the commands
below.

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml markdown
cargo test --manifest-path tools/docs-parity/Cargo.toml links
cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --local --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
```

Expected: focused tests and current local repository checks pass; external network checks remain scheduled/manual, not a required per-PR network gate.

- [ ] **Step 7: Commit**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/src/markdown.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/diagrams.toml tools/docs-parity/manifests/orphans.toml tools/docs-parity/tests/markdown.rs tools/docs-parity/tests/links.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Add checked documentation regions and links"
```

### Task 8: Extract settings semantics and execute the example harness

**Files:**

- Modify as dependencies are introduced: `tools/docs-parity/Cargo.toml`
- Modify as dependencies are introduced: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/src/settings.rs`
- Modify: `tools/docs-parity/src/main.rs`
- Modify: `tools/docs-parity/src/lib.rs`
- Modify: `tools/docs-parity/src/model.rs`
- Modify: `tools/docs-parity/src/repository.rs`
- Create: `tools/docs-parity/manifests/settings-companions.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Create/Test: `tools/docs-parity/tests/settings.rs`
- Modify/Test: `crates/trusted-server-core/src/config.rs`
- Modify/Test: `crates/trusted-server-core/src/settings.rs`
- Modify/Test: `crates/trusted-server-core/src/auction/profile.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/prebid.rs`

- [ ] **Step 1: Write extractor grammar fixtures**

Cover Serde field/container/variant attributes (`rename`, `rename_all`, `alias`, `tag`, `content`, `untagged`, `flatten`, `skip`, `skip_serializing`), literal/nonliteral defaults, custom deserializers, `Option`, validation ranges, and an unknown shape-changing attribute. Expected: unclassified custom behavior fails closed.

- [ ] **Step 2: Implement the AST plus companion chain**

Resolve literal defaults from AST, require companion entries for custom deserializers/nonliteral defaults/validator functions, and verify companion claims with compiled positive and negative probes. Emit independent lifecycle, key identity, serialization, runtime, and secret-handling axes; never collapse overlapping dispositions.

- [ ] **Step 3: Write the eight-phase template harness failures**

Prove the unmodified template fails for the exact placeholder set; prove a typo, unknown disabled integration key, bad profile config, unresolved secret, stranded literal substitution, inactive-block shortcut, missing profile compiler probe, and wrong failure diagnostic do not pass.

- [ ] **Step 4: Implement the harness through production APIs**

Parse the source template, customize non-secret values in memory, preserve secret key names through deploy validation/blob serialization, resolve with a fake store, run runtime validation, and probe every optional integration/provider block in isolation with it forced enabled. Enumerate exact-string consumers in a checked record.

- [ ] **Step 5: Add visibility-local set-equality seams**

Replace the one-directional deploy-ID assertion with equality against the checked record. Keep production behavior unchanged and expose no new public API solely for the tool; put private-registry assertions in module-local `#[cfg(test)]` tests.

- [ ] **Step 6: Run focused and target-matched tests**

Add each AST/settings dependency only in the standalone manifest, regenerate
its lockfile, and require `git diff --quiet -- Cargo.lock` before the commands
below.

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml settings
cargo test-fastly config
cargo test-fastly settings
cargo test-fastly profile
```

Expected: extractor/harness tests pass and core behavior is unchanged.

- [ ] **Step 7: Commit the extractor checkpoint**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/src/settings.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/settings-companions.toml tools/docs-parity/tests/settings.rs crates/trusted-server-core/src/config.rs crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/auction/profile.rs crates/trusted-server-core/src/integrations/aps.rs crates/trusted-server-core/src/integrations/prebid.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Check configuration documentation semantics"
```

### Task 9: Check integration capabilities and adapter routes

**Files:**

- Modify as dependencies are introduced: `tools/docs-parity/Cargo.toml`
- Modify as dependencies are introduced: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/src/integrations.rs`
- Create: `tools/docs-parity/src/routes.rs`
- Modify: `tools/docs-parity/src/main.rs`
- Modify: `tools/docs-parity/src/lib.rs`
- Modify: `tools/docs-parity/src/model.rs`
- Modify: `tools/docs-parity/src/repository.rs`
- Create: `tools/docs-parity/manifests/integrations.toml`
- Create: `tools/docs-parity/manifests/routes.toml`
- Create: `tools/docs-parity/manifests/adapter-support.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Create/Test: `tools/docs-parity/tests/integrations.rs`
- Create/Test: `tools/docs-parity/tests/routes.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/mod.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/datadome.rs`
- Modify/Test: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify/Test: `crates/trusted-server-core/src/auction/mod.rs`
- Modify/Test: `crates/trusted-server-core/src/auction/profile.rs`
- Modify/Test: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify/Test: `crates/trusted-server-adapter-axum/src/app.rs`
- Modify/Test: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Modify/Test: `crates/trusted-server-adapter-spin/src/app.rs`
- Modify/Test: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify/Test: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Modify/Test: `crates/trusted-server-adapter-spin/tests/routes.rs`

- [ ] **Step 1: Write inventory equality failures**

Prove missing and extra deploy IDs, builders, plan registrations, profiles, mediator, JS module/bundle/loading-mode entries, route/method/predicate rows, and startup-router semantics all fail.

- [ ] **Step 2: Add behavioral capability probes**

Instantiate every integration across its predicate matrix and compare observed proxy routes, rewriters, injectors, post-processors, filters, and JS modes. Exercise APS rendering and DataDome protection in both states. Keep operational/release status manual with owner/review date.

- [ ] **Step 3: Add complete adapter route seams**

Snapshot Fastly, Axum, and Spin through named private test-only route
collections. Parse Cloudflare's builder with a closed grammar that expands only
the known constants, loops, method arrays, and publisher fallback helper; make
an unknown builder construct fail rather than undercount. If obtaining a
collection requires a production-source extraction, keep it private and
behavior-preserving: capture the pre-change route/method/predicate/status and
startup-router sets, then require exact equality after the extraction. Add no
public API.

- [ ] **Step 4: Compare checked records as sets**

Assert methods, literal/template/config-derived/conditional predicates,
unsupported/guarded semantics, fan-out capability, and degraded startup
routers. Every adapter regression suite plus the before/after equality proof
must pass; do not change routing behavior to make the records convenient.

- [ ] **Step 5: Verify every affected target**

Add each integration/route dependency only in the standalone manifest,
regenerate its lockfile, and require `git diff --quiet -- Cargo.lock` before the
commands below.

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml integrations
cargo test --manifest-path tools/docs-parity/Cargo.toml routes
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all pass and a generated route/capability update followed by `generate --check` is clean.

- [ ] **Step 6: Commit**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/src/integrations.rs tools/docs-parity/src/routes.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/integrations.toml tools/docs-parity/manifests/routes.toml tools/docs-parity/manifests/adapter-support.toml tools/docs-parity/tests/integrations.rs tools/docs-parity/tests/routes.rs crates/trusted-server-core/src/integrations/mod.rs crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/integrations/aps.rs crates/trusted-server-core/src/integrations/datadome.rs crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/auction/profile.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/src/app.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/src/app.rs crates/trusted-server-adapter-spin/tests/routes.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Check integration and adapter inventories"
```

### Task 10: Check CLI help, snippets, gates, workflows, and snapshots

**Files:**

- Modify as dependencies are introduced: `tools/docs-parity/Cargo.toml`
- Modify as dependencies are introduced: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/src/cli_help.rs`
- Create: `tools/docs-parity/src/snippets.rs`
- Create: `tools/docs-parity/src/gates.rs`
- Create: `tools/docs-parity/src/workflow.rs`
- Create: `tools/docs-parity/src/dependency_snapshot.rs`
- Modify: `tools/docs-parity/src/main.rs`
- Modify: `tools/docs-parity/src/lib.rs`
- Modify: `tools/docs-parity/src/model.rs`
- Modify: `tools/docs-parity/src/repository.rs`
- Create: `tools/docs-parity/manifests/{cli-overrides,snippets,gates}.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Create: `tools/docs-parity/goldens/{cli-linux,cli-macos}.txt`
- Create/Test: `tools/docs-parity/tests/{cli_help,snippets,gates,workflow,dependency_snapshot}.rs`
- Create: `docs/internal/runbooks/documentation-automation-rollback.md`
- Create: `docs/internal/runbooks/patches/docs-links-c2.patch`
- Create: `docs/internal/runbooks/patches/docs-links-rollback-c2.patch`
- Create: `.github/workflows/docs-links.yml`

- [ ] **Step 1: Capture and union CLI help**

Add test seams if required so recursive Clap help can be captured without
process termination. Implement a capture command that detects the compiled
host OS and has no caller-supplied platform override. After the capture-ready
source commit in Step 8, check out that exact 40-character SHA on one native
Linux runner and one native macOS runner and run the same recursive capture
command. For each raw result record runner identity, `uname -a`, `rustc -vV`,
Node version, source SHA, and SHA-256. A Linux VM/container is acceptable only
when it executes the Linux-target binary; the macOS capture must execute on
macOS. Never copy or infer one platform's output from the other. Import the two
hashed raw captures through the deterministic tool command, generate
`cli-linux.txt` and `cli-macos.txt`, annotate platform-only commands, and require
every prose override to carry owner/rationale/expiry plus an exact source-text
staleness fingerprint.

- [ ] **Step 2: Write snippet-mode tests**

Cover every language and mode: executable, compile/validation expected failure with phase and stable diagnostic, illustrative fragment with expiring owner waiver, missing classification, wrong diagnostic despite nonzero exit, and a formerly invalid example becoming valid.

- [ ] **Step 3: Implement the canonical gate manifest**

Define each command once with its runner/target/mode and generate or check every consumer region. Link-only consumers must contain no copied command bodies; AGENTS and canonical test docs use generated regions.

- [ ] **Step 4: Write workflow security fixtures first**

Add positive fixtures for an ordinary net-empty PR, a
divergent-history/net-identical rc release PR, c2, normal e, abandonment e,
rollback-c2, same-lifecycle repair/sync, and post-handoff maintenance. Add one
negative fixture per spec class, including a repair that changes lifecycle
state, pre-handoff maintenance, caller-selected maintenance tool, maintenance
AST weakening, an unexpected `merge_group` trigger, non-`main`
dispatch, stale `main` controller, stale base/head, fork executable SHA, extra
path, PR-files truncation attempt, unsafe mode/symlink, mixed inputs, open #1049
SHA used by `validate_main_pr`, pending candidate, failed validation attestation,
checkout/cache/service-container/secret escalation, stale snapshot, malformed
or oversized artifact, and caller-supplied refresh SHA.

- [ ] **Step 5: Implement exact-diff and workflow AST policy**

Use a separate bare object store, fetch the authenticated base/head objects,
and never check out or execute the files object. Classify the merge result with
a NUL-delimited two-tree `git diff --name-status <base> <head>`, never a
merge-base/three-dot diff, and compare mode/blob IDs for both protected paths.
Ordinary and rc release PRs pass only when those paths are net-identical even if
history diverged. c2/e/rollback-c2/same-lifecycle repair-sync/post-handoff
maintenance use the full two-tree candidate diff, change at most the two named protected files, and
change no other path; each resulting blob is at most 384 KiB and the protected
blobs at most 512 KiB total. Require exact lifecycle patch shapes; require
repair/sync to equal the authenticated rc protected blobs without changing the
validation-only or active-rc state; require
maintenance to use the authenticated current-main tool and preserve every trust
and AST invariant; require action SHA pins, least privilege, safe events, and
byte equality between c2 result and the authenticated rc workflow.
Expose the same policy through a local-index subcommand so a trusted tool
worktree can validate another worktree's fully staged candidate as inert Git
objects before its first commit; this local form never executes candidate
files. Its `candidate-kind` argument is an assertion checked against the
inferred shape, never a selector that relaxes checks, and the hosted workflow
accepts no caller-supplied candidate kind.

- [ ] **Step 6: Implement the controller state machine**

The validation-only workflow has `validate_rc`, `validate_main_pr`,
`validate_main_maintenance`, and base-controlled `pull_request_target`; it
asserts the current `main` controller SHA before inputs. Maintenance accepts no
`tool_sha`, executes only the authenticated current-main tool, and fails until
the base has tooling, main-targeted automation, and no temporary rc snapshot
jobs/refresh. Validation has `contents: read`/`pull-requests: read`;
attestation has only `statuses: write`, no checkout, fixed
`docs/automation-delta`, authenticated 40-hex head, and fixed result enum.
`validate_rc` cannot reach attestation or any writer. Materialize the complete
link reader/issue writer jobs in their final dormant form now: final
permissions/conditions, 30/5-minute timeouts, fixed schedule/refresh
concurrency with no cancellation, bounded artifact schema, dedup/auto-close
logic, and pinned action references. They remain unreachable solely because
the validation-only workflow has no `schedule:` trigger.

Enforce the exact artifact bounds before any writer starts. Each archive has
exactly one regular member, respectively `link-results.json` or
`dependency-snapshot.json`, and rejects links, traversal, and extra members.
Link results use a 2 MiB maximum archive, 1 MiB decoded JSON, 500 findings, and
2,048-byte strings; dependency snapshots use a 4 MiB archive, 2 MiB decoded
JSON, 5,000 records, and 2,048-byte strings. Both schemas reject unknown fields
and every overflow.

- [ ] **Step 7: Implement snapshot generation and templates**

Generate the schema-versioned snapshot from the exact authenticated rc tip with fixed detector/correlator/ref. The future writer template has only `contents: write`, no checkout or repository code, revalidates schema and current rc SHA, and submits only after validation. Produce and test c2 plus inverse rollback patch templates; keep schedule/snapshot jobs unreachable in the rc workflow's validation-only copy used by PR (c).

- [ ] **Step 8: Verify the whole WP8a tool**

Add each CLI/workflow/snapshot dependency only in the standalone manifest,
regenerate its lockfile, and require `git diff --quiet -- Cargo.lock` before the
commands below. First stage the complete capture-ready Task 10 source except the
two not-yet-generated golden files, review it under the package checkpoint rule,
and commit the authenticated source used by both native runners:

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/src/cli_help.rs tools/docs-parity/src/snippets.rs tools/docs-parity/src/gates.rs tools/docs-parity/src/workflow.rs tools/docs-parity/src/dependency_snapshot.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/cli-overrides.toml tools/docs-parity/manifests/snippets.toml tools/docs-parity/manifests/gates.toml tools/docs-parity/tests/cli_help.rs tools/docs-parity/tests/snippets.rs tools/docs-parity/tests/gates.rs tools/docs-parity/tests/workflow.rs tools/docs-parity/tests/dependency_snapshot.rs .github/workflows/docs-links.yml docs/internal/runbooks/documentation-automation-rollback.md docs/internal/runbooks/patches/docs-links-c2.patch docs/internal/runbooks/patches/docs-links-rollback-c2.patch docs/internal/audits/documentation-refresh-evidence.md
git diff --cached --check
git commit -m "Add documentation enforcement scaffolding"
```

Run the two native captures at that exact commit, import them without hand
editing, regenerate `tracked-files.toml`/`maintained-sources.toml`, and append
the complete provenance and raw/output hashes to the evidence ledger. Fully
stage those five exact paths, require `git diff --quiet`, and then run:

```bash
./scripts/test-cli.sh
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
```

Expected: all positive and negative fixtures pass for their intended reasons; update mode followed by check mode yields no diff.

- [ ] **Step 9: Commit WP8a**

```bash
git add tools/docs-parity/goldens/cli-linux.txt tools/docs-parity/goldens/cli-macos.txt tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml docs/internal/audits/documentation-refresh-evidence.md
git diff --cached --check
git commit -m "Record cross-platform CLI help goldens"
```

Expected: Task 10 is the adjacent two-commit series above; both capture
provenance records name the first commit, all final checks run with the second
commit's candidate bytes, and neither commit is squashed.

### Task 11: Install the validation-only `main` controller in PR (c)

**Files on the isolated `main` branch:**

- Create: `.github/workflows/docs-links.yml`

- [ ] **Step 1: Cut from a fresh `origin/main` and record the PR-specific tip**

Use `@superpowers:using-git-worktrees`; record the full `audited_main_tip` and
the rc PR #1049 head used as the reviewed source in PR (c)'s external handoff
block. Attach the complete pre-change `main` ruleset/branch-protection JSON
there. Do not add the rc-only audit records to this isolated branch.

- [ ] **Step 2: Materialize only the validation form**

Copy the complete reviewed validation-only workflow: the base-controlled PR
gate, manual `validate_rc`/`validate_main_pr`/`validate_main_maintenance`, status
attestation, and the schedule-only link reader/issue writer definitions, which
remain unreachable because there is no `schedule:` trigger. The maintenance
operation is also present but fail-closed until the normal post-(e) steady-state
predicate is true. Omit `schedule:`,
`refresh_dependency_snapshot`, snapshot jobs, and rc-targeted Dependabot
entries. Record the full workflow blob hash; the controller, attestation, and
unreachable link/issue definitions are frozen through c2.

- [ ] **Step 3: Validate the exact candidate statically**

Run the rc PR tool against the candidate workflow as data. Expected:
controller-current-main assertions, event guards, exact permissions, no
untrusted checkout/execution, action pins, dispatch input closure,
base-vs-head net-equality handling, c2/e/rollback templates, and the
same-lifecycle repair/sync plus pre-/post-handoff maintenance predicates all
pass.

- [ ] **Step 4: Commit and open PR (c)**

```bash
git add .github/workflows/docs-links.yml
git diff --cached --check
git diff --cached --name-status "$AUDITED_MAIN_TIP"
git commit -m "Install documentation automation controller"
```

Before merge, refetch and require the PR's current base to equal its recorded
tip. Put review and a successful manual `validate_rc` dispatch using `tool_sha`
equal to the current #1049 head and `ref: main` in the external handoff block.

- [ ] **Step 5: Seed all existing `main` PRs before requiring the context**

Inventory every open `main` PR and authenticate its current head/base. Retrigger
the automatic net-empty-protected-delta path or use the authenticated bootstrap
path; record a `docs/automation-delta` result for every head. Do not enable the
required context while any existing PR is missing it.

- [ ] **Step 6: Enable and prove branch protection**

Require `docs/automation-delta` from the GitHub Actions app for every `main` PR
and enable strict/up-to-date enforcement. Demonstrate an unauthorized
protected-file delta is blocked and an unrelated PR receives success without a
privileged checkout. Record that merge queues are disabled for `main`; if they
are active, stop until the approved question-7 branch disables them or a
separate reviewed design updates the spec, plan, controller, and fixtures.
Record queue adoption as blocked through PR (e).

- [ ] **Step 7: Exercise controller rollback on paper**

Review the exact procedure: remove only the new required context and restore prior strictness before disabling/reverting the controller; repair and re-prove block/pass before re-enabling. Attach the ruleset IDs and owner.

- [ ] **Step 8: Import controller rollout evidence into rc**

After PR (c), status seeding, and protection proofs complete, return to the rc
worktree. Append the external handoff block, authenticated merge/base/rc-source
SHAs, full before/after ruleset captures, seeded PR results, block/pass proof,
queue state, and rollback owner to the rc records. Commit this adjacent evidence
checkpoint before Task 12:

```bash
git add docs/internal/audits/documentation-refresh-evidence.md docs/internal/audits/documentation-refresh-decisions.md
git diff --cached --check
git diff --cached --name-status HEAD
git commit -m "Record documentation controller rollout"
git status --porcelain
```

Expected: only the two rc audit records are committed and final status is empty.

### Task 12: Complete WP2 truth pass and dispositions

**Files:**

- Create/Modify: `docs/internal/audits/documentation-refresh-inventory.toml`
- Modify: `docs/guide/ad-serving.md`
- Modify: `docs/guide/architecture.md`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/creative-processing.md`
- Modify: `docs/guide/error-reference.md`
- Modify: `docs/guide/integration-guide.md`
- Modify: `docs/guide/roadmap.md`
- Modify: `docs/guide/integrations/gam.md`
- Modify: `docs/guide/integrations/kargo.md`
- Create: `docs/guide/auction-testing.md`
- Modify: `crates/trusted-server-core/src/auction/README.md`
- Modify: `TESTING.md`
- Retire/Move/Modify: `FAQ_POC.md`
- Create only on FAQ archive path: `docs/superpowers/archive/FAQ_POC.md`
- Create only on FAQ rewrite path: `docs/guide/faq.md`
- Modify only on FAQ rewrite path: `docs/guide/index.md`
- Modify only on FAQ rewrite path: `docs/.vitepress/config.mts`
- Modify: `CHANGELOG.md`
- Modify: `.env.example`
- Modify: `.env.dev`
- Modify: `.claude/agents/code-architect.md`
- Modify: `.claude/agents/issue-creator.md`
- Modify: `.github/workflows/test.yml`
- Modify: `scripts/test-cli.sh`
- Modify: `crates/trusted-server-openrtb/generate.sh`
- Modify: `crates/trusted-server-core/src/html_processor.test.html` only if the scanner finds a real value
- Modify: human-facing script/workflow comments selected by the checked inventory
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Modify: `tools/docs-parity/manifests/sensitive-allowlist.toml`
- Modify: `tools/docs-parity/manifests/retired-identifiers.toml`
- Modify: `tools/docs-parity/manifests/snippets.toml`
- Modify: `tools/docs-parity/manifests/pages.toml`
- Modify: `tools/docs-parity/manifests/orphans.toml`

- [ ] **Step 1: Generate and fail the initial disposition inventory**

Run the inventory command over all three active sets. Expected: check mode fails for every missing whole-file/region disposition and records the audited merge-base SHA plus candidate source anchors.

- [ ] **Step 2: Disposition every candidate before rewriting**

Choose verified/rewrite/retire/create for each file and region. Manually review semantic sensitivity beyond scanner patterns. The inventory is complete only when set equality holds; do not use a wildcard disposition.

- [ ] **Step 3: Remove the named fabricated/retired content**

Replace `RequestWrapper` with real platform traits; remove Equativ, `.with_asset`, `npm run type-check`, `settings_data::get_settings`, dead `SEQUENCE.md`, APS `mock`, stale auction provider layout/routes, and retired env overlay keys. Fix only `request_ext` reserved-field protection in docs and record the `imp_ext` code follow-up.

- [ ] **Step 4: Resolve FAQ and tombstones**

The selected FAQ branch is **archive**: move `FAQ_POC.md` to
`docs/superpowers/archive/FAQ_POC.md`. The retire and rewrite instructions
remain rejected/reference-only unless Task 1's FAQ decision is formally
reopened and this plan is amended and re-approved. For reference, retire
deletes `FAQ_POC.md`; rewrite moves it to
`docs/guide/faq.md`, verifies every answer against code, links it from the
Guide landing page and Reference navigation, registers `/guide/faq` in
`pages.toml`, and adds a built-page smoke for `guide/faq.html`. Every branch
removes the root path from the active-repo inventory; only rewrite adds an
active public page. Independently replace GAM/Kargo with route-preserving
tombstones, remove sidebar reachability, and add old-route/tombstone smokes to
`pages.toml`.

- [ ] **Step 5: Rewrite testing and operator records**

Make root `TESTING.md` the test-matrix index, move the verified auction runbook into `docs/guide/auction-testing.md`, normalize the deterministic no-release CHANGELOG form, distinguish runtime env from CLI overlay, fix roadmap status, and repair the three known workflow/script comments.

- [ ] **Step 6: Reverify rc-delta content instead of blindly changing it**

Check allowed-domain semantics, `/first-party/sign` 403 plus `href`/`base`, proxy-signing recommendation, and `--staging` limitation against code. Mark verified with anchors when correct; edit only proven drift.

- [ ] **Step 7: Run full-set acceptance**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- inventory --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- scan --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- retired --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- pages --check
cd docs && npm run lint && npm run format && npm run build
```

On the FAQ rewrite branch, additionally assert
`docs/.vitepress/dist/guide/faq.html` exists and contains the expected FAQ
heading and that `/guide/faq` is present in generated navigation. On retire and
archive, assert that artifact and route are absent.

Expected: set equality passes; retired terms are absent from active sets with only the spec-defined historical exceptions; every executable fence has a valid manifest entry and diagnostic.

- [ ] **Step 8: Run regression tests for any non-doc fixture changed**

If `html_processor.test.html` changes, run `cargo test-fastly html_processor`. Run focused tests for every other non-Markdown fixture touched.

- [ ] **Step 9: Commit WP2**

Stage the common WP2 paths first, then the selected archive branch and only the
conditional fixture paths that actually changed:

```bash
git add docs/internal/audits/documentation-refresh-inventory.toml docs/internal/audits/documentation-refresh-evidence.md docs/guide/ad-serving.md docs/guide/architecture.md docs/guide/configuration.md docs/guide/creative-processing.md docs/guide/error-reference.md docs/guide/integration-guide.md docs/guide/roadmap.md docs/guide/integrations/gam.md docs/guide/integrations/kargo.md docs/guide/auction-testing.md TESTING.md CHANGELOG.md .env.example .env.dev .claude/agents/code-architect.md .claude/agents/issue-creator.md .github/workflows/test.yml scripts/test-cli.sh crates/trusted-server-core/src/auction/README.md crates/trusted-server-openrtb/generate.sh tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/sensitive-allowlist.toml tools/docs-parity/manifests/retired-identifiers.toml tools/docs-parity/manifests/snippets.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/orphans.toml
# Retire branch:
git add -A -- FAQ_POC.md
# Archive branch instead:
git add -A -- FAQ_POC.md docs/superpowers/archive/FAQ_POC.md
# Rewrite branch instead:
git add -A -- FAQ_POC.md docs/guide/faq.md docs/guide/index.md docs/.vitepress/config.mts
# Only when the scanner required this fixture edit:
git add crates/trusted-server-core/src/html_processor.test.html
git commit -m "Correct maintained documentation truth"
```

The comments preserve mutually exclusive reference branches; execute the
archive command only unless the decision is formally reopened and this plan is
amended and re-approved.
If the mechanical inventory selects another human-facing comment path, add
that one exact path to the reviewed list before running the checkpoint—never
replace this list with `git add docs`, `git add .github`, or another directory.

### Task 13: Complete WP3 configuration reference and template

**Files:**

- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/cli.md`
- Modify: `trusted-server.example.toml`
- Modify: `tools/docs-parity/manifests/settings-companions.toml`

- [ ] **Step 1: Make parity fail on the baseline gaps**

Run settings check before edits. Expected diagnostics: missing `[consent]`, `[debug]`, and standalone `[tinybird]`; 10/17 key-section rows; 5/14 integration subsections; missing profile schemas; stale template store selectors; duplicate `[trusted_client_ip]`; incomplete directional/secret dispositions.

- [ ] **Step 2: Generate canonical field/profile regions**

Render all 17 roots, 14 deploy IDs, three profile configs, resolved defaults/requiredness/grammars/ranges/limits, and every independent disposition axis. Manual prose stays outside markers with an ownership marker.

- [ ] **Step 3: Repair the example template conservatively**

Audit all existing root blocks, remove the four accepted-and-discarded store selectors and duplicate trusted-client-IP block, preserve exact placeholder strings/key references, and do not normalize deprecated/ignored fields into recommended examples.

- [ ] **Step 4: Document the secret model and CLI exposure**

Classify the 11 store-resolved paths, inline trusted-client-IP secret, and discarded Tinybird secret. Warn that config diff/dry-run/push output can expose inline values.

- [ ] **Step 5: Run extractor, harness, and docs gates**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- settings --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- examples --check
cargo test-fastly config
cd docs && npm run lint && npm run format && npm run build
```

Expected: every active canonical field appears in reference/template, noncanonical paths are labeled only, all literal consumers remain connected, and all eight example-harness phases pass.

- [ ] **Step 6: Commit WP3**

```bash
git add docs/guide/configuration.md docs/guide/cli.md trusted-server.example.toml tools/docs-parity/manifests/settings-companions.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Complete configuration documentation"
```

### Task 14: Complete WP4 generated API contracts

**Files:**

- Modify: `docs/guide/api-reference.md`
- Modify: `tools/docs-parity/manifests/routes.toml`
- Modify: `tools/docs-parity/manifests/adapter-support.toml`
- Modify/Test only for test seams: the four adapter `src/app.rs` files and route tests from Task 9

- [ ] **Step 1: Make route generation fail on reader drift**

Temporarily alter one checked record in a test fixture and prove check mode rejects an unregenerated region. Prove an unknown Cloudflare route-builder construct fails parsing.

- [ ] **Step 2: Generate the route/availability regions**

Render all adapters, methods, route families, predicates, Fastly-only routes, guarded/unsupported admin behavior, publisher fallback, startup failure, middleware facts, and fan-out support from the checked records.

- [ ] **Step 3: Complete manually owned endpoint contracts**

For every endpoint, cover auth, schemas, status codes, cache/CORS, config gates, rate limits, and examples or mark a typed not-applicable value. Keep minting (`/first-party/sign`) distinct from validation (`/proxy`, `/click`, `/proxy-rebuild`).

- [ ] **Step 4: Verify set equality and rendered prose**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- routes --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cd docs && npm run lint && npm run format && npm run build
```

Expected: no adapter can add/remove/change a route without a record and generated diff; all ownership markers are present.

- [ ] **Step 5: Commit WP4**

```bash
git add docs/guide/api-reference.md tools/docs-parity/manifests/routes.toml tools/docs-parity/manifests/adapter-support.toml crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/src/app.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/src/app.rs crates/trusted-server-adapter-spin/tests/routes.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Generate adapter API documentation"
```

### Task 15: Add deployment guides and recurring first-success smokes

**Files:**

- Modify: `docs/guide/fastly.md`
- Create: `docs/guide/cloudflare.md`
- Create: `docs/guide/spin.md`
- Create: `docs/guide/axum-dev.md`
- Create: `scripts/smoke-fastly.sh`
- Create: `scripts/smoke-cloudflare.sh`
- Create: `scripts/smoke-spin.sh`
- Create: `scripts/smoke-axum.sh`
- Modify: `.tool-versions`
- Modify: `.github/workflows/integration-tests.yml`
- Modify as fixtures, not operator sources: `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml`
- Modify as needed: `crates/trusted-server-adapter-cloudflare/wrangler.ci.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Modify: `tools/docs-parity/manifests/pages.toml`
- Modify: `tools/docs-parity/manifests/orphans.toml`

- [ ] **Step 1: Write smoke failures before guides**

For each adapter, add a clean-state positive scenario with exact status, stub-origin sentinel, and Trusted Server rewrite/header; add independent missing-config and per-required-secret failures with specific diagnostics. Prove health-only/status-only/degraded-router responses do not satisfy the oracle.

Select the current stable Wrangler version at implementation time, record its
source/version, add it to `.tool-versions`, and make the Cloudflare smoke/CI
fixture fail if that pin is absent or a different executable is used.

- [ ] **Step 2: Implement Axum and Fastly scripts**

Axum exports the exact config/secret bridge and exercises a publisher request. Fastly runs config init/validate/local push, seeds all three `ts_secrets` entries, serves through Viceroy, asserts health plus publisher behavior, and restores `fastly.toml` in a trap.

- [ ] **Step 3: Implement the Cloudflare bridge exactly**

Provision/map the selected KV binding before push, run local push, read the envelope with explicit binding/namespace, double-encode with `jq`, write only gitignored generated vars/manifest files, run Wrangler, assert rewritten content, and clean up. Keep local and remote secret instructions separate.

- [ ] **Step 4: Implement the Spin path**

Set the required store mapping to `default`, local-push into `.spin/`, encode/export every secret variable name, run `spin up`, assert a non-health publisher response with the strong oracle, and clean all local state. If CI cannot run Spin, record owner/SHA/tool versions/expiry for recurring manual evidence.

- [ ] **Step 5: Write the guides from the scripts**

Every guide command must be copyable and remain in the same order as the recurring script. State maturity, fan-out, health/startup, and unwired-store limitations from `adapter-support.toml`; a successful push is not described as a configured runtime where the bridge is still required.

Register the four new public pages immediately. Until Task 16 adds their final
navigation, give any genuinely unreachable page a typed temporary orphan entry
owned by WP5 and expiring at Task 16; Task 16 must remove that entry.

- [ ] **Step 6: Wire runnable scripts into integration CI**

Run `chmod +x scripts/smoke-axum.sh scripts/smoke-fastly.sh
scripts/smoke-cloudflare.sh scripts/smoke-spin.sh`. Run Axum, Fastly, and
Cloudflare smokes in the existing integration workflow after their artifacts
are prepared. Consume Wrangler from the Task 15 `.tool-versions` pin, not an
unpinned global latest. Preserve existing integration suites.

- [ ] **Step 7: Verify focused journeys**

```bash
bash -n scripts/smoke-axum.sh scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh
./scripts/smoke-axum.sh
./scripts/smoke-fastly.sh
./scripts/smoke-cloudflare.sh
```

Run Spin or attach its time-bounded evidence. Then run the integration-test parity target and docs build.

- [ ] **Step 8: Commit the deployment half of WP5**

```bash
git add docs/guide/fastly.md docs/guide/cloudflare.md docs/guide/spin.md docs/guide/axum-dev.md scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh scripts/smoke-axum.sh .tool-versions .github/workflows/integration-tests.yml crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml crates/trusted-server-adapter-cloudflare/wrangler.ci.toml tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/orphans.toml docs/internal/audits/documentation-refresh-evidence.md
git ls-files --stage scripts/smoke-axum.sh scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh | awk '$1 != "100755" { bad=1 } END { exit bad }'
git commit -m "Document adapter deployment journeys"
```

### Task 16: Complete WP5 product coverage and navigation

**Files:**

- Create: `docs/guide/edgezero.md`
- Create: `docs/guide/telemetry.md`
- Create: `docs/guide/tsjs.md`
- Modify: `docs/guide/cli.md`
- Create: `docs/guide/integrations/adserver_mock.md`
- Modify: `docs/guide/integrations/gpt.md`
- Create: `docs/guide/integrations/testlight.md`
- Modify: `docs/guide/integrations-overview.md`
- Modify: `docs/guide/integration-guide.md`
- Create: `tinybird/README.md`
- Modify: `docs/.vitepress/config.mts`
- Modify: `docs/package.json`
- Modify: `.github/workflows/deploy-docs.yml`
- Read/verify: `.tool-versions` (Wrangler pin established in Task 15)
- Create/Test: `crates/trusted-server-integration-tests/tests/documentation_snippets.rs`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Modify: `tools/docs-parity/manifests/integrations.toml`
- Modify: `tools/docs-parity/manifests/adapter-support.toml`
- Modify: `tools/docs-parity/manifests/cli-overrides.toml`
- Modify: `tools/docs-parity/manifests/snippets.toml`
- Modify: `tools/docs-parity/manifests/pages.toml`
- Modify: `tools/docs-parity/manifests/diagrams.toml`
- Modify: `tools/docs-parity/manifests/orphans.toml`

- [ ] **Step 1: Generate the support matrix and inventory regions**

Render adapter status/fan-out/startup rows and all 14 deploy IDs plus creative from checked records. Prove repeated status prose cannot diverge from the matrix.

- [ ] **Step 2: Add missing platform/system pages**

Write EdgeZero lifecycle/store/blob flow, telemetry plus `browser_family`/Tinybird/Fastly-only emission, tsjs 12-module/13-bundle/three-loading-mode model (including the standalone `gpt_diagnostics` tag), and adserver-mock mediator semantics from their truth sources. Render `docs/guide/cli.md` from the checked two-platform help union while preserving owned explanatory prose outside the generated region.

- [ ] **Step 3: Complete integration journeys**

Document GPT slot handoff, script guards, and Testlight. Replace broken integration-guide snippets with one compiling, core-neutral fixture using complete `RuntimeServices`; register every fence and expected diagnostic.

- [ ] **Step 4: Restructure discoverability**

Create Operator/Deployment/Reference navigation groups, make every ID reachable, enable local search and `lastUpdated`, and add prose equivalents for every diagram. Keep tombstones out of navigation and containment exclusions intact.

- [ ] **Step 5: Add rolling-main provenance**

Inject `GITHUB_SHA` into the build without exposing secrets, render a rolling-main banner, and make deploy-docs paths include `.tool-versions`. Add a built-output assertion for the exact supplied SHA.

- [ ] **Step 6: Verify journeys, snippets, and publication**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- integrations --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- pages --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets
cd docs && npm run lint && npm run format && GITHUB_SHA="$(git rev-parse HEAD)" npm run build
```

Expected: nav/page set equality passes, banner contains the current SHA, excluded pages remain absent, all journeys/diagrams have owners, and the compiling fixture passes.

- [ ] **Step 7: Commit the remaining WP5 checkpoint**

```bash
git add docs/guide/edgezero.md docs/guide/telemetry.md docs/guide/tsjs.md docs/guide/cli.md docs/guide/integrations/adserver_mock.md docs/guide/integrations/gpt.md docs/guide/integrations/testlight.md docs/guide/integrations-overview.md docs/guide/integration-guide.md tinybird/README.md docs/.vitepress/config.mts docs/package.json .github/workflows/deploy-docs.yml crates/trusted-server-integration-tests/tests/documentation_snippets.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/integrations.toml tools/docs-parity/manifests/adapter-support.toml tools/docs-parity/manifests/cli-overrides.toml tools/docs-parity/manifests/snippets.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/diagrams.toml tools/docs-parity/manifests/orphans.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Add full documentation product coverage"
```

### Task 17: Complete WP6 root and crate documentation

**Files:**

- Modify: `README.md`
- Modify: `CONTRIBUTING.md`
- Modify: `CLAUDE.md`
- Modify: `ProjectGovernance.md`
- Modify: `crates/trusted-server-core/README.md`
- Modify: `crates/trusted-server-integration-tests/README.md`
- Create: `crates/trusted-server-adapter-axum/README.md`
- Create: `crates/trusted-server-adapter-cloudflare/README.md`
- Create: `crates/trusted-server-adapter-fastly/README.md`
- Create: `crates/trusted-server-adapter-spin/README.md`
- Create: `crates/trusted-server-cli/README.md`
- Create: `crates/trusted-server-js/README.md`
- Create: `crates/trusted-server-openrtb-codegen/README.md`
- Create: `scripts/README.md`
- Modify: each corresponding crate `Cargo.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`

- [ ] **Step 1: Add the failing README equality test**

Use `cargo metadata --no-deps` to enumerate every package. Expected before edits: seven missing README files and/or missing `readme =` metadata. Include an extra/unlisted README negative fixture.

- [ ] **Step 2: Correct canonical contributor/operator prose**

Make root quick starts satisfy the first-success scripts; make contributing link to canonical gates; correct target/integration-model/example policy in CLAUDE; apply the selected factual-governance fallback.

- [ ] **Step 3: Write responsibility-focused READMEs**

Each crate README states purpose, runtime/target, important boundaries, build/test command, and links to canonical guides without copying volatile matrices. Rewrite core as an actual module overview and correct integration-test scope. Add a scripts index with inputs/side effects/cleanup.

- [ ] **Step 4: Connect Cargo metadata**

Add exact `readme = "README.md"` entries to the seven new crate manifests and any existing package missing the metadata. Do not alter dependency or feature resolution.

- [ ] **Step 5: Verify**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- readmes --check
cargo metadata --no-deps --format-version 1
cargo fmt --all -- --check
cd docs && npm run lint && npm run format && npm run build
```

Expected: every package maps to an existing README and all active root/crate/skill/agent dispositions remain closed.

- [ ] **Step 6: Commit WP6**

```bash
git add README.md CONTRIBUTING.md CLAUDE.md ProjectGovernance.md crates/trusted-server-core/README.md crates/trusted-server-core/Cargo.toml crates/trusted-server-integration-tests/README.md crates/trusted-server-integration-tests/Cargo.toml crates/trusted-server-adapter-axum/README.md crates/trusted-server-adapter-cloudflare/README.md crates/trusted-server-adapter-fastly/README.md crates/trusted-server-adapter-spin/README.md crates/trusted-server-cli/README.md crates/trusted-server-js/README.md crates/trusted-server-openrtb-codegen/README.md crates/trusted-server-adapter-axum/Cargo.toml crates/trusted-server-adapter-cloudflare/Cargo.toml crates/trusted-server-adapter-fastly/Cargo.toml crates/trusted-server-adapter-spin/Cargo.toml crates/trusted-server-cli/Cargo.toml crates/trusted-server-js/Cargo.toml crates/trusted-server-openrtb-codegen/Cargo.toml scripts/README.md tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Refresh contributor and crate documentation"
```

### Task 18: Complete WP7 rustdoc and JSDoc

**Files:**

- Modify: `crates/trusted-server-core/src/lib.rs`
- Modify: the ten production files in `crates/trusted-server-core/src/platform/`: `backend_naming.rs`, `error.rs`, `http.rs`, `image_optimizer.rs`, `kv.rs`, `mod.rs`, `template_assembly.rs`, `template_cache.rs`, `traits.rs`, and `types.rs`
- Modify: `crates/trusted-server-core/src/auth.rs`
- Modify: `crates/trusted-server-core/src/constants.rs`
- Modify: `crates/trusted-server-core/src/host_rewrite.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/http_util.rs`
- Modify: `crates/trusted-server-core/src/openrtb.rs`
- Modify: `crates/trusted-server-core/src/price_bucket.rs`
- Modify: `crates/trusted-server-core/src/proxy.rs`
- Modify: `crates/trusted-server-core/src/rsc_flight.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `crates/trusted-server-core/src/settings_data.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`
- Modify: `crates/trusted-server-core/src/storage/kv_store.rs`
- Modify: `crates/trusted-server-core/src/storage/mod.rs`
- Modify: `crates/trusted-server-core/src/integrations/datadome.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: all six files under `crates/trusted-server-core/src/integrations/nextjs/`: `html_post_process.rs`, `mod.rs`, `rsc.rs`, `rsc_placeholders.rs`, `script_rewriter.rs`, and `shared.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/lib.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/platform.rs`
- Modify: `crates/trusted-server-adapter-spin/src/lib.rs`
- Modify: `crates/trusted-server-adapter-spin/src/platform.rs`
- Modify: `crates/trusted-server-adapter-axum/src/lib.rs` when the checked WP7 inventory marks its existing crate header incomplete
- Modify: `crates/trusted-server-cli/src/lib.rs`
- Modify: `crates/trusted-server-cli/src/run.rs`
- Modify: undocumented CLI command modules recorded by the exact WP2 inventory before this task starts
- Modify: `crates/trusted-server-js/src/lib.rs`
- Modify: `crates/trusted-server-js/lib/src/core/{registry,render,types}.ts`
- Modify: `crates/trusted-server-js/lib/src/shared/globals.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/creative/*.ts`
- Modify: `crates/trusted-server-js/lib/build-prebid-external.mjs`
- Modify: `crates/trusted-server-js/lib/eslint.config.js`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/file-overview.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/exported-function.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/exported-class.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/exported-interface.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/exported-type-alias.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/exported-variable.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/default-export.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/re-export.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/alignment.ts`
- Create/Test: `tools/docs-parity/tests/fixtures/jsdoc/types.ts`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`

- [ ] **Step 1: Capture failing rustdoc/JSDoc evidence**

Run the complete rustdoc matrix with `RUSTDOCFLAGS="-D warnings"`, native core doctests, and current JS lint. Record every existing failure; do not suppress a warning solely to get green.

- [ ] **Step 2: Complete the exact Rust worklist**

Add crate/module/item docs with correct errors/panics/examples only where they earn their keep. Repair Cloudflare/Spin store claims to state the unwired reality and link the follow-up. Keep test-only modules excluded from coverage counts.

- [ ] **Step 3: Activate scoped JSDoc rules test-first**

Add separate synthesized failures for file overview, exported function/class/interface/type alias/variable/default export/re-export, alignment, and types. Configure paths relative to `crates/trusted-server-js/lib`; do not accidentally impose this scope on generated/vendor files.

- [ ] **Step 4: Document the scoped TS/MJS files**

Add file headers and declaration docs, especially complete `core/types.ts`; document behavior, not TypeScript syntax. Keep runtime code unchanged.

- [ ] **Step 5: Run the exact rustdoc matrix**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features -p trusted-server-core -p trusted-server-js -p trusted-server-openrtb --target wasm32-wasip1
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p trusted-server-adapter-fastly --target wasm32-wasip1
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p trusted-server-adapter-spin --target wasm32-wasip1 --features spin
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features -p trusted-server-adapter-axum
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features -p trusted-server-cli -p trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')"
cargo test --doc -p trusted-server-core
```

Expected: warning-free docs and passing native doctests with Node available for the JS build script.

- [ ] **Step 6: Run JSDoc and target regression suites**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- jsdoc-fixtures --check
cd crates/trusted-server-js/lib && npm run lint && npm run format && npx vitest run && npm run build
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: all pass with no runtime behavior diff.

- [ ] **Step 7: Commit WP7**

Stage only the exact modified paths enumerated in this task plus the ten named
JSDoc fixtures and the evidence ledger. The WP2 inventory may add individual
CLI command modules to this list; record and stage each path explicitly.
Directory-wide `git add crates` is forbidden.

```bash
git add crates/trusted-server-core/src/lib.rs crates/trusted-server-core/src/platform/backend_naming.rs crates/trusted-server-core/src/platform/error.rs crates/trusted-server-core/src/platform/http.rs crates/trusted-server-core/src/platform/image_optimizer.rs crates/trusted-server-core/src/platform/kv.rs crates/trusted-server-core/src/platform/mod.rs crates/trusted-server-core/src/platform/template_assembly.rs crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/platform/traits.rs crates/trusted-server-core/src/platform/types.rs crates/trusted-server-core/src/auth.rs crates/trusted-server-core/src/constants.rs crates/trusted-server-core/src/host_rewrite.rs crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/http_util.rs crates/trusted-server-core/src/openrtb.rs crates/trusted-server-core/src/price_bucket.rs crates/trusted-server-core/src/proxy.rs crates/trusted-server-core/src/rsc_flight.rs crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/settings_data.rs crates/trusted-server-core/src/tsjs.rs crates/trusted-server-core/src/storage/kv_store.rs crates/trusted-server-core/src/storage/mod.rs crates/trusted-server-core/src/integrations/datadome.rs crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/integrations/nextjs/html_post_process.rs crates/trusted-server-core/src/integrations/nextjs/mod.rs crates/trusted-server-core/src/integrations/nextjs/rsc.rs crates/trusted-server-core/src/integrations/nextjs/rsc_placeholders.rs crates/trusted-server-core/src/integrations/nextjs/script_rewriter.rs crates/trusted-server-core/src/integrations/nextjs/shared.rs crates/trusted-server-adapter-fastly/src/main.rs crates/trusted-server-adapter-cloudflare/src/lib.rs crates/trusted-server-adapter-cloudflare/src/platform.rs crates/trusted-server-adapter-spin/src/lib.rs crates/trusted-server-adapter-spin/src/platform.rs crates/trusted-server-cli/src/lib.rs crates/trusted-server-cli/src/run.rs crates/trusted-server-js/src/lib.rs crates/trusted-server-js/lib/src/core/registry.ts crates/trusted-server-js/lib/src/core/render.ts crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/src/shared/globals.ts crates/trusted-server-js/lib/src/integrations/creative/click.ts crates/trusted-server-js/lib/src/integrations/creative/dynamic_src_guard.ts crates/trusted-server-js/lib/src/integrations/creative/iframe.ts crates/trusted-server-js/lib/src/integrations/creative/image.ts crates/trusted-server-js/lib/src/integrations/creative/index.ts crates/trusted-server-js/lib/src/integrations/creative/proxy_sign.ts crates/trusted-server-js/lib/build-prebid-external.mjs crates/trusted-server-js/lib/eslint.config.js tools/docs-parity/tests/fixtures/jsdoc/file-overview.ts tools/docs-parity/tests/fixtures/jsdoc/exported-function.ts tools/docs-parity/tests/fixtures/jsdoc/exported-class.ts tools/docs-parity/tests/fixtures/jsdoc/exported-interface.ts tools/docs-parity/tests/fixtures/jsdoc/exported-type-alias.ts tools/docs-parity/tests/fixtures/jsdoc/exported-variable.ts tools/docs-parity/tests/fixtures/jsdoc/default-export.ts tools/docs-parity/tests/fixtures/jsdoc/re-export.ts tools/docs-parity/tests/fixtures/jsdoc/alignment.ts tools/docs-parity/tests/fixtures/jsdoc/types.ts tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml docs/internal/audits/documentation-refresh-evidence.md
# Append `crates/trusted-server-adapter-axum/src/lib.rs` only if selected and
# each exact CLI command-module path named by the WP2 inventory.
git commit -m "Complete in-code documentation"
```

### Task 19: Activate WP8b CI gates and release controls

**Files:**

- Modify: `.github/workflows/format.yml`
- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/integration-tests.yml`
- Modify: `.github/workflows/codeql.yml`
- Modify: `.github/workflows/deploy-docs.yml`
- Modify: `.github/workflows/docs-links.yml`
- Modify: `.github/dependabot.yml`
- Modify: `.tool-versions`
- Modify: `crates/trusted-server-openrtb-codegen/Cargo.toml`
- Modify: `CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `TESTING.md`
- Modify: `docs/guide/testing.md`
- Create: `docs/internal/runbooks/documentation-automation-release.md`
- Create: `docs/internal/runbooks/patches/docs-links-release-retarget.patch`
- Create: `docs/internal/runbooks/patches/docs-links-release-disable.patch`
- Modify: `tools/docs-parity/src/gates.rs`
- Modify: `tools/docs-parity/src/workflow.rs`
- Modify: `tools/docs-parity/src/dependency_snapshot.rs`
- Modify/Test: `tools/docs-parity/tests/gates.rs`
- Modify/Test: `tools/docs-parity/tests/workflow.rs`
- Modify/Test: `tools/docs-parity/tests/dependency_snapshot.rs`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Modify: `tools/docs-parity/manifests/gates.toml`

- [ ] **Step 1: Make static workflow fixtures fail for current gaps**

Assert missing rc CodeQL triggers, rustdoc/doctest/docs-parity jobs, nested lockfile cache keys, setup-node lockfile paths, Dependabot roots, Wrangler pin, gate-region equality, schedule trust split, release patches, and openrtb-codegen workspace lints.

- [ ] **Step 2: Wire blocking deterministic checks**

Add host docs-parity fmt/clippy/test/check, generated clean-diff, settings/examples/inventory/snippets/scanner/local links/readmes/jsdoc/workflow fixtures, rustdoc matrix, and native doctests. Pin Node wherever the JS build script runs. Keep external network links scheduled, not a flaky PR dependency.

- [ ] **Step 3: Normalize existing automation**

Add CodeQL `rc/*` PR triggers, `.tool-versions` deploy paths, lockfile-based
setup-node cache keys, pinned Wrangler, GitHub
Actions/browser/Next.js/docs-parity Dependabot roots, and
`[lints] workspace = true`. Choose current stable action versions at
implementation time and pin every new `uses` reference by full SHA with its
source version recorded. The `docs-links.yml` action pins were selected and
frozen in Tasks 10-11; verify them here but do not repin or otherwise change a
non-activation byte. A required repin takes the controller-repair path.

- [ ] **Step 4: Finalize the rc workflow's activated form**

Starting from the exact workflow hash recorded in Task 11, add only the
reviewed c2 activation regions: weekly `17 9 * * 1`, the fixed stateful
schedule/refresh path already configured on the dormant link jobs, and the
split snapshot generator/writer plus closed refresh operation with their
20/5-minute timeouts. Preserve every
non-activation byte, including per-PR serialization, controller,
attestation, current-`main` assertion, fail-closed future maintenance mode, and
unreachable link/issue definitions.
The issue writer schema-validates bounded results, deduplicates the single
owned report issue, and auto-closes it after a clean scheduled run. Generate a
fresh c2 patch and its inverse covering both protected files. Record the final
rc mode/blob IDs for `.github/workflows/docs-links.yml` and
`.github/dependabot.yml`; prove the c2 base workflow hashes to the Task 11 blob,
and prove applying c2 produces mode/blob identity with rc for both protected
paths. The inverse must restore both base blobs without overwriting unrelated
base changes. If a non-activation byte must change, stop and ship a separately
reviewed controller-repair PR before continuing; do not hide it in c2.

- [ ] **Step 5: Finalize release and rollback runbooks**

Provide normal and abandonment patch templates, freeze semantics, queued-run
enumeration, optional separately scoped `actions: write` cancellation token,
empty same-identity retirement request, 201 receipt fields,
automatic-main-graph verification, branch deletion gate, CNAME fallback, and
branch-protection restoration ordering. The normal runbook requires
base-vs-rc-head mode/blob identity for both protected paths before the broad
release merge and a separately reviewed sync/repair PR if they differ; it also
proves post-(e) maintenance before closure. Add an rc dependency-change
checklist with a named owner: every merge changing
`tools/docs-parity/Cargo.toml` or `tools/docs-parity/Cargo.lock` remains
incomplete until a post-merge `refresh_dependency_snapshot` receipt is
attached; missed/failed refreshes are security-coverage incidents and the
weekly run is only a reconciliation backstop.

- [ ] **Step 6: Generate all gate consumers**

Regenerate CLAUDE/AGENTS/TESTING/guide testing regions from `gates.toml`; prove command files and CONTRIBUTING remain link-only. A second generation must produce no diff.

- [ ] **Step 7: Run WP8b focused acceptance**

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
cargo fmt --all -- --check
cd docs && npm run lint && npm run format && npm run build
```

Expected: all static/runtime negative fixtures fail for their intended diagnostic, then the real repository passes; generation is idempotent.

- [ ] **Step 8: Deduplicate and file every code follow-up**

Search the tracker for each item in the spec's “Follow-up issues to file” section. File or record an existing-issue disposition for all twelve items, with URL, owner, and labels in the evidence record; do not collapse the adapter-store, config-bridge, health-contract, reserved-field, placeholder, inline-secret, deploy-ID, CLI-help, telemetry, env-store, or staging-blob issues into vague umbrella tickets.

- [ ] **Step 9: Commit WP8b**

```bash
git add .github/workflows/format.yml .github/workflows/test.yml .github/workflows/integration-tests.yml .github/workflows/codeql.yml .github/workflows/deploy-docs.yml .github/workflows/docs-links.yml .github/dependabot.yml .tool-versions crates/trusted-server-openrtb-codegen/Cargo.toml CLAUDE.md AGENTS.md TESTING.md docs/guide/testing.md docs/internal/runbooks/documentation-automation-release.md docs/internal/runbooks/patches/docs-links-release-retarget.patch docs/internal/runbooks/patches/docs-links-release-disable.patch tools/docs-parity/src/gates.rs tools/docs-parity/src/workflow.rs tools/docs-parity/src/dependency_snapshot.rs tools/docs-parity/tests/gates.rs tools/docs-parity/tests/workflow.rs tools/docs-parity/tests/dependency_snapshot.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/gates.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Activate documentation enforcement gates"
```

### Task 20: Close Epoch 1 and make PR (a) implementation-ready

**Files:**

- Modify: `docs/internal/audits/documentation-refresh-evidence.md`
- Modify: `docs/internal/audits/documentation-refresh-decisions.md`

- [ ] **Step 1: Reassert the exact rc baseline at final PR head**

Fetch and require `origin/rc/202608` still equals `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`; require the implementation branch contains it. Any advance triggers a focused delta audit, spec update, regenerated records, and re-review before continuing.

- [ ] **Step 2: Run the complete local CI gate list**

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy --package trusted-server-cli --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets --all-features -- -D warnings
cargo clippy --package trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets -- -D warnings
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
cargo test --package trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')"
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
cd crates/trusted-server-js/lib
npm ci
npm run lint
npx vitest run
npm run format
npm run build
cd ../../..
cd docs
npm ci
npm run lint
npm run format
npm run build
cd ..
```

Also run Task 18's rustdoc matrix, all four Task 15 smokes/evidence paths, and the integration workflow's runnable smoke targets.

- [ ] **Step 3: Confirm the hosted check topology provisionally**

Confirm CodeQL with rc triggers, all format jobs, all seven `test.yml` jobs,
all four `integration-tests.yml` jobs, release builds, JS build/test, docs build,
and the new WP8 jobs report with the expected names and GitHub App identities.
Record provisional run URLs and the current head, but do not call this the final
hosted proof because Step 8 may create one last evidence commit. Local
substitutes do not replace hosted evidence.

- [ ] **Step 4: Prove every acceptance surface**

Record: generated no-diff; source/disposition equality; all retired/privacy scans; dead-link negatives; route/settings/integration equality; snippet diagnostics; README/JSDoc/rustdoc gates; Pages containment/CNAME smokes; all follow-up issue URLs/dispositions; PR (b), (c), and (d) URLs; c2 issue/owner; e issue plus reviewed runbooks.

- [ ] **Step 5: Activate required checks on rc**

Require the full WP8 suite on `rc/202608`, record context names/source apps/bypass policy, and demonstrate one planted failure actually prevents an rc merge. Remove the planted failure and show success.

- [ ] **Step 6: Reprove the active `main` controller**

Record strict/up-to-date protection, every pre-existing PR seed result, unauthorized protected-file block, unrelated-PR success, and full-suite `main` deferral. Do not claim the full suite is required on `main` yet.

- [ ] **Step 7: Review commit/package shape**

Require one reviewable commit or small series per package, generated-output commits separated where review needs it, no unrelated runtime changes, and no squash-on-merge. Run `git diff --check` and review `git diff origin/rc/202608...HEAD` path by path.

- [ ] **Step 8: Commit the final Epoch 1 records and prove a clean tree**

```bash
git add docs/internal/audits/documentation-refresh-evidence.md docs/internal/audits/documentation-refresh-decisions.md
git diff --cached --check
git commit -m "Record documentation refresh acceptance"
git status --porcelain
```

Expected: the status command prints nothing. If the final records do not
change, omit the empty commit but still require the clean status.

- [ ] **Step 9: Re-run hosted checks on the exact final head and mark implementation-ready**

After Step 8, record the new exact PR head in the PR description and require
every hosted check named in Step 3 to report green on that SHA. Record the
final run URLs and app identities in the PR description, which can be updated
without advancing the commit. Reassert the rc baseline and clean-tree proof.
Only then may the owner mark PR (a) #1049 implementation-ready. All approvals
must be current and the PR ready to merge. Record that activation still
requires the rc merge plus Task 21.

### Task 21: Execute Epoch 2 activation PR (c2)

**Files on the isolated `main` branch:**

- Modify: `.github/workflows/docs-links.yml`
- Modify: `.github/dependabot.yml`

**External system of record:** the named c2 tracking issue and c2 PR timeline.
The Epoch 1 repository evidence ledger already contains the issue URL and
evidence schema. Do not create an evidence-only branch or advance rc merely to
store receipts that exist only after c2 merges. Use append-only issue comments
under the Task 1 schema: paste the redacted request/response and graph/ruleset
captures with body hashes and exact SHAs; links alone are not evidence.

- [ ] **Step 1: Record and authenticate the rc merge**

After #1049 merges, record its exact merge result as `merged_rc_tip` and initial `validated_rc_tip`. Require the latter equals current `origin/rc/202608`; if rc advanced, audit the full delta and record a new exact `validated_rc_tip`.

- [ ] **Step 2: Dispatch read-only post-merge validation**

Dispatch `.github/workflows/docs-links.yml` at `ref: main` with `validate_rc` and exact `tool_sha`. Assert the run's `github.sha` was the authenticated current `main` tip and no writer/attestation job ran.

- [ ] **Step 3: Cut c2 from a fresh `main` tip**

Record c2's own `audited_main_tip`. Apply only the reviewed c2 patch: weekly
schedule, rc-targeted Dependabot version roots, snapshot reader/writer, and
`refresh_dependency_snapshot`. Require both resulting protected paths,
including modes and blob IDs, to equal their counterparts at exact
`validated_rc_tip`; workflow-only equality is not sufficient.

- [ ] **Step 4: Stage, statically validate, and commit the exact c2 candidate**

Use a detached trusted-tool worktree at exact `validated_rc_tip`. In the c2
worktree run:

```bash
git add .github/workflows/docs-links.yml .github/dependabot.yml
git diff --cached --check
git diff --cached --name-status "$AUDITED_MAIN_TIP"
git diff --quiet
git ls-files --stage .github/workflows/docs-links.yml .github/dependabot.yml
git ls-tree "$VALIDATED_RC_TIP" .github/workflows/docs-links.yml .github/dependabot.yml
test "$(git -C "$TRUSTED_RC_WORKTREE" rev-parse HEAD)" = "$VALIDATED_RC_TIP"
cargo run --manifest-path "$TRUSTED_RC_WORKTREE/tools/docs-parity/Cargo.toml" -- workflow validate-local-index --repository "$C2_WORKTREE" --base "$AUDITED_MAIN_TIP" --candidate-kind c2
git commit -m "Activate documentation automation"
```

Expected: cached name/status is exactly the two protected files, byte/count
budgets pass, the two `ls-*` outputs have identical modes/blob IDs by path, and
the trusted validator accepts the reviewed c2 shape without executing candidate
content. Record the commit SHA before opening the PR.

- [ ] **Step 5: Let the automatic gate publish pending**

Open c2 and record that the base controller authenticates the candidate set but publishes `docs/automation-delta: pending`, never success.

- [ ] **Step 6: Run exact dual-ref validation**

Dispatch `validate_main_pr` at `ref: main` with exact `validated_rc_tip`, c2 head, PR number, and base SHA. Require the local uncapped diff and AST policy pass; verify the manual run replaces pending with success on exactly that head/base.

- [ ] **Step 7: Reassert head/base and merge**

Immediately before merge, require current PR head/base equal the validated pair and the base equals recorded `audited_main_tip`. Merge without extra paths.

- [ ] **Step 8: Submit and verify the first dependency snapshot**

Dispatch `refresh_dependency_snapshot` with no SHA/PR inputs. Record in the c2
tracking issue the authenticated rc SHA/ref, fixed detector/correlator,
external snapshot ID, 201 receipt, dependency-graph visibility, and
alert-triage owner/runbook/two-business-day SLA. Paste and hash the redacted
request, 201 response, and graph API capture; link the exact workflow run for
navigation.

- [ ] **Step 9: Observe a real scheduled run**

Require a genuine cron run (not manual emulation) to complete the link reader,
schedule-only issue writer, snapshot reader/writer, and same-identity
reconciliation. Record timeouts/concurrency behavior and resulting
issue/snapshot state in the c2 tracking issue and link the run; no repository
evidence commit follows.

- [ ] **Step 10: Prove rollback readiness**

Validate the reverse-c2 patch for both protected files against the current base,
including restored mode/blob IDs. If activation is unhealthy, merge only that
inverse, stop resubmission, drain/cancel prior snapshot runs, submit an empty
same-identity snapshot, and reopen the activated milestone until a repaired c2
passes again.

- [ ] **Step 11: Mark this refresh activated**

Activation closes this refresh. Do not claim lifecycle closure; Task 22 remains release-owned.

### Task 22: Hand off the Epoch 3 release lifecycle

**Files:**

- Use: `docs/internal/runbooks/documentation-automation-release.md`
- Use: `docs/internal/runbooks/patches/docs-links-release-retarget.patch`
- Use: `docs/internal/runbooks/patches/docs-links-release-disable.patch`
- Modify later in PR (e): `.github/workflows/docs-links.yml`
- Modify later in PR (e): `.github/dependabot.yml`

**External system of record:** the named release-handoff issue and PR (e)
timeline. Post-merge retirement, ruleset, graph, and deletion receipts are
captured there under the Task 1 append-only schema, with exact SHAs, redacted
bodies, statuses, and SHA-256 hashes; URLs are navigation only. No follow-on
repository evidence PR is part of this lifecycle.

- [ ] **Step 1: Verify the tracked issue, owner, and both reviewed paths exist before activation closes**

The normal path retargets to `main` and removes temporary snapshot jobs/refresh. The abandonment path removes the workflow, schedule, Dependabot entries, snapshot jobs, and refresh; it never points automation at missing tooling.

- [ ] **Step 2: At release, freeze and authenticate rc**

Pause queue/direct writes, record bypass policy and exact tip, audit any delta into `validated_rc_tip`, and recheck the freeze immediately before validation, retirement, and deletion.

- [ ] **Step 3: Prove the normal release merge is net-empty on protected paths**

On the normal path, authenticate the current-main base and frozen rc head and
compare modes/blob IDs for `.github/workflows/docs-links.yml` and
`.github/dependabot.yml`; require both identical. Demonstrate that the
base-vs-head protected delta is empty while the three-dot history may contain
the earlier independent copies, then merge the rc release PR with
`docs/automation-delta` green. If either path differs, stop and use a separately
reviewed sync/repair PR before retrying. On abandonment, record this step as
inapplicable and do not merge rc.

- [ ] **Step 4: Open the concrete PR (e) from the matching template**

Record a fresh `audited_main_tip`; use the active base controller's automatic pending state and exact dual-ref manual validation. Reassert head/base before merge.

- [ ] **Step 5: Drain and retire the temporary snapshot**

After (e) stops resubmission, enumerate every queued/in-progress same-identity run, wait or cancel with a separate `actions: write` token, and submit an empty snapshot at the exact (e) merge SHA/ref `refs/heads/main`. Record the 201 receipt and prove it is the final submission.

Attach the run enumeration, cancellation/drain result, request body hash, 201
response, and final-submission proof to the release-handoff issue.

- [ ] **Step 6: Close branch and protection state in order**

Normal: verify automatic `main` dependency parsing, activate the deferred full
WP8 suite on `main`, and run both a permitted action-repin maintenance fixture
and an AST-weakening rejection through `validate_main_maintenance` using the
authenticated current-main tool. Keep `docs/automation-delta` required,
re-smoke Pages/CNAME, then delete rc. Abandonment: remove the nonreporting
automation-delta required context immediately after (e), prove the next `main`
PR is not stranded, then delete rc. Never delete rc first.

Attach the final graph, ruleset, live-site, and branch-deletion receipts to the
release-handoff issue and close it only after the selected path is complete.

## Final plan-to-spec traceability

| Spec surface                            | Plan tasks |
| --------------------------------------- | ---------- |
| Owner questions and immutable tips      | 1          |
| WP1 containment/CNAME/hygiene           | 2–4        |
| WP8a tool, manifests, controller design | 5–10       |
| `main` validation controller PR (c)     | 11         |
| WP2 truth pass                          | 12         |
| WP3 configuration                       | 13         |
| WP4 API/routes                          | 14         |
| WP5 deployment and coverage             | 15–16      |
| WP6 root/crate docs                     | 17         |
| WP7 in-code docs                        | 18         |
| WP8b blocking enforcement               | 19         |
| Epoch 1 / PR (a)                        | 20         |
| Epoch 2 / activation PR (c2)            | 21         |
| Epoch 3 / release PR (e)                | 22         |

The implementation is complete only at Task 21's **activated** milestone. Task 22 is deliberately specified and owned but is not part of this refresh's implementation completion claim.

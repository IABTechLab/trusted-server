# Documentation Refresh (Full Surface) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refresh every maintained documentation surface, derive reader-facing inventories from checked records, and commit all implementation work to PR #1049 without exposing write credentials to pull-request-controlled code.

**Architecture:** The existing `spec-docs-refresh` branch and PR #1049 are the only implementation branch and PR. Reviewable package commits build one standalone `docs-parity` tool and final-state documentation automation; Pages, schedules, dependency submission, and optional `main` protection effects that cannot run from rc are recorded as release-pending rather than represented by auxiliary PRs.

**Tech Stack:** Rust 1.95 (`syn`, Serde, `error-stack`, Cargo), VitePress/Node 24, ESLint/JSDoc, GitHub Actions and REST APIs, shell smoke scripts, Fastly Viceroy, Wrangler, Spin, Axum

**Revised:** 2026-08-31 for the approved single-PR delivery model

---

## Execution gate

- Work only in `/Users/ag/projects/iab/trusted-server/.claude/worktrees/spec-docs-refresh` on branch `spec-docs-refresh`.
- Push implementation commits only to PR #1049. Do not create containment, CNAME, controller, activation, or release-handoff implementation PRs.
- PR #1104 is closed. Its source branch remains only long enough to transfer reviewed commits `34b0613dc603ba6529396dad4dd4b7e68b1e11a9` and `e6554f24f58f6122fb806ce25432f66033765c65`.
- Before every package, fetch `origin/rc/202608` and require it to equal `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`; require the implementation branch to contain that commit. Any target advance stops execution for a full delta audit and spec/plan re-review.
- Owner decisions are fixed: delete CNAME, archive `FAQ_POC.md`, use the factual-governance fallback, and expire the Fastly service-ID exception at `2026-09-30T00:00:00Z`.
- Live Pages, real scheduled runs, dependency submission, graph visibility, and optional `main` protection changes are release-pending. Never substitute local output for those receipts or create another PR to obtain them.

## File map

### Program records

| File                                                                | Responsibility                                                                    |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `docs/superpowers/specs/2026-08-19-documentation-refresh-design.md` | Approved single-PR design and immutable baseline.                                 |
| `docs/superpowers/plans/2026-08-30-documentation-refresh.md`        | This package-by-package execution plan.                                           |
| `docs/internal/audits/documentation-refresh-decisions.md`           | Owner decisions, exact #1049 identity, closed #1104 transfer, and release bounds. |
| `docs/internal/audits/documentation-refresh-inventory.toml`         | Per-file or per-region WP2 dispositions and source anchors.                       |
| `docs/internal/audits/documentation-refresh-evidence.md`            | Package evidence, hosted runs, smokes, issues, and release-pending schema.        |
| `docs/internal/runbooks/documentation-automation-release.md`        | Post-main Pages, schedule, snapshot, graph, and optional protection verification. |

### `docs-parity` crate and checked records

`tools/docs-parity` is a standalone Cargo workspace with its own committed lockfile; it is not added to the repository workspace members.

| File                                           | Responsibility                                                                                        |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `tools/docs-parity/Cargo.toml`                 | Standalone binary/library metadata, `[workspace]`, dependencies, and lint policy.                     |
| `tools/docs-parity/Cargo.lock`                 | Reproducible host-tool dependency graph.                                                              |
| `tools/docs-parity/README.md`                  | Subcommands, manifest ownership, update/check flow, and failure semantics.                            |
| `tools/docs-parity/src/main.rs`                | Thin CLI parsing and exit-code mapping.                                                               |
| `tools/docs-parity/src/lib.rs`                 | Subcommand dispatch and shared `Report<DocsParityError>` API.                                         |
| `tools/docs-parity/src/model.rs`               | Checked schemas, ownership/expiry types, and generated markers.                                       |
| `tools/docs-parity/src/repository.rs`          | Root discovery, tracked files, safe paths, Git object reads, and atomic writes.                       |
| `tools/docs-parity/src/classification.rs`      | Text/binary classification and exhaustive candidate/span closure.                                     |
| `tools/docs-parity/src/scanner.rs`             | Domain, email, credential, identifier, encoded-token, lockfile, binary-string, and metadata scanning. |
| `tools/docs-parity/src/markdown.rs`            | Links, anchors, fences, ownership markers, orphan/tombstone checks, and generated regions.            |
| `tools/docs-parity/src/settings.rs`            | Serde-aware settings extraction, companions, compiled probes, and template harness.                   |
| `tools/docs-parity/src/integrations.rs`        | Integration/provider inventory and behavioral capability checks.                                      |
| `tools/docs-parity/src/routes.rs`              | Route records, Cloudflare fail-closed parser, and adapter-support rendering.                          |
| `tools/docs-parity/src/cli_help.rs`            | Native Linux/macOS capture, annotated union, overrides, and goldens.                                  |
| `tools/docs-parity/src/snippets.rs`            | Fence modes, diagnostics, isolated execution, and waiver expiry.                                      |
| `tools/docs-parity/src/gates.rs`               | Canonical gate manifest and generated/link-only consumers.                                            |
| `tools/docs-parity/src/workflow.rs`            | YAML policy for read-only PR checks and final default-branch readers/writers.                         |
| `tools/docs-parity/src/dependency_snapshot.rs` | Bounded dependency snapshot schema and deterministic generation.                                      |

Checked records live under `tools/docs-parity/manifests/`: `tracked-files.toml`, `maintained-sources.toml`, `sensitive-allowlist.toml`, `retired-identifiers.toml`, `snippets.toml`, `settings-companions.toml`, `routes.toml`, `integrations.toml`, `adapter-support.toml`, `cli-overrides.toml`, `gates.toml`, `pages.toml`, `diagrams.toml`, and `orphans.toml`. CLI goldens live at `tools/docs-parity/goldens/cli-linux.txt` and `tools/docs-parity/goldens/cli-macos.txt`. Synthetic fixtures live under `tools/docs-parity/tests/fixtures/`; never add a live secret, internal contact, or real customer value.

### Existing product/documentation surfaces

- Publishing/policy: `docs/.vitepress/config.mts`, `docs/guide/index.md`, `docs/internal/onboarding.md`, `docs/business-use-cases.md`, `docs/public/CNAME`, `docs/package.json`, `docs/package-lock.json`, `fastly.toml`, `CLAUDE.md`, `AGENTS.md`, `.github/pull_request_template.md`, and `.claude/commands/{check-ci,review-changes,test-all,test-crate,verify}.md`.
- Truth/config/API/product: the exact WP2-WP5 paths enumerated in Tasks 10-14.
- README/rustdoc/JSDoc: the WP6/WP7 paths enumerated in Tasks 15-16.
- Automation: `.github/workflows/{codeql,deploy-docs,docs-links,format,integration-tests,test}.yml`, `.github/dependabot.yml`, `.tool-versions`, and `crates/trusted-server-openrtb-codegen/Cargo.toml`.

## Package checkpoint rule

1. Fetch and reassert the immutable rc tip. Record `package_start_head="$(git rev-parse HEAD)"` before editing.
2. Execute one test-first leaf at a time: add one named failing fixture/assertion, run the focused red command and record its diagnostic, implement the minimum change, then rerun the focused command and immediate regressions.
3. Fully stage only the package allowlist. Never use `git add -N` or directory-wide staging. Review `git diff --cached --name-status "$package_start_head"`. Require `git ls-files --others --exclude-standard` to print nothing and `git diff --quiet` to exit 0.
4. From Task 5 onward, regenerate and stage `tracked-files.toml` and `maintained-sources.toml` whenever a tracked path is created, moved, or deleted. Public-page changes also regenerate page/orphan records. Tasks 1-4 are bootstrap exceptions because Task 5 creates the classification records.
5. Run focused tests, regenerate checked outputs, restage exact output paths, run `docs-parity check` where available, and prove a second generation is byte-stable.
6. Append commands/results to `documentation-refresh-evidence.md`. Restage that exact file, rerun any scanner/classifier consuming it, and reassert no unstaged bytes.
7. Run `git diff --cached --check` and inspect the cached content. Commit with the exact task message. If final identifiers require a receipt, create one immediately adjacent evidence-only commit; do not create recursive self-SHA receipts.
8. Push the clean package commits to `origin/spec-docs-refresh` so PR #1049 is the only hosted review surface. Require clean status before advancing.

### Atomic execution rule

Composite parser, scanner, workflow, settings, route, and smoke tasks are packages, not single coding actions. Copy each named negative fixture into the evidence checklist and complete its red/green cycle before the next leaf. Never batch multiple parser or trust classes into one unreviewed implementation change.

### Task 1: Align program records to the single PR

**Files:**

- Modify: `docs/superpowers/specs/2026-08-19-documentation-refresh-design.md`
- Modify: `docs/internal/audits/documentation-refresh-decisions.md`
- Modify: `docs/internal/audits/documentation-refresh-evidence.md`

- [ ] **Step 1: Revalidate PR #1049 and the immutable target**

Run:

```bash
git fetch origin rc/202608 spec-docs-refresh
test "$(git rev-parse origin/rc/202608)" = 07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf
git merge-base --is-ancestor 07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf HEAD
gh pr view 1049 --json url,state,isDraft,baseRefName,baseRefOid,headRefName,headRefOid
```

Expected: exact target SHA, ancestry success, and PR #1049 open from `spec-docs-refresh` to `rc/202608`. Record the current remote head as a timestamped capture, not as a permanent final SHA.

- [ ] **Step 2: Replace obsolete delivery records**

Make #1049 the only implementation row. Record PR #1104 as closed/superseded with its two reviewed source commits and no live merge/deploy receipt. Remove executable fields for PRs (b), (c), (d), (c2), and (e), all Epoch terminology, temporary protection/status changes, snapshot retirement, and cross-worktree import blocks.

- [ ] **Step 3: Define release-pending evidence**

Keep the durable hashed-body schema, but use it only for real external captures. Add explicit `release-pending` rows for Pages/CNAME, first schedule, dependency submission/graph, and optional `main` protection. State that local/fixture output cannot complete those rows.

- [ ] **Step 4: Mark the reviewed spec executable**

Change the spec status to approved for implementation and record the written-spec approval date and owner. Do not change WP scope.

- [ ] **Step 5: Verify and commit**

Run `cd docs && npm run format && npm run lint && npm run build`, remove only generated VitePress temp output, then run `git diff --check`.

Stage exactly the three files and commit:

```bash
git add docs/superpowers/specs/2026-08-19-documentation-refresh-design.md docs/internal/audits/documentation-refresh-decisions.md docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Align documentation refresh records to one PR"
```

### Task 2: Transfer the reviewed containment commits

**Files:**

- Modify: `docs/.vitepress/config.mts`
- Modify: `docs/guide/index.md`
- Delete: `docs/guide/onboarding.md`
- Create: `docs/internal/onboarding.md`
- Modify: `docs/internal/audits/documentation-refresh-evidence.md`

- [ ] **Step 1: Authenticate the closed source PR and commits**

Require PR #1104 closed, base `d516a9e94249e10cbc36e41beb4269f9255cf407`, and source commits `34b0613dc603ba6529396dad4dd4b7e68b1e11a9` and `e6554f24f58f6122fb806ce25432f66033765c65`. Verify their combined base-to-head path set is exactly the four paths above.

- [ ] **Step 2: Transfer the two commits**

Cherry-pick the commits in order. Any conflict outside the four authorized paths stops execution. Resolve an authorized-path conflict only by preserving the reviewed containment behavior on the rc version; record the conflict and resulting blob comparison.

```bash
git cherry-pick 34b0613dc603ba6529396dad4dd4b7e68b1e11a9
git cherry-pick e6554f24f58f6122fb806ce25432f66033765c65
```

Expected commit subjects: `Contain internal documentation pages` and `Fix internal onboarding links`.

- [ ] **Step 3: Reprove containment on rc**

Run `cd docs && npm ci && npm run lint && npm run format && npm run build`. Assert no output for the six excluded families, required Home/Guide/API artifacts with expected content, no excluded hrefs, and every repository-relative onboarding target exists.

- [ ] **Step 4: Record the transfer**

Append source PR URL/state, source base/head, original and resulting commit SHAs, exact path set, commands, and local-only status. Commit only the evidence ledger:

```bash
git add docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Record documentation containment transfer"
```

### Task 3: Complete WP1 CNAME and policy hygiene

**Files:**

- Delete: `docs/public/CNAME`
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

- [ ] **Step 1: Add failing policy assertions**

Prove the banner, package privacy/license, empty authors, fixture labels, KV comments, canonical gate link, generated AGENTS region, exception taxonomy, and CNAME deletion are absent or stale.

- [ ] **Step 2: Delete the selected CNAME**

Remove `docs/public/CNAME` and retain `base: '/trusted-server'`. Assert no tracked placeholder remains and build assets use the project path. Commit the exact deletion:

```bash
git add -A -- docs/public/CNAME
git diff --cached --check
git commit -m "Resolve documentation site domain"
```

- [ ] **Step 3: Apply policy and hygiene edits**

Add the unverified marketing banner; scrub `fastly.toml` while preserving only the expiring service-ID record; set docs package private/Apache-2.0 and refresh lock metadata; add the typed exception taxonomy to CLAUDE; make command files and PR template link-only; generate the AGENTS fallback region.

- [ ] **Step 4: Prove privacy and policy state**

Search all tracked files for removed contacts, handles, channels, access phrases, placeholder CNAME, and prohibited exception shapes. Expected: no match outside a typed, unexpired decision entry.

- [ ] **Step 5: Verify and commit**

Run `cd docs && npm ci && npm run lint && npm run format && npm run build`, exact included/excluded artifact assertions, `git diff --check`, and the package checkpoint checks. Record live Pages/CNAME as release-pending.

```bash
git add docs/business-use-cases.md fastly.toml docs/package.json docs/package-lock.json CLAUDE.md AGENTS.md .github/pull_request_template.md .claude/commands/check-ci.md .claude/commands/review-changes.md .claude/commands/test-all.md .claude/commands/test-crate.md .claude/commands/verify.md docs/internal/audits/documentation-refresh-evidence.md docs/internal/audits/documentation-refresh-decisions.md
git commit -m "Clean documentation publishing policy"
```

### Task 4: Scaffold the standalone `docs-parity` crate

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

### Task 5: Close tracked-file classification and sensitive-data scanning

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
- Modify: `tools/docs-parity/README.md`

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
cargo test --manifest-path tools/docs-parity/Cargo.toml --test classification
cargo test --manifest-path tools/docs-parity/Cargo.toml --test scanner
cargo test --manifest-path tools/docs-parity/Cargo.toml --test cli
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo run --manifest-path tools/docs-parity/Cargo.toml -- classify --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- scan --check
```

Expected: synthesized violations fail for the intended diagnostic; the repository scan passes only with typed, unexpired entries.

- [ ] **Step 7: Commit the closed universe**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/README.md tools/docs-parity/src/classification.rs tools/docs-parity/src/scanner.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/sensitive-allowlist.toml tools/docs-parity/manifests/retired-identifiers.toml tools/docs-parity/tests/classification.rs tools/docs-parity/tests/scanner.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Enforce documentation source classification"
```

### Task 6: Implement generated regions, Markdown ownership, and link checks

**Files:**

- Modify as dependencies are introduced: `tools/docs-parity/Cargo.toml`
- Modify as dependencies are introduced: `tools/docs-parity/Cargo.lock`
- Modify: `tools/docs-parity/README.md`
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
- Modify for renderer-accurate public anchor: `docs/guide/error-reference.md`
- Modify for exact verification/staging scope: `docs/superpowers/plans/2026-08-30-documentation-refresh.md`
- Modify for package receipts: `docs/internal/audits/documentation-refresh-evidence.md`
- Modify if scanner offsets change: `tools/docs-parity/manifests/sensitive-allowlist.toml`

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
cargo test --manifest-path tools/docs-parity/Cargo.toml --test markdown
cargo test --manifest-path tools/docs-parity/Cargo.toml --test links
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --local --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- classify --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- scan --check
git diff --quiet -- Cargo.lock
git diff --check
```

Expected: focused tests and current local repository checks pass; external network checks remain scheduled/manual, not a required per-PR network gate.

- [ ] **Step 7: Commit**

```bash
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/README.md tools/docs-parity/src/markdown.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/sensitive-allowlist.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/diagrams.toml tools/docs-parity/manifests/orphans.toml tools/docs-parity/tests/markdown.rs tools/docs-parity/tests/links.rs docs/guide/error-reference.md docs/superpowers/plans/2026-08-30-documentation-refresh.md docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Harden documentation Markdown contracts"
```

### Task 7: Extract settings semantics and execute the example harness

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

### Task 8: Check integration capabilities and adapter routes

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

### Task 9: Check CLI help, snippets, gates, and final workflow foundations

**Files:**

- Modify: `tools/docs-parity/Cargo.toml`
- Modify: `tools/docs-parity/Cargo.lock`
- Create: `tools/docs-parity/src/cli_help.rs`
- Create: `tools/docs-parity/src/snippets.rs`
- Create: `tools/docs-parity/src/gates.rs`
- Create: `tools/docs-parity/src/workflow.rs`
- Create: `tools/docs-parity/src/dependency_snapshot.rs`
- Modify: `tools/docs-parity/src/{main,lib,model,repository}.rs`
- Create: `tools/docs-parity/manifests/{cli-overrides,snippets,gates}.toml`
- Modify: `tools/docs-parity/manifests/{tracked-files,maintained-sources}.toml`
- Create: `tools/docs-parity/goldens/{cli-linux,cli-macos}.txt`
- Create/Test: `tools/docs-parity/tests/{cli_help,snippets,gates,workflow,dependency_snapshot}.rs`
- Modify: `.github/workflows/test.yml`
- Create: `.github/workflows/docs-links.yml`
- Modify: `docs/internal/audits/documentation-refresh-evidence.md`

- [ ] **Step 1: Write CLI capture and snippet failures**

Cover recursive help, host-OS detection with no caller platform override, missing native capture provenance, stale overrides, every snippet mode, wrong failure phase/diagnostic, missing classification, expired waiver, and formerly invalid examples becoming valid.

- [ ] **Step 2: Add native capture CI**

Add a permanent Linux/macOS PR matrix job that checks out the exact PR head, runs the same capture command, records runner/`uname`/Rust/Node/source SHA metadata, and uploads bounded raw artifacts. Commit capture-ready code before generating goldens and push that commit to #1049.

- [ ] **Step 3: Import authenticated goldens**

Download both artifacts from the same hosted run and exact source SHA. Verify hashes and provenance, import through the deterministic tool command, never hand-edit the goldens, and prove a second import is unchanged.

- [ ] **Step 4: Implement snippets and canonical gates**

Define every command once with runner/target/mode; generate checked regions or enforce link-only consumers. Execute fences in isolated working directories and require stable phase/diagnostic matches.

- [ ] **Step 5: Write final-workflow security fixtures first**

Positive fixtures: ordinary read-only PR validation, scheduled clean/finding link paths, issue dedup/auto-close, dependency generation/submission, and closed manual refresh. Negative fixtures: `pull_request_target`, `merge_group`, status write, caller tool/SHA input, privileged PR checkout, unpinned action, expanded permissions, unsafe cache/service/local action, stale source, extra path/member, traversal, unsafe mode/symlink, mixed inputs, malformed/oversized artifacts, unknown schema fields, and write job executing repository code.

- [ ] **Step 6: Implement workflow and snapshot policy**

Parse YAML as data. Require read-only PR jobs, full action SHA pins, default-deny permissions, separated no-checkout writers, fixed concurrency/timeouts, exact archive member names, schema closure, and authenticated source SHA. Link bounds: 2 MiB archive, 1 MiB JSON, 500 findings, 2,048-byte strings. Snapshot bounds: 4 MiB archive, 2 MiB JSON, 5,000 records, 2,048-byte strings.

- [ ] **Step 7: Materialize the final workflow foundation**

Create `docs-links.yml` directly in final-state shape: ordinary read-only PR validation; default-branch schedule; split link reader/issue writer; split snapshot reader/writer; and no-input manual refresh. It contains no temporary rc target, controller attestation, protected-file lifecycle, or caller-selected executable.

- [ ] **Step 8: Verify and commit in two adjacent checkpoints**

First commit capture-ready sources and workflow foundation:

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml cli_help
cargo test --manifest-path tools/docs-parity/Cargo.toml snippets
cargo test --manifest-path tools/docs-parity/Cargo.toml gates
cargo test --manifest-path tools/docs-parity/Cargo.toml workflow
cargo test --manifest-path tools/docs-parity/Cargo.toml dependency_snapshot
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
git add tools/docs-parity/Cargo.toml tools/docs-parity/Cargo.lock tools/docs-parity/src/cli_help.rs tools/docs-parity/src/snippets.rs tools/docs-parity/src/gates.rs tools/docs-parity/src/workflow.rs tools/docs-parity/src/dependency_snapshot.rs tools/docs-parity/src/main.rs tools/docs-parity/src/lib.rs tools/docs-parity/src/model.rs tools/docs-parity/src/repository.rs tools/docs-parity/manifests/cli-overrides.toml tools/docs-parity/manifests/snippets.toml tools/docs-parity/manifests/gates.toml tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/tests/cli_help.rs tools/docs-parity/tests/snippets.rs tools/docs-parity/tests/gates.rs tools/docs-parity/tests/workflow.rs tools/docs-parity/tests/dependency_snapshot.rs .github/workflows/test.yml .github/workflows/docs-links.yml docs/internal/audits/documentation-refresh-evidence.md
git diff --cached --check
git commit -m "Add documentation enforcement foundations"
git push origin spec-docs-refresh
```

After the native artifacts return, stage only the goldens, regenerated tracked/source manifests, and evidence:

```bash
git add tools/docs-parity/goldens/cli-linux.txt tools/docs-parity/goldens/cli-macos.txt tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml docs/internal/audits/documentation-refresh-evidence.md
git diff --cached --check
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
git commit -m "Record cross-platform CLI help goldens"
```

### Task 10: Complete WP2 truth pass and dispositions

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
- Modify: `docs/.vitepress/config.mts`
- Create: `docs/guide/auction-testing.md`
- Modify: `crates/trusted-server-core/src/auction/README.md`
- Modify: `TESTING.md`
- Retire/Move/Modify: `FAQ_POC.md`
- Create only on FAQ archive path: `docs/superpowers/archive/FAQ_POC.md`
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
reopened and this plan is amended and re-approved. The rejected retire and rewrite alternatives are not executable plan branches. Remove the root path from the active-repo inventory and add no active FAQ page or route. Independently replace GAM/Kargo with route-preserving
tombstones, remove sidebar reachability, and add old-route/tombstone smokes to
`pages.toml`.

- [ ] **Step 5: Rewrite testing and operator records**

Make root `TESTING.md` the test-matrix index, move the verified auction runbook into `docs/guide/auction-testing.md`, normalize the deterministic no-release CHANGELOG form, distinguish runtime env from CLI overlay, fix roadmap status, and repair the three known workflow/script comments.

Record whether operator-visible CHANGELOG entries are complete for the audited range; if an entry is intentionally omitted, record the exact exclusion and source anchor. Formatting alone does not satisfy this check.

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

Assert that `docs/.vitepress/dist/guide/faq.html` and `/guide/faq` navigation are absent on the selected archive path.

Expected: set equality passes; retired terms are absent from active sets with only the spec-defined historical exceptions; every executable fence has a valid manifest entry and diagnostic.

- [ ] **Step 8: Run regression tests for any non-doc fixture changed**

If `html_processor.test.html` changes, run `cargo test-fastly html_processor`. Run focused tests for every other non-Markdown fixture touched.

- [ ] **Step 9: Commit WP2**

Stage the common WP2 paths first, then the selected archive branch and only the
conditional fixture paths that actually changed:

```bash
git add docs/internal/audits/documentation-refresh-inventory.toml docs/internal/audits/documentation-refresh-evidence.md docs/guide/ad-serving.md docs/guide/architecture.md docs/guide/configuration.md docs/guide/creative-processing.md docs/guide/error-reference.md docs/guide/integration-guide.md docs/guide/roadmap.md docs/guide/integrations/gam.md docs/guide/integrations/kargo.md docs/guide/auction-testing.md docs/.vitepress/config.mts TESTING.md CHANGELOG.md .env.example .env.dev .claude/agents/code-architect.md .claude/agents/issue-creator.md .github/workflows/test.yml scripts/test-cli.sh crates/trusted-server-core/src/auction/README.md crates/trusted-server-openrtb/generate.sh tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/sensitive-allowlist.toml tools/docs-parity/manifests/retired-identifiers.toml tools/docs-parity/manifests/snippets.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/orphans.toml
git add -A -- FAQ_POC.md docs/superpowers/archive/FAQ_POC.md
# Only when the scanner required this fixture edit:
git add crates/trusted-server-core/src/html_processor.test.html
git commit -m "Correct maintained documentation truth"
```

If the mechanical inventory selects another human-facing comment path, add
that one exact path to the reviewed list before running the checkpoint—never
replace this list with `git add docs`, `git add .github`, or another directory.

### Task 11: Complete WP3 configuration reference and template

**Files:**

- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/cli.md`
- Modify: `trusted-server.example.toml`
- Modify: `tools/docs-parity/manifests/settings-companions.toml`
- Modify: `tools/docs-parity/manifests/snippets.toml`
- Modify: PR #1049 description through GitHub API/CLI

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
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo test-fastly config
cd docs && npm run lint && npm run format && npm run build
```

Expected: every active canonical field appears in reference/template, noncanonical paths are labeled only, all literal consumers remain connected, and all eight example-harness phases pass.

- [ ] **Step 6: Commit WP3**

```bash
git add docs/guide/configuration.md docs/guide/cli.md trusted-server.example.toml tools/docs-parity/manifests/settings-companions.toml tools/docs-parity/manifests/snippets.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Complete configuration documentation"
```

- [ ] **Step 7: Push and publish the Appendix B parity checklist to #1049**

Push the clean WP3 commit to `origin/spec-docs-refresh`, then update only the bounded `<!-- docs-refresh:settings-parity:start -->` / `<!-- docs-refresh:settings-parity:end -->` region of PR #1049's description. Render the checklist from the checked settings record and include all 17 roots, all 14 deploy IDs, all three provider profile schemas, the directional-disposition axes, the secret classifications, and the exact WP3 check results. Read the description back through the GitHub API/CLI and require the rendered rows to equal the checked record; preserve every unrelated PR-description section.

Append the readback timestamp, PR body hash, and equality result to `documentation-refresh-evidence.md`; commit it immediately as `Record configuration checklist publication` and push it. Do not include the receipt commit's own SHA in its body.

### Task 12: Complete WP4 generated API contracts

**Files:**

- Modify: `docs/guide/api-reference.md`
- Modify: `tools/docs-parity/manifests/routes.toml`
- Modify: `tools/docs-parity/manifests/adapter-support.toml`
- Modify: `tools/docs-parity/manifests/snippets.toml`
- Modify/Test only for test seams: the four adapter `src/app.rs` files and route tests from Task 8

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
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cd docs && npm run lint && npm run format && npm run build
```

Expected: no adapter can add/remove/change a route without a record and generated diff; all ownership markers are present.

- [ ] **Step 5: Commit WP4**

```bash
git add docs/guide/api-reference.md tools/docs-parity/manifests/routes.toml tools/docs-parity/manifests/adapter-support.toml tools/docs-parity/manifests/snippets.toml crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/src/app.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/src/app.rs crates/trusted-server-adapter-spin/tests/routes.rs docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Generate adapter API documentation"
```

### Task 13: Add deployment guides and recurring first-success smokes

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
- Modify: `tools/docs-parity/manifests/snippets.toml`
- Modify: PR #1049 description through GitHub API/CLI

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

Register the four new public pages immediately. Until Task 14 adds their final
navigation, give any genuinely unreachable page a typed temporary orphan entry
owned by WP5 and expiring at Task 14; Task 14 must remove that entry.

- [ ] **Step 6: Wire runnable scripts into integration CI**

Run `chmod +x scripts/smoke-axum.sh scripts/smoke-fastly.sh
scripts/smoke-cloudflare.sh scripts/smoke-spin.sh`. Run Axum, Fastly, and
Cloudflare smokes in the existing integration workflow after their artifacts
are prepared. Consume Wrangler from the Task 13 `.tool-versions` pin, not an
unpinned global latest. Preserve existing integration suites.

- [ ] **Step 7: Verify focused journeys**

```bash
bash -n scripts/smoke-axum.sh scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh
./scripts/smoke-axum.sh
./scripts/smoke-fastly.sh
./scripts/smoke-cloudflare.sh
```

Run Spin or attach its time-bounded evidence. Then run the integration-test parity target and docs build.

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cd docs && npm run lint && npm run format && npm run build
```

- [ ] **Step 8: Commit the deployment half of WP5**

```bash
git add docs/guide/fastly.md docs/guide/cloudflare.md docs/guide/spin.md docs/guide/axum-dev.md scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh scripts/smoke-axum.sh .tool-versions .github/workflows/integration-tests.yml crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml crates/trusted-server-adapter-cloudflare/wrangler.ci.toml tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/pages.toml tools/docs-parity/manifests/orphans.toml tools/docs-parity/manifests/snippets.toml docs/internal/audits/documentation-refresh-evidence.md
git ls-files --stage scripts/smoke-axum.sh scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh | awk '$1 != "100755" { bad=1 } END { exit bad }'
git commit -m "Document adapter deployment journeys"
```

- [ ] **Step 9: Push and publish exact smoke journeys to #1049**

Push the clean deployment-guide commit to `origin/spec-docs-refresh` and wait for its hosted smoke jobs. Then update only the bounded `<!-- docs-refresh:adapter-smokes:start -->` / `<!-- docs-refresh:adapter-smokes:end -->` region of PR #1049's description. For Axum, Fastly, Cloudflare, and Spin, include the exact command sequence invoked by the recurring script, its generated-state and process cleanup procedure, the strong success oracle, the two independent negative cases, and the immutable run URL or time-bounded Spin receipt. Read the description back and require each script's command and cleanup sequence to match its checked snippet records; preserve every unrelated PR-description section.

Append the readback timestamp, PR body hash, run URLs or Spin receipt, and equality result to `documentation-refresh-evidence.md`; commit it immediately as `Record adapter smoke publication` and push it. Do not include the receipt commit's own SHA in its body.

### Task 14: Complete WP5 product coverage and navigation

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
- Read/verify: `.tool-versions` (Wrangler pin established in Task 13)
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

### Task 15: Complete WP6 root and crate documentation

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
- Read/verify: `crates/trusted-server-openrtb/README.md`
- Create: `scripts/README.md`
- Modify: each corresponding crate `Cargo.toml`
- Modify: `crates/trusted-server-openrtb/Cargo.toml`
- Modify: `tools/docs-parity/manifests/tracked-files.toml`
- Modify: `tools/docs-parity/manifests/maintained-sources.toml`
- Modify: `tools/docs-parity/manifests/snippets.toml`

- [ ] **Step 1: Add the failing README equality test**

Use `cargo metadata --no-deps` to enumerate every package. Expected before edits: seven missing README files and ten package manifests missing `readme =` metadata. Include an extra/unlisted README negative fixture.

- [ ] **Step 2: Correct canonical contributor/operator prose**

Make root quick starts satisfy the first-success scripts; make contributing link to canonical gates; correct target/integration-model/example policy in CLAUDE; apply the selected factual-governance fallback.

- [ ] **Step 3: Write responsibility-focused READMEs**

Each crate README states purpose, runtime/target, important boundaries, build/test command, and links to canonical guides without copying volatile matrices. Rewrite core as an actual module overview and correct integration-test scope. Add a scripts index with inputs/side effects/cleanup.

- [ ] **Step 4: Connect Cargo metadata**

Add exact `readme = "README.md"` entries to the seven new crate manifests and any existing package missing the metadata. Do not alter dependency or feature resolution.

- [ ] **Step 5: Verify**

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- readmes --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo metadata --no-deps --format-version 1
cargo fmt --all -- --check
cd docs && npm run lint && npm run format && npm run build
```

Expected: every package maps to an existing README and all active root/crate/skill/agent dispositions remain closed.

- [ ] **Step 6: Commit WP6**

```bash
git add README.md CONTRIBUTING.md CLAUDE.md ProjectGovernance.md crates/trusted-server-core/README.md crates/trusted-server-core/Cargo.toml crates/trusted-server-integration-tests/README.md crates/trusted-server-integration-tests/Cargo.toml crates/trusted-server-adapter-axum/README.md crates/trusted-server-adapter-cloudflare/README.md crates/trusted-server-adapter-fastly/README.md crates/trusted-server-adapter-spin/README.md crates/trusted-server-cli/README.md crates/trusted-server-js/README.md crates/trusted-server-openrtb-codegen/README.md crates/trusted-server-openrtb/Cargo.toml crates/trusted-server-adapter-axum/Cargo.toml crates/trusted-server-adapter-cloudflare/Cargo.toml crates/trusted-server-adapter-fastly/Cargo.toml crates/trusted-server-adapter-spin/Cargo.toml crates/trusted-server-cli/Cargo.toml crates/trusted-server-js/Cargo.toml crates/trusted-server-openrtb-codegen/Cargo.toml scripts/README.md tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/snippets.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Refresh contributor and crate documentation"
```

### Task 16: Complete WP7 rustdoc and JSDoc

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

### Task 17: Activate final CI and release-pending controls

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
- Modify: `tools/docs-parity/src/{gates,workflow,dependency_snapshot}.rs`
- Modify/Test: `tools/docs-parity/tests/{gates,workflow,dependency_snapshot}.rs`
- Modify: `tools/docs-parity/manifests/{tracked-files,maintained-sources,gates,snippets}.toml`
- Modify: `docs/internal/audits/documentation-refresh-evidence.md`

- [ ] **Step 1: Write failing automation fixtures**

Assert missing rc CodeQL triggers, docs-parity jobs, rustdoc/doctest jobs, nested lockfile cache inputs, Node pins, Dependabot roots, Wrangler pin, generated gate equality, action SHA pins, final `main` targets, reader/writer separation, closed manual refresh, and release-pending runbook fields.

- [ ] **Step 2: Wire blocking deterministic checks**

Add host docs-parity fmt/clippy/test/check, generated no-diff, settings/examples/inventory/snippets/scanner/local links/readmes/JSDoc/workflow fixtures, rustdoc matrix, native doctests, docs build, and existing target regression jobs. External network links remain scheduled, not a PR dependency.

- [ ] **Step 3: Normalize existing automation**

Add CodeQL `rc/*` PR triggers, `.tool-versions` deploy paths, exact setup-node lockfile paths, pinned Wrangler, all approved Dependabot roots targeting `main`, and `[lints] workspace = true`. Choose current stable action/tool versions at implementation time, cite their primary release sources, and pin every new `uses` by full SHA.

- [ ] **Step 4: Finalize `docs-links.yml`**

Preserve ordinary read-only PR validation. Finalize weekly `17 9 * * 1` schedule, fixed non-canceling concurrency, 30/20/5-minute timeouts, bounded artifacts, issue dedup/auto-close, fixed snapshot identity, authenticated default-branch SHA, and no-input manual refresh. Require no `pull_request_target`, `merge_group`, status writer, temporary rc target, retirement path, or caller-selected tool.

- [ ] **Step 5: Write the release-pending runbook**

Document exact post-main Pages/CNAME smoke, first scheduled link run, first dependency submission and 201/graph proof, Dependabot/action-pin inspection, alert owner/SLA, and optional branch-protection activation only after contexts report from expected apps. Mark every receipt release-pending; do not create another PR or claim execution from rc.

- [ ] **Step 6: Generate all gate consumers**

Regenerate CLAUDE/AGENTS/TESTING/guide testing from `gates.toml`. Prove command files, CONTRIBUTING, and PR template remain link-only. A second generation produces no diff.

- [ ] **Step 7: Deduplicate all code follow-ups**

Search the tracker for every item in the spec. File or record an exact existing-issue disposition for all twelve, with URL, owner, and labels. Do not collapse distinct adapter-store, config-bridge, health, reserved-field, placeholder, inline-secret, deploy-ID, CLI-help, telemetry, env-store, or staging-blob findings.

- [ ] **Step 8: Run WP8 acceptance and commit**

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- snippets --check
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
cargo fmt --all -- --check
cd docs && npm run lint && npm run format && npm run build
```

Stage exactly the enumerated files and commit:

```bash
git add .github/workflows/format.yml .github/workflows/test.yml .github/workflows/integration-tests.yml .github/workflows/codeql.yml .github/workflows/deploy-docs.yml .github/workflows/docs-links.yml .github/dependabot.yml .tool-versions crates/trusted-server-openrtb-codegen/Cargo.toml CLAUDE.md AGENTS.md TESTING.md docs/guide/testing.md docs/internal/runbooks/documentation-automation-release.md tools/docs-parity/src/gates.rs tools/docs-parity/src/workflow.rs tools/docs-parity/src/dependency_snapshot.rs tools/docs-parity/tests/gates.rs tools/docs-parity/tests/workflow.rs tools/docs-parity/tests/dependency_snapshot.rs tools/docs-parity/manifests/tracked-files.toml tools/docs-parity/manifests/maintained-sources.toml tools/docs-parity/manifests/gates.toml tools/docs-parity/manifests/snippets.toml docs/internal/audits/documentation-refresh-evidence.md
git commit -m "Activate documentation enforcement gates"
```

### Task 18: Close PR #1049 implementation

**Files:**

- Modify: `docs/internal/audits/documentation-refresh-evidence.md`
- Modify: `docs/internal/audits/documentation-refresh-decisions.md`
- Modify: PR #1049 description through GitHub API/CLI

- [ ] **Step 1: Reassert immutable state**

Fetch and require `origin/rc/202608` still equals `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`, the branch contains it, PR #1049 targets rc from `spec-docs-refresh`, and the worktree has no unrelated bytes.

- [ ] **Step 2: Run the complete local matrix**

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
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets
./scripts/test-cli.sh
cargo test --package trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')"
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
cd crates/trusted-server-js/lib && npm ci && npm run lint && npx vitest run && npm run format && npm run build
cd ../../..
cd docs && npm ci && npm run lint && npm run format && npm run build
```

Also run Task 16 rustdoc commands and all four Task 13 smoke/evidence paths. Record exact commands, tool versions, durations, and results.

- [ ] **Step 3: Prove every acceptance surface**

Record generated no-diff; classification/disposition equality; retired/privacy scans; route/settings/integration equality; snippet diagnostics; README/JSDoc/rustdoc gates; local Pages/CNAME artifact proof; first-success smokes; all follow-up issue URLs/dispositions; package commit/path review; and release-pending fields without fabricated receipts. Read PR #1049's description back through the GitHub API/CLI and require the bounded settings-parity region to equal the Appendix B checklist and the bounded adapter-smokes region to contain each of the four exact script command/cleanup sequences plus its immutable run evidence.

- [ ] **Step 4: Commit final records**

```bash
git add docs/internal/audits/documentation-refresh-evidence.md docs/internal/audits/documentation-refresh-decisions.md
git diff --cached --check
git commit -m "Record documentation refresh acceptance"
git status --porcelain
```

If records do not change, omit the empty commit but still require clean status.

- [ ] **Step 5: Push and verify hosted checks on the exact head**

Push to `origin/spec-docs-refresh`. Update only the bounded `<!-- docs-refresh:final-acceptance:start -->` / `<!-- docs-refresh:final-acceptance:end -->` region of PR #1049's description with the exact final head/base, run URLs, job/app identities, local evidence, and release-pending operations. Preserve every unrelated region. Read the final PR description back and revalidate the settings-parity and adapter-smokes regions after the final-acceptance update. Require every expected hosted job green on that SHA; a prior head does not count.

- [ ] **Step 6: Mark implementation complete**

Review `git diff origin/rc/202608...HEAD` path by path, require no unrelated runtime change, keep package commits unsquashed, and mark PR #1049 ready for review without opening or requesting approval on any other PR. Completion means repository implementation is finished; merge and release-pending operations remain outside this plan.

## Final plan-to-spec traceability

| Spec surface                               | Plan tasks |
| ------------------------------------------ | ---------- |
| Single-PR decisions and evidence           | 1-3        |
| WP1 containment, CNAME, policy             | 2-3        |
| WP8a tool, manifests, workflow foundations | 4-9        |
| WP2 truth pass                             | 10         |
| WP3 configuration                          | 11         |
| WP4 API/routes                             | 12         |
| WP5 deployment and product coverage        | 13-14      |
| WP6 root/crate docs                        | 15         |
| WP7 in-code docs                           | 16         |
| WP8b final CI/release-pending controls     | 17         |
| Final PR #1049 acceptance                  | 18         |

The implementation is complete only when Task 18 verifies the exact final #1049 head. No individual implementation PR or default-branch receipt is part of this plan.

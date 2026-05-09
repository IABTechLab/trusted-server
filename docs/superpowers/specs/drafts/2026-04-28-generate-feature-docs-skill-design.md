---
status: draft
---

# Generate Feature Docs Skill Design

_April 2026_

## 1. Overview

A Claude Code skill that converts implemented engineering design specs into publisher-facing documentation pages on the Trusted Server VitePress site. The skill runs in two interactive stages: an extraction pass that produces a structured outline for user review, and a generation pass that writes prose, applies mechanical updates to reference docs, and shows a diff before any commit.

The skill exists to close a recurring gap. Specs are written, code ships, but the docs at iabtechlab.github.io/trusted-server do not get updated. The team is moving fast and doc-writing is the work that gets dropped. This skill compresses that work from hours of authoring to minutes of review, on demand.

## 2. Audience

The generated docs are aimed at publishers and integrators running Trusted Server. They want concrete answers: what a feature is, when to enable it, how to configure it, and what its API contract looks like. The output voice and structure match existing pages in `docs/guide/`. Where applicable, generated docs include concrete API specifics: endpoint paths, request and response headers, request and response shapes, and error variant names.

## 3. Goals

1. Produce a publishable feature page from one implemented spec, in one invocation, that the spec's author would merge with light editing rather than rewrite from scratch.
2. Apply mechanical, additive updates to `configuration.md`, `api-reference.md`, and `error-reference.md` when a spec introduces new config keys, endpoints, headers, or error variants.
3. Verify every concrete handle (config key, file path, endpoint, header, error variant) against the actual code before writing prose. Mismatches surface as outline issues, not as silent false claims in generated docs.
4. Surface drift between spec and code at the outline stage, before any prose is written.
5. Keep human authors in control. The user reviews the outline, can redirect any field, reviews the diff, and explicitly approves the commit.

## 4. Non-goals

The following are deliberately out of scope for this skill, to keep the implementation focused and to define a clean handoff to a future skill #2 (spec-vs-reality gap analysis).

1. Detecting drift between spec and code _behavior_. The skill verifies that handles exist; it does not verify that the code does what the spec says it does. That is skill #2.
2. Updating non-reference narrative docs. `getting-started.md`, `gdpr-compliance.md`, `architecture.md`, and similar pages are humans' responsibility.
3. Translating, localizing, or summarizing the spec for marketing.
4. Generating diagrams. If a Sequence section is needed, the skill emits a numbered list. Mermaid diagrams are added by humans in follow-up edits.
5. Touching code under `crates/`. The skill is read-only against the codebase.
6. Pulling engineering feedback from GitHub PR review comments. Discussed in Section 13 (Out of scope, deferred to skill #2).
7. Running in CI. The skill is interactive by design. CI integration is a future option once the skill is mature.

## 5. Skill identity and invocation

**Skill location:** `.claude/skills/generate-feature-docs/SKILL.md` at the repo root. Project-level so the convention ships in git and the whole team gets it via `git pull`.

**Slash command:** `.claude/commands/generate-feature-docs.md`. A thin file that takes `$ARGUMENTS` (the spec path) and delegates to the skill. Matches the existing convention for `check-ci.md`, `verify.md`, and the other project-level commands.

**Invocation:** `/generate-feature-docs <spec-path>`. The argument is a path to a spec file under `docs/superpowers/specs/implemented/`. If the user invokes the command without an argument, the skill resolves to the most recent file in that directory and confirms the choice before proceeding.

**Output contract:** the skill writes only to:

- One file under `docs/guide/<feature-slug>.md` (created or augmented).
- Up to three additive updates to `docs/guide/configuration.md`, `docs/guide/api-reference.md`, and `docs/guide/error-reference.md`.

The skill never writes anything else, never opens PRs, never pushes, never deploys, never modifies code under `crates/`, and never modifies the spec it is reading.

## 6. Spec readiness convention

The skill operates only on specs that the team considers final. To make that signal explicit, every spec carries YAML frontmatter:

```yaml
---
status: implemented
implemented_in: PR#581
last_reviewed: 2026-04-15
---
```

Three accepted values for `status`:

- `draft`: brainstorm output. Not ready for documentation.
- `in-progress`: implementation in flight, design may still evolve.
- `implemented`: code has shipped, spec reflects what shipped, ready to document.

Three optional fields:

- `implemented_in`: PR number where the implementation landed. Used by future skill #2.
- `last_reviewed`: date of the most recent engineering review of the spec, in `YYYY-MM-DD` format.
- `verified_against_commit`: commit SHA the engineer asserts the spec was verified against at promotion time. Audit trail; the skill records but does not validate. Added in Section 16.3.

**Skill behavior on `status`:**

- `implemented`: proceed normally (subject to the verification-rate gate added in Section 16.1).
- Any other value, or missing `status` field: skill stops and prompts `Continue without status: implemented? Reply y to proceed.` Wait for the user's reply. Treat any reply other than a single `y` (case-insensitive) as abort. Explicit `y` proceeds with a one-line warning that the docs may drift from product. The override path is intentionally a little annoying so it does not become the default.

The skill never adds frontmatter on the user's behalf. Frontmatter is added at spec-authoring time, by the brainstorming skill (for new specs) or by hand (for existing specs). Section 12 lists the one-time backfill of the 12 existing specs as a prerequisite.

## 7. Directory layout for specs

Specs live under `docs/superpowers/specs/`, split into two subdirectories by lifecycle stage:

- `docs/superpowers/specs/drafts/`: brainstorm output. Every file here has `status: draft`. The brainstorming skill writes here. Engineers refine here. The doc-generation skill ignores this directory.
- `docs/superpowers/specs/implemented/`: post-implementation truth. Every file here has `status: implemented`. The doc-generation skill operates only on this directory. Engineers move specs here when implementation is complete and the spec body has been updated to match what shipped.

Promotion is one operation: `git mv drafts/<file>.md implemented/<file>.md`, with body edits to reflect reality and a frontmatter update from `status: draft` to `status: implemented`. The promotion lands in a PR alongside any final spec edits.

This directory split serves three purposes:

1. The directory listing is the truth at a glance: drafts and implemented specs are visibly separated.
2. The promotion is a visible signal in PR review, more meaningful than a frontmatter line edit.
3. The skill's input filtering is trivial: read only from `implemented/`.

## 8. Stage 1: extraction pass

Read-only. Produces a structured outline shown to the user in chat.

**Inputs:**

- Spec file path under `docs/superpowers/specs/implemented/`.
- The codebase under `crates/`.
- The existing `docs/guide/` directory.

**Steps:**

1. **Validate spec readiness.** Check frontmatter `status` field. If not `implemented`, run the override prompt described in Section 6.
2. **Parse the spec.** Pull the H1 title (feature name), intro paragraph (description), section headings, and code blocks. Identify TOML config blocks, JSON examples, Rust error enums, and endpoint URLs.
3. **Detect spec kind.** Heuristic on section names: a spec with sections like "Configuration", "Public API", or "Endpoints" is a feature spec. A spec with "Migration phases" or "Rollout plan" is a migration spec. A spec with "Pre-prod checklist" is a readiness report. Only feature specs proceed automatically. Other kinds trigger a prompt: "this looks like a `<kind>` spec, not a feature spec. Continue anyway, or abort?"
4. **Resolve target page path.** Slug the feature name (e.g., "RSL AI Crawler Licensing" becomes `ai-crawler-licensing.md`) and check for `docs/guide/<slug>.md`. If a near-match exists (e.g., the spec is a v2 of an existing feature), surface the candidate so the user can confirm "augment existing" vs. "create new".
5. **Detect Sequence-section need.** Heuristic: numbered request-flow steps in the spec, or language like "first ... then ... finally". If present, mark `needs_sequence_section: yes` in the outline.
6. **Detect multi-feature specs.** If the spec has 2 or more top-level "Feature" sections, or the H1 is ambiguous, list candidate features and ask the user to choose: one page per feature, one combined page, or a subset. No default. The user must pick.
7. **Extract handles.** Walk the spec body for: config keys (TOML keys, `[section]` headers), endpoint paths (URL strings), HTTP headers (`X-...` patterns), and error variants (`SomethingError::Variant` patterns).
8. **Verify each handle against code.** Grep `crates/**/*.rs` and `trusted-server.toml` for each handle. Capture `file:line` when found. Mark `NOT FOUND` when not.
9. **Detect spec inconsistencies.** Same config key spelled two ways, two endpoints with the same path, two error variants with conflicting descriptions. Surface as an "Inconsistencies" subsection.
10. **Render the outline** as a markdown chat message and wait for the user.

**Outline format** (rendered in chat, not written to disk):

```markdown
## Extraction summary for `2026-04-22-rsl-ai-crawler-licensing-design.md`

**Feature:** RSL AI Crawler Licensing
**Target page:** `docs/guide/ai-crawler-licensing.md` (NEW)
**Spec kind:** feature
**Sequence section:** yes (request, token check, log, response)

### Config keys

| Key             | Status    | Location          |
| --------------- | --------- | ----------------- |
| `rsl.enabled`   | verified  | `settings.rs:142` |
| `rsl.allowlist` | NOT FOUND | spec only         |

### Endpoints

| Path                    | Methods | Status   | Location            |
| ----------------------- | ------- | -------- | ------------------- |
| `/.well-known/rsl.json` | GET     | verified | `rsl_handler.rs:25` |

### Headers

| Name          | Direction | Status   | Location           |
| ------------- | --------- | -------- | ------------------ |
| `X-RSL-Token` | request   | verified | `rsl/headers.rs:8` |

### Error variants

| Variant                  | Status   | Location          |
| ------------------------ | -------- | ----------------- |
| `RslError::InvalidToken` | verified | `rsl/error.rs:14` |

### Issues

- `rsl.allowlist` referenced in spec but not in code. Options:
  (A) Mark inline as "planned, not yet shipped"
  (B) Drop the row from configuration.md
  (C) Pause and let me fix the spec or the code first

Reply `proceed`, redirect specific fields, or pick A, B, or C for each issue.
```

The user can redirect any field. Wrong slug, wrong target page, drop a handle that is a spec typo, override the spec-kind heuristic. Substantial redirects regenerate the outline. Minor ones are noted and the skill proceeds. The skill never proceeds to stage 2 without an explicit `proceed` (or equivalent affirmative).

The structured representation behind the rendered outline is kept internally as a JSON-shaped object, since it is the input for the future skill #2.

## 9. Stage 2: generation pass

Runs only after the user types `proceed`. Inputs are the spec, the approved outline, and the existing docs. Output is files written to disk; nothing is committed until the user approves the diff.

### 9.1 Branch check before any writes

Before stage 2 writes any files:

1. Detect current git branch. If `main` or `master`: stop, do not write anything yet. Propose a branch name in the form `docs/<feature-slug>` (e.g., `docs/ai-crawler-licensing`) and ask: "you are on `main`. Create branch `docs/ai-crawler-licensing` and switch to it?" The skill refuses to proceed on `main` under any circumstance. The user can specify a different branch name if they want.
2. Check working tree. If uncommitted changes exist outside the planned doc files, refuse: "uncommitted changes detected. Commit, stash, or revert them before running this skill, since the doc commit must contain only doc files." Hard stop, no override.

### 9.2 Prose-writing rules

The skill follows these constraints when writing prose:

- **Voice:** second-person, direct, present tense. Match the register of `edge-cookies.md` and `integration-guide.md`.
- **No marketing language.** Forbidden words: "powerful", "seamless", "robust", "efficiently", "appropriately", "leveraging". The skill scans its own output for these and removes them.
- **No em-dashes.** Use commas, colons, or semicolons. The skill scans its own output for em-dashes and replaces them.
- **No emojis, no decorative characters, no exclamation marks.** Status indicators in tables use text (`verified`, `NOT FOUND`), not symbols.
- **VitePress-flavored markdown.** Relative links (`[link text](/guide/page)`), code blocks with language tags, callouts (`::: tip`, `::: warning`) only for genuinely non-obvious gotchas.
- **Grounding.** Every concrete reference (config key, file path, endpoint, header, error variant) must be one of the verified handles from stage 1, or an explicit `<!-- TODO -->` for something the user opted into during the issues prompt.
- **Empty sections drop.** A feature with no consent implications has no Privacy section. The template is a maximum, not a minimum.
- **No filler.** If the spec does not say enough to write a section, the skill writes a single sentence, not a paragraph of speculation.

### 9.3 Output template

Standard sections, in order. Empty sections are omitted.

1. **Overview.** What the feature is, who it is for. One to three short paragraphs.
2. **How it works.** Mechanism, key concepts, anything an operator needs to understand the feature's behavior at a high level.
3. **Sequence** (optional). Numbered list describing a multi-step user-visible flow. Included only when stage 1 detected this need.
4. **Configuration.** One to two paragraphs naming the config keys, with a link to `configuration.md` for the full reference table.
5. **API contract.** Endpoints, headers, request and response shapes. Code blocks for each.
6. **Error handling.** Error variants, what triggers them, what the response looks like.
7. **Privacy and consent considerations.** Included only when the feature has consent or PII implications.
8. **Related docs.** Internal links to adjacent feature pages.

### 9.4 Augment-in-place rules for existing pages

When the target page already exists:

1. Walk the page's H2 and H3 structure.
2. For each template section that already exists in the page: leave existing prose alone. Only add new items, for example a new row in a config table or a new bullet in a list. Never rewrite human-authored prose for stylistic reasons.
3. For sections in the template that do not exist in the page: insert them in template order.
4. For prose that _contradicts_ the new spec or current code (e.g., a sentence mentioning a config key that no longer exists, or a behavioral claim that the spec has revised): show the existing text and the proposed replacement, and ask the user to approve, skip, or edit per item. This is the only path by which the skill rewrites existing prose.

The default posture is conservative. Under-augmenting is recoverable; destroying a teammate's hand-edits is not.

### 9.5 Mechanical reference-doc updates

For each of `configuration.md`, `api-reference.md`, and `error-reference.md`:

1. The skill reads the file first to learn its existing structural pattern (column layout in tables, section ordering, code-block formatting).
2. New entries are appended or inserted following that pattern.
3. If an entry already exists for the same key (config key, endpoint path, error variant) and the spec defines it differently: prompt the user with the existing text, the new text, and three options (overwrite, keep existing, pause).
4. All updates are otherwise additive and idempotent. Running the skill twice on the same spec produces no second diff.

### 9.6 Diff review

After all files are written, the skill posts a chat message:

```
Generated 4 files:
  - [docs/guide/ai-crawler-licensing.md](docs/guide/ai-crawler-licensing.md) (NEW, 87 lines)
  - [docs/guide/configuration.md](docs/guide/configuration.md) (+12 lines, 1 section updated)
  - [docs/guide/api-reference.md](docs/guide/api-reference.md) (+18 lines, 1 endpoint added)
  - [docs/guide/error-reference.md](docs/guide/error-reference.md) (+4 lines, 2 errors added)

Inline TODOs: 1 (`rsl.allowlist` marked "planned, not yet shipped")

Reply `commit`, `show diff`, or redirect a section.
```

File paths are clickable in the user's editor (the skill emits markdown links with relative paths). The user can:

- `commit`: the skill creates one commit on the current branch with message `Add docs for <feature>` (new pages) or `Update docs for <feature>` (augmentations). The skill stages files explicitly via `git add <paths>`, never `git add -A` or `git add .`. The commit, by construction, contains only doc files.
- `show diff`: the skill prints the diff inline in chat.
- Redirect a section, e.g. "Overview is too long, cut it in half". The skill rewrites only that section and re-shows the diff.

## 10. Edge cases and failure modes

| Case                                                    | Behavior                                                                                                                                                                       |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Spec lacks `status: implemented`                        | Prompt with `(y/N)` default N, abort unless explicit `y`.                                                                                                                      |
| Spec covers multiple features                           | List candidates, ask user to pick: one page per feature, combined page, or subset. No default.                                                                                 |
| Non-feature spec (migration, readiness, tech-spec)      | Prompt: "this looks like a `<kind>` spec, continue anyway?". No automatic fallback.                                                                                            |
| No shipped code (zero handles verify)                   | Prompt: "no shipped code found. Generate stub page with sections marked 'planned, not yet shipped', or abort?". The deeper "is the behavior correct" question is for skill #2. |
| Spec is internally contradictory                        | Surface in stage 1 under "Inconsistencies", ask user to resolve before proceeding.                                                                                             |
| Target page name cannot be determined                   | Ask the user for the target path explicitly.                                                                                                                                   |
| Spec file not found                                     | Hard error, abort.                                                                                                                                                             |
| Spec file outside `docs/superpowers/specs/implemented/` | Warn once, ask "is this really an implemented spec?", then proceed if confirmed.                                                                                               |
| Current branch is `main` or `master`                    | Hard stop, no override. Propose `docs/<feature-slug>` branch name.                                                                                                             |
| Working tree has unrelated uncommitted changes          | Hard stop, no override. User must clean up first.                                                                                                                              |
| Re-run on a spec that has already produced docs         | Supported. Stage 1 finds the existing page. Stage 2 augments per Section 9.4. A clean re-run with no spec or code changes produces zero diff (idempotency).                    |

## 11. Verification and validation

The skill is considered shippable when the following validation cases pass.

1. **Greenfield case.** Run on the RSL AI crawler licensing spec (after backfilling its frontmatter and moving it to `implemented/`). No `docs/guide/<rsl>.md` exists. Skill produces a new page that an integrator could read and act on. Manual review verifies: Configuration section is complete, all referenced handles are verified against `crates/`, and the page passes `cd docs && npm run build` without broken links.
2. **Augmentation case.** Run on the EC KV schema extensions spec (after promotion) against the existing `edge-cookies.md`. Skill detects the existing page, extends it per the new spec, leaves existing prose intact except where it contradicts current code. Manual review verifies: no human-authored content was destroyed, stale-prose detection fired correctly.
3. **Non-feature case.** Run on the EdgeZero migration spec. Skill detects this is not a feature spec and prompts before proceeding.
4. **Drift case.** Run on a spec with one or more deliberately-broken handles (e.g., a config key that does not exist in code). Skill surfaces the broken handle in stage 1, the user picks an option, the generated docs reflect that choice rather than silently writing the false claim.
5. **Idempotency case.** Re-run the skill from case 1 with no intervening spec or code changes. Output is zero diff.
6. **Style case.** Search the generated docs for em-dashes, emojis, exclamation marks, and the words "powerful", "seamless", "robust", "efficiently", "leveraging". Any hit is a bug.

**Success bar:** cases 1 and 2 produce drafts that the spec's author would merge with light editing, not start-from-scratch rewrites. Cases 3 and 4 prompt correctly without silently proceeding. Case 5 produces no diff. Case 6 finds zero violations. Generated docs build cleanly with `cd docs && npm run build`.

This bar is partially subjective. Reasonable people disagree on what "merge with light editing" means. The skill is iterated based on real usage. When output is wrong in some pattern, the SKILL.md is edited and committed. The skill is versioned in git and improvements ship with normal PRs.

## 12. Prerequisites

The following one-time tasks must complete before the skill can be used in normal operation. They are listed here so the implementation plan can sequence them correctly.

1. **Exclude internal specs from the public docs site.** Add `srcExclude` (or equivalent) to `docs/.vitepress/config.mts` so that everything under `docs/superpowers/` is omitted from the VitePress build. Verify by running `cd docs && npm run build` and confirming the spec pages are not in the output.
2. **Create directory layout.** Add `docs/superpowers/specs/drafts/` and `docs/superpowers/specs/implemented/` to the repo. Both are committed (with `.gitkeep` if empty) to establish the convention.
3. **Bulk-move existing specs to `drafts/`.** Move all 12 existing specs into `drafts/`. Add `status: draft` frontmatter to each. This is a single PR.
4. **Update CLAUDE.md.** Add a short section describing the spec-readiness convention (Sections 6 and 7) and instructing the brainstorming skill to write to `drafts/` when invoked from this project. The CLAUDE.md instruction takes precedence over the skill default per the harness rules.
5. **(For testing only) Promote one spec to `implemented/`.** To validate the skill end-to-end, one spec needs to live in `implemented/` with `status: implemented` frontmatter. The RSL AI crawler licensing spec is a good candidate since it is recent and has no corresponding guide page. Promotion is a one-step `git mv` plus frontmatter update.

The skill itself depends on tasks 1, 2, and 4 being complete. Task 3 is necessary for clean repository state but does not block the skill mechanically. Task 5 is required only for validation, not for normal operation.

## 13. Out of scope, deferred to skill #2

The companion skill, planned next, is responsible for:

- Comparing actual code behavior against the spec (not just handle existence).
- Identifying gaps where the implementation falls short of the spec's promises.
- Producing a roadmap-style "planned for later" list with rough estimates.
- Optionally consuming GitHub PR review comments via `gh pr view <implemented_in> --comments` to incorporate post-spec engineering decisions.

This skill (#1) is intentionally limited to handle existence and prose generation so that skill #2 has a clear boundary. The structured outline produced by stage 1 is the contract between the two skills: skill #2 consumes the same outline shape and adds behavior verification on top.

## 14. Related work

**Upstream contribution to the Superpowers brainstorming skill.** The directory split (`drafts/` and `implemented/`) and the `status:` frontmatter convention are useful beyond this project. A PR to the Superpowers plugin should:

1. Update the brainstorming skill to write new specs to `<spec-root>/drafts/` instead of `<spec-root>/`.
2. Add `status: draft` frontmatter to newly-written specs by default.
3. Document the lifecycle in the brainstorming skill's README.

This is a separate change from skill #1, with its own PR upstream. Listed here as related work so it does not get forgotten.

## 15. Implementation summary

The implementation plan, written next, will sequence the prerequisites in Section 12, the slash-command and skill files described in Section 5, and the validation cases in Section 11. The skill itself is one markdown file (`.claude/skills/generate-feature-docs/SKILL.md`) plus one slash-command file (`.claude/commands/generate-feature-docs.md`). The skill content encodes the rules in Sections 6 through 10.

## 16. Post-validation amendment: stricter readiness verification

The first validation run (greenfield case against the JS Asset Auditor spec on 2026-04-28) exposed a workflow gap: a spec can be promoted to `implemented/` with `status: implemented` even when most of the described feature is not yet in `crates/`. The skill correctly produced a stub feature page with `<!-- TODO: planned -->` annotations, which is graceful, but the deeper signal is that promotion happened prematurely. This section documents three amendments to close that gap. They are not a re-design; they are tightening of existing rules.

### 16.1 Verification-rate threshold

Stage 1 already verifies each handle (config keys, endpoints, headers, error variants) against `crates/` and `trusted-server.toml`. The skill currently refuses only when zero handles verify (the "no shipped code" edge case in Section 10). Tighten this:

- Compute a verification rate during stage 1: `verified_count / total_extracted_count`.
- If the rate is **below 50%**, surface this in the outline's "Issues" subsection as a hard prompt:
  > "Stage 1 verified `<X>` of `<N>` handles in this codebase (`<rate>%`). Below 50% suggests this spec may not be fully implemented in this branch. Options: (A) generate stubs for unverified handles, (B) abort and check status, (C) override and proceed normally."
- The threshold of 50% is an initial value; tune based on real usage. Tracked as a known knob.
- The skill still emits the outline normally so the user can see exactly which handles failed verification before choosing.

This catches the JS Asset Auditor case directly: 1 of ~5 handles verified, roughly 20%, well below 50%.

### 16.2 Branch-state heuristic

In addition to handle verification, stage 1 inspects whether the current branch has actually touched code relevant to the feature:

```bash
git log --name-only <merge-base>..HEAD -- crates/ trusted-server.toml
```

Where `<merge-base>` is the merge base with `main` (or the equivalent default branch). If the result is empty (the current branch has not touched any code), surface this as an additional signal in the outline:

> "Note: this branch has no commits touching `crates/` or `trusted-server.toml`. If you expect the implementation to be on this branch, you may be on the wrong branch."

This is informational, not a hard stop. It pairs with 16.1 to give a more complete picture: low verification rate combined with no code on the branch is a strong indicator the engineer is on `main` rather than on the implementation branch.

### 16.3 `verified_against_commit` frontmatter field

Add an optional field to the spec readiness convention (Section 6):

```yaml
---
status: implemented
implemented_in: PR#581
last_reviewed: 2026-04-15
verified_against_commit: a1b2c3d4
---
```

`verified_against_commit` records the commit SHA the engineer asserts the spec was verified against when promoted. The skill records but does not validate this field; it is an audit trail field for future review and for use by skill #2.

Use case: if the skill produces a stub page (low verification rate), the user can later compare the recorded `verified_against_commit` against the current branch state to understand whether the promotion was premature or whether the implementation has been reverted since.

### 16.4 Out of scope, deferred to a future enhancement

Validation against the GitHub PR (calling `gh pr view <implemented_in>`) was considered and explicitly deferred. Reasons: external network calls in a hot path complicate testing, require `gh` auth, and tie the skill to GitHub. If we find we still need this signal after 16.1 and 16.2, it can be added as a `--strict` flag or a CI-only mode in a follow-up.

### 16.5 Implementation impact

These amendments require:

1. `SKILL.md` update: add verification-rate computation and the threshold prompt to Stage 1 (Step 1.7 or a new Step 1.7a).
2. `SKILL.md` update: add the branch-state check to Stage 1, surfaced in the outline.
3. Section 6 update: extend the frontmatter convention with the optional `verified_against_commit` field.
4. `CLAUDE.md` update: keep the documented frontmatter schema in sync with the extended Section 6.

These amendments do NOT require:

- Changes to the prerequisites in Section 12 (those have already landed).
- Changes to the directory layout in Section 7.
- Changes to the prose-writing rules or template in Section 9.
- Re-running the validation cases that already passed in Section 11. The amendments are additive checks and do not affect happy-path behavior.

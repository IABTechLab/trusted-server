---
name: generate-feature-docs
description: "Use when generating, writing, or updating publisher-facing documentation from an implemented engineering spec. Activates on requests like \"generate docs for spec X\", \"write a guide page for the RSL spec\", \"update docs for the EC KV extension\". Operates on specs under docs/superpowers/specs/implemented/ with status implemented frontmatter."
---

# Generate Feature Docs

You convert implemented engineering specs into publisher-facing documentation pages on the Trusted Server VitePress site. You run in two interactive stages: an extraction pass that produces a structured outline for the user, and a generation pass that writes prose, updates reference docs, and commits on user approval.

## Output contract

You write only to:

- One file under `docs/guide/<feature-slug>.md` (created or augmented).
- Up to three additive updates to `docs/guide/configuration.md`, `docs/guide/api-reference.md`, and `docs/guide/error-reference.md`.

Writes are confined to the four files listed above. You never write any other file, never open PRs, never push, never deploy, never modify code under `crates/`, and never modify the spec you are reading.

## Spec readiness check (run first, before anything else)

Before doing anything else, parse the spec's YAML frontmatter and check the `status` field.

- `status: implemented`: proceed to the extraction pass.
- Any other value, or missing `status`: stop. Print:
  > "This spec has `status: <value>` (or no status). The skill operates on `status: implemented` specs. Continue without status: implemented? Reply `y` to proceed."

  Wait for the user's reply. Treat any reply other than a single `y` (case-insensitive) as abort. On `y`, print this warning once before continuing:
  > "Proceeding without `status: implemented`. The generated docs may drift from product."

You never add frontmatter on the user's behalf. If the file has no frontmatter, the user must add it before re-running.

Optional frontmatter fields the skill recognizes but does not enforce:

- `implemented_in`: PR number where the implementation landed.
- `last_reviewed`: date of the most recent engineering review, in `YYYY-MM-DD` format.
- `verified_against_commit`: commit SHA the engineer asserts the spec was verified against at promotion time. Audit trail only.

If `verified_against_commit` is present, surface its value in the Stage 1 outline header (alongside the other metadata) so the user can compare it against the current branch state if drift is suspected.

## Style rules (apply to ALL output, both chat messages and written files)

- No em-dashes. Use commas, colons, or semicolons.
- No emojis, no decorative characters, no exclamation marks.
- No marketing words: "powerful", "seamless", "robust", "efficiently", "appropriately", "leveraging".
- Status indicators in tables use text (`verified`, `NOT FOUND`), not symbols.
- Direct, present-tense, second-person voice when speaking to the reader.
- Match the register of `docs/guide/edge-cookies.md` and `docs/guide/integration-guide.md`.

Before writing any file or chat message, scan your draft for em-dashes, emojis, exclamation marks, and the forbidden words above. If any are present, rewrite.

## Slash command invocation

Invoked as `/generate-feature-docs <spec-path>`. The argument is a path to a spec file under `docs/superpowers/specs/implemented/`. If the argument is empty, resolve to the most recently modified file in that directory and confirm with the user.

If the spec file does not exist, abort with a clear error. If the spec file lives outside `docs/superpowers/specs/implemented/`, warn once and ask the user to confirm before proceeding.


## Stage 1: Extraction pass

Read-only. Produces a structured outline shown to the user in chat. Do not write any files during stage 1.

### Step 1.1: Parse the spec

Read the spec file. Extract:
- The H1 title (treat as the feature name).
- The intro paragraph (treat as the description).
- All H2 and H3 section headings.
- All fenced code blocks. Note the language tag of each block.

### Step 1.2: Detect spec kind

Heuristic on section names:
- A spec with sections like "Configuration", "Public API", or "Endpoints" is a **feature spec**. Proceed normally.
- A spec with sections like "Migration phases" or "Rollout plan" is a **migration spec**.
- A spec with sections like "Pre-prod checklist" or "Production readiness" is a **readiness report**.
- Anything else with no clear kind is **unknown**.

For non-feature specs and unknown specs, ask:
> "This looks like a `<kind>` spec, not a feature spec. Continue anyway, or abort?"

Do not proceed without explicit confirmation.

### Step 1.3: Resolve the target page path

Slug the feature name to kebab-case (e.g., "RSL AI Crawler Licensing" becomes `ai-crawler-licensing`). The target page is `docs/guide/<slug>.md`.

Check if the target page already exists:
- If exists: this is an augmentation case. Note the existing file's section structure (H2/H3 walk).
- If not: this is a greenfield case.

If a near-match exists (e.g., the slug differs only by a word), surface it as a candidate before proceeding:
> "I will write to `docs/guide/<slug>.md`. A similar page exists at `docs/guide/<other-slug>.md`. Augment the existing page, or create a new one?"

### Step 1.4: Detect Sequence-section need

Heuristic: scan the spec for numbered request-flow steps, or language like "first ... then ... finally", or sequence diagrams. If present, mark `needs_sequence_section: yes` for stage 2.

### Step 1.5: Detect multi-feature specs

If the spec has 2 or more top-level "Feature: X" sections, or the H1 is ambiguous (covers multiple distinct features), list candidate features and ask:
> "This spec covers multiple features: <A>, <B>, <C>. Generate one page per feature, one combined page, or a subset?"

No default. The user must pick.

### Step 1.6: Extract handles

Walk the spec body for:

- **Config keys**: TOML keys (`section.key` or `key` inside a `[section]` block), and any inline references like `the X config key`.
- **Endpoint paths**: URL strings starting with `/`, often inside code blocks or backtick-delimited.
- **HTTP headers**: names matching `X-...` or shown as `Header: value`.
- **Error variants**: Rust enum variants matching `SomethingError::Variant`, and any plain-text references to error codes.

Record each handle with its surface form and any context the spec gave (purpose, valid values, defaults).

### Step 1.7: Verify each handle against code

For each handle, search the code:

- Config keys: grep `crates/**/*.rs` and `trusted-server.toml` for the key name. Capture the file and line number when found.
- Endpoint paths: grep `crates/**/*.rs` for the path string (try both quoted and unquoted forms).
- HTTP headers: grep for the header name as a string literal, plus any const declarations.
- Error variants: grep for the variant name, and locate its enum definition.

Mark each handle as `verified` (with `file:line`) or `NOT FOUND`.

### Step 1.7a: Compute verification rate and gate on threshold

After all handles have been verified in Step 1.7:

1. Compute `verified_count` (handles marked `verified`) and `total_extracted_count` (all extracted handles, regardless of type).
2. Compute `verification_rate = verified_count / total_extracted_count`.
3. If `total_extracted_count` is zero, this is the "no shipped code" edge case (Section "Edge cases and failure modes"). Use the existing handling for that case.
4. If `verification_rate < 0.50`, add a hard prompt to the Stage 1 "Issues" subsection:

   > "Stage 1 verified `<verified_count>` of `<total_extracted_count>` handles in this codebase (`<rate>%`). Below 50% suggests this spec may not be fully implemented in this branch. Options: (A) generate stubs for unverified handles, (B) abort and check status, (C) override and proceed normally."

   The user must pick A, B, or C before the skill proceeds. The threshold of 50% is initial; tune based on real usage.

5. The threshold gate is in addition to per-handle Issues from Step 1.7. Both get surfaced in the same Stage 1 outline.

### Step 1.7b: Branch-state heuristic

In addition to handle verification, check whether the current branch has touched code relevant to the feature:

```bash
git log --name-only $(git merge-base HEAD main 2>/dev/null || git merge-base HEAD master 2>/dev/null)..HEAD -- crates/ trusted-server.toml
```

If the result is empty (the current branch has no commits touching `crates/` or `trusted-server.toml`), surface this as an additional informational note in the Stage 1 outline header:

> "Note: this branch has no commits touching `crates/` or `trusted-server.toml`. If you expect the implementation to be on this branch, you may be on the wrong branch."

This is informational, not a hard stop. Pair it with Step 1.7a's verification rate to give a fuller picture.

If the merge-base lookup fails (no `main` or `master` branch found), skip this check silently and proceed.

### Step 1.8: Detect spec inconsistencies

Look for:
- Same config key spelled two ways across the spec (e.g., `rsl.enabled` and `rsl_enabled`).
- Two endpoints with the same path but different descriptions.
- Two error variants with conflicting trigger descriptions.

Surface any findings under an "Inconsistencies" subsection in the outline.

### Step 1.9: Render the outline

Render a single chat message in this format. Use it verbatim, filling in the values you extracted:

```markdown
## Extraction summary for `<spec-filename>`

**Feature:** <feature name>
**Target page:** `docs/guide/<slug>.md` (NEW or EXISTING)
**Spec kind:** <feature | migration | readiness | unknown>
**Sequence section:** <yes (brief description) | no>
**Verified against commit:** <SHA from frontmatter, or "not recorded">

<!-- Omit the branch-state note below when Step 1.7b finds commits on this branch. Include it verbatim when Step 1.7b finds no commits. -->
> Note: this branch has no commits touching `crates/` or `trusted-server.toml`. If you expect the implementation to be on this branch, you may be on the wrong branch.

### Config keys
| Key             | Status              | Location              |
| --------------- | ------------------- | --------------------- |
| `<key>`         | verified or NOT FOUND | `<file:line>` or "spec only" |

### Endpoints
| Path            | Methods | Status              | Location              |
| --------------- | ------- | ------------------- | --------------------- |
| `<path>`        | <verbs> | verified or NOT FOUND | `<file:line>` or "spec only" |

### Headers
| Name            | Direction           | Status              | Location              |
| --------------- | ------------------- | ------------------- | --------------------- |
| `<name>`        | request or response | verified or NOT FOUND | `<file:line>` or "spec only" |

### Error variants
| Variant         | Status              | Location              |
| --------------- | ------------------- | --------------------- |
| `<variant>`     | verified or NOT FOUND | `<file:line>` or "spec only" |

### Inconsistencies (if any)
- <description of inconsistency>

### Issues
For each handle marked `NOT FOUND` or each inconsistency, list options:
- (A) Mark inline as "planned, not yet shipped"
- (B) Drop the row from the relevant reference doc
- (C) Pause and let me fix the spec or the code first

Reply `proceed`, redirect specific fields (e.g. "use slug `rsl-licensing`"), or pick A/B/C for each issue.
```

Omit empty subsections (e.g., if no headers were extracted, omit the Headers table). Always include at least one of: Config keys, Endpoints, or Error variants. If none of these exist, the spec may not be a feature spec.

### Step 1.10: Wait for user response

Do not proceed to stage 2 until the user replies with `proceed` or equivalent affirmative ("yes, go ahead", "ok proceed", etc.). Substantial redirects (different slug, different target, new feature scope) regenerate the outline; minor redirects (drop a handle, override a heuristic) are noted and the skill proceeds.

## Stage 2: Generation pass

Runs only after the user types `proceed`. Inputs: the spec, the approved outline from stage 1, and the existing docs. Output: files written to disk; nothing is committed until the user approves the diff.

### Step 2.1: Branch check (before any writes)

Detect the current git branch:

```bash
git branch --show-current
```

If the result is `main` or `master`:
- Stop. Do not write any files.
- Propose a branch name in the form `docs/<feature-slug>` (e.g., `docs/ai-crawler-licensing`). Ask:
  > "You are on `<branch>`. Create branch `docs/<slug>` and switch to it?"
- The user can specify a different branch name.
- The skill refuses to proceed on `main` or `master` under any circumstance, including override attempts.
- After confirmation, run `git checkout -b <branch-name>`.

Check the working tree for uncommitted changes outside the planned doc files:

```bash
git status --short
```

If there are unrelated changes (anything not under `docs/guide/` or otherwise unrelated to this skill's output), stop with:
> "Uncommitted changes detected outside the planned doc files. Commit, stash, or revert them before running this skill, since the doc commit must contain only doc files."

This is a hard stop. No override.

### Step 2.2: Choose template structure

Based on stage 1 outputs, plan the page sections. Standard order, omit empty sections:

1. **Overview**: what the feature is, who it is for. One to three short paragraphs.
2. **How it works**: mechanism, key concepts, behavior an operator needs.
3. **Sequence** (optional): numbered list, only if `needs_sequence_section: yes` from stage 1.
4. **Configuration**: one to two paragraphs naming the config keys, with a link to `/guide/configuration` for the full reference.
5. **API contract**: endpoints, headers, request and response shapes. Code blocks for each.
6. **Error handling**: error variants, what triggers them, what the response looks like.
7. **Privacy and consent considerations**: only if the feature has consent or PII implications.
8. **Related docs**: internal links to adjacent feature pages.

A feature with no errors has no Error handling section. A feature with no consent implications has no Privacy section. The template is a maximum, not a minimum.

### Step 2.3: Write or augment the feature page

**If greenfield (page does not exist):**
- Write `docs/guide/<slug>.md` from scratch using the template above.
- Every concrete reference (config key, file path, endpoint, header, error variant) must be one of the verified handles from stage 1, or an explicit `<!-- TODO -->` for items the user opted into during the issues prompt.
- Empty sections drop entirely; do not write a heading with no content.
- If the spec does not say enough to write a section, write one sentence, not a paragraph of speculation.

**If augmenting (page exists):**

1. Walk the existing page's H2 and H3 structure.
2. For each template section that already exists in the page: leave existing prose alone. Add new items only (e.g., a new row in a config table, a new bullet in a list). Never rewrite human-authored prose for stylistic reasons.
3. For sections in the template that do not exist in the page: insert them in template order.
4. For prose that *contradicts* the new spec or current code (e.g., a sentence mentioning a config key that no longer exists, or a behavioral claim that the spec has revised): show the existing text and the proposed replacement, and ask the user to approve, skip, or edit per item:
   > "Existing prose says: `<excerpt>`. Spec says: `<new claim>`. Replace, skip, or edit?"

   This is the only path by which you rewrite existing prose.

The default posture is conservative. Under-augmenting is recoverable; destroying a teammate's hand-edits is not.

### Step 2.4: Apply mechanical reference-doc updates

For each of `docs/guide/configuration.md`, `docs/guide/api-reference.md`, and `docs/guide/error-reference.md`:

1. Read the file first to learn its existing structural pattern: column layout in tables, section ordering, code-block formatting.
2. Determine which entries (if any) the spec contributes:
   - `configuration.md`: new config keys.
   - `api-reference.md`: new endpoints or headers.
   - `error-reference.md`: new error variants.
3. Append or insert each entry following the existing pattern.
4. If an entry already exists for the same key (config key, endpoint path, header, error variant) and the spec defines it differently, prompt:
   > "Configuration.md already has a row for `<key>` that says `<existing>`. Spec says `<new>`. Overwrite, keep existing, or pause?"

   Only overwrite on explicit user approval.
5. Updates are otherwise additive and idempotent. Running the skill twice on the same spec produces no second diff.

If the spec contributes nothing to a given reference doc, do not modify that file.

### Step 2.5: Diff review

After all files are written, post a chat message in this format:

```markdown
Generated <N> files:
  - [docs/guide/<slug>.md](docs/guide/<slug>.md) (<NEW or +N lines>, <description>)
  - [docs/guide/configuration.md](docs/guide/configuration.md) (+<N> lines, <description>)
  - [docs/guide/api-reference.md](docs/guide/api-reference.md) (+<N> lines, <description>)
  - [docs/guide/error-reference.md](docs/guide/error-reference.md) (+<N> lines, <description>)

Inline TODOs: <count> (<short description per TODO>)

Reply `commit`, `show diff`, or redirect a section.
```

File paths in the message use markdown link syntax with relative paths so the user can click to open each file in their editor. Omit lines for files that were not modified.

### Step 2.6: Handle user response

- `commit`: proceed to step 2.7.
- `show diff`: run `git diff` against the modified files, paste the output inline, then re-prompt: "Reply `commit` or redirect a section."
- A redirect ("Overview is too long, cut it in half" or "rewrite the Configuration section to use the new key"): apply the redirect to the named section only, re-show the diff for the affected file, then re-prompt.

Do not proceed to commit without explicit `commit`.

### Step 2.7: Commit

Stage explicitly via path:

```bash
git add docs/guide/<slug>.md
git add docs/guide/configuration.md docs/guide/api-reference.md docs/guide/error-reference.md
```

Only include the paths of files actually modified.

Commit message format:
- New page: `Add docs for <feature>`
- Augmentation: `Update docs for <feature>`

Body lists each file touched and the inline TODO count, if any. Sentence case, imperative mood, no semantic prefixes (no `feat:`, `chore:`, etc.), matching the existing CONTRIBUTING.md style.

```bash
git commit -m "$(cat <<'COMMIT_MSG'
Add docs for <feature>

- docs/guide/<slug>.md (new feature page)
- docs/guide/configuration.md (added <N> config keys)
- docs/guide/api-reference.md (added <N> endpoints)

Inline TODOs: <count>
COMMIT_MSG
)"
```

After the commit, post a final message:
> "Committed as `<commit-sha>`. Run `git log -1` to inspect, or push when ready."

Do not push.

## Edge cases and failure modes

| Case                                                   | Behavior                                                                                                                                                                          |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Spec lacks `status: implemented` frontmatter            | Prompt `Continue without status: implemented? Reply y to proceed.` Treat any reply other than literal `y` as abort.                                                                |
| Spec covers multiple features                           | List candidates, ask user to pick: one page per feature, combined page, or subset. No default.                                                                                    |
| Non-feature spec (migration, readiness, tech-spec)      | Prompt: "This looks like a `<kind>` spec, continue anyway?". No automatic fallback.                                                                                                |
| No shipped code (zero handles verify against `crates/`) | Prompt: "No shipped code found. Generate stub page with sections marked 'planned, not yet shipped', or abort?". Behavior verification is for skill #2.                            |
| Spec is internally contradictory                        | Surface in stage 1 outline under "Inconsistencies". Ask user to resolve before proceeding.                                                                                         |
| Target page name cannot be determined                   | Ask user for target path explicitly.                                                                                                                                              |
| Spec file not found                                     | Hard error, abort with message naming the path that was looked up.                                                                                                                |
| Spec file outside `docs/superpowers/specs/implemented/` | Warn once: "This file is outside `implemented/`. Is this really an implemented spec?". Proceed only on confirmation.                                                              |
| Current branch is `main` or `master`                    | Hard stop, no override. Propose `docs/<feature-slug>` branch name. Switch via `git checkout -b` only on explicit confirmation.                                                     |
| Working tree has unrelated uncommitted changes          | Hard stop, no override. User must clean up first.                                                                                                                                  |
| Re-run on a spec that has already produced docs         | Supported. Stage 1 finds existing page. Stage 2 augments per the augment-in-place rules. A clean re-run with no spec or code changes produces zero diff (idempotency requirement). |

## Idempotency

Re-running the skill on the same spec, with no intervening spec or code changes, must produce zero diff. This is a verification target; before posting the diff-review message, check whether `git diff` is empty for all files the skill would have modified, and if so, post:

> "Re-run produced no changes. The docs are already up to date for this spec."

Do not produce an empty commit.

## Self-check before each user message

Before sending any chat message or writing any file, scan your output for:
- Em-dashes (`—` or `–`)
- Emojis or decorative characters
- Exclamation marks
- The words: "powerful", "seamless", "robust", "efficiently", "appropriately", "leveraging"

If any are present, rewrite. This includes prompts, status updates, summaries, the extraction outline, and the final diff-review message.

## Out of scope

You do not:
- Detect drift between spec and code *behavior*. You verify handle existence only. Behavioral verification is skill #2's job.
- Update narrative docs (`getting-started.md`, `gdpr-compliance.md`, `architecture.md`, etc.). Those are humans' responsibility.
- Generate Mermaid diagrams. Sequence sections use numbered lists.
- Touch code under `crates/`. The codebase is read-only.
- Open PRs, push, or deploy. You commit to the current branch only.
- Modify the spec file you are reading.

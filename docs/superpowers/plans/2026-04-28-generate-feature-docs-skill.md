# Generate Feature Docs Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Claude Code skill that converts implemented engineering specs in `docs/superpowers/specs/implemented/` into publisher-facing documentation pages in `docs/guide/`, with handle verification against code, augment-in-place behavior for existing pages, and mechanical updates to `configuration.md`, `api-reference.md`, and `error-reference.md`.

**Architecture:** Two files compose the skill: a thin slash command at `.claude/commands/generate-feature-docs.md` and the actual skill instructions at `.claude/skills/generate-feature-docs/SKILL.md`. The skill runs in two interactive stages: stage 1 reads the spec, verifies handles against code, and produces a structured outline for user review; stage 2 writes prose, applies mechanical reference-doc updates, shows a diff, and commits on user approval. Built entirely on Claude Code's existing skill and slash-command primitives. No new runtime infrastructure. Skill content is plain markdown, versioned in git, deployed via `git pull`.

**Tech Stack:** Markdown for skill instructions and slash command, VitePress for the docs site (already configured), Git for versioning, Bash for one-time migrations and validation grep checks. No language runtime required to build the skill itself.

**Source spec:** [docs/superpowers/specs/drafts/2026-04-28-generate-feature-docs-skill-design.md](../specs/drafts/2026-04-28-generate-feature-docs-skill-design.md)

---

## Phase 1: Prerequisites

Setup work that must complete before the skill itself is built. None of these tasks change runtime behavior; they establish conventions and clean up the docs site.

### Task 1: Exclude internal specs from the public VitePress build

**Why:** The docs site at iabtechlab.github.io/trusted-server is currently rendering internal engineering specs from `docs/superpowers/specs/` because the VitePress config does not exclude them. This is a pre-existing privacy issue that the skill design depends on. Must be fixed before the skill is used in normal operation.

**Files:**
- Modify: `docs/.vitepress/config.mts`

- [ ] **Step 1: Read the existing config**

Open `docs/.vitepress/config.mts` and locate the `defineConfig({...})` call (currently around line 36, wrapped in `withMermaid(...)`).

- [ ] **Step 2: Add srcExclude option to defineConfig**

Add the following property inside the `defineConfig({...})` object, near `base:` and before `markdown:`:

```ts
srcExclude: ['superpowers/**', '**/node_modules/**'],
```

After the change, the relevant region of the file should look like:

```ts
export default withMermaid(
  defineConfig({
    title: 'Trusted Server',
    description:
      'Privacy-preserving edge computing for ad serving and edge cookie (EC) generation',
    base: '/trusted-server',
    srcExclude: ['superpowers/**', '**/node_modules/**'],

    // Replace version placeholders like {{NODEJS_VERSION}} with values from .tool-versions
    markdown: {
      // ...unchanged
```

- [ ] **Step 3: Build the docs locally**

Run:
```bash
cd docs && npm run build
```

Expected: build completes without errors. Output goes to `docs/.vitepress/dist/`.

- [ ] **Step 4: Verify excluded pages are absent from the build output**

Run:
```bash
find docs/.vitepress/dist -path '*/superpowers/*' | head
```

Expected: zero output. If anything prints, the exclusion did not work; revisit Step 2.

- [ ] **Step 5: Commit**

```bash
git add docs/.vitepress/config.mts
git commit -m "Exclude internal specs from VitePress build"
```

---

### Task 2: Create the implemented/ subdirectory

**Why:** The directory split (drafts/ vs implemented/) is the structural signal for the spec lifecycle. The `drafts/` directory already exists (created when the design spec was written). The `implemented/` directory does not exist yet; create it now with a `.gitkeep` so the convention is in place before any specs are promoted.

**Files:**
- Create: `docs/superpowers/specs/implemented/.gitkeep`

- [ ] **Step 1: Create the directory and the keepfile**

```bash
mkdir -p docs/superpowers/specs/implemented
touch docs/superpowers/specs/implemented/.gitkeep
```

- [ ] **Step 2: Verify both lifecycle directories exist**

```bash
ls -d docs/superpowers/specs/drafts docs/superpowers/specs/implemented
```

Expected: both paths print without errors.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/implemented/.gitkeep
git commit -m "Add implemented/ directory for promoted specs"
```

---

### Task 3: Move existing specs to drafts/ with status frontmatter

**Why:** All 12 existing specs at `docs/superpowers/specs/*.md` are brainstorm output, not finalized. They must live under `drafts/` and carry `status: draft` frontmatter to match the convention. The new design spec already lives in `drafts/`; this task handles the other 12.

**Files:**
- Move and modify: all 12 files matching `docs/superpowers/specs/*.md`

The 12 files:

```
2026-01-15-attestation-design.md
2026-03-11-production-readiness-report-design.md
2026-03-19-auction-orchestration-flow-design.md
2026-03-19-edgezero-migration-design.md
2026-03-24-ssc-prd-design.md
2026-03-24-ssc-technical-spec-design.md
2026-03-25-streaming-response-design.md
2026-03-30-pr7-geo-client-info-design.md
2026-04-02-ec-kv-schema-extensions-design.md
2026-04-02-ec-kv-seeding-design.md
2026-04-18-microsoft-monetize-server-side-ad-templates-codex-reviewed-design.md
2026-04-22-rsl-ai-crawler-licensing-design.md
```

Note: 3 of these are currently untracked in git (per the repo's working state). The script below handles tracked and untracked files identically by working at the filesystem level and letting `git add` figure out the rest.

- [ ] **Step 1: Confirm exactly 12 files at the top level of specs/**

```bash
ls docs/superpowers/specs/*.md | wc -l
```

Expected: `12`. If more or fewer, stop and investigate; the file list above may be stale.

- [ ] **Step 2: Verify none of the 12 files already has frontmatter**

```bash
for f in docs/superpowers/specs/*.md; do
  head -1 "$f" | grep -q '^---' && echo "ALREADY HAS FRONTMATTER: $f"
done
```

Expected: zero output. If any file already has frontmatter, the prepend would corrupt it; that file must be handled by hand.

- [ ] **Step 3: Move each file to drafts/ and prepend status frontmatter**

```bash
for f in docs/superpowers/specs/*.md; do
  base=$(basename "$f")
  newpath="docs/superpowers/specs/drafts/$base"
  { printf -- '---\nstatus: draft\n---\n\n'; cat "$f"; } > "$newpath"
  rm "$f"
done
```

After this, the top level of `docs/superpowers/specs/` should contain only `drafts/` and `implemented/` subdirectories.

- [ ] **Step 4: Verify all 12 files now live in drafts/ with frontmatter**

```bash
ls docs/superpowers/specs/drafts/*.md | wc -l
```

Expected: `13` (the 12 moved files plus the design spec written earlier).

```bash
for f in docs/superpowers/specs/drafts/*.md; do
  head -3 "$f" | grep -q 'status: draft' || echo "MISSING FRONTMATTER: $f"
done
```

Expected: zero output.

- [ ] **Step 5: Verify nothing remains at the top level of specs/**

```bash
ls docs/superpowers/specs/*.md 2>/dev/null | wc -l
```

Expected: `0`. The shell may print `ls: cannot access` to stderr; that is fine.

- [ ] **Step 6: Build the docs site to confirm nothing broke**

```bash
cd docs && npm run build
```

Expected: build completes cleanly.

- [ ] **Step 7: Stage and commit**

```bash
git add docs/superpowers/specs/
git status --short docs/superpowers/specs/
```

Expected: a mix of `R` (rename) and `A` (added) entries, depending on whether each source file was tracked. No untracked files should remain under `docs/superpowers/specs/`.

```bash
git commit -m "Move existing specs to drafts/ with status: draft frontmatter"
```

---

### Task 4: Document the spec-readiness convention in CLAUDE.md

**Why:** The directory layout and `status:` frontmatter are conventions Claude must follow when invoked from this repo. CLAUDE.md is the right place because the harness rules give CLAUDE.md priority over default skill behavior. This is also where future contributors will look.

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Read CLAUDE.md and locate a suitable insertion point**

A reasonable location is after the existing `## Project Overview` section and before `## Workspace Layout`. Or as a new top-level section near the end, before `## What NOT to Do`.

- [ ] **Step 2: Add a new section to CLAUDE.md**

Insert the following block at the chosen location:

```markdown
## Engineering Spec Conventions

Implementation specs live under `docs/superpowers/specs/`, split by lifecycle:

- `docs/superpowers/specs/drafts/`: brainstorm output. The Superpowers brainstorming skill writes new specs here. Specs are not yet ready for documentation. Frontmatter: `status: draft`.
- `docs/superpowers/specs/implemented/`: post-implementation truth. Specs are promoted here once code has shipped and the spec body has been updated to reflect what shipped. Frontmatter: `status: implemented`.

**Required frontmatter** (every spec):

\`\`\`yaml
---
status: draft | in-progress | implemented
implemented_in: PR#123    # optional, set on promotion
last_reviewed: 2026-04-15  # optional, YYYY-MM-DD
---
\`\`\`

**For agents writing new specs:** when invoked from this project, the brainstorming skill must write to `docs/superpowers/specs/drafts/`, not the parent directory. Add `status: draft` frontmatter at write time.

**For agents generating user-facing docs:** the `generate-feature-docs` skill operates only on specs under `implemented/` with `status: implemented`. See `.claude/skills/generate-feature-docs/SKILL.md`.

Promoting a spec from draft to implemented is a `git mv` plus a frontmatter edit, ideally landed in the same PR that updates the spec body to match shipped code.
```

(The triple-backticks above use `\`\`\`` to escape; replace those with literal backticks when writing into CLAUDE.md.)

- [ ] **Step 3: Verify the markdown renders correctly**

```bash
grep -A 30 'Engineering Spec Conventions' CLAUDE.md
```

Expected: the new section is present and readable.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "Document spec-readiness convention in CLAUDE.md"
```

---

## Phase 2: Skill Implementation

Build the slash command and the skill itself. After Phase 2, the skill is invokable but has not yet been validated against real specs.

### Task 5: Create the slash command file

**Why:** The slash command is the user-facing entry point. Following the existing convention (`check-ci.md`, `verify.md`, etc.), it is a thin file that invokes the skill with `$ARGUMENTS`.

**Files:**
- Create: `.claude/commands/generate-feature-docs.md`

- [ ] **Step 1: Read an existing slash command to match the pattern**

```bash
cat .claude/commands/check-ci.md
```

The existing pattern is plain prose instructions. Slash commands in this repo do not currently use frontmatter or any special directives.

- [ ] **Step 2: Create the slash command file**

Write the following content to `.claude/commands/generate-feature-docs.md`:

```markdown
Generate publisher-facing documentation from an implemented engineering spec.

Spec path: $ARGUMENTS

Use the `generate-feature-docs` skill at `.claude/skills/generate-feature-docs/SKILL.md` to perform this task. The skill runs in two interactive stages (extraction pass for outline review, generation pass for prose and reference-doc updates) and commits the result on user approval.

If `$ARGUMENTS` is empty, ask the user which spec to document, defaulting to the most recently modified file under `docs/superpowers/specs/implemented/`.
```

- [ ] **Step 3: Verify the file was written correctly**

```bash
cat .claude/commands/generate-feature-docs.md
```

Expected: the content above.

- [ ] **Step 4: Commit**

```bash
git add .claude/commands/generate-feature-docs.md
git commit -m "Add /generate-feature-docs slash command"
```

---

### Task 6: Create SKILL.md with identity, invocation, and readiness rules

**Why:** SKILL.md is the heart of the skill. It is loaded into Claude's context whenever the skill is invoked. The file is built up across Tasks 6, 7, 8, and 9, one logical section per task, so each commit is reviewable in isolation. This task lays the foundation: the skill's identity, when it activates, what it operates on, and the spec-readiness convention.

**Files:**
- Create: `.claude/skills/generate-feature-docs/SKILL.md`

- [ ] **Step 1: Create the directory**

```bash
mkdir -p .claude/skills/generate-feature-docs
```

- [ ] **Step 2: Write SKILL.md with the identity and readiness sections**

Write the following content to `.claude/skills/generate-feature-docs/SKILL.md`. This is the initial file; subsequent tasks extend it.

````markdown
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

<!-- Tasks 7, 8, 9 will append stage 1, stage 2, and edge cases below this comment. Remove this comment when those sections are added. -->
````

- [ ] **Step 3: Verify the file**

```bash
head -20 .claude/skills/generate-feature-docs/SKILL.md
```

Expected: shows the YAML frontmatter and the first part of the content.

- [ ] **Step 4: Commit**

```bash
git add .claude/skills/generate-feature-docs/SKILL.md
git commit -m "Add SKILL.md with identity and readiness rules for generate-feature-docs"
```

---

### Task 7: Add stage 1 (extraction pass) instructions to SKILL.md

**Why:** Stage 1 is read-only and produces the structured outline. This is the key checkpoint where the user redirects the skill before any prose is written. Mistakes here are cheap; mistakes in stage 2 are expensive.

**Files:**
- Modify: `.claude/skills/generate-feature-docs/SKILL.md` (append)

- [ ] **Step 1: Append the stage 1 section to SKILL.md**

Append the following content to the end of the file:

````markdown

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
````

- [ ] **Step 2: Verify the file size grew as expected**

```bash
wc -l .claude/skills/generate-feature-docs/SKILL.md
```

Expected: substantially more than the previous step (somewhere around 130-180 lines depending on formatting).

- [ ] **Step 3: Commit**

```bash
git add .claude/skills/generate-feature-docs/SKILL.md
git commit -m "Add stage 1 extraction pass to generate-feature-docs skill"
```

---

### Task 8: Add stage 2 (generation pass) instructions to SKILL.md

**Why:** Stage 2 is where prose gets written, reference docs get updated, and commits are produced. This is the largest section of the skill.

**Files:**
- Modify: `.claude/skills/generate-feature-docs/SKILL.md` (append)

- [ ] **Step 1: Append the stage 2 section to SKILL.md**

Append the following content to the end of the file:

````markdown

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
  - [docs/guide/<slug>.md](docs/guide/<slug>.md) (<NEW or +<N> lines>, <description>)
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
````

- [ ] **Step 2: Verify the file grew**

```bash
wc -l .claude/skills/generate-feature-docs/SKILL.md
```

Expected: now in the range of 280-360 lines.

- [ ] **Step 3: Commit**

```bash
git add .claude/skills/generate-feature-docs/SKILL.md
git commit -m "Add stage 2 generation pass to generate-feature-docs skill"
```

---

### Task 9: Add edge cases section to SKILL.md

**Why:** Edge cases capture the "what if" scenarios from Section 10 of the spec. Without explicit guidance, Claude will default to forging ahead in cases where it should pause or abort. This task closes those holes.

**Files:**
- Modify: `.claude/skills/generate-feature-docs/SKILL.md` (append)

- [ ] **Step 1: Append the edge cases section**

Append the following content to the end of the file:

````markdown

## Edge cases and failure modes

| Case                                                   | Behavior                                                                                                                                                                          |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Spec lacks `status: implemented` frontmatter            | Prompt `Continue without status: implemented? (y/N)`. Default N. Abort unless explicit `y`.                                                                                       |
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
````

- [ ] **Step 2: Verify the file is complete**

```bash
wc -l .claude/skills/generate-feature-docs/SKILL.md
```

Expected: roughly 360-440 lines total.

```bash
grep -c '^## ' .claude/skills/generate-feature-docs/SKILL.md
```

Expected: at least 7 top-level sections (Output contract, Spec readiness check, Style rules, Slash command invocation, Stage 1, Stage 2, Edge cases, Idempotency, Self-check, Out of scope).

- [ ] **Step 3: Run a style self-check on the SKILL.md itself**

```bash
grep -nE "—|–" .claude/skills/generate-feature-docs/SKILL.md
grep -niE "powerful|seamless|robust|efficiently|leveraging" .claude/skills/generate-feature-docs/SKILL.md | grep -v "Forbidden words"
```

Expected: zero output for em-dashes. The marketing-words grep may show lines where the words are listed AS forbidden (those are correct uses); any other hits are bugs.

- [ ] **Step 4: Commit**

```bash
git add .claude/skills/generate-feature-docs/SKILL.md
git commit -m "Add edge cases and self-check rules to generate-feature-docs skill"
```

---

## Phase 3: Validation

The skill is now built but has not been run. Phase 3 invokes it against real specs and checks the output against the success criteria from Section 11 of the design spec. If validation reveals issues, edit `SKILL.md` and re-run; treat each tweak as a small commit.

### Task 10: Promote the RSL spec to implemented/ for greenfield validation

**Why:** Validation case 1 (greenfield) requires at least one spec to live in `implemented/`. The RSL AI crawler licensing spec is a good candidate because it is recent, has no corresponding guide page yet, and represents a complete feature.

**Files:**
- Move: `docs/superpowers/specs/drafts/2026-04-22-rsl-ai-crawler-licensing-design.md` to `docs/superpowers/specs/implemented/`
- Modify: the same file's frontmatter

- [ ] **Step 1: Find the implementation PR**

The spec was added in commit `8d081287 Add RSL AI crawler licensing design spec`. Identify the PR that landed the actual implementation (if any). If implementation has not yet shipped, the spec is not yet truly `implemented`, and this validation task should wait. Run:

```bash
git log --all --oneline | grep -i "rsl\|crawler\|licensing" | head -10
```

Inspect the output. If only the spec commit appears, implementation has not landed; pick a different spec for validation, such as one of the older specs whose features are clearly shipped (e.g., the Edge Cookie work).

For the rest of this task, assume the implementation has shipped. Substitute the actual PR number where the placeholder `<PR-NUMBER>` appears.

- [ ] **Step 2: Move the spec file**

```bash
git mv docs/superpowers/specs/drafts/2026-04-22-rsl-ai-crawler-licensing-design.md \
       docs/superpowers/specs/implemented/2026-04-22-rsl-ai-crawler-licensing-design.md
```

- [ ] **Step 3: Update the frontmatter**

Edit `docs/superpowers/specs/implemented/2026-04-22-rsl-ai-crawler-licensing-design.md`. Replace the existing frontmatter:

```yaml
---
status: draft
---
```

with:

```yaml
---
status: implemented
implemented_in: PR#<PR-NUMBER>
last_reviewed: 2026-04-28
---
```

- [ ] **Step 4: Verify**

```bash
head -6 docs/superpowers/specs/implemented/2026-04-22-rsl-ai-crawler-licensing-design.md
```

Expected: shows `status: implemented` and the optional fields.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/
git commit -m "Promote RSL AI crawler licensing spec to implemented"
```

---

### Task 11: Run greenfield validation case

**Why:** Validates the most common path: spec lands, code ships, docs do not exist, skill produces a publishable page.

**Files:**
- Will be created by the skill: `docs/guide/ai-crawler-licensing.md` (or similar slug; the skill resolves it)
- Will be modified by the skill: `docs/guide/configuration.md`, `docs/guide/api-reference.md`, and/or `docs/guide/error-reference.md`

- [ ] **Step 1: Confirm preconditions**

```bash
git branch --show-current
```

Expected: not `main` or `master`. If on main, switch first: `git checkout -b docs/ai-crawler-licensing`.

```bash
git status --short
```

Expected: empty (no uncommitted changes). If not, commit or stash unrelated work first.

- [ ] **Step 2: Invoke the skill**

In Claude Code:

```
/generate-feature-docs docs/superpowers/specs/implemented/2026-04-22-rsl-ai-crawler-licensing-design.md
```

The skill should run stage 1 and produce an extraction outline.

- [ ] **Step 3: Review the extraction outline**

Verify in the chat output:
- The feature name is reasonable.
- The target page slug is reasonable.
- All extracted handles (config keys, endpoints, headers, errors) are listed.
- Each handle has a status (verified or NOT FOUND).
- Issues, if any, are surfaced with A/B/C options.
- No em-dashes, no emojis, no marketing words appear in the outline.

If anything is wrong, redirect the skill or capture the issue and edit `SKILL.md`. Common issues to fix in SKILL.md: outline format incorrect, missing sections, handle types not extracted.

- [ ] **Step 4: Approve and proceed to stage 2**

Reply `proceed`. The skill should:
- Check the current branch (already verified in Step 1).
- Write the new feature page.
- Apply mechanical updates to the relevant reference docs.
- Post the diff-review message.

- [ ] **Step 5: Inspect the generated docs**

For each file in the diff-review message:
- Open the file. Read it as if you were a publisher integrating the feature.
- Verify the page follows the template (Overview, How it works, optional Sequence, Configuration, API contract, Error handling, Privacy, Related docs).
- Verify every concrete reference (config key, file path, endpoint, etc.) matches a verified handle from stage 1.
- Verify the prose is direct, present-tense, second-person, and contains no em-dashes, emojis, exclamation marks, or marketing words.

If the page reads as a draft you would merge with light editing: success. If it reads as something you would rewrite from scratch: capture what is wrong, edit SKILL.md, do not commit, re-run.

- [ ] **Step 6: Build the docs site to confirm no broken links**

```bash
cd docs && npm run build
```

Expected: build completes cleanly. Any errors point to broken VitePress link references.

- [ ] **Step 7: Style verification**

```bash
grep -nE "—|–" docs/guide/ai-crawler-licensing.md
grep -niE "powerful|seamless|robust|efficiently|leveraging" docs/guide/ai-crawler-licensing.md
grep -nE "[\xF0-\xF7][\x80-\xBF]+|✅|❌|⚠️|🔥" docs/guide/ai-crawler-licensing.md
```

Expected: zero output from all three. Any hits are skill bugs; fix SKILL.md and re-run.

- [ ] **Step 8: Approve commit**

If all checks pass, reply `commit` to the skill. The skill creates one commit on the current branch.

If any check fails, do not commit. Edit SKILL.md to fix the issue, then re-run from Step 2.

---

### Task 12: Run idempotency case

**Why:** Validates that re-running the skill produces no diff when neither spec nor code has changed. This is a hard requirement: a non-idempotent skill produces noise on every run.

**Files:**
- None modified (this is the assertion)

- [ ] **Step 1: Confirm clean working tree after Task 11**

```bash
git status --short
```

Expected: empty.

- [ ] **Step 2: Re-run the skill on the same spec**

```
/generate-feature-docs docs/superpowers/specs/implemented/2026-04-22-rsl-ai-crawler-licensing-design.md
```

- [ ] **Step 3: Approve through the outline and reach diff review**

Stage 1 should produce the same outline as Task 11. Reply `proceed`.

- [ ] **Step 4: Verify the skill detects zero changes**

The skill should post:
> "Re-run produced no changes. The docs are already up to date for this spec."

It must NOT produce an empty commit. It must NOT prompt for commit if nothing changed.

If the skill produces a non-empty diff on the second run, that is a bug. Inspect the diff to find which step is non-deterministic, edit SKILL.md, and re-run.

---

### Task 13: Run augmentation validation case

**Why:** Validates the augment-in-place behavior on an existing page. Picks a feature with an existing guide page and a spec that extends the feature.

**Files:**
- Will be modified by the skill: `docs/guide/edge-cookies.md` (or another existing page)
- Will be modified by the skill: `docs/guide/configuration.md`, `docs/guide/api-reference.md`, and/or `docs/guide/error-reference.md` if applicable

- [ ] **Step 1: Choose a candidate spec**

Pick one of the EC-related drafts (e.g., `2026-04-02-ec-kv-schema-extensions-design.md`) and promote it to `implemented/` if and only if its implementation has shipped. Use the same promotion procedure as Task 10. If implementation has not shipped, choose a different spec.

- [ ] **Step 2: Invoke the skill on the promoted spec**

```
/generate-feature-docs docs/superpowers/specs/implemented/<chosen-spec>.md
```

- [ ] **Step 3: Verify the extraction outline shows EXISTING for the target page**

The outline must show the target page (e.g., `docs/guide/edge-cookies.md`) with `(EXISTING)` tag. If it shows NEW, the slug resolution is wrong; either the spec's H1 produces a different slug than the existing page name, or the slug logic in SKILL.md is wrong.

- [ ] **Step 4: Approve and proceed**

Reply `proceed`.

- [ ] **Step 5: Verify augment-in-place behavior**

Open the modified existing page and compare to its prior content (use `git diff`). Confirm:
- Existing prose is intact except where contradiction-detection prompted you per item.
- New content was added in the right sections (Configuration table got new rows, etc.).
- No human-authored content was destroyed.

If the skill rewrote prose without prompting, that is a bug. Edit SKILL.md to enforce the conservative augment rule, undo the changes (`git checkout docs/guide/`), and re-run.

- [ ] **Step 6: Approve commit if checks pass**

Reply `commit` to the skill.

---

### Task 14: Run non-feature validation case

**Why:** Validates that the skill correctly detects a non-feature spec and prompts before proceeding rather than silently producing nonsense.

**Files:**
- None expected to be modified (the skill should refuse to proceed without confirmation)

- [ ] **Step 1: Promote the EdgeZero migration spec to implemented/**

Use the procedure from Task 10:

```bash
git mv docs/superpowers/specs/drafts/2026-03-19-edgezero-migration-design.md \
       docs/superpowers/specs/implemented/2026-03-19-edgezero-migration-design.md
```

Update its frontmatter to `status: implemented`, then commit.

- [ ] **Step 2: Invoke the skill on the migration spec**

```
/generate-feature-docs docs/superpowers/specs/implemented/2026-03-19-edgezero-migration-design.md
```

- [ ] **Step 3: Verify the skill detects spec kind and prompts**

Expected behavior: stage 1 detects `spec_kind: migration` and emits:
> "This looks like a `migration` spec, not a feature spec. Continue anyway, or abort?"

If the skill silently proceeds, that is a bug; the spec-kind detection in SKILL.md needs strengthening.

- [ ] **Step 4: Reply with abort**

Reply something like "abort" or "no, this is a migration spec, do not proceed". Confirm the skill exits cleanly without writing any files.

- [ ] **Step 5: Verify no files were written**

```bash
git status --short
```

Expected: empty (or whatever was there before, unchanged).

---

### Task 15: Run drift validation case

**Why:** Validates handle verification. Tests that a spec with a config key, endpoint, header, or error variant that does not exist in code is flagged in stage 1 with a NOT FOUND status, and the skill correctly handles the user's choice (mark as TODO, drop, or pause).

**Files:**
- A test spec with a deliberately-broken handle

- [ ] **Step 1: Create a test spec with a broken handle**

Make a minimal feature spec under `docs/superpowers/specs/implemented/` named `2026-04-28-test-drift-validation.md` with the following content:

```markdown
---
status: implemented
last_reviewed: 2026-04-28
---

# Test Drift Validation

A synthetic feature for validating handle drift detection. Not a real feature.

## Configuration

```toml
[test_drift]
enabled = true
nonexistent_key = "this key does not exist in code"
```

## API contract

GET `/test-drift/nonexistent-endpoint` returns drift validation data.
```

- [ ] **Step 2: Invoke the skill on the test spec**

```
/generate-feature-docs docs/superpowers/specs/implemented/2026-04-28-test-drift-validation.md
```

- [ ] **Step 3: Verify the extraction outline flags the drift**

The outline must show:
- Config key `test_drift.enabled`: NOT FOUND, location "spec only"
- Config key `test_drift.nonexistent_key`: NOT FOUND, location "spec only"
- Endpoint `/test-drift/nonexistent-endpoint`: NOT FOUND, location "spec only"

The Issues section must list these and offer A/B/C options for each.

If the skill marks any of these as `verified` (since they do not exist in code), that is a bug; the verification logic in SKILL.md is wrong.

- [ ] **Step 4: Reply with abort to clean up**

Reply: "abort, this was just a drift test".

- [ ] **Step 5: Delete the test spec**

```bash
git rm docs/superpowers/specs/implemented/2026-04-28-test-drift-validation.md
git commit -m "Remove drift validation test spec"
```

---

### Task 16: Final verification and handoff

**Why:** Confirms all moving parts are in place and the team can use the skill from this point forward.

- [ ] **Step 1: Verify the slash command is recognized**

In Claude Code, type `/generate-feature-docs` (no arguments) and confirm Claude offers to use the skill. The skill should ask which spec to document, defaulting to the most recently modified file under `docs/superpowers/specs/implemented/`.

- [ ] **Step 2: Run the full test suite to confirm nothing else regressed**

```bash
cd /Users/jevans/trusted-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cd crates/js/lib && npx vitest run
cd ../../../docs && npm run format && npm run build
```

Expected: all pass. Any failure indicates a side effect from this work.

- [ ] **Step 3: Verify the public docs site no longer indexes specs**

```bash
find docs/.vitepress/dist -path '*/superpowers/*'
```

Expected: empty.

- [ ] **Step 4: Update the team**

Post a short note in the team channel describing: the skill is available, where to find the source (`.claude/skills/generate-feature-docs/`), how to invoke it (`/generate-feature-docs <spec-path>`), and the spec lifecycle convention (drafts/ vs implemented/, status frontmatter).

- [ ] **Step 5: Open a follow-up issue for skill #2**

Create an issue titled "Build spec-vs-reality gap-analysis skill (skill #2)" referencing this skill's design spec section 13 (Out of scope, deferred to skill #2). Skill #2 closes the behavioral-drift gap that this skill cannot detect.

- [ ] **Step 6: Open a follow-up issue for the upstream Superpowers PR**

Create an issue titled "Propose drafts/implemented split to Superpowers brainstorming skill" referencing this skill's design spec section 14 (Related work). The PR is to update the brainstorming skill to write to `<spec-root>/drafts/` by default and add `status: draft` frontmatter automatically.

---

## Self-Review

After writing this plan, the following coverage check confirms each spec section maps to a task:

- Spec section 1 (Overview): implicit in plan goals.
- Spec section 2 (Audience): encoded in SKILL.md style rules (Task 6).
- Spec section 3 (Goals): drives validation criteria (Tasks 11-15).
- Spec section 4 (Non-goals): encoded in SKILL.md "Out of scope" section (Task 9).
- Spec section 5 (Skill identity and invocation): Tasks 5, 6.
- Spec section 6 (Spec readiness convention): Task 4 (CLAUDE.md), Task 6 (SKILL.md readiness check).
- Spec section 7 (Directory layout): Tasks 2, 3.
- Spec section 8 (Stage 1 extraction): Task 7.
- Spec section 9 (Stage 2 generation): Task 8.
- Spec section 10 (Edge cases): Task 9.
- Spec section 11 (Verification and validation): Tasks 11-15.
- Spec section 12 (Prerequisites): Tasks 1-4.
- Spec section 13 (Out of scope, deferred to skill #2): Task 16 step 5 (open follow-up issue).
- Spec section 14 (Related work, upstream PR): Task 16 step 6 (open follow-up issue).
- Spec section 15 (Implementation summary): meta, no task needed.

**Placeholder scan:** the plan uses literal placeholders only inside code blocks that demonstrate format (e.g., `<feature name>`, `<PR-NUMBER>`). These are documentation, not unfilled gaps. The plan itself contains no TBDs or TODOs.

**Type consistency:** the slash command name (`/generate-feature-docs`), the skill path (`.claude/skills/generate-feature-docs/SKILL.md`), the slash command path (`.claude/commands/generate-feature-docs.md`), the directory paths (`docs/superpowers/specs/drafts/`, `docs/superpowers/specs/implemented/`), and the frontmatter field name (`status`) are used consistently throughout.

The plan is ready for execution.

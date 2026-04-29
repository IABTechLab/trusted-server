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

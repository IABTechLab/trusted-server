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

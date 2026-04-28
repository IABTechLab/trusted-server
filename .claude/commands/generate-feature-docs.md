Generate publisher-facing documentation from an implemented engineering spec.

Spec path: $ARGUMENTS

Use the `generate-feature-docs` skill at `.claude/skills/generate-feature-docs/SKILL.md` to perform this task. The skill runs in two interactive stages (extraction pass for outline review, generation pass for prose and reference-doc updates) and commits the result on user approval.

If `$ARGUMENTS` is empty, ask the user which spec to document, defaulting to the most recently modified file under `docs/superpowers/specs/implemented/`.

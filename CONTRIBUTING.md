# Contributing

Trusted Server accepts focused code and documentation contributions. By
submitting a contribution, you agree that it is licensed under the repository's
Apache License 2.0.

## Before implementation

Search the issue tracker before opening a new issue. For a substantial change,
describe the problem, intended behavior, affected adapters, and compatibility
constraints before investing in an implementation. Do not place credentials or
non-public vulnerability details in an issue; contact the maintainers privately
before disclosure.

Read [CLAUDE.md](CLAUDE.md) for repository architecture, target constraints,
coding conventions, error handling, documentation rules, and commit policy.
The [project governance document](ProjectGovernance.md) defines decision and
release responsibilities.

## Make a focused change

- Keep one pull request centered on one coherent outcome.
- Do not refactor or reformat unrelated code.
- Add tests for new behavior and regression tests for defects.
- Keep platform-specific dependencies out of `trusted-server-core`.
- Use `error-stack` reports for production errors; the Spin entry-point FFI is
  the sole documented `anyhow` exception.
- Use fictional example data by default. A real public vendor endpoint requires
  the exact reviewed exception described in `CLAUDE.md`.
- Update the canonical guide or crate README when a user-visible contract
  changes; do not copy volatile matrices into multiple documents.

## Verify

Run the target-matched checks in [CLAUDE.md](CLAUDE.md#ci-gates), the
canonical command surface for local and CI verification. [TESTING.md](TESTING.md)
links focused runbooks without duplicating that matrix.

Keep a pull request in draft while required checks or known changes remain.
Before requesting review, inspect the complete diff, resolve all failures, and
state any platform path that could not be exercised.

## Commits

Write concise, imperative, sentence-case subjects without semantic prefixes or
bracketed tags. Keep the subject at 50 characters when practical and do not end
it with a period. Use a wrapped body when the reason, compatibility effect, or
non-obvious tradeoff is not evident from the diff.

Examples:

- `Document Cloudflare startup configuration`
- `Reject ambiguous integration route records`

## Review

Review the change, not the author. Tie blocking feedback to a correctness,
security, compatibility, maintainability, or documented-requirement concern.
Mark optional improvements as non-blocking and move unrelated work to a
separate issue.

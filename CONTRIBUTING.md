# Contributing

Trusted Server accepts focused code and documentation contributions. By
submitting a contribution, you agree that it is licensed under the repository's
Apache License 2.0.

## Before implementation

Search the issue tracker before opening a new issue. Features and other
substantial changes need an issue that describes the problem, intended behavior,
affected adapters, and compatibility constraints; open it before you invest in
an implementation. Smaller pull requests link an issue when one exists. Do not
place credentials or non-public vulnerability details in an issue; contact the
maintainers privately before disclosure.

Read [AGENTS.md](AGENTS.md) for repository architecture, target constraints,
coding conventions, error handling, documentation rules, and commit policy.
The [project governance document](ProjectGovernance.md) defines decision and
release responsibilities.

## Features start with a spec

Trusted Server has a lot of moving parts. A single feature can touch four
runtime adapters, consent enforcement, Edge Cookie identity, the auction
pipeline, streaming HTML rewriting, and the browser bundles, and the ways those
pieces interact are easy to miss in code review. A design problem caught in a
spec costs an afternoon. Caught in code review, it costs a rewrite.

So every pull request that adds a feature needs a written spec, worked out with
an AI coding agent and then reviewed with one over at least two rounds. We know
this is more work before you write any code. It pays off. Reviewers check your
code against a design everyone has already agreed on, and you are much less
likely to be asked to start over.

### What counts as a feature

A feature adds or changes behavior that people outside the code rely on. That
includes a new integration, route, CLI command, or configuration key, and any
change to request handling, consent, identity, auctions, or a public API.

Bug fixes, documentation, dependency updates, CI changes, and refactors that
keep behavior the same don't need a spec. A maintainer may still ask for one
when a fix changes a contract or spans several crates.

When in doubt, write a spec.

### Write it with the superpowers brainstorming skill

Use the `brainstorming` skill from
[superpowers](https://github.com/obra/superpowers) to arrive at the spec. It
runs in Claude Code, Codex, Cursor, and several other coding agents, and the
superpowers README covers installation for each.

With the skill loaded, the agent reads the relevant code, asks you about
purpose and constraints one question at a time, proposes two or three
approaches with their tradeoffs, and walks you through the design section by
section. Then it writes the spec to
`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`, next to the existing
specs.

The skill sorts work into spike, bounded, and architectural paths, and only the
architectural path produces a spec file. For a feature, tell the agent to take
the architectural path even if it suggests a lighter one.

Run brainstorming even when you already know what you want to build. Its
questions are where the constraints you didn't know about tend to surface.

Don't force a large feature into one huge spec. If the work splits into natural
phases, write one spec per phase, each with its own review rounds and pull
request, and link the phase specs to each other so reviewers can see the whole
plan. Brainstorming suggests a split when it sees one; take it.

Before you move on to review, check that the spec covers:

- each adapter the change affects (Fastly, Cloudflare, Spin, Axum), and why the
  others are unaffected
- new or changed configuration, including defaults and validation
- consent and privacy effects
- failure handling
- compatibility and rollout for existing deployments
- the tests that prove it works

### Review it with an agent, round by round

First drafts have gaps. Before you open the pull request, review the spec with
an AI coding agent in rounds:

1. Start a fresh agent session, separate from the one that wrote the spec, so
   the agent isn't reviewing its own work. Switching agents or models between
   rounds helps too, since each one misses different things.
2. Ask it to check every claim the spec makes about existing code by reading
   the source, and to look for the gaps listed above. The brainstorming skill
   ships a reviewer prompt in `spec-document-reviewer-prompt.md` that you can
   start from.
3. Fix what it finds, or write down in the spec why you disagree.
4. Run at least two rounds, and keep going until a round finds nothing
   blocking. Larger features usually take more.

End the spec with a `## Review log` section, one entry per round:

```markdown
## Review log

- Round 1 (Claude Code): found the Spin adapter route missing and no handling
  for a failed KV write. Added both to Design and Testing.
- Round 2 (Codex): no blocking findings. Clarified the default for
  `example_timeout_ms`.
```

Reviewers read the log to see which questions have already been asked and
answered.

### Open the pull request

Commit the spec on your feature branch, or land it first in its own pull
request. A separate spec pull request suits larger features, since the design
can settle before any code depends on it. Either way, link the spec in the
pull request description.

The agent review doesn't replace human review. Maintainers review the design as
well as the code, and they may ask for spec changes before they read the
implementation. You own every line of the spec, so read it closely and don't
submit anything you can't explain.

Don't paste credentials, customer data, or non-public vulnerability details
into an AI tool. The rules for fictional example data apply to specs too.

### Without a spec

If a feature pull request arrives without a linked, agent-reviewed spec, a
maintainer will move it back to draft and point you here. Reviewers won't read
the code until the spec is in place. The pull request stays open. Add the spec,
link it in the description, and mark it ready for review again.

## Make a focused change

- Keep one pull request centered on one coherent outcome.
- Do not refactor or reformat unrelated code.
- Add tests for new behavior and regression tests for defects.
- Keep platform-specific dependencies out of `trusted-server-core`.
- Use `error-stack` reports for production errors; the Spin entry-point FFI is
  the sole documented `anyhow` exception.
- Use fictional example data by default. A real host needs a reviewed entry in
  the URL-host allowlist or a per-line `allow-domain` suppression; see
  [Pre-commit URL-host linter](#pre-commit-url-host-linter).
- Update the canonical guide or crate README when a user-visible contract
  changes; do not copy volatile matrices into multiple documents.

## Verify

Run the target-matched checks in [AGENTS.md](AGENTS.md#ci-gates), the
canonical command surface for local and CI verification. [TESTING.md](TESTING.md)
covers auction-orchestration testing specifically and repeats the adapter test
aliases relevant to that runbook; it isn't a link index for other runbooks.

Keep a pull request in draft while required checks or known changes remain.
Before requesting review, inspect the complete diff, resolve all failures, and
state any platform path that could not be exercised.

### Pre-commit URL-host linter

`ts dev lint domains` flags non-allowlisted URL hosts in source, config, and
docs. Install the managed hook once per checkout so staged changes are checked
before every commit:

```bash
ts dev install-hooks
```

This writes a `pre-commit` hook into `.git/hooks`, shared by every linked
worktree of the clone. It never edits git configuration and never writes into
the working tree. It refuses to replace a hook it did not write (`--force`
replaces it and prints the backup path), and refuses when `core.hooksPath` is
set, since git then runs hooks from that directory instead. To uninstall, delete
`.git/hooks/pre-commit` and restore any `pre-commit.bak.*` backup.

To scan on demand, `ts dev lint domains` audits the whole working tree and
`--changed-vs <ref>` limits it to lines added against a ref. The full audit
reports pre-existing violations, so a non-zero exit is expected today.

CI runs the same check on every pull request: the `lint-domains` job scans the
lines the PR adds against its base branch, so `--no-verify` or a missing hook
does not let a host through. Editing or moving a line that already contains a
non-allowlisted host counts as adding it. Reproduce the job locally with
`ts dev lint domains --changed-vs origin/main`, using the PR's base branch in
place of `main` when it targets another branch.

To allow a new host, add it to the allowlist constants in
`crates/trusted-server-cli/src/commands/dev/lint/domains.rs`. To suppress a
single line, append `allow-domain: <host>` in the file's comment syntax:
`// allow-domain: <host>`, `# allow-domain: <host>`,
`<!-- allow-domain: <host> -->`, or `* allow-domain: <host>`.

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

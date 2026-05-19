# `ts dev lint domains` — Design

**Date:** 2026-05-18
**Status:** Draft (revised after third review — pivoted to Rust / `ts` CLI)

## Goal

Fail commits that introduce new **URL hosts** (extracted from `http(s)://`
and protocol-relative `//host/` URLs) that are not on an explicit
allowlist, across source, config, and documentation files. Catches
accidental test-pollution domains (e.g., `test.com`, `partner.com`,
`new.com`) and hardcoded third-party endpoints that have not been vetted
as integration proxies.

Enforces the rule: **production code, tests, and config may only reference
`example.com` (and its subdomains), loopback addresses, an explicit list of
integration-proxy endpoints, or a small set of reference/doc-link hosts.**

The term **URL host** (not "domain") is used throughout because the linter
only inspects the host portion of an extracted URL. Bare hostnames written
as plain strings (e.g., `cookie_domain = "test-publisher.com"`,
`exclude_domains = ["foo.com"]`) are **not** detected.

## Prerequisite

This design **depends on PR #669** (`Add the Trusted Server CLI`, branch
`feature/ts-cli`). PR #669 introduces the `crates/trusted-server-cli`
crate, the `ts` binary, the `cargo install_cli` alias, the host-target
CI lane, and the clap command-surface conventions this design extends.

**Required base for any implementation work:** a branch whose ancestry
contains PR #669. Two acceptable bases:

- `main`, after #669 has merged, **or**
- `origin/feature/ts-cli` directly (stacked on PR #669's branch), with
  a rebase onto `main` once #669 merges.

A plain `main` checkout that *predates* #669's merge cannot host this
implementation — the CLI surface this design extends does not exist
there. See [Implementation Readiness](#implementation-readiness) for
the full start-condition checklist.

## Implementation Readiness

**Status today: ready to start *only on a branch stacked on PR #669*.**
A plain `main` checkout has no `crates/trusted-server-cli`, no `ts`
binary, no `cargo install_cli` alias, and no host-target CI lane —
starting there would force the implementer to reinvent or duplicate
PR #669's surface. Implementation must happen on a branch whose base
includes #669.

**Two acceptable execution paths:**

1. **Wait for #669 to merge to `main`.** Then start implementation on
   a branch off `main`. Simplest history; lowest coordination cost.
2. **Stack on `origin/feature/ts-cli` (PR #669's branch) now.**
   Create the implementation branch off `feature/ts-cli`. The branch
   carries PR #669's commits as ancestors; once #669 merges, rebase
   onto `main` (the rebase is a no-op for the ancestors). Faster to
   start; requires re-syncing if #669 force-pushes.

**Start conditions** (all must be true on whichever base is chosen):

1. `crates/trusted-server-cli` exists at the branch base — verify
   with `ls crates/trusted-server-cli/src/`.
2. This PR owns the `ts dev` subcommand-group refactor: today's
   `ts dev` leaf becomes `ts dev serve`, and the same PR adds
   `ts dev lint domains` and `ts dev install-hooks`. Do not defer
   this refactor to a later cleanup PR — without it, the command
   surface described here does not exist.
3. The chosen `gix` + `gix-config` version pair resolves against the
   workspace's transitive dep graph without forcing duplicates
   (verify with `cargo tree -p gix -p gix-config`).

**Suggested first-implementation order** (front-loads the riskiest
API assumptions, matches reviewer guidance):

1. **Spike — gix feasibility.** In a throwaway branch off the chosen
   #669-containing base (either `main` after #669 merges or the
   stacked `feature/ts-cli` base), pin a matched `gix` +
   `gix-config` release-family pair (verify via
   `cargo tree -p gix -p gix-config` that no duplicate versions land
   in the lock file), then write three integration tests that drive
   the conceptual operations end-to-end against a `tempfile`-built
   repo: (a) staged blob diff with new-side line numbers; (b)
   merge-base + tree-vs-tree blob diff; (c) durable `core.hooksPath`
   write via `gix-config::File`.

   **Spike acceptance gate** — all of the following:
   - The three tests pass deterministically on a clean run.
   - `cargo tree -p gix -p gix-config` shows exactly one version
     of each, no `(*)` duplicate-version markers in the dep graph.
   - The chosen `gix` entry points for index-vs-tree / tree-vs-tree
     walking and blob diff are pinned in test source (no
     placeholder names).

   **Spike deliverables back into this spec** (single PR alongside
   the spike code):
   - Update the version pins in
     [Cargo dependencies](#cargo-dependencies) with the chosen
     numbers and a short comment naming the release family.
     Replacing the `<pin-during-spike>` placeholders is part of the
     spike's definition-of-done, not a follow-up.
   - Update Open Questions to reflect the chosen `gix` API entry
     points (Open Q5) and the pinned version (Open Q6).
   - Update the "prototype-required" callout in
     [Line collection: --staged mode (gitoxide)](#line-collection---staged-mode-gitoxide)
     to name the chosen entry points instead of the placeholder
     `index_vs_tree_changes` / `tree_vs_tree_changes` /
     `blob_diff_added_hunks` helpers.
2. **URL extraction + allowlist + suppression.** Pure-function
   layer, fully unit-testable without `gix`. Implement against the
   regex / allowlist / marker grammar in this spec; cover every
   test case enumerated in [Testing Strategy](#testing-strategy)
   that does not require git.
3. **CLI wiring.** Add the `Commands::Dev` subcommand-group
   skeleton (preserving the existing `serve` subcommand wholesale),
   then add `dev lint domains` dispatching to the function from
   step 2 plus the diff collectors from step 1.
4. **`dev install-hooks`.** Wires steps 1 and 2 together for the
   config write + hook file write + shell-escape path.
5. **End-to-end `assert_cmd` tests** matching `Testing Strategy`.
6. **Stage 1 doc cleanup** (separate PR series — see
   [Stage 1 Doc Cleanup Plan](#stage-1-doc-cleanup-plan)).

If start conditions aren't satisfied when this design is up for
implementation, the answer is "wait for #669," not "build a parallel
CLI surface."

## Non-Goals

- No CI gate in v1. The pre-commit hook is the only enforcement mechanism.
  See [Migration to CI](#migration-to-ci).
- No baseline file. Existing violations are tolerated; the linter is scoped
  to new lines.
- No autofix.
- No detection of bare hostnames without an `http(s)://` or `//` prefix.
- **Publisher-capture HTML fixtures are excluded by path** —
  specifically the `crates/trusted-server-core/src/integrations/**/fixtures/**`
  tree, which contains real-world captured publisher pages used as
  test fixtures for the HTML processor. Those files have hundreds
  of legitimate third-party URLs (Facebook, typekit, ad networks)
  that cannot reasonably be allowlisted; trying would either
  drown the linter in noise or force a giant allowlist that
  defeats its review purpose. **Other HTML, CSS, and Dockerfile
  files are scanned** (see [File extensions scanned](#file-extensions-scanned)).

## CLI Surface

A new top-level subcommand on the `ts` CLI:

```
ts dev lint domains [--staged | --changed-vs <ref> | <paths>...]
                [--format human|json] [--verbose]
```

Modes (mutually exclusive):

| Invocation                           | Behavior                                                                                                                 |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| `ts dev lint domains`                    | Full-repo audit. Walks tracked files matching the extension filter and scans every line. **Diagnostic only in Stage 1.** |
| `ts dev lint domains --staged`           | Pre-commit mode. Scans only added lines in `git diff --cached`. Existing violations not reported.                        |
| `ts dev lint domains --changed-vs <ref>` | CI/PR mode (Stage 2). Scans only added lines in the diff **equivalent to** `git diff $(git merge-base <ref> HEAD)..HEAD` — computed via gitoxide, not by shelling out. |
| `ts dev lint domains path/...`           | Scans the listed files in full.                                                                                          |

Output format defaults to `human`. `--format json` emits a structured
report (see [Output Format](#output-format)).

Exit codes: `0` no violations; `1` violations found; `2` usage or
environment error.

**Required change to existing CLI exit-code mapping.** PR #669's
`crates/trusted-server-cli/src/lib.rs::run()` currently maps every
non-`CliError::Cancelled` error to `ExitCode::from(1)`. That collapses
the violation-vs-environment-error distinction this contract requires
— in CI, a failed git open and a real violation would be
indistinguishable.

**This PR therefore must extend the existing `CliError` and `run()`:**

1. Add a `CliError::EnvironmentError` variant (name TBD; could be
   `EnvIo` or similar to match the crate's existing naming) that
   carries the underlying `Report` as context.
2. The lint module wraps env-class errors (gix open fails, no git
   repo, missing base ref, no working tree, gix-config write fails,
   filesystem permission errors at install-hooks time) as
   `CliError::EnvironmentError`.
3. When the scan finds violations, the lint module **returns
   `Err(CliError::ViolationsFound { count })`**. This is a
   semantically-meaningful "error" — it carries the violation count
   for the message and surfaces through the same `run()` dispatch
   that maps `CliError::Cancelled` to exit 130. Pick one model: in
   this spec, violations propagate as `Err`, not `Ok(())`. The
   match arm in step 4 is what distinguishes a "violations found"
   exit from an environment-error exit.
4. `lib.rs::run()` pattern-matches:

   ```rust
   match execute() {
       Ok(()) => ExitCode::SUCCESS,
       Err(error) => match error.current_context() {
           CliError::Cancelled => ExitCode::from(130),
           CliError::ViolationsFound { .. } => ExitCode::from(1),
           CliError::EnvironmentError => ExitCode::from(2),
           // … all other existing variants map to 1 unchanged
           _ => ExitCode::from(1),
       },
   }
   ```

The two new variants and the dispatch arm are part of this PR's
scope, not a follow-up. The sketch function signature shown later in
this spec — `fn run(...) -> Result<i32, Report<DomainsLintError>>`
— is illustrative; the production shape returns
`Result<(), Report<CliError>>` matching the existing convention,
with the exit code emerging from the `current_context()` match
above.

### Why `ts dev` as the parent?

`lint domains` and `install-hooks` are developer-workflow commands —
they only matter when working on the codebase, not when operating a
deployed Trusted Server. Grouping them under `dev` keeps the
top-level `ts` surface focused on operator concerns (`config`,
`auth`, `audit`, `provision`) and gives developer tooling a natural
home for future additions (`ts dev lint deps`, `ts dev format`,
`ts dev check`, etc.).

Within `dev`, `lint` is itself a subcommand group (so future lints
slot in as `ts dev lint <thing>`).

## Crate Layout

PR #669 ships `ts dev` as a single-file leaf command
(`crates/trusted-server-cli/src/dev.rs`, ~161 lines) that starts the
local Fastly dev server. To host nested subcommands, that file is
converted into a module directory:

```
crates/trusted-server-cli/src/
  lib.rs                          # add Commands::Dev(DevArgs) variant
                                  # if not already present; dispatch
                                  # to dev::run
  dev/
    mod.rs                        # Dev subcommand enum + dispatch.
                                  # Includes the existing dev-server
                                  # behavior as `ts dev serve` so
                                  # the PR #669 functionality is
                                  # preserved under the new group.
    serve.rs                      # the existing dev.rs body moved
                                  # under `ts dev serve`
    install_hooks.rs              # `ts dev install-hooks`
    lint/
      mod.rs                      # Lint subsubcommand enum + dispatch
      domains.rs                  # this design's implementation
```

Existing code touched:

- `crates/trusted-server-cli/src/lib.rs` — extend the existing
  `Commands::Dev` variant so it owns a nested `DevCommand` enum
  (subcommands: `Serve`, `Lint(LintCommand)`, `InstallHooks(...)`).
- `crates/trusted-server-cli/src/dev.rs` → split into the directory
  above. The existing dev-server function moves into `dev/serve.rs`
  with its public API unchanged. **This PR must make the CLI-surface
  change**: today's `ts dev` becomes `ts dev serve`. This is not a
  follow-up task; `ts dev lint domains` and `ts dev install-hooks`
  cannot be added cleanly while `ts dev` remains a leaf command.

  **`ts dev serve` must preserve every flag and behavior of today's
  `ts dev` leaf**, byte-for-byte from a user's perspective:

  | Existing `ts dev` flag                                  | `ts dev serve` requirement |
  | ------------------------------------------------------- | -------------------------- |
  | `--adapter / -a` (default `fastly`)                     | Same default, same enum    |
  | `--config` (`Option<PathBuf>`)                          | Preserved unchanged        |
  | `--env` (default `local`)                               | Preserved unchanged        |
  | Trailing `passthrough` args (`trailing_var_arg = true`, `allow_hyphen_values = true`) | Preserved unchanged — the `serve` subcommand still forwards everything after the recognized flags to the underlying runner |

  In other words: any shell invocation that works today as
  `ts dev --adapter=fastly --config=... --env=local -- --extra ...`
  must work tomorrow as `ts dev serve --adapter=fastly
  --config=... --env=local -- --extra ...` with identical effect.
  The refactor is a structural rename, not a behavior change.
  Verification: an end-to-end test asserts that
  `ts dev serve --help` lists the same flags as today's
  `ts dev --help`, and that trailing-arg passthrough still reaches
  the runner.
- `crates/trusted-server-cli/src/error.rs` — add `LintError` and
  `InstallHooksError` variants if needed for typed propagation,
  otherwise reuse the crate's existing `Report<CliError>` plumbing.

No changes to `trusted-server-core` or `trusted-server-adapter-fastly`.

## Allowlist (Rust constants)

Three arrays as `const &[&str]` at module top of `dev/lint/domains.rs`:
`EXACT_HOSTS` (integration proxies + loopback), `SUBDOMAIN_HOSTS`
(allow `*.host`), and `REFERENCE_HOSTS` (well-known doc/spec
sources, exact-match, allowed everywhere). The split keeps the
security review for each group focused: integration-proxy additions
need vendor justification; reference-host additions just need "is this
a legitimate documentation source we link to repeatedly?"

### Exact-match hosts (`EXACT_HOSTS`)

Integration proxies and loopback. Subdomains are **not** allowed
(e.g., `anything.api.privacy-center.org` is disallowed).

| Category                                             | Hosts                                                                          |
| ---------------------------------------------------- | ------------------------------------------------------------------------------ |
| Loopback                                             | `127.0.0.1`, `::1`, `localhost`                                                |
| Integration proxies (didomi)                         | `api.privacy-center.org`, `sdk.privacy-center.org`                             |
| Integration proxies (sourcepoint)                    | `cdn.privacy-mgmt.com`                                                         |
| Integration proxies (lockr)                          | `aim.loc.kr`, `identity.loc.kr`                                                |
| Integration proxies (datadome)                       | `js.datadome.co`, `api-js.datadome.co`                                         |
| Integration proxies (aps / Amazon)                   | `aax.amazon-adsystem.com`, `aax-events.amazon-adsystem.com`                    |
| Integration proxies (permutive)                      | `api.permutive.com`, `secure-signals.permutive.app`, `cdn.permutive.com`       |
| Integration proxies (Google Tag Manager / Analytics) | `www.googletagmanager.com`, `www.google-analytics.com`, `analytics.google.com` |
| Integration proxies (adserver mock)                  | `securepubads.g.doubleclick.net`, `origin-mocktioneer.cdintel.com`             |
| Integration proxies (Prebid CDN)                     | `cdn.prebid.org`                                                               |
| Integration proxies (Fastly platform)                | `api.fastly.com`                                                               |

### Subdomain-permitting hosts (`SUBDOMAIN_HOSTS`)

The host equals one of these **or** ends with `.` + one of these.

| Host                 | Allows                                              | Why subdomain matching                                                                                                                                                                                |
| -------------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `example.com`        | `example.com`, `foo.example.com`, `a.b.example.com` | IANA RFC 2606 reserved; arbitrary subdomains expected in test fixtures and docs                                                                                                                       |
| `example.net`        | `example.net`, `assets.example.net`, etc.           | IANA RFC 2606 reserved; appears in real docs (`https://assets.example.net`)                                                                                                                           |
| `example.org`        | `example.org`, `*.example.org`                      | IANA RFC 2606 reserved                                                                                                                                                                                |
| `edge.permutive.app` | `edge.permutive.app`, `<org>.edge.permutive.app`    | Permutive constructs the host as `{organization_id}.edge.permutive.app` at runtime (see `crates/trusted-server-core/src/integrations/permutive.rs:93`); subdomains are vendor-controlled per customer |

### Reference / doc hosts (`REFERENCE_HOSTS`)

Exact-match. Allowed in every scanned file (no docs-vs-code split).
These are well-known documentation and spec sources that appear as
markdown link targets, `///` doc-comment URLs, `#` config comments,
etc.

**The table below is the seed list curated from a sampling of current
`.md` files. It is expected to be incomplete on first pass.** The
Stage 1 cleanup workstream (see
[Stage 1 Doc Cleanup Plan](#stage-1-doc-cleanup-plan)) drives the
actual final list by running the full-repo audit, sorting hosts by
frequency, and triaging each into one of: add to `REFERENCE_HOSTS`,
add to integration `EXACT_HOSTS`, rewrite to a reserved host, or
suppress per-line.

| Category                | Hosts                                                                                          |
| ----------------------- | ---------------------------------------------------------------------------------------------- |
| Git / GitHub            | `github.com`, `docs.github.com`, `help.github.com`, `token.actions.githubusercontent.com`      |
| Git commit conventions  | `chris.beams.io`                                                                               |
| Rust                    | `docs.rs`, `doc.rust-lang.org`, `crates.io`                                                    |
| Web / W3C standards     | `www.w3.org`, `schema.org`                                                                     |
| Versioning / changelogs | `semver.org`, `keepachangelog.com`                                                             |
| IAB Tech Lab            | `iab.com`, `iabtechlab.com`, `iabtechlab.github.io`, `iabeurope.github.io`                     |
| Specs (supply chain)    | `in-toto.io`, `rslstandard.org`                                                                |
| Specs (other)           | `webassembly.org`                                                                              |
| Fastly docs             | `www.fastly.com`, `developer.fastly.com`, `manage.fastly.com`                                  |
| Cloudflare docs         | `developers.cloudflare.com`                                                                    |
| Vendor docs             | `docs.datadome.co`, `docs.prebid.org`                                                          |
| Tooling docs            | `vitepress.dev`, `playwright.dev`, `testcontainers.com`, `grafana.com`, `docsearch.algolia.com` |

One-off references not on this list (e.g., a single arxiv.org link in
a security spec) should use the per-line suppression marker —
inflating `REFERENCE_HOSTS` with single-use entries defeats its review
purpose.

### IANA-reserved TLD rule

Any host ending in `.example`, `.test`, `.invalid`, or `.localhost`
is allowed (IANA RFC 2606 reserves these TLDs for documentation,
testing, and special use). Hard-coded suffix check, not list entries.

### Matching summary

| Host                                | Allowed?                                  |
| ----------------------------------- | ----------------------------------------- |
| `example.com`                       | yes (subdomain-list)                      |
| `foo.example.com`                   | yes (subdomain-list)                      |
| `assets.example.net`                | yes (subdomain-list)                      |
| `example.com.evil.com`              | **no** (not a subdomain of `example.com`) |
| `api.fastly.com`                    | yes (exact)                               |
| `v2.api.fastly.com`                 | **no** (exact-only)                       |
| `developer.fastly.com`              | yes (reference)                           |
| `testlight.example`                 | yes (reserved TLD rule)                   |
| `something.test`                    | yes (reserved TLD rule)                   |
| `127.0.0.1`                         | yes (exact)                               |
| `192.168.1.1`                       | **no** (RFC 1918 private IP, not loopback) |
| `1.2.3.4`                           | no                                        |
| `[::1]` → `::1` after bracket strip | yes (exact)                               |

Matching is case-insensitive on the host after lowercasing.

### Allowlist Maintenance Policy

All three arrays are security-relevant artifacts. Different bars
apply:

**`EXACT_HOSTS` (integration proxies + loopback):**

1. **Vendor + integration**: must correspond to a named integration
   in the registry. No personal preferences, no test domains, no
   speculative entries.
2. **Justification in a `//`-comment** above the entry, naming the
   integration and role (e.g., `// didomi: config endpoint`).
3. **Narrowest workable host**: prefer the subdomain
   (`api.privacy-center.org`) over the apex (`privacy-center.org`).
4. **Exact by default**: only move to `SUBDOMAIN_HOSTS` when the
   vendor uses multiple subdomains in real traffic and we accept
   trusting all of them.

**`SUBDOMAIN_HOSTS`:**

1. Same vendor-justification bar as `EXACT_HOSTS`.
2. **Plus** an explicit comment naming *why* subdomain matching is
   needed (runtime host construction, vendor-controlled subdomain
   sharding, etc.).

**`REFERENCE_HOSTS`:**

1. Host must be a **legitimate documentation or specification source**
   that we link to in multiple places. One-off references use
   per-line suppression instead — inflating `REFERENCE_HOSTS` with
   single-use entries defeats its review purpose.
2. **Justification in a `//`-comment** naming the category
   (e.g., `// IAB Tech Lab spec source`).

Changes to any array must be reviewed as part of the PR.

### Per-Line Suppression

Some legitimate uses are not part of any integration — most notably
security tests using attacker-controlled placeholders. Real example:
`crates/trusted-server-core/src/integrations/google_tag_manager.rs:838`
contains `"https://evil.com/?redirect=https://www.google-analytics.com/collect"`.

The linter recognizes a **comment-anchored, host-named** marker:

```rust
let attacker = "https://evil.com/path"; // allow-domain: evil.com
```

```toml
upstream = "https://evil.com"  # allow-domain: evil.com
```

```html
<!-- allow-domain: evil.com -->
```

**Marker grammar (Rust regex):**

```
(?im)(?:^|\s)(?://|\#|<!--|\*\s)\s*allow-domain:\s*([A-Za-z0-9.\-:\[\],\s]+?)(?:-->|$)
```

- The comment introducer (`//`, `#`, `<!--`, or `*` followed by
  whitespace for jsdoc/block-comment continuation) must be **preceded
  by start-of-line or whitespace**. This is what makes the marker
  bypass-resistant.
- Captures a comma-separated host list.
- Each listed host must **actually match a violation on that line**;
  if a listed host does not appear among the line's violations, a
  warning is emitted (stderr) but the suppression for matched hosts
  still applies.
- A violation host that is **not** in the listed set is reported
  normally.

**Bypass-resistance:**

- `fetch("https://evil.com/allow-domain")` — `allow-domain` substring is
  inside a URL path, not after a comment introducer → no suppression.
- `fetch("https://evil.com//allow-domain: evil.com")` — the second `//`
  is preceded by `m` (from `.com`), not whitespace or start-of-line, so
  the marker anchor fails to match → no suppression.
- `https://allow-domain:8080/path` — pathological URL with literal host
  `allow-domain`: the `//` is preceded by `:` (scheme separator), not
  whitespace or start-of-line → no suppression. (The host
  `allow-domain` itself would be flagged as disallowed in the normal
  path.)

## Scope

### File extensions scanned

`.rs`, `.ts`, `.tsx`, `.js`, `.mjs`, `.cjs`, `.toml`, `.yml`, `.yaml`,
`.json`, `.md`, `.css`, `.html`, plus any file matching `.env*`.

Plus the special-case files matched by exact basename (these have no
extension):

- `Dockerfile`, `Dockerfile.*` (e.g., `Dockerfile.prod`)

**`.md` is scanned.** Markdown documentation files (`README.md`,
`CHANGELOG.md`, `CONTRIBUTING.md`, everything under `docs/`) are real
publishing surfaces and accidental hardcoded third-party hosts there
matter as much as in source. The legitimate reference links those
files contain are handled by an explicit
[`REFERENCE_HOSTS`](#reference-hosts-exact-match-allowed-in-every-scanned-file)
list (see Allowlist below) rather than by excluding the file type.

**Fenced code blocks are scanned, not skipped.** The repo's docs
and spec files include config snippets and `curl`/shell examples,
which are exactly the places an accidental real host can land. The
linter treats fenced blocks like any other content.

**Suppression inside fenced blocks: use the language's native
comment syntax, not HTML comments.** A line like
`<!-- allow-domain: foo -->` inside a ```` ```bash ```` fence is
displayed to readers as a literal HTML comment in their shell
example — confusing and misleading. The linter's marker regex
accepts several comment introducers; pick the one that matches the
fenced block's language:

| Fence language       | Use this marker form                |
| -------------------- | ----------------------------------- |
| `bash`, `sh`, `toml` | `# allow-domain: <host>`            |
| `rust`, `ts`, `js`   | `// allow-domain: <host>`           |
| HTML (or no fence)   | `<!-- allow-domain: <host> -->`     |

**Strongly prefer rewriting the example to a reserved host instead
of suppressing** — see [Stage 1 Doc Cleanup
Plan](#stage-1-doc-cleanup-plan). Per-line suppression is for true
one-offs (security write-ups citing a real CVE host, etc.). HTML
comments are reserved for **prose** Markdown contexts outside
fenced code blocks.

### Always excluded (paths)

- `Cargo.lock`
- Lockfiles by **exact basename** (not glob): `package-lock.json`,
  `pnpm-lock.yaml`, `pnpm-lock.json`, `yarn.lock`,
  `npm-shrinkwrap.json`. Listing each by name avoids the bug where
  a `*-lock.json` glob would miss `pnpm-lock.yaml` while `.yaml` is
  in the scanned extensions. **This is a supply-chain trade-off,
  not just dependency noise.** The current `package-lock.json`
  files contain `registry.npmjs.org`, `funding`/`sponsor` URLs, and
  many transitive package-repository URLs. Excluding lockfiles
  means a malicious or unreviewed registry URL added to a lockfile
  would not be flagged. Mitigated by the fact that lockfile changes
  are themselves a high-signal review surface (PR reviewers should
  already inspect lockfile diffs). Revisit if a real incident
  occurs.
- `node_modules/` (any depth)
- `target/`
- `dist/`
- `.git/`
- `.worktrees/`, `.claude/worktrees/`
- `crates/trusted-server-cli/src/dev/lint/domains.rs` itself (so the
  module's own allowlist constants and doc comments cannot self-flag)
- **`crates/trusted-server-core/src/integrations/**/fixtures/**` —
  publisher-capture HTML/JS fixtures.** Real-world snapshots used as
  test inputs for the HTML processor; they contain hundreds of
  legitimate third-party URLs that cannot reasonably be
  allowlisted. This is a narrow path exclusion, NOT the older
  too-broad `**/fixtures/**` rule (that earlier draft would have
  hidden the integration-test app source under
  `crates/integration-tests/fixtures/frameworks/nextjs/app/*.tsx`,
  which we deliberately scan).

**Source files under `crates/integration-tests/fixtures/frameworks/*` —
including `.tsx`, `.ts`, `.json`, `next.config.mjs`, `Dockerfile` —
ARE scanned.** Only the publisher-capture path above is excluded.

## Implementation

### Module structure

```rust
// crates/trusted-server-cli/src/dev/lint/domains.rs

use core::error::Error;
use std::path::PathBuf;

use derive_more::Display;
use error_stack::{Report, ResultExt};
use regex::Regex;

// gix = "gitoxide": pure-Rust git implementation. No external git binary
// required; no subprocess; typed diff/merge-base/index APIs.
use gix;

/// Hosts that must match exactly. Subdomains are NOT allowed.
const EXACT_HOSTS: &[&str] = &[
    // Loopback
    "127.0.0.1",
    "::1",
    "localhost",
    // didomi
    "api.privacy-center.org",
    "sdk.privacy-center.org",
    // ... etc.
];

/// Hosts that match exactly OR via subdomain (`*.host`).
const SUBDOMAIN_HOSTS: &[&str] = &[
    "example.com",
];

#[derive(Debug, Display)]
pub enum DomainsLintError {
    #[display("failed to open git repository")]
    OpenRepo,
    #[display("failed to read git index")]
    Index,
    #[display("failed to compute diff")]
    Diff,
    #[display("failed to resolve reference `{_0}`")]
    Reference(String),
    #[display("failed to compute merge-base of `{base}` and HEAD")]
    MergeBase { base: String },
    #[display("failed to read file `{_0}`")]
    ReadFile(PathBuf),
    #[display("invalid mode combination")]
    InvalidMode,
}
impl Error for DomainsLintError {}

pub struct DomainsLintArgs {
    pub mode: LintMode,
    pub format: OutputFormat,
    pub verbose: bool,
}

pub enum LintMode {
    Staged,
    ChangedVs(String),
    Paths(Vec<PathBuf>),
    FullRepo,
}

pub fn run(args: DomainsLintArgs) -> Result<i32, Report<DomainsLintError>> {
    let lines = collect_lines(&args.mode)?;
    let violations = scan_lines(&lines);
    emit_report(&violations, args.format);
    Ok(if violations.is_empty() { 0 } else { 1 })
}
```

### Cargo dependencies

Add to `crates/trusted-server-cli/Cargo.toml`:

```toml
[dependencies]
gix = { version = "<pin-during-spike>", default-features = false, features = [
    "blob-diff",   # blob-level line diffs (gix-diff)
    "index",       # read the git index for staged-vs-HEAD diffs
    "revision",    # merge-base computation (gix-revision)
] }
gix-config = "<must-match-the-gix-release-family>"
              # direct File-level read/write of <repo>/.git/config
              # for ts dev install-hooks (see "Persisting
              # core.hooksPath" below)
regex = "1"
```

Notes:

- **Version pinning is deferred to the gix feasibility spike (see
  [Implementation Readiness](#implementation-readiness)).** Do not
  hardcode `gix = "0.66"` / `gix-config = "0.40"` based on this
  spec alone — gitoxide companion crates evolve together and the
  release-family pairing matters. For example, the `gix 0.66`
  release line shipped with `gix-config 0.39.x`, not `0.40`, so the
  combination written here would cause cargo to pull two
  incompatible versions of `gix-config` into the tree. The spike
  pins both crates against the same release family, verifies with
  `cargo tree -p gix -p gix-config` that no duplicate versions
  appear, and **updates this dependency table** with the pinned
  numbers as part of step 1's deliverable.
- **Release blocker.** This spec is not implementation-complete
  until the `<pin-during-spike>` / `<must-match-the-gix-release-family>`
  placeholders above are replaced with concrete pinned versions by
  the spike PR. The Implementation Readiness section's spike step
  is the only acceptable mechanism for replacing them; downstream
  PRs should not invent their own pins.
- `gix-config` is pulled in **explicitly** for the durable
  `<repo>/.git/config` write performed by `ts dev install-hooks`.
  `gix::Repository::config_snapshot_mut()` only modifies an
  in-memory snapshot and is not the persistence path; the hook
  installer therefore uses `gix-config::File` directly. Do not
  rely on `config_snapshot/_mut` for persistence.
- No networking, credential helpers, or worktree mutation features
  are enabled — the linter only reads from the local repo and does
  one targeted config write in `ts dev install-hooks`.
- The exact feature names match the `gix` crate's documented features
  (`blob-diff`, `index`, `revision` — see docs.rs/gix). If a feature
  has been renamed or split in the version the spike selects, the
  closest documented equivalent is used and the change is flagged
  in the implementation PR.

### URL extraction (without lookahead)

Rust's standard `regex` crate does not support lookahead. The patterns
are designed to work without it — host character classes naturally bound
the match.

**Absolute URL regex:**

```
(?i)https?://(\[[0-9a-fA-F:]+\]|[A-Za-z0-9][A-Za-z0-9.\-]*)
```

- The non-IPv6 host branch `[A-Za-z0-9][A-Za-z0-9.\-]*` requires the
  host to **start with an alphanumeric** character. This rejects
  placeholder noise like `https://...` (which the earlier
  `[A-Za-z0-9.\-]+` would have matched, producing the bogus host
  `...`). A leading `-` or `.` is rejected by the same rule; that's
  fine, both are invalid per RFC 1035 anyway.
- Greedy match stops at the first character outside the class
  (e.g., `/`, `:`, `?`, `"`, `>`).
- Bracketed IPv6 is captured as `[…]`; surrounding brackets stripped
  in normalisation.

**Protocol-relative URL regex:**

```
(?i)(?:^|[\s"'(=<>{,\[\]`])//([A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,})
```

- The non-capturing group `(?:^|[\s"'(=<>{,\[\]` + backtick + `])`
  requires a boundary character before the `//`: start-of-line,
  whitespace, quote (`"` or `'`), paren `(`, `=`, `<`, `>`, `{`,
  `,`, `[`, `]`, or backtick (template literal). Backtick covers
  JavaScript/TypeScript template literals
  (`` `//cdn.example.com/${path}` ``); `{`, `[`, `,` cover
  JSON / TS object literals where a URL string follows a key.
- **Why not `:`?** `:` deliberately excluded — `http://foo.com` has
  `//` preceded by `:` (the URL scheme separator). Adding `:` to the
  boundary class would cause the protocol-relative regex to also
  match the host portion of every absolute URL, double-flagging.
- Prevents matching `// comment text` (the `//` is at column 0 or
  preceded by code, but the trailing TLD constraint also filters
  out comment dividers like `// foo bar`).
- The host capture `[A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,}`
  requires at least one dot followed by a TLD-like suffix and a
  leading alphanumeric character.
- **Known limitation**: back-to-back protocol-relative URLs without a
  separator (`//foo.com//bar.com`) miss the second one because the
  engine continues from `/bar.com` with no boundary char. Accepted
  for v1; no real-world occurrence.

### Suppression marker regex

The canonical regex (single source of truth — matches the form
documented in [Per-Line Suppression](#per-line-suppression)):

```
(?im)(?:^|\s)(?://|\#|<!--|\*\s)\s*allow-domain:\s*([A-Za-z0-9.\-:\[\],\s]+?)(?:-->|$)
```

The `(?:^|\s)` anchor is what closes the URL-content bypass (see
[Bypass-resistance](#per-line-suppression)). Any implementation must
use this exact regex; do not introduce a second variant elsewhere.

**Captured-group handling.** The host capture
`([A-Za-z0-9.\-:\[\],\s]+?)` includes `\s` (whitespace) because hosts
may be comma-separated with surrounding spaces, and an HTML-comment
marker like `<!-- allow-domain: test.com -->` has a space before
`-->` that the lazy quantifier will pull into the capture. The
implementation **must**:

1. Take the captured string.
2. Split on `,`.
3. Trim each resulting segment of leading/trailing whitespace
   (including any spaces the lazy quantifier picked up before
   `-->`).
4. Drop empty segments.
5. Lowercase each remaining host for comparison.

Tests exercise both `<!-- allow-domain: test.com -->` (with the
trailing space before `-->`) and
`// allow-domain: test.com, other.com` (multi-host with spaces).

### Host normalisation

```rust
fn normalise_host(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('[').trim_end_matches(']');
    trimmed.to_lowercase()
}
```

### Allow check

```rust
const RESERVED_TLDS: &[&str] = &[".example", ".test", ".invalid", ".localhost"];

fn is_allowed(host: &str, suppressed_on_line: &HashSet<String>) -> bool {
    if suppressed_on_line.contains(host) { return true; }
    if RESERVED_TLDS.iter().any(|t| host.ends_with(t)) { return true; }
    if EXACT_HOSTS.iter().any(|e| host == *e) { return true; }
    if REFERENCE_HOSTS.iter().any(|e| host == *e) { return true; }
    if SUBDOMAIN_HOSTS.iter().any(|e| {
        host == *e || host.ends_with(&format!(".{}", e))
    }) { return true; }
    false
}
```

### Line collection: `--staged` mode (gitoxide)

**No subprocess. No `git` binary on PATH required.** All git operations
go through `gix` APIs.

The flow:

1. Open the repo: `gix::open(".")`.
2. Resolve the HEAD tree.
3. Resolve the index (the staging area).
4. Compute the tree-vs-index changes — this is the set of files with
   staged modifications, additions, renames, or deletions.
5. For each `Modified` / `Added` / `Renamed` change:
   - Load the **old blob** from the HEAD tree (empty for additions).
   - Load the **new blob** from the index.
   - Run a **blob diff** using `gix-diff::blob` (which wraps
     `imara-diff`, the Myers diff implementation `gix` uses
     internally).
   - Walk the resulting hunks; for each hunk's **post-image (new) line
     range**, emit `DiffLine { path, line_no, content }` for each added
     line.
6. Skip `Deleted` changes (deletions cannot introduce a violation).
7. Apply the extension/path filter to the _post-image path_ before
   loading blobs (cheap filter, avoids unnecessary diffing).

Sketch (prototype-shaped — concrete `gix` API surface is identified
during implementation; helper names below are placeholders):

```rust
fn staged_added_lines() -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(".").change_context(DomainsLintError::OpenRepo)?;
    let head_tree = repo
        .head_commit()
        .change_context(DomainsLintError::OpenRepo)?
        .tree()
        .change_context(DomainsLintError::OpenRepo)?;
    let index = repo.index().change_context(DomainsLintError::Index)?;

    let mut out = Vec::new();
    // Iterate index-vs-tree changes.
    for change in index_vs_tree_changes(&repo, &head_tree, &index)? {
        let DiffEntry { new_path, old_blob, new_blob, .. } = change;
        if !path_is_scanned(&new_path) { continue; }
        let hunks = blob_diff_added_hunks(old_blob.as_deref(), new_blob.as_deref())
            .change_context(DomainsLintError::Diff)?;
        for hunk in hunks {
            for (line_no, content) in hunk.added_lines {
                out.push(DiffLine { path: new_path.clone(), line_no, content });
            }
        }
    }
    Ok(out)
}
```

**The `gix` API surface for this is a prototype-required decision.**
The conceptual operations the spec commits to are:

1. Open the repository (concrete: `gix::open` / `gix::ThreadSafeRepository::open`).
2. Resolve the HEAD commit's tree.
3. Read the index.
4. Compute the set of paths where the index differs from the HEAD
   tree, with each path classified as Added / Modified / Renamed /
   Deleted, and with access to both the old (HEAD) and new (index)
   blob ids.
5. Read each blob's content.
6. Run a line-level diff and obtain hunks whose **new-side** line
   range and content are accessible.

The exact gix entry points for (4) and (6) — `gix::diff` /
`gix::index::diff` / `gix::object::tree::diff` for the index-vs-tree
walk; `gix::diff::blob` (which wraps `imara-diff`) for the blob diff —
will be pinned during the first implementation pass, against the
specific `gix` version selected. If the chosen surface area doesn't
include one of these operations as a high-level helper, the helper
will be implemented in-crate using the lower-level
`gix-diff::*` building blocks. This is called out as a
**prototype-required** step in the plan, not a free-hand assumption.

**Why this is better than shelling out:**

- No `git` binary on PATH required.
- No diff-text parsing — line numbers and content come from typed
  hunk structs.
- No locale / quote-path / `b/` prefix / `/dev/null` edge cases.
- Renamed files are handled by `gix`'s change-detection (provides both
  old and new path).
- Filenames with spaces or non-UTF8 characters: `gix` paths are
  `BString` (byte strings). The script lossy-converts to UTF-8 for
  output and emits a stderr warning for non-UTF-8 paths.

### Line collection: `--changed-vs <ref>` mode (gitoxide)

Same blob-diff machinery, but the two trees are HEAD's tree and the
merge-base tree:

```rust
fn changed_vs_added_lines(reference: &str) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(".").change_context(DomainsLintError::OpenRepo)?;
    let head_id = repo.head_id().change_context(DomainsLintError::OpenRepo)?;
    let base_id = resolve_base_ref(&repo, reference)?;
    let merge_base = repo
        .merge_base(base_id, head_id)
        .change_context_lazy(|| DomainsLintError::MergeBase { base: reference.into() })?;
    let base_tree = repo.find_commit(merge_base)?.tree()?;
    let head_tree = repo.find_commit(head_id)?.tree()?;

    let mut out = Vec::new();
    for change in tree_vs_tree_changes(&repo, &base_tree, &head_tree)? {
        // same as staged: extension filter → blob diff → added-line hunks
    }
    Ok(out)
}
```

#### Base-ref resolution order

In CI, `$GITHUB_BASE_REF` is typically a bare branch name like
`main`. On a freshly-cloned PR working tree, `main` often **does
not exist as a local ref** — only `origin/main` (a remote-tracking
ref) does. A naive `repo.find_reference("main")` would fail.

`resolve_base_ref(repo, reference)` tries the following candidates
in order and returns the first one that resolves to an object id:

1. `<reference>` exactly (works when the caller passes e.g.
   `refs/remotes/origin/main` directly).
2. `refs/heads/<reference>` (local branch).
3. `refs/remotes/origin/<reference>` (remote-tracking branch — the
   common CI case where `<reference> == "main"`).
4. `refs/tags/<reference>` (tag — covers release-gate use).

If none resolve, the linter exits **2** with a message naming all
four candidates that were tried, so the CI failure mode is
diagnosable from log output alone.

**CI requirements (documented when Stage 2 lands):**

- `actions/checkout@v4` with `fetch-depth: 0` so the base ref and
  the full PR-branch history are reachable. Without it, `gix`
  cannot compute a merge-base on a shallow clone and the linter
  exits 2.
- Pass the base ref as a bare branch name (`main`) — the
  resolution order above handles the `origin/<ref>` lookup. Callers
  may also pass `origin/main` or `refs/remotes/origin/main`
  directly if they prefer to be explicit.
- For fork PRs, the base ref must still be present in the local
  clone. `actions/checkout@v4 fetch-depth: 0` covers this.
- **No `git` binary required on the runner.** `gix` reads the
  on-disk repo directly.

### Line collection: full-repo (gitoxide)

Full-repo audit enumerates tracked files via the index
(`gix::index::State::entries()`), then **reads working-tree content
from disk** (not the index/HEAD blob).

**Working-tree semantics — explicit decision.** A full-repo audit
therefore reports hosts that appear in the _current local edits_,
including unstaged and uncommitted changes. This is the right
behavior for an interactive developer audit ("what's currently in my
files?") and matches what someone running the linter as a
diagnostic-mode sanity check would expect. It is **not** a stable
"what is committed in this repo" audit.

If a stable, commit-state audit is needed later (e.g., for a release
gate that reports the state at a tagged commit), a separate mode like
`--at <rev>` would scan blob content from that revision's tree
instead. Out of scope for v1; deferred to follow-up if real demand
appears.

Untracked files are intentionally skipped — they cannot land in a
commit, and scanning them would falsely flag scratch/tmp files.

#### Handling tracked-but-missing files and symlinks

Because we enumerate the **index** and then read the **working
tree**, the two can disagree. Cases the implementation must handle
explicitly:

1. **Tracked but absent from the working tree** (`rm file` without
   `git rm`, or a partial checkout): `symlink_metadata` returns
   `NotFound`. Skip with a stderr warning naming the path. Do not
   fail — the user may be mid-task.
2. **Symlink** (`symlink_metadata().file_type().is_symlink()`):
   skip with a stderr warning ("symlink not followed"). Rationale:
   following symlinks would (a) potentially escape the repo
   (`/etc/passwd`), (b) double-scan if the target is also tracked,
   and (c) is rarely what a linter wants. If a real use case
   appears, add `--follow-symlinks` later. **Broken symlinks fall
   into this case** — `symlink_metadata` returns information about
   the link itself, not the (missing) target, so `is_symlink()` is
   `true` and the entry is skipped here. (If we used
   `std::fs::metadata` instead, a broken symlink would yield
   `NotFound`; we deliberately use `symlink_metadata` to keep
   symlink detection independent of target reachability.)
3. **Non-regular file** (FIFO, socket, device): skip with a stderr
   warning. Almost never in a real repo, but defensive.
4. **Non-UTF-8 path component**: `gix` returns path entries as
   `BString` (byte strings). On Unix, a byte sequence that is not
   valid UTF-8 is still a valid path; on Windows, paths must be
   convertible to UTF-16 and arbitrary bytes are not accepted.
   For consistency and simplicity, the linter **skips non-UTF-8
   entries with a stderr warning** on all platforms in v1. The
   working-tree-content read is therefore safe to perform on a
   `PathBuf` built from validated UTF-8 only. (A future v2 could
   add Unix-only lossless handling via
   `std::os::unix::ffi::OsStringExt::from_vec` if real repos hit
   this; not expected for trusted-server.)
5. **Binary file** (`std::fs::read_to_string` returns
   `InvalidData`): skip with a stderr warning. The extension
   filter already excludes most binaries, but a `.json` file with
   embedded NULs (rare) would hit this.

All five cases are warnings, not errors — the audit continues to
the next entry. Exit code reflects only the violation count.

```rust
fn full_repo_lines() -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(".").change_context(DomainsLintError::OpenRepo)?;
    let index = repo.index().change_context(DomainsLintError::Index)?;
    let work_dir = repo.work_dir().ok_or_else(|| Report::new(DomainsLintError::OpenRepo))?;

    let mut out = Vec::new();
    for entry in index.entries() {
        let rel_path = entry.path(&index);  // BString
        // Skip non-UTF-8 paths with a warning (see case 4 above).
        let rel_str = match std::str::from_utf8(rel_path.as_ref()) {
            Ok(s) => s,
            Err(_) => {
                warn_skip_bytes(rel_path.as_ref(), "non-UTF-8 path");
                continue;
            }
        };
        let path = work_dir.join(rel_str);
        if !path_is_scanned(&rel_path) { continue; }
        // See "Handling tracked-but-missing files and symlinks" above.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warn_skip(&path, "tracked but missing from working tree");
                continue;
            }
            Err(e) => {
                warn_skip(&path, &format!("metadata error: {e}"));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            warn_skip(&path, "symlink not followed");
            continue;
        }
        if !meta.file_type().is_file() {
            warn_skip(&path, "non-regular file");
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                warn_skip(&path, "binary content");
                continue;
            }
            Err(e) => return Err(Report::new(DomainsLintError::ReadFile(path.clone()))
                .attach_printable(e.to_string())),
        };
        for (i, line) in content.lines().enumerate() {
            out.push(DiffLine {
                path: rel_path.into(),
                line_no: i + 1,
                content: line.into(),
            });
        }
    }
    Ok(out)
}
```

### Line collection: explicit paths

Each path the user named is processed individually. Two layered
behaviors that differ from full-repo mode:

**Policy filters (extension, path-exclusion, symlink, non-regular,
binary) behave the same as full-repo: warn and skip.** The reason
is consistency — a file that would not be scanned in the full-repo
audit must not be scanned when named explicitly either. Specifically:

- Path matches an always-excluded location (`node_modules/`,
  `.worktrees/`, lockfile basename, etc.): warn and skip.
- Extension not in the scanned set (`.html`, `.css`, etc.):
  warn and skip with `note: <path> is not in scanned extensions;
  skipping`. The deferred `--force-scan path/...` escape hatch
  remains an Open Question.
- Symlink, non-regular file, binary content (`InvalidData`):
  warn and skip per the
  [full-repo handling table](#handling-tracked-but-missing-files-and-symlinks).

**Note on non-UTF-8 paths.** The non-UTF-8 handling described in
the full-repo section applies to **git/index-derived `BString`
paths** (full-repo, `--staged`, `--changed-vs` modes), where the
linter has to convert bytes back into an OS path. Explicit-path
mode receives an OS-supplied `PathBuf` from clap (which on Unix is
an `OsString` byte sequence that may not be UTF-8 but is already a
valid OS path) and passes it directly to the filesystem APIs — no
conversion step, no detection step. If the user explicitly named a
path that the OS accepts, the linter reads it; the non-UTF-8
classification is best-effort only and primarily applies to paths
the linter discovered via git.

**Access failures on a user-named path are hard errors, not
warnings.** Differing from full-repo here is intentional: if the
user typed `ts dev lint domains some/file.rs` and `some/file.rs`
does not exist or cannot be read for permissions reasons, that is
almost certainly a typo or a real environment problem the user
should know about — not the "tracked-but-missing during a sweep"
case full-repo handles silently. Treatment:

- `NotFound`: exit `2` with `CliError::EnvironmentError`, message
  `path not found: <path>`. No partial-success — if any explicit
  path fails to open, no violations are reported.
- `PermissionDenied` or other `io::Error`: same, with the
  underlying error in the message.

```rust
fn explicit_path_lines(paths: &[PathBuf]) -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let mut out = Vec::new();
    for path in paths {
        // Policy filters first (warn-and-skip).
        if !path_is_scanned_named(path) { continue; }
        let meta = std::fs::symlink_metadata(path)
            .change_context_lazy(|| DomainsLintError::ReadFile(path.clone()))?;
        if meta.file_type().is_symlink() { warn_skip(path, "symlink not followed"); continue; }
        if !meta.file_type().is_file() { warn_skip(path, "non-regular file"); continue; }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                warn_skip(path, "binary content"); continue;
            }
            Err(e) => return Err(Report::new(DomainsLintError::ReadFile(path.clone()))
                .attach_printable(e.to_string())),
        };
        for (i, line) in content.lines().enumerate() {
            out.push(DiffLine { path: path.clone(), line_no: i + 1, content: line.into() });
        }
    }
    Ok(out)
}
```

The hard-vs-soft split is documented as the user contract:
explicit paths are "I told you to look at this file"; full-repo is
"sweep over everything the index claims exists." Different intent,
different error behavior.

### Output Format (`human`)

```
crates/trusted-server-core/src/foo.rs:42: disallowed host test.com
trusted-server.toml:15: disallowed host 68.183.113.79

2 disallowed hosts found in 2 files.
To allow a new integration proxy, add it to EXACT_HOSTS in
crates/trusted-server-cli/src/dev/lint/domains.rs and document the
integration in a comment.
To suppress one line (e.g., security-test attacker hosts), append
`// allow-domain: <host>` in a comment.
Run `ts dev lint domains` (no args) for a full-repo audit.
```

### Output Format (`json`)

```json
{
  "violations": [
    {
      "path": "crates/trusted-server-core/src/foo.rs",
      "line": 42,
      "host": "test.com",
      "url": "https://test.com/path"
    }
  ],
  "count": 1,
  "files_affected": 1
}
```

### Pre-commit hook

Git invokes the hook as an executable file; the hook itself is
necessarily an OS-executable artifact (this is git's hook contract,
not "shelling out from Rust"). The hook is a minimal one-liner that
runs the `ts` binary.

**PATH fragility — addressed by embedding the absolute path at install
time.** GUI git tools (Sourcetree, GitHub Desktop, VS Code's git
integration) often do not inherit the shell's PATH, so a hook that
just calls `ts` may fail to find the binary even when
`cargo install_cli` has placed it in `~/.cargo/bin`. To avoid this:

`ts dev install-hooks` captures the absolute path of the currently-running
`ts` binary (via `std::env::current_exe()`) and writes that absolute
path into the hook:

```sh
#!/usr/bin/env bash
# .githooks/pre-commit — installed by `ts dev install-hooks`. DO NOT EDIT.
# Generated <timestamp> from <ts_version>.
exec "/Users/example/.cargo/bin/ts" dev lint domains --staged
```

If the user later rebuilds or moves the binary, re-running
`ts dev install-hooks` regenerates the hook with the new absolute path.
Without this, the fallback path `exec ts dev lint domains --staged`
relying on PATH is brittle in GUI contexts.

### Hook installer (Rust subcommand)

To keep the workflow Rust-only — no shell scripts in `scripts/`,
no `git config` invocation from a script — install via a `ts`
subcommand:

```
ts dev install-hooks
```

This is a small Rust subcommand on the `ts` CLI that:

1. Opens the repo via `gix::open(".")`.
2. Resolves the absolute path of the current `ts` executable via
   `std::env::current_exe()`.
3. **Preflight: read the existing local `core.hooksPath`** (via
   `gix-config::File`):
   - **Unset, empty, or already `.githooks`:** proceed. Idempotent
     re-run on an existing installation is a no-op for this check.
   - **Set to a different path** (`hooks`, `.husky`, `.cargo-husky`,
     anything else): **refuse unless `--force`**. The user likely
     has another hook chain (husky, cargo-husky, lefthook, a
     hand-rolled `hooks/` directory). Silently rewriting their
     `core.hooksPath` would disable that chain. Message:
     ```
     ts dev install-hooks: refusing to override existing core.hooksPath
       current: hooks
       would set: .githooks
     This would disable your existing hook chain. Choose one of:
       1. Re-run with --force (your existing core.hooksPath value is
          printed above; you can restore it later with
          `git config --local core.hooksPath hooks`).
       2. Manually add `exec <path-to-ts> dev lint domains --staged`
          to your existing pre-commit hook chain. The absolute path
          for this binary is: <ts_path>
     ```
     Exit code: 2 (environment error per the exit-code contract —
     this is a configuration conflict, not a violation).
4. **Checks for an existing `.githooks/pre-commit`:**
   - **Absent:** writes the file fresh.
   - **Present, and contains the `# ts-install-hooks: managed`
     marker on a known line:** overwrites silently. This is the
     managed-file case.
   - **Present, but content does not match the managed marker:**
     refuses to overwrite. Prints the path of the existing hook,
     suggests `--force` to overwrite or merging the contents
     manually. Exits non-zero. Rationale: the user may have
     hand-edited a custom hook (lint chain, secret scan, etc.); we
     never silently clobber.
5. With `--force`, the existing hook (if any) is renamed to
   `.githooks/pre-commit.bak.<timestamp>` before writing fresh, and
   the existing `core.hooksPath` value (if it pointed elsewhere) is
   printed in the success message so the user can restore it later.
6. Sets the executable bit via `std::fs::Permissions` /
   `set_permissions` (Unix `0o755`).
7. Sets `core.hooksPath = .githooks` in the local repo config via
   the `gix-config::File` write path described under "Persisting
   `core.hooksPath`" below (no subprocess).
8. Prints a confirmation message including the embedded binary path
   and (under `--force`) any displaced previous `core.hooksPath`.

Pseudocode (managed-file overwrite policy elided for brevity; see
above):

```rust
pub fn install_hooks(force: bool) -> Result<(), Report<InstallHooksError>> {
    let repo = gix::open(".")
        .change_context(InstallHooksError::OpenRepo)?;
    let work_dir = repo.work_dir()
        .ok_or_else(|| Report::new(InstallHooksError::NoWorkdir))?;
    let ts_path = std::env::current_exe()
        .change_context(InstallHooksError::CurrentExe)?;

    // Preflight: refuse to clobber a foreign core.hooksPath.
    let existing_hooks_path = read_local_config_value(
        &repo, "core", None, "hooksPath",
    )?;
    let displaced_hooks_path = match existing_hooks_path.as_deref() {
        None | Some("") | Some(".githooks") => None,   // safe to proceed
        Some(other) if !force => {
            return Err(Report::new(InstallHooksError::ForeignHooksPath {
                current: other.to_string(),
                proposed: ".githooks".to_string(),
            })
            .attach_printable("re-run with --force to override; existing value will be printed for manual restoration"));
        }
        Some(other) => Some(other.to_string()),        // --force; remember to surface
    };

    let hooks_dir = work_dir.join(".githooks");
    let hook_path = hooks_dir.join("pre-commit");
    std::fs::create_dir_all(&hooks_dir)
        .change_context(InstallHooksError::WriteHook)?;

    if hook_path.exists() && !is_managed(&hook_path)? && !force {
        return Err(Report::new(InstallHooksError::WouldClobber {
            path: hook_path,
        })
        .attach_printable("re-run with --force to overwrite (existing hook is backed up)"));
    }
    if hook_path.exists() && force {
        // Backup timestamp via std::time, no chrono dependency needed.
        let ts_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = hook_path.with_extension(format!("bak.{ts_secs}"));
        std::fs::rename(&hook_path, &backup)
            .change_context(InstallHooksError::WriteHook)?;
    }

    let content = render_hook(&ts_path);
    std::fs::write(&hook_path, content)
        .change_context(InstallHooksError::WriteHook)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    // Persistent local-repo config write: set core.hooksPath = .githooks
    // in <repo>/.git/config. See "Persisting core.hooksPath" below for
    // the concrete file-level write plan via the gix-config crate.
    set_local_config_value(&repo, "core", None, "hooksPath", ".githooks")?;

    println!(
        "Installed: pre-commit hook → {} (calls {})",
        hook_path.display(),
        ts_path.display(),
    );
    if let Some(prev) = displaced_hooks_path {
        eprintln!(
            "note: previous core.hooksPath was '{prev}'. \
             To restore: git config --local core.hooksPath {prev}"
        );
    }
    Ok(())
}

fn render_hook(ts_path: &Path) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # Installed by `ts dev install-hooks`. DO NOT EDIT.\n\
         # ts-install-hooks: managed\n\
         exec {} dev lint domains --staged\n",
        shell_quote(&ts_path.to_string_lossy()),
    )
}

fn is_managed(hook_path: &Path) -> Result<bool, Report<InstallHooksError>> {
    // Returns true if the file contains the marker line
    // `# ts-install-hooks: managed` in its first ~10 lines.
}
```

The `# ts-install-hooks: managed` marker on a known line is the
signal `is_managed` uses to detect prior-installed hooks. Hand-written
hooks won't have this marker, so they're treated as user content and
preserved unless `--force` is passed.

#### Shell-safe path quoting in the hook

`render_hook` writes the hook script's `exec` line. `Path::display()`
and `format!("{:?}", path)` are **not** shell-safe — paths containing
spaces, `$`, backticks, single quotes, or backslashes would break
the hook or silently misbehave (and on some systems open a command
injection through the install-time path).

The implementation uses POSIX-shell single-quote escaping, which is
trivial and bulletproof — single quotes inside the wrapper are
escaped as `'\''`:

```rust
fn shell_quote(s: &str) -> String {
    // POSIX single-quote escaping: wrap in '...', and any embedded
    // single quote becomes '\''  (close, escaped-quote, reopen).
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}
```

Tests cover paths containing: spaces (`/Users/Alice Q/.cargo/bin/ts`),
single quotes (`/path/with'quote/ts`), `$` (`/opt/$HOME/ts`),
backticks, backslashes (on Windows-style installer outputs).

#### Persisting `core.hooksPath`

`gix::Repository::config_snapshot_mut()` modifies an in-memory
snapshot; persisting back to `<repo>/.git/config` is not a single
stable call in current `gix`. The plan is to write the file directly
using the `gix-config` crate's file-level API:

```rust
fn set_local_config_value(
    repo: &gix::Repository,
    section: &str,
    subsection: Option<&str>,
    key: &str,
    value: &str,
) -> Result<(), Report<InstallHooksError>> {
    use gix_config::File;
    let config_path = repo.path().join("config"); // <repo>/.git/config

    // Read existing file. If missing, start with an empty File.
    let mut file = match File::from_path_no_includes(
        config_path.clone(),
        gix_config::Source::Local,
    ) {
        Ok(f) => f,
        Err(_) => File::default(),
    };

    // Set the value in the requested section/subsection/key.
    file.set_raw_value_by(section, subsection, key, value.as_bytes())
        .change_context(InstallHooksError::ConfigWrite)?;

    // Serialize and write back atomically (write to a temp file in
    // the same directory, then rename).
    let serialized = file.to_bstring();
    write_atomic(&config_path, serialized.as_slice())
        .change_context(InstallHooksError::ConfigWrite)?;
    Ok(())
}

/// Read a single value from the local repo config. Returns Ok(None)
/// if the file or key is absent (i.e., never set). Used by the
/// install-hooks preflight to detect a foreign `core.hooksPath`.
fn read_local_config_value(
    repo: &gix::Repository,
    section: &str,
    subsection: Option<&str>,
    key: &str,
) -> Result<Option<String>, Report<InstallHooksError>> {
    use gix_config::File;
    let config_path = repo.path().join("config");
    let file = match File::from_path_no_includes(
        config_path,
        gix_config::Source::Local,
    ) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    Ok(file
        .raw_value_by(section, subsection, key)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}
```

`write_atomic` is a small helper that writes to `config.tmp.<rand>`
then `rename`s to `config` (atomic on the same filesystem). This
matches git's own behavior of never leaving a partially-written
`.git/config`.

This replaces the earlier sketch using `config_snapshot_mut` /
`commit()` which is in-memory only. The `gix-config` file-write
path is the documented stable way to durably modify a local repo's
git config without subprocess.

`ts dev install-hooks` is a one-time setup contributors run after cloning,
alongside `cargo install_cli`. Documented in CONTRIBUTING.md.

## Testing Strategy

Following the conventions established in PR #669: unit tests live under
`#[cfg(test)] mod tests` in each module; end-to-end CLI tests use
`assert_cmd` and `tempfile`.

### Unit tests (in `dev/lint/domains.rs`)

Pure functions tested directly: `normalise_host`, `is_allowed`,
`extract_hosts_from_line`, `parse_suppression_marker`.

Diff-collection functions (`staged_added_lines`,
`changed_vs_added_lines`, `full_repo_lines`) are exercised via
end-to-end tests that build a real temp git repo with `gix` and assert
on the collected `DiffLine` values.

### Allowed-host cases

1. Plain allowed hosts — `https://example.com`, `https://foo.example.com`,
   `https://api.privacy-center.org`, `http://127.0.0.1:8080`,
   `https://github.com/x/y`.
2. Subdomain-list rule — `https://foo.example.com` allowed.
3. **Reserved TLDs** — `https://testlight.example`,
   `https://something.test`, `https://thing.invalid`,
   `https://my.localhost` all allowed.
4. Bracketed IPv6 loopback — `http://[::1]:8080` allowed.
5. Uppercase host — `HTTPS://Example.COM/path` allowed.
6. Quoted / trailing punctuation — `"https://example.com",`,
   `(https://example.com)`, `<https://example.com>` parse cleanly.
7. Multiple URLs on one line, all allowed — no violations.
8. Protocol-relative allowed — `//www.googletagmanager.com/gtm.js`.
9. Legitimate suppression — `// allow-domain: evil.com` passes when host
   matches.
10. Multi-host suppression — `// allow-domain: evil.com, bad.org`.
11. Block-comment / jsdoc suppression — line beginning with `   *` and
    immediately followed by the marker, e.g.,
    `   * allow-domain: evil.com` paired with a URL on the same line:
    `let bad = "https://evil.com";   * allow-domain: evil.com`
    (constructed; in practice the marker would more often be a `//`
    trailing comment on the same line as the URL). The point of the
    test is to confirm the `\*\s` branch of the regex fires when the
    marker is adjacent to the comment introducer.

### Disallowed-host cases

12. Plain disallowed hosts — `https://test.com`, `https://partner.com`,
    `https://1.2.3.4` → 3 violations.
13. Subdomain-attack lookalike — `https://example.com.evil.com` flagged.
14. Exact-only subdomain attempt — `https://anything.api.privacy-center.org`
    flagged.
15. Non-loopback IPv6 — `http://[2001:db8::1]/` flagged as `2001:db8::1`.
16. Protocol-relative disallowed — `//cdn.example.evil/foo` flagged.
17. Multiple disallowed on one line — both reported.
18. **Bypass attempt via URL content** —
    `fetch("https://evil.com/allow-domain")` → flagged.
19. **Bypass attempt via URL-path comment-lookalike** —
    `fetch("https://evil.com/x//allow-domain: evil.com")` → flagged.
20. **Wrong host in marker** —
    `https://evil.com // allow-domain: other.com` → `evil.com` flagged;
    stderr warning notes `other.com` was listed but did not match.
20a. **Placeholder URL with malformed host** —
    `https://...` in a Markdown placeholder must NOT extract host
    `...` (the regex requires an alphanumeric first character).
    Asserts the URL is silently skipped (it is not a real URL).
20b. **Template-literal protocol-relative URL** —
    `` `//cdn.example.evil/${path}` `` (JS/TS template literal)
    flagged as `cdn.example.evil`. Asserts backtick boundary works.
20c. **JSON object value with protocol-relative URL** —
    `{"src": "//cdn.example.evil/x"}` flagged. Asserts `{` and `,`
    boundary characters work for JSON contexts.
20d. **Suppression marker with trailing whitespace before `-->`** —
    `<!-- allow-domain: test.com   -->` correctly trims the host
    (captured group ends with spaces, but split+trim yields
    `["test.com"]`).
20e. **Suppression marker with multi-host whitespace** —
    `// allow-domain: a.com ,  b.com , c.com` correctly yields
    `["a.com", "b.com", "c.com"]`.

### `--staged` mode cases (`assert_cmd` end-to-end)

Each test sets up a temp git repo using `gix::init`, populates blobs
and the index with `gix` APIs (no shell), runs the binary with
`assert_cmd`, asserts exit code and stdout/stderr.

21. New violation in staged change → exits 1 with correct `path:line`.
22. Existing violation, unrelated staged change → exits 0.
23. Renamed file with added violation → reported at new path.
24. File deletion of a file containing disallowed URL → exits 0.
25. Filename with spaces or non-ASCII characters — handled correctly
    by `gix` (no quoting layer to fight with); reported normally.
    **Non-UTF-8 path component in a staged diff: reported normally
    with a stderr warning that the path is being displayed
    lossy-UTF-8.** This intentionally differs from
    [full-repo mode](#handling-tracked-but-missing-files-and-symlinks)
    (case 4), which skips non-UTF-8 entries. The reason: a staged
    diff is built from blob ids and tree entries, so the host
    extraction happens against blob content regardless of how the
    path renders for display. Skipping a staged change just because
    its path bytes are not valid UTF-8 would silently hide a
    violation the user is actively trying to commit — exactly the
    opposite of what `--staged` mode exists for. Full-repo mode,
    by contrast, has no commit-intent signal and the working-tree
    `read_to_string` path is simpler to keep consistent by
    skipping.

    Implementers: do not generalize the full-repo non-UTF-8 skip
    rule to `--staged` / `--changed-vs` modes.
26. Multiple hunks in one file — all added lines reported correctly.

### `--changed-vs` mode cases

27. Two commits on a branch, second adds a violation → reported.
28. Merge-base correctly computed when branch is behind base.
29. Missing remote ref → exits 2 with clear message.

### Path-exclusion and inclusion cases

30. `node_modules/foo.js` with `https://test.com` → ignored.
31. `.worktrees/x/y.rs` → ignored.
32. `*.html` extension → scanned. Files under
    `crates/trusted-server-core/src/integrations/**/fixtures/**` are
    skipped by path; other `.html` files (e.g.,
    `crates/trusted-server-core/src/html_processor.test.html`) are
    scanned normally.
33. **Proves the `**/fixtures/**` blanket exclusion was removed**:
    `crates/integration-tests/fixtures/frameworks/nextjs/app/page.tsx`
    fixture with `https://test.com` → reported.
34. `package-lock.json` → ignored.

### Markdown-specific cases

35. **Allowed reference link in normal Markdown** —
    `[the Fastly docs](https://developer.fastly.com/learning)` in a
    `.md` file → no violation (covered by `REFERENCE_HOSTS`).
36. **Disallowed Markdown link target** —
    `[bad](https://test.com)` → flagged as `test.com` at the
    correct line.
37. **Autolink form** — `<https://test.com>` flagged; the angle
    brackets are wrapping, not part of the URL.
38. **HTML comment suppression in Markdown** —
    a line containing `https://test.com` followed by
    `<!-- allow-domain: test.com -->` → suppressed; same line with a
    wrong-host marker `<!-- allow-domain: other.com -->` → flagged
    with the stderr warning.
39. **Multiple links on one line** —
    `see [a](https://github.com/x) and [b](https://test.com)` →
    one violation reported (`test.com`).
40. **Fenced code block — disallowed** —
    a triple-backtick block containing
    `curl https://test.com/foo` is scanned and reported. Documents
    that fenced blocks are NOT skipped; per-line suppression
    (`<!-- allow-domain: test.com -->` outside the fence on the
    same logical line is impractical) requires either an inline HTML
    comment in the code-block language's comment syntax (e.g.,
    `# allow-domain: test.com` for shell) or rewriting the example
    to use `.example`.
41. **Fenced code block — allowed reference** —
    triple-backtick block referencing `https://docs.rs/clap` → no
    violation.
42. **Reference list at end of Markdown** — link-reference syntax
    `[1]: https://test.com` is scanned (the URL is still extracted
    by the absolute-URL regex regardless of Markdown semantics).
43. **Image link** —
    `![alt](https://test.com/img.png)` flagged.

### Environment cases

44. **Not inside a git repo** — `gix::open` fails →
    exits 2 with `DomainsLintError::OpenRepo` and a clear message.
45. **Bare repo / no working tree** — `gix::open` succeeds but
    `repo.work_dir()` is `None` (only relevant for the full-repo
    mode that reads working-tree files) → exits 2 with a clear
    message.
46. **No git binary on PATH at all** — the linter still works
    end-to-end (verified by running the binary under `env -i PATH=""`,
    confirming `gix` is self-contained).
47. Run unit tests under `cargo test --package trusted-server-cli`
    on the host target (matches PR #669's split CI lanes).

## Trade-offs

- **Pre-commit-only enforcement is bypassable.** `git commit --no-verify`
  skips the hook. Closed by the migration plan.
- **`--staged` mode misses violations introduced via rebase/merge** that
  don't go through `git commit`. CI follow-up catches them.
- **Inline allowlist requires editing the Rust source.** Each new
  integration proxy requires a code change + review. Acceptable given
  expected low churn.
- **Existing violations are not addressed.** They remain until those
  files are touched. The full-repo audit (`ts dev lint domains` no args) is
  **diagnostic-only** in Stage 1 — it will report many existing
  violations; that is expected, not a failure.
- **Bare-string hostnames are not detected.** Config values like
  `cookie_domain = "test-publisher.com"` are out of scope.
- **HTML/CSS/Dockerfile blind spot.** Accepted; not mitigated by other
  code paths.
- **`REFERENCE_HOSTS` are allowed in every scanned file, including
  production source.** This is intentional. A production `.rs`
  change that introduces `let x = "https://github.com/...";` will
  pass the linter. The alternative — restricting reference hosts
  to comment-only contexts (`///` in `.rs`, `#` in `.toml`,
  `<!-- -->` in `.md`) — would require a comment-aware tokenizer
  per language and was rejected as over-engineering for a small
  risk surface. Code review catches stray reference URLs that
  matter; the linter's purpose is preventing test-pollution and
  unvetted *integration* endpoints, not policing every documentation
  link. If a real incident shows production code routinely embedding
  reference URLs as runtime values, revisit with a per-context
  policy.
- **Non-UTF-8 filenames** are skipped in full-repo / explicit-path
  working-tree reads with a stderr warning. `gix` preserves diff paths
  as `BString` internally, but v1 intentionally avoids platform-specific
  lossless path reconstruction from arbitrary bytes.
- **Back-to-back protocol-relative URLs without a separator**
  (`//a.com//b.com`) miss the second host. No real-world occurrence in
  this repo.
- **PR #669 hard prerequisite.** This work requires a base that already
  contains #669's CLI crate and host-target CI lane. The implementation
  may either wait for #669 to merge to `main` or stack on PR #669's
  branch; if #669 stalls without a stackable branch, this design needs
  revisiting (alternative: ship as a standalone `trusted-server-lint`
  crate).
- **New top-level dependency: `gix`.** Pulls in ~15 sub-crates
  (gix-diff, gix-revision, gix-index, gix-config, etc.). Adds
  meaningful compile time to the host-target CLI build. Mitigation:
  use `default-features = false` and enable only the needed features
  (`blob-diff`, `revision`, `index`, `config`). Acceptable because the
  alternative (shelling to `git`) was rejected as a hard requirement.

## Stage 1 Doc Cleanup Plan

Bringing `.md` into scope means the current docs have many
non-allowlisted hosts that need triage. The full-repo audit
(`ts dev lint domains` with no args) is diagnostic-only in Stage 1
precisely so this cleanup can happen incrementally — but it is a
**committed workstream**, not "incidental noise we'll get to."

### Verified disallowed hosts in current `.md` files

A grep against the current `docs/` and root-level Markdown surfaces
these example categories (representative, not exhaustive — the
implementation runs the full audit and produces the complete list):

| Host                                     | Category                                | Resolution                                                                  |
| ---------------------------------------- | --------------------------------------- | --------------------------------------------------------------------------- |
| `aps.amazon.com`                         | Real Amazon doc/product page            | Add to `REFERENCE_HOSTS` if linked repeatedly, otherwise suppress per-line  |
| `api.lockr.io`                           | Legitimate lockr integration endpoint   | Add to integration `EXACT_HOSTS` (lockr) — verify it is actually proxied |
| `krk.kargo.com`                          | Kargo bidder host                       | Verify if proxied; add to integration list OR rewrite illustrative usage to `.example` |
| `sync.ssp.com`, `ec.publisher.com`, `tracker.com`, `advertiser.com`, `cdn.com`, `short.link`, `redirect1.com`, `redirect2.com`, `final.com`, `new-server.com`, `publisher.com`, `partner.com`, `web.prebidwrapper.com`, `prebid-server.com`, `your-server.com` | Illustrative placeholders in `docs/guide/creative-processing.md`, `docs/guide/first-party-proxy.md`, etc. | **Rewrite to RFC 2606 reserved hosts** (`tracker.example.com`, `advertiser.example.com`, `cdn.example.com`, `short.example`, etc.) |
| `formally-vital-lion.edgecompute.app`    | One-off Fastly Compute test URL         | Suppress per-line where it appears |
| `getpurpose.ai`                          | Test site in PR #669 reviewer instructions | Rewrite to `example.com` or suppress |
| `192.168.1.1`                            | RFC 1918 private IP example             | Rewrite to a reserved host or `127.0.0.1` |

### Cleanup policy

1. **Strongly prefer rewriting illustrative example hosts to RFC 2606
   reserved names** (`*.example.com`, `*.example.net`, `*.example.org`,
   `*.example`, `*.test`, `*.invalid`, `*.localhost`). This is the
   default for placeholder URLs in tutorials, prose, and code
   snippets. It is also the answer to the
   "multi-line fenced-code-block suppression" pain point — the linter
   has no block-level suppression mechanism (intentional: keeps the
   tool simple), so multi-line examples that would otherwise need
   one marker per line should be rewritten to reserved hosts instead.
2. **Add legitimate integration / vendor hosts to the appropriate
   allowlist** when they appear in multiple files and have a real
   integration backing them (e.g., `api.lockr.io`).
3. **Suppress per-line only for true one-offs** — security write-ups
   referencing a CVE-relevant domain, attacker placeholders in
   security tests (`evil.com`), single citations of an external
   resource. Suppressing 20 illustrative occurrences of a placeholder
   is a smell — rewrite to reserved instead.

### Cleanup execution

The cleanup PR(s) land **after** the linter ships (Stage 1) but
**before** Stage 2 (CI gate on changed lines), so contributors get
the protection of the local hook immediately while the doc cleanup
happens in parallel without blocking the main release.

Suggested execution order:

1. Land the linter and pre-commit hook (this design).
2. Produce a frequency-ordered host report. The human output
   includes file paths and summary lines, so naive `sort | uniq -c`
   over the human format counts *lines*, not hosts. Use the JSON
   output and a small parser:

   ```sh
   ts dev lint domains --format json \
     | jq -r '.violations[].host' \
     | sort | uniq -c | sort -rn
   ```

   This gives `<count>  <host>` lines sorted by frequency, which
   feeds the triage in step 3.

   **Requires `jq`** (Homebrew: `brew install jq`; most CI runners
   already have it). If `jq` is not available locally, a
   no-extra-tool alternative until a built-in `--summary hosts`
   mode is added (deferred Open Question):

   ```sh
   ts dev lint domains --format json \
     | python3 -c 'import json,sys,collections; d=json.load(sys.stdin); \
                    c=collections.Counter(v["host"] for v in d["violations"]); \
                    [print(f"{n:6d} {h}") for h,n in c.most_common()]'
   ```
3. Triage the top ~80% of violations into the three categories above.
4. Submit cleanup PRs grouped by file (so each PR is reviewable):
   `docs/guide/creative-processing.md`,
   `docs/guide/first-party-proxy.md`,
   `docs/guide/api-reference.md`,
   etc.
5. Each cleanup PR runs the linter's `--changed-vs main` mode as a
   self-check.
6. Once the audit is clean (or down to a small known list), enable
   Stage 2 CI.

## Migration to CI

**Stage 1 (this design + the cleanup workstream above):** Pre-commit
hook calling `ts dev lint domains --staged`. Prevents _new_
violations. Full-repo audit available but diagnostic-only; the doc
cleanup runs in parallel.

**Stage 2:** GitHub Actions workflow runs
`ts dev lint domains --changed-vs $GITHUB_BASE_REF` on every PR. Same
delta-only enforcement, unbypassable. Requirements:

- `actions/checkout@v4` with `fetch-depth: 0` (or explicit fetch of
  `$GITHUB_BASE_REF`).
- Reuse the host-target CI lane introduced by PR #669 (since `ts`
  binary is host-target only).

**Stage 3 (optional, deferred):** Either (a) clean existing violations
and add full-repo audit as a CI gate, or (b) snapshot a baseline file
and run full-repo audit with baseline subtraction. Choice deferred
until Stages 1 and 2 are stable.

## Resolved Decisions

Settled choices that the implementer should not re-litigate. Kept
here as historical context with the rationale, so future readers can
see *why* each decision went the way it did rather than re-opening
the question.

1. **Subcommand naming and ownership.** `ts dev lint domains` and
   `ts dev install-hooks`. Both `lint` and `install-hooks` are
   developer-workflow commands and belong under `dev`, not on the
   operator-facing top level (`config`, `auth`, `audit`,
   `provision`). This PR owns the required refactor of the existing
   PR #669 `ts dev` leaf into a subcommand group, with `ts dev serve`
   for the existing behavior. The earlier review's suggestion to keep
   `ts lint domains` top-level was explicitly rejected by the spec
   owner — `dev` parent is the chosen shape.
2. **`cdn.prebid.org` on the integration allowlist** (rather than
   rewriting the `prebid.rs` test code to `.example`). The tests
   verify rewriting of real-world Prebid CDN URLs; converting them
   to reserved hosts would weaken the test's intent.
3. **Stage 1 ships without a full cleanup of existing violations.**
   Existing violations are cleaned incrementally as files are
   touched, with the dedicated workstream tracked in
   [Stage 1 Doc Cleanup Plan](#stage-1-doc-cleanup-plan). The
   linter ships now; the doc audit happens in parallel.
4. **Suppression marker syntax: `allow-domain: <host>`,
   comment-anchored, host-validated.** Alternatives considered:
   bare `allow-domain` without a host (rejected — bypassable via
   URL paths), `allowed-domain:` (rejected — verbose without
   benefit), block-level suppression markers (rejected — adds
   state tracking and complexity; rewriting to reserved hosts
   covers the multi-line case).
5. **`ts dev install-hooks` clobber-detection signature.** The
   `# ts-install-hooks: managed` marker on a known line is the
   detection heuristic. Unmanaged hooks are refused without
   `--force`. A `--append-to-existing` mode is left for later if
   demand surfaces.
6. **`--force-scan` escape hatch for explicit paths is NOT in
   v1.** Explicit paths honour the extension filter (skipped with
   stderr warning). Adding `--force-scan` is deferred until a real
   workflow needs it.

## Open Questions

Genuine unresolved items the implementer must close during
implementation.

1. **Exact `gix` API entry points for index-vs-tree and
   tree-vs-tree diff walking, and for blob diff with new-side line
   numbers.** Marked as prototype-required in the
   [Line collection: --staged mode](#line-collection---staged-mode-gitoxide)
   section. Pinned by the gix feasibility spike
   (see [Implementation Readiness](#implementation-readiness)
   step 1). The spec commits to the conceptual operations, not
   concrete function names.
2. **`gix` and `gix-config` version pins.** Both are deliberately
   left as placeholders in [Cargo dependencies](#cargo-dependencies)
   because (a) gitoxide companion crates must come from the same
   release family and (b) workspace consistency with any `gix`
   pulled in transitively takes precedence. The feasibility spike
   chooses the pair, verifies with `cargo tree -p gix -p gix-config`,
   and updates the dependency table.
3. **Stable-commit audit mode (`--at <rev>`).** Full-repo audit
   currently reads working-tree content (current local edits
   included). If a release-gate use case appears that needs an
   "at a tagged commit" view, add an `--at <rev>` mode that scans
   blob content from that revision's tree. Deferred until real
   demand surfaces; not part of v1.

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
`feature/ts-cli`) being merged first. PR #669 introduces the
`crates/trusted-server-cli` crate, the `ts` binary, the
`cargo install_cli` alias, the host-target CI lane, and the clap
command-surface conventions this design extends. None of the work in this
spec begins until #669 is on `main`.

## Non-Goals

- No CI gate in v1. The pre-commit hook is the only enforcement mechanism.
  See [Migration to CI](#migration-to-ci).
- No baseline file. Existing violations are tolerated; the linter is scoped
  to new lines.
- No autofix.
- No detection of bare hostnames without an `http(s)://` or `//` prefix.
- No HTML, CSS, or Dockerfile scanning. **Accepted blind spot**: a
  disallowed URL added to a publisher-capture HTML fixture, a CSS
  `url(...)`, or a Dockerfile `FROM`/`RUN curl` line will not be
  detected. HTML fixtures at
  `crates/trusted-server-core/src/integrations/*/fixtures/*.html` contain
  hundreds of legitimate captured third-party URLs that cannot reasonably
  be allowlisted.

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
| `ts dev lint domains --changed-vs <ref>` | CI/PR mode (Stage 2). Scans only added lines in `git diff $(git merge-base <ref> HEAD)..HEAD`.                           |
| `ts dev lint domains path/...`           | Scans the listed files in full.                                                                                          |

Output format defaults to `human`. `--format json` emits a structured
report (see [Output Format](#output-format)).

Exit codes: `0` no violations; `1` violations found; `2` usage or
environment error.

**Exit-code wiring defers to PR #669's convention.** The sketch
function signature shown later in this spec —
`fn run(...) -> Result<i32, Report<DomainsLintError>>` — is
illustrative, not prescriptive. The actual command function will match
whatever pattern `trusted-server-cli` uses for the other subcommands
(`config validate`, `audit`, `provision fastly plan`, etc.) introduced
in PR #669. If that crate centralizes exit handling in `main()` via a
`Result<(), Report<CliError>>` shape and maps specific errors to
specific exit codes, this subcommand follows the same pattern. The
three exit-code semantics above are the **contract**, not the
**implementation shape**.

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
                                  # behavior as `ts dev serve` (or
                                  # the equivalent name chosen during
                                  # the refactor) so the PR #669
                                  # functionality is preserved.
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
  with its public API unchanged. **This is a CLI-surface change to
  PR #669**: today's `ts dev` becomes `ts dev serve` (or whatever
  subcommand name is chosen during the refactor). Since #669 has not
  merged, this can be coordinated as part of the same review cycle
  rather than as a follow-up that breaks released behavior.
- `crates/trusted-server-cli/src/error.rs` — add `LintError` and
  `InstallHooksError` variants if needed for typed propagation,
  otherwise reuse the crate's existing `Report<CliError>` plumbing.

No changes to `trusted-server-core` or `trusted-server-adapter-fastly`.

## Allowlist (Rust constants)

Two arrays as `const &[&str]` at module top of `dev/lint/domains.rs`.

### Exact-match hosts

The host must equal one of these exactly. Subdomains are **not** allowed
(e.g., `anything.api.privacy-center.org` is disallowed).

| Category                                             | Hosts                                                                                                        |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Loopback                                             | `127.0.0.1`, `::1`, `localhost`                                                                              |
| Integration proxies (didomi)                         | `api.privacy-center.org`, `sdk.privacy-center.org`                                                           |
| Integration proxies (sourcepoint)                    | `cdn.privacy-mgmt.com`                                                                                       |
| Integration proxies (lockr)                          | `aim.loc.kr`, `identity.loc.kr`                                                                              |
| Integration proxies (datadome)                       | `js.datadome.co`, `api-js.datadome.co`                                                                       |
| Integration proxies (aps / Amazon)                   | `aax.amazon-adsystem.com`, `aax-events.amazon-adsystem.com`                                                  |
| Integration proxies (permutive)                      | `api.permutive.com`, `secure-signals.permutive.app`, `cdn.permutive.com`                                     |
| Integration proxies (Google Tag Manager / Analytics) | `www.googletagmanager.com`, `www.google-analytics.com`, `analytics.google.com`                               |
| Integration proxies (adserver mock)                  | `securepubads.g.doubleclick.net`, `origin-mocktioneer.cdintel.com`                                           |
| Integration proxies (Prebid CDN)                     | `cdn.prebid.org`                                                                                             |
| Integration proxies (Fastly platform)                | `api.fastly.com`                                                                                             |
| Reference / doc links                                | `github.com`, `docs.rs`, `crates.io`, `iabeurope.github.io`, `doc.rust-lang.org`, `www.w3.org`, `schema.org` |

### Subdomain-permitting hosts

The host equals one of these **or** ends with `.` + one of these.

| Host                 | Allows                                              | Why subdomain matching                                                                                                                                                                                |
| -------------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `example.com`        | `example.com`, `foo.example.com`, `a.b.example.com` | IANA reserved; arbitrary subdomains expected in test fixtures                                                                                                                                         |
| `edge.permutive.app` | `edge.permutive.app`, `<org>.edge.permutive.app`    | Permutive constructs the host as `{organization_id}.edge.permutive.app` at runtime (see `crates/trusted-server-core/src/integrations/permutive.rs:93`); subdomains are vendor-controlled per customer |

### The `.example` TLD rule

Any host ending in `.example` is allowed (IANA RFC 2606). Hard-coded
suffix check, not a list entry.

### Matching summary

| Host                                | Allowed?                                  |
| ----------------------------------- | ----------------------------------------- |
| `example.com`                       | yes (subdomain-list)                      |
| `foo.example.com`                   | yes (subdomain-list)                      |
| `example.com.evil.com`              | **no** (not a subdomain of `example.com`) |
| `api.fastly.com`                    | yes (exact)                               |
| `v2.api.fastly.com`                 | **no** (exact-only)                       |
| `testlight.example`                 | yes (`.example` TLD rule)                 |
| `127.0.0.1`                         | yes (exact)                               |
| `1.2.3.4`                           | no                                        |
| `[::1]` → `::1` after bracket strip | yes (exact)                               |

Matching is case-insensitive on the host after lowercasing.

### Allowlist Maintenance Policy

The allowlist is a security-relevant artifact. Adding an entry requires:

1. **Vendor + integration**: the entry must correspond to a named
   integration or a well-known reference/doc host. No personal
   preferences, no test domains, no speculative entries.
2. **Justification in a `//`-comment** above the entry, naming the
   integration and role.
3. **Narrowest workable host**: prefer the subdomain
   (`api.privacy-center.org`) over the apex (`privacy-center.org`).
4. **Exact by default**: new vendor entries go into
   `EXACT_HOSTS`. Move to `SUBDOMAIN_HOSTS` only when the vendor uses
   multiple subdomains in real traffic and we accept trusting all of
   them.
5. **Source-code reference hosts are allowed everywhere** (not split
   between docs and code).

Changes to either array must be reviewed as part of the PR.

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
`.json`, plus any file matching `.env*`.

**`.md` is intentionally NOT scanned.** Markdown documentation files
(`README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, everything under
`docs/`) routinely contain hundreds of legitimate third-party
reference links — `docs.github.com`, `www.fastly.com`,
`developer.fastly.com`, `manage.fastly.com`, `vitepress.dev`,
`keepachangelog.com`, `semver.org`, `grafana.com`, `docs.prebid.org`,
and many more, all verified present in the current repo. Doc-link
hygiene is a different problem with different rules (broken-link
checking, etc.) and is out of scope for this linter. The doc-link
allowlist (`github.com`, `docs.rs`, `crates.io`, …) is still applied
to in-code reference URLs that appear in `///` doc comments inside
`.rs` files and `#` comments inside `.toml` files — that surface is
high-signal and worth checking.

### Always excluded (paths)

- `Cargo.lock`
- `*-lock.json` (matches `package-lock.json`, `pnpm-lock.json`). **This
  is a supply-chain trade-off, not just dependency noise.** The current
  `package-lock.json` files contain `registry.npmjs.org`,
  `funding`/`sponsor` URLs, and many transitive package-repository
  URLs. Excluding lockfiles means a malicious or unreviewed registry
  URL added to a lockfile would not be flagged. Mitigated by the fact
  that lockfile changes are themselves a high-signal review surface
  (PR reviewers should already inspect lockfile diffs). Revisit if a
  real incident occurs.
- `node_modules/` (any depth)
- `target/`
- `dist/`
- `.git/`
- `.worktrees/`, `.claude/worktrees/`
- `crates/trusted-server-cli/src/dev/lint/domains.rs` itself (so the
  module's own allowlist constants and doc comments cannot self-flag)

**Note:** `**/fixtures/**` is **not** a blanket exclusion. Publisher-capture
HTML fixtures under
`crates/trusted-server-core/src/integrations/*/fixtures/*.html` are
already skipped because `.html` is not in the scanned extension list.
Source files under `crates/integration-tests/fixtures/frameworks/*` —
including `.tsx`, `.ts`, `.json`, `next.config.mjs` — **are** scanned.

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
gix = { version = "0.66", default-features = false, features = [
    "blob-diff",   # blob-level line diffs (gix-diff)
    "index",       # read the git index for staged-vs-HEAD diffs
    "revision",    # merge-base computation (gix-revision)
] }
regex = "1"
```

Notes:

- `config` reading and writing is part of `gix`'s default surface
  exposed via `Repository::config_snapshot` / `_mut` and does not
  require an explicit feature flag in this gix version line.
- No networking, credential helpers, or worktree mutation features
  are enabled — the linter only reads from the local repo and does
  one targeted config write in `ts dev install-hooks`.
- The exact feature names match the `gix` crate's documented features
  (`blob-diff`, `index`, `revision` — see docs.rs/gix). If a feature
  has been renamed in the version pinned at implementation time, the
  closest documented equivalent is used.

### URL extraction (without lookahead)

Rust's standard `regex` crate does not support lookahead. The patterns
are designed to work without it — host character classes naturally bound
the match.

**Absolute URL regex:**

```
(?i)https?://(\[[0-9a-fA-F:]+\]|[A-Za-z0-9.\-]+)
```

- `[A-Za-z0-9.\-]+` greedily captures the host; matching stops at the
  first character outside the class (e.g., `/`, `:`, `?`, `"`, `>`).
- Bracketed IPv6 is captured as `[…]`; surrounding brackets stripped
  in normalisation.

**Protocol-relative URL regex:**

```
(?i)(?:^|[\s"'(=<>])//([A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,})
```

- The non-capturing group `(?:^|[\s"'(=<>])` requires a boundary
  character (start-of-line, whitespace, quote, paren, `=`, `<`, `>`)
  before the `//`. Prevents matching the `//` in `// comment text` or
  in `http://foo` (where `//` is preceded by `:`).
- The host capture `[A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,}` requires
  at least one dot followed by a TLD-like suffix — filters out
  comment dividers like `// foo bar` and `// 1.2`.
- **Known limitation**: back-to-back protocol-relative URLs without a
  separator (`//foo.com//bar.com`) would miss the second one because
  the engine continues from `/bar.com` with no boundary char. Accepted
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

### Host normalisation

```rust
fn normalise_host(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('[').trim_end_matches(']');
    trimmed.to_lowercase()
}
```

### Allow check

```rust
fn is_allowed(host: &str, suppressed_on_line: &HashSet<String>) -> bool {
    if suppressed_on_line.contains(host) { return true; }
    if host.ends_with(".example") { return true; }
    if EXACT_HOSTS.iter().any(|e| host == *e) { return true; }
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
    let base_id = repo
        .find_reference(reference)
        .change_context_lazy(|| DomainsLintError::Reference(reference.into()))?
        .into_fully_peeled_id()
        .change_context_lazy(|| DomainsLintError::Reference(reference.into()))?;
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

**CI requirements (documented when Stage 2 lands):**

- `actions/checkout@v4` with `fetch-depth: 0` so that the base
  ref is locally reachable from the working clone. Without it,
  `find_reference` or `merge_base` returns an error and the linter
  exits 2 with a clear message.
- For fork PRs, the base ref must be fetched (`fetch-depth: 0` covers
  this in `actions/checkout@v4`).
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

```rust
fn full_repo_lines() -> Result<Vec<DiffLine>, Report<DomainsLintError>> {
    let repo = gix::open(".").change_context(DomainsLintError::OpenRepo)?;
    let index = repo.index().change_context(DomainsLintError::Index)?;
    let work_dir = repo.work_dir().ok_or_else(|| Report::new(DomainsLintError::OpenRepo))?;

    let mut out = Vec::new();
    for entry in index.entries() {
        let rel_path = entry.path(&index);  // BString
        let path = work_dir.join(/* lossy utf8 of rel_path */);
        if !path_is_scanned(&rel_path) { continue; }
        let content = std::fs::read_to_string(&path)
            .change_context_lazy(|| DomainsLintError::ReadFile(path.clone()))?;
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

Each path is read with `std::fs::read_to_string`, every line emitted.
No git operations involved (the user named the files directly).

**Explicit paths still honour the extension/path filter.** If a user
runs `ts dev lint domains some.html`, the file is **skipped** and a
warning is printed to stderr (`note: some.html is not in scanned
extensions; skipping`). Rationale: the goal is consistent behavior
across modes — a file that would not be scanned in the full-repo
audit must not be scanned when named explicitly either. The override
escape hatch, if it becomes needed, is `--force-scan path/...`;
deferred until a real need surfaces.

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
3. **Checks for an existing `.githooks/pre-commit`:**
   - **Absent:** writes the file fresh.
   - **Present, and the first three lines match the documented
     header signature** (e.g., the `# Installed by `ts dev install-hooks`
     marker on a known line): overwrites silently. This is the
     managed-file case.
   - **Present, but content does not match the managed signature:**
     refuses to overwrite. Prints the path of the existing hook,
     suggests `--force` to overwrite or merging the contents
     manually. Exits non-zero. Rationale: the user may have
     hand-edited a custom hook (lint chain, secret scan, etc.); we
     never silently clobber.
4. With `--force`, the existing hook is renamed to
   `.githooks/pre-commit.bak.<timestamp>` and a fresh hook written.
5. Sets the executable bit via `std::fs::Permissions` /
   `set_permissions` (Unix `0o755`).
6. Sets `core.hooksPath = .githooks` in the local repo config via
   `gix`'s config-writing API (no subprocess).
7. Prints a confirmation message including the embedded binary path.

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
        let backup = hook_path.with_extension(format!(
            "bak.{}",
            chrono::Utc::now().timestamp()
        ));
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

    // gix config write: set core.hooksPath = .githooks (local repo config).
    let mut config = repo.config_snapshot_mut();
    config.set_raw_value(&"core.hooksPath", ".githooks")
        .change_context(InstallHooksError::ConfigWrite)?;
    config.commit().change_context(InstallHooksError::ConfigWrite)?;

    println!(
        "Installed: pre-commit hook → {} (calls {})",
        hook_path.display(),
        ts_path.display(),
    );
    Ok(())
}

fn render_hook(ts_path: &Path) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # Installed by `ts dev install-hooks`. DO NOT EDIT.\n\
         # ts-install-hooks: managed\n\
         exec {:?} dev lint domains --staged\n",
        ts_path,
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
3. `.example` TLD — `https://testlight.example` allowed.
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
    Non-UTF-8 path component emits a stderr warning but the host is
    still flagged.
26. Multiple hunks in one file — all added lines reported correctly.

### `--changed-vs` mode cases

27. Two commits on a branch, second adds a violation → reported.
28. Merge-base correctly computed when branch is behind base.
29. Missing remote ref → exits 2 with clear message.

### Path-exclusion and inclusion cases

30. `node_modules/foo.js` with `https://test.com` → ignored.
31. `.worktrees/x/y.rs` → ignored.
32. `*.html` extension → ignored regardless of path.
33. **Proves the `**/fixtures/**` blanket exclusion was removed**:
    `crates/integration-tests/fixtures/frameworks/nextjs/app/page.tsx`
    fixture with `https://test.com` → reported.
34. `package-lock.json` → ignored.

### Environment cases

35. **Not inside a git repo** — `gix::open` fails →
    exits 2 with `DomainsLintError::OpenRepo` and a clear message.
36. **Bare repo / no working tree** — `gix::open` succeeds but
    `repo.work_dir()` is `None` (only relevant for the full-repo
    mode that reads working-tree files) → exits 2 with a clear
    message.
37. **No git binary on PATH at all** — the linter still works
    end-to-end (verified by running the binary under `env -i PATH=""`,
    confirming `gix` is self-contained).
38. Run unit tests under `cargo test --package trusted-server-cli`
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
- **Non-UTF-8 filenames** are lossy-converted for display and emit a
  stderr warning. `gix` preserves them as `BString` internally so
  scanning works correctly; only the printed `path:line` output is
  affected.
- **Back-to-back protocol-relative URLs without a separator**
  (`//a.com//b.com`) miss the second host. No real-world occurrence in
  this repo.
- **PR #669 hard prerequisite.** This work cannot start until #669
  merges. If #669 stalls, this design needs revisiting (alternative:
  ship as a standalone `trusted-server-lint` crate).
- **New top-level dependency: `gix`.** Pulls in ~15 sub-crates
  (gix-diff, gix-revision, gix-index, gix-config, etc.). Adds
  meaningful compile time to the host-target CLI build. Mitigation:
  use `default-features = false` and enable only the needed features
  (`blob-diff`, `revision`, `index`, `config`). Acceptable because the
  alternative (shelling to `git`) was rejected as a hard requirement.

## Migration to CI

**Stage 1 (this design):** Pre-commit hook calling
`ts dev lint domains --staged`. Prevents _new_ violations. Full-repo audit
available but diagnostic-only.

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

## Open Questions

1. **Subcommand naming.** `ts dev lint domains` (current pick) vs other
   placements considered: top-level `ts lint domains`, top-level
   `ts check domains`, under audit as `ts audit domains`. Current pick
   nests under `dev` because both `lint` and `install-hooks` are
   developer-workflow commands and don't belong on the operator-facing
   top level. Confirm the existing PR #669 `ts dev` (single-file leaf
   that starts the dev server) being refactored into a subcommand
   group with `ts dev serve` for the existing behavior is acceptable
   to the #669 reviewers.
2. **`cdn.prebid.org` on allowlist vs converting `prebid.rs` tests to
   `.example`?** Current pick: allowlist. Revisit if rigorous
   separation is preferred.
3. **Reference-doc hosts and subdomains.** `github.com` is exact-only,
   meaning `docs.github.com` (sometimes appears in `.github/workflows`)
   would have to be added explicitly. Currently not added; line-level
   suppression covers occasional uses.
4. **Stage 1 cleanup expectations.** Do we ship with existing
   violations intact and clean them incrementally as files are
   touched, or open a follow-up cleanup PR? Current pick: ship
   without cleanup; cleanup is a separate workstream.
5. **Boilerplate `package.json` URLs.** `crates/integration-tests/fixtures/frameworks/nextjs/`
   contains `opencollective.com`, `tidelift.com`, `registry.npmjs.org`.
   Allowlist them, suppress per-line, or rewrite to `.example`?
   Current pick: suppress per-line since these are non-recurring
   boilerplate.
6. **Suppression marker syntax** — `allow-domain: host` vs
   `// allowed-domain: host` vs other forms. Current pick:
   `allow-domain: host`, comment-anchored, host-validated.
7. **Exact `gix` API entry points for index-vs-tree and tree-vs-tree
   diff walking.** Marked as prototype-required in the implementation
   section; pinned during first implementation pass against the
   selected `gix` version. Spec commits to the conceptual operations,
   not the concrete function names.
8. **`gix` version pin.** The spec uses `0.66` as an example; the
   actual pin happens at implementation time with the `gix` version
   current at that point. Workspace consistency (matching any
   `gix` already pulled in transitively by other dependencies) takes
   precedence.
9. **`ts dev install-hooks` clobber detection signature.** The
   `# ts-install-hooks: managed` marker on a known line is the
   detection heuristic. If a contributor wants a custom multi-hook
   chain, they keep their existing hook (we refuse to overwrite
   without `--force`), and they must add an `exec ts dev lint domains
   --staged` line manually. We could add a `--append-to-existing`
   mode later if demand surfaces.
10. **`--force-scan` escape hatch for explicit paths.** Current pick:
    explicit paths honour the extension filter (skipped + warning if
    extension is excluded). If real workflows need to scan a one-off
    `.html` file, add `--force-scan` later.
11. **Stable-commit audit mode (`--at <rev>`).** Full-repo audit
    currently reads working-tree content. If a stable, commit-state
    audit is needed later (e.g., a release gate at a tag), add an
    `--at <rev>` mode that scans blob content from that revision's
    tree. Deferred until real demand appears.

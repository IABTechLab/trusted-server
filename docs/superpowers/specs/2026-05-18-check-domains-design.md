# `check-domains` Pre-Commit Linter — Design

**Date:** 2026-05-18
**Status:** Draft (revised after first review)

## Goal

Fail commits that introduce new URLs to non-allowlisted domains in source,
config, or documentation files. Catches accidental test-pollution domains
(e.g., `test.com`, `partner.com`, `new.com`) and hardcoded third-party
endpoints that have not been vetted as integration proxies.

Enforces the rule: **production code, tests, and config may only reference
`example.com` (and its subdomains), loopback addresses, an explicit list of
integration-proxy endpoints, or a small set of reference/doc-link domains.**

## Non-Goals

- No CI gate in v1. The pre-commit hook is the only enforcement mechanism.
  See the [Migration to CI](#migration-to-ci) section for the explicit path
  to enabling CI later.
- No baseline file. Existing violations are tolerated; the linter is scoped
  to new lines.
- No autofix.
- No detection of bare hostnames without an `http(s)://` or `//` prefix
  (e.g., a string literal `"foo.example.com"` is not scanned).
- No HTML/CSS/Dockerfile scanning. Publisher-capture HTML fixtures contain
  hundreds of legitimate third-party URLs (Facebook, typekit, ad networks)
  that are out of scope for an allowlist policy.

## Allowlist

Maintained as a constant array near the top of `scripts/check-domains.sh`.

| Category | Hosts |
|---|---|
| Example TLDs (IANA RFC 2606) | `example.com` + any subdomain; any `*.example` host (e.g., `testlight.example`) |
| Loopback | `127.0.0.1`, `::1`, `localhost` |
| Integration proxies (didomi) | `api.privacy-center.org`, `sdk.privacy-center.org` |
| Integration proxies (sourcepoint) | `cdn.privacy-mgmt.com` |
| Integration proxies (lockr) | `aim.loc.kr` |
| Integration proxies (datadome) | `js.datadome.co`, `api-js.datadome.co` |
| Integration proxies (aps / Amazon) | `aax.amazon-adsystem.com`, `aax-events.amazon-adsystem.com` |
| Integration proxies (Google Tag Manager / Analytics) | `www.googletagmanager.com`, `www.google-analytics.com`, `analytics.google.com` |
| Integration proxies (adserver mock) | `securepubads.g.doubleclick.net`, `origin-mocktioneer.cdintel.com` |
| Integration proxies (Fastly platform) | `api.fastly.com` |
| Reference/doc links | `github.com`, `docs.rs`, `crates.io`, `iabeurope.github.io`, `doc.rust-lang.org`, `www.w3.org`, `schema.org` |

Matching is **case-insensitive**. For each allowlist entry `E`, a host `H`
matches if `H == E` **or** `H` ends with `.E` (i.e., is a subdomain of `E`).

Worked examples:

| Allowlist entry | Allows | Does NOT allow |
|---|---|---|
| `example.com` | `example.com`, `foo.example.com`, `a.b.example.com` | `notexample.com`, `example.com.evil.com`, `example.org` |
| `api.fastly.com` | `api.fastly.com`, `v2.api.fastly.com` | `other.fastly.com`, `fastly.com` |

The `.example` TLD is handled as a separate hard-coded suffix rule (matches
any host ending in `.example`), not a list entry.

### Allowlist Maintenance Policy

The allowlist is a security-relevant artifact. Adding an entry requires:

1. **Vendor + integration**: the entry must correspond to a named integration
   (didomi, sourcepoint, lockr, etc.) or a well-known reference/doc host. No
   personal preferences, no test domains, no "we'll need this later" entries.
2. **Justification in the comment**: each entry has a trailing comment naming
   the integration and the role (`# didomi config endpoint`,
   `# Fastly management API`).
3. **Narrowest workable host**: prefer the specific subdomain
   (`api.privacy-center.org`) over the apex (`privacy-center.org`). The
   subdomain rule means listing `privacy-center.org` would allow *every*
   subdomain.
4. **Source-code reference hosts are allowed everywhere** (not split between
   docs and code). Listing `github.com` allows it in `.rs`, `.md`, `.toml`
   alike — splitting by file type is more complexity than it's worth.

Changes to `ALLOWED_HOSTS` must be reviewed as part of the PR; reviewers
should verify the integration actually exists in the registry and the host
is the one being proxied/called.

### Per-Line Suppression

Some legitimate uses are not part of any integration — most notably security
tests that use `evil.com` and similar attacker-controlled placeholders
(real example: `crates/trusted-server-core/src/integrations/google_tag_manager.rs:838`).
To allow these without polluting the global allowlist, the linter
recognizes the literal token `allow-domain` anywhere on the same source
line:

```rust
let attacker = "https://evil.com/path"; // allow-domain
```

```toml
upstream = "https://evil.com"  # allow-domain
```

The marker is comment-syntax-agnostic — any occurrence of the substring
`allow-domain` on the line suppresses all disallowed domains on that line.
The marker is intentionally non-specific (no host listed) to keep the
scanner simple; reviewers verify the line's intent at PR time. If misuse
becomes a problem, future versions can require `allow-domain: evil.com`
with named hosts.

## Scope

### File extensions scanned

`.rs`, `.ts`, `.tsx`, `.js`, `.mjs`, `.cjs`, `.toml`, `.md`, `.yml`,
`.yaml`, `.json`, plus any file matching `.env*`.

### Always excluded (paths)

- `Cargo.lock`
- `*-lock.json` (matches `package-lock.json`, `pnpm-lock.json`)
- `node_modules/` (any depth)
- `target/`
- `dist/`
- `.git/`
- `.worktrees/`, `.claude/worktrees/` (temporary git worktrees with
  duplicated content)
- `**/fixtures/**` (real-world publisher captures and test fixtures
  containing third-party URLs)
- `scripts/check-domains.sh` itself (so the script's own allowlist comments
  cannot self-flag)

## Components

### 1. `scripts/check-domains.sh`

The linter. Modes:

| Invocation | Behavior |
|---|---|
| `scripts/check-domains.sh` | Full-repo audit. Walks tracked files matching the extension filter and scans every line. |
| `scripts/check-domains.sh --staged` | Pre-commit mode. Scans only added lines (`^+` lines) in `git diff --cached`. Existing violations are not reported. |
| `scripts/check-domains.sh path/...` | Scans the listed files in full. |

Exit codes: `0` if no violations; `1` if any violations.

### 2. `.githooks/pre-commit`

```sh
#!/usr/bin/env bash
exec "$(git rev-parse --show-toplevel)/scripts/check-domains.sh" --staged
```

### 3. `scripts/install-hooks.sh`

```sh
#!/usr/bin/env bash
set -euo pipefail
git config core.hooksPath .githooks
echo "Installed: git hooks now run from .githooks/"
```

### 4. `CONTRIBUTING.md` addition

Short subsection under a "Local setup" heading explaining the one-time
install command and what the hook checks for.

## Detection Logic

For each line under inspection:

1. If the line contains a suppression marker (`// allow-domain` or
   `# allow-domain`), skip URL extraction on that line.
2. Extract URL tokens with **two** regexes (case-insensitive):
   - **Absolute**: `https?://(?:\[[0-9a-fA-F:]+\]|[A-Za-z0-9.\-]+)` —
     matches `https://example.com`, `http://[::1]:8080`,
     `https://1.2.3.4`.
   - **Protocol-relative**: `(?:^|[\s"'(=<>])//([A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,})(?=[\s"')/<>?#])` —
     matches `//www.googletagmanager.com/gtm.js` and similar. The leading
     boundary character requirement (whitespace, quote, paren, `=`, `<`,
     `>`) prevents matching `// foo bar.example` style code comments. The
     trailing lookahead ensures a recognisable URL delimiter follows.
3. For each match, strip to bare host:
   - Drop scheme, port, path, query, fragment.
   - For bracketed IPv6, strip the surrounding `[ ]` before normalisation.
4. Lowercase the host.
5. **Allow** if any of:
   - Host equals an allowlist entry (exact match).
   - Host ends with `.` followed by an allowlist entry (subdomain match).
   - Host ends with `.example` (reserved TLD rule).
6. Otherwise, emit a violation line.

Raw IPv4/IPv6 literals that are not loopback (e.g., `68.183.113.79` in
`trusted-server.toml`) are treated as disallowed hosts and reported.

## `--staged` Mode Implementation

To scan only added lines while preserving file paths and line numbers, the
script pipes `git diff --cached -U0 --diff-filter=ACMR` into awk that
tracks the post-image line number from each `@@` hunk header and
normalises the file path from the `+++ ` line:

```awk
/^\+\+\+ / {
  raw = substr($0, 7)
  if (raw == "/dev/null") { file = ""; next }      # file deletion
  # Strip git's "b/" prefix from new-side path. Quoted paths
  # (filenames with spaces / special chars) are not supported in v1;
  # they appear as `"b/path with spaces"` and would need C-style
  # unescaping. Documented as a known limitation.
  if (substr(raw, 1, 2) == "b/") raw = substr(raw, 3)
  file = raw
  next
}
/^@@/ { match($0, /\+([0-9]+)/, a); ln = a[1] - 1; next }
/^\+/ { ln++; if (file != "") print file ":" ln ":" substr($0, 2); next }
/^ /  { ln++; next }
/^-/  { next }
```

Each emitted `path:line:content` line is then passed through the URL
extraction and allowlist check. The extension/path filter is applied to
`file` before printing.

To handle quoted/escaped paths defensively, the script runs
`git -c core.quotepath=false diff --cached ...` so non-ASCII paths are not
quoted (paths containing literal spaces still emit a warning rather than a
silent miss).

## Output Format

```
crates/trusted-server-core/src/foo.rs:42: disallowed domain test.com
trusted-server.toml:15: disallowed domain 68.183.113.79

2 disallowed domains found in 2 files.
To allow a new integration proxy, add it to ALLOWED_HOSTS in scripts/check-domains.sh.
To suppress one line (e.g., security-test attacker domains), append `// allow-domain`.
Run `scripts/check-domains.sh` (no args) for a full-repo audit.
```

When clean: no output, exit 0.

## Setup Flow for Contributors

```
git clone ...
./scripts/install-hooks.sh   # one-time per clone
```

After that, every `git commit` runs the linter against staged changes.
Bypass with `git commit --no-verify` (intentional escape hatch; see
[Migration to CI](#migration-to-ci)).

## Testing Strategy

A small `scripts/check-domains.test.sh` exercises the linter end-to-end.

### Allowed-host cases (must pass clean)

1. **Plain allowed hosts** — `https://example.com`,
   `https://foo.example.com`, `https://api.privacy-center.org`,
   `http://127.0.0.1:8080`, `https://github.com/x/y`.
2. **Subdomain rule** — `https://api.fastly.com` allowed.
3. **`.example` TLD** — `https://testlight.example` allowed.
4. **Bracketed IPv6 loopback** — `http://[::1]:8080` allowed.
5. **Uppercase host** — `HTTPS://Example.COM/path` allowed.
6. **Quoted / trailing punctuation** — `"https://example.com",`,
   `(https://example.com)`, `<https://example.com>` all parse cleanly to
   `example.com`.
7. **Multiple URLs on one line** — `see [a](https://github.com/a) and
   [b](https://example.com/b)` → no violations.
8. **Protocol-relative allowed** — `//www.googletagmanager.com/gtm.js`
   allowed.
9. **Suppression marker** — line with `https://evil.com  // allow-domain`
   passes.

### Disallowed-host cases (must fail with the expected hosts reported)

10. **Plain disallowed hosts** — `https://test.com`, `https://partner.com`,
    `https://1.2.3.4` → 3 violations.
11. **Subdomain-attack lookalike** — `https://example.com.evil.com` →
    flagged as `example.com.evil.com` (must NOT be allowed by the
    `example.com` rule).
12. **Non-loopback IPv6** — `http://[2001:db8::1]/` flagged.
13. **Protocol-relative disallowed** — `//cdn.example.evil/foo` flagged.
14. **Multiple disallowed on one line** —
    `<a href="https://test.com">x</a><a href="https://partner.com">y</a>`
    → 2 violations on the same line.

### `--staged` mode cases

15. **New violation in staged change** — temp repo, stage a file
    containing `https://test.com` → fails with correct `path:line`.
16. **Existing violation, unrelated staged change** — pre-commit a file
    with `https://test.com`, then stage an unrelated change in the same
    file → passes (only added lines scanned).
17. **Renamed file** — rename `a.rs` → `b.rs` with no content change →
    no spurious violations; with an added violation line → reported as
    `b.rs:N`.
18. **File deletion** — staged deletion of a file containing a disallowed
    URL → no violations (deletion is not an addition).
19. **Filename with spaces** — staged file `dir/with spaces.rs` containing
    `https://test.com` → reported (test that the awk doesn't silently
    drop the file). If unsupported, must emit a clear warning, not pass
    silently.

### Path-exclusion cases

20. **`node_modules/`** — file under `node_modules/foo.js` with
    `https://test.com` is ignored.
21. **`**/fixtures/**`** — file under
    `crates/trusted-server-core/src/integrations/nextjs/fixtures/x.html`
    is ignored.
22. **`.worktrees/`** — file under `.worktrees/x/y.rs` is ignored.

Run as `scripts/check-domains.test.sh`; exit non-zero on any failure.

## Trade-offs

- **Pre-commit-only enforcement is bypassable.** `git commit --no-verify`
  skips the hook. Closing this gap requires the migration plan below.
- **`--staged` mode misses violations introduced via rebase/merge** that do
  not go through `git commit`. Acceptable for v1; CI follow-up catches them.
- **Inline allowlist requires editing the script** to add a new integration
  proxy. Acceptable given expected low churn; switching to a config file is
  trivial later.
- **Existing violations are not addressed.** They will remain until those
  files are touched. Acceptable because the goal is to prevent regression,
  not force an immediate cleanup.
- **HTML/CSS/Dockerfile not scanned.** Real-world publisher HTML fixtures
  contain third-party URLs that cannot reasonably be allowlisted. The risk
  is that disallowed domains could land in those files without detection;
  mitigated by the fact that the integration code reading those fixtures
  is already covered.
- **Per-line `allow-domain` marker is host-agnostic.** A line marked
  `allow-domain` suppresses *any* disallowed host on that line. This is
  intentional to keep the scanner simple; reviewers verify intent at PR
  time. If misuse becomes a problem, future versions can require
  `allow-domain: evil.com` with named hosts.
- **Filenames with spaces in `--staged` mode are not fully supported.**
  Git escapes them in diff output; v1 emits a warning rather than a silent
  miss.

## Migration to CI

The pre-commit hook is bypassable and machine-specific. To make this rule
authoritatively enforced, a CI gate is required. The migration is
**deliberately staged** because turning on a full-repo audit today would
fail on the ~30 existing violations:

**Stage 1 (this design):** Pre-commit hook with `--staged` mode. Prevents
*new* violations.

**Stage 2:** Add a CI workflow that runs `scripts/check-domains.sh
--changed-vs origin/main` — scanning only lines added relative to the PR
base. Same enforcement model as the local hook, but unbypassable per PR.
Requires implementing the `--changed-vs <ref>` mode (small extension of
`--staged`; same awk parser, different diff command).

**Stage 3 (optional):** Either (a) clean the existing violations and add a
full-repo audit as a CI gate, or (b) snapshot a baseline file
(`scripts/.allowed-domains-baseline`) and run the full-repo audit with
baseline subtraction. Stage 3 is not committed-to in this design; the
decision can be made after Stages 1 and 2 are stable.

## Open Questions

None.

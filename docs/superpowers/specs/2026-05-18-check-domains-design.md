# `check-domains` Pre-Commit Linter — Design

**Date:** 2026-05-18
**Status:** Draft

## Goal

Fail commits that introduce new URLs to non-allowlisted domains in source,
config, or documentation files. Catches accidental test-pollution domains
(e.g., `test.com`, `partner.com`, `new.com`) and hardcoded third-party
endpoints that have not been vetted as integration proxies.

Enforces the rule: **production code, tests, and config may only reference
`example.com` (and its subdomains), loopback addresses, an explicit list of
integration-proxy endpoints, or a small set of reference/doc-link domains.**

## Non-Goals

- No CI gate in v1 (follow-up). The pre-commit hook is the only enforcement
  mechanism. A future GitHub Action can run the full-repo audit.
- No baseline file. Existing violations are tolerated; the linter is scoped
  to new lines.
- No protocol-relative URL detection (`//example.com/path`) in v1.
- No autofix.
- No detection of bare hostnames without an `http(s)://` prefix.

## Allowlist

Maintained as a constant array near the top of `scripts/check-domains.sh`.

| Category | Hosts |
|---|---|
| Example TLDs (IANA RFC 2606) | `example.com` + any subdomain; any `*.example` host (covers `testlight.example`, etc.) |
| Loopback | `127.0.0.1`, `::1`, `localhost` |
| Integration proxies | `api.privacy-center.org` (didomi), `aax.amazon-adsystem.com`, `aax-events.amazon-adsystem.com` (aps), `js.datadome.co`, `api-js.datadome.co` (datadome), `api.fastly.com` (Fastly management API) |
| Reference/doc links | `github.com`, `docs.rs`, `crates.io`, `iabeurope.github.io` |

Matching is **case-insensitive**. For each allowlist entry `E`, a host `H`
matches if `H == E` **or** `H` ends with `.E` (i.e., is a subdomain of `E`).

Worked examples:

| Allowlist entry | Allows | Does NOT allow |
|---|---|---|
| `example.com` | `example.com`, `foo.example.com`, `a.b.example.com` | `notexample.com`, `example.org` |
| `api.fastly.com` | `api.fastly.com`, `v2.api.fastly.com` | `other.fastly.com`, `fastly.com` |

The `.example` TLD is handled as a separate hard-coded suffix rule (matches
any host ending in `.example`), not a list entry.

## Scope

### File extensions scanned

`.rs`, `.ts`, `.tsx`, `.js`, `.mjs`, `.cjs`, `.toml`, `.md`, plus any
file matching `.env*` (e.g., `.env.dev`, `.env.local`).

### Always excluded

- `Cargo.lock`
- `package-lock.json`
- `node_modules/` (any depth)
- `target/`
- `dist/`
- `.git/`
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

1. Extract URL tokens with the regex `https?://[A-Za-z0-9.\-]+`.
2. Strip to bare host (drop scheme, port, path, query, fragment).
3. Lowercase the host.
4. **Allow** if any of:
   - Host equals an allowlist entry (exact match).
   - Host ends with `.` followed by an allowlist entry (subdomain match).
   - Host ends with `.example` (reserved TLD rule).
5. Otherwise, emit a violation line.

`example.com` and the loopback hosts (`127.0.0.1`, `::1`, `localhost`) are
ordinary allowlist entries; the subdomain rule covers `*.example.com`.

Raw IPv4/IPv6 literals that are not loopback (e.g., `68.183.113.79` in
`trusted-server.toml`) are treated as disallowed hosts and reported.

## `--staged` Mode Implementation

To scan only added lines while preserving file paths and line numbers, the
script pipes `git diff --cached -U0 --diff-filter=ACMR -- <ext-filter>` into
awk that tracks the post-image line number from each `@@` hunk header:

```
/^\+\+\+ / { file = substr($0, 7); next }    # path of new file
/^@@/      { match($0, /\+([0-9]+)/, a); ln = a[1] - 1; next }
/^\+/      { ln++; print file ":" ln ":" substr($0, 2); next }
/^ /       { ln++; next }
/^-/       { next }
```

Each emitted `path:line:content` line is then passed through the URL regex
and allowlist check.

## Output Format

```
crates/trusted-server-core/src/foo.rs:42: disallowed domain test.com
trusted-server.toml:15: disallowed domain 68.183.113.79

2 disallowed domains found in 2 files.
To allow a new integration proxy, add it to ALLOWED_HOSTS in scripts/check-domains.sh.
Run `scripts/check-domains.sh` (no args) for a full-repo audit.
```

When clean: no output, exit 0.

## Setup Flow for Contributors

```
git clone ...
./scripts/install-hooks.sh   # one-time per clone
```

After that, every `git commit` runs the linter against staged changes.
Bypass with `git commit --no-verify` (intentional escape hatch — closed in
follow-up CI work).

## Testing Strategy

A small `scripts/check-domains.test.sh` exercises the linter end-to-end:

1. **Allowed hosts** — fixture with `https://example.com`, `https://foo.example.com`,
   `https://api.privacy-center.org`, `http://127.0.0.1:8080`, `https://github.com/x/y`
   → exit 0, no output.
2. **Disallowed hosts** — fixture with `https://test.com`, `https://partner.com`,
   `https://1.2.3.4` → exit 1, all three reported.
3. **Subdomain rule** — `https://api.fastly.com` allowed; `https://other.fastly.com`
   disallowed.
4. **`.example` TLD** — `https://testlight.example` allowed.
5. **`--staged` mode** — set up a temp repo, stage a file containing a
   disallowed URL, confirm the hook fails with the correct path:line.
6. **`--staged` mode (existing violation)** — pre-commit existing file with a
   disallowed URL, then stage an unrelated change in the same file → hook
   passes (only added lines are scanned).
7. **Excluded paths** — file under `node_modules/` containing a disallowed URL
   is ignored.

Run as `scripts/check-domains.test.sh`; exit non-zero on any failure.

## Trade-offs

- **Pre-commit-only enforcement is bypassable.** `git commit --no-verify`
  skips the hook. Adding a CI job that runs the full-repo audit on every PR
  closes the gap; deferred to a follow-up.
- **`--staged` mode misses violations introduced via rebase/merge** that do
  not go through `git commit`. Acceptable for v1; CI follow-up catches them.
- **Inline allowlist requires editing the script** to add a new integration
  proxy. Acceptable given expected low churn; switching to a config file is
  trivial later.
- **Existing violations are not addressed.** They will remain until those
  files are touched. Acceptable because the goal is to prevent regression,
  not force an immediate cleanup.

## Open Questions

None.

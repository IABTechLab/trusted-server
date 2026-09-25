# Deployed git version in `x-ts-version`

**Issues:** not filed yet

**Date:** 2026-09-25

**Plan:** [../plans/2026-09-25-git-version-header.md](../plans/2026-09-25-git-version-header.md)

**Status:** Pending maintainer review

## Problem

Nothing in a Trusted Server response says which Trusted Server code is
running. The Fastly adapter does send `x-ts-version`, but it fills it from
`FASTLY_SERVICE_VERSION`. That is the Fastly service version number (for
example `42`). It changes on every activation, config-only or not, and it
cannot be mapped back to a commit without the Fastly console. The name suggests
a Trusted Server version, but the value is a Fastly one.

This matters for deploy pipelines that restrict production to published
releases. Operators and
monitors need to see from a response that production runs a release, and which
one. Staging keeps deploying branches and commits, so it needs to be
identifiable too.

## Goals

- Report the deployed git version in `x-ts-version`: the tag when the deployed
  ref is a tag, else the branch name, else the first 6 characters of the commit
  hash.
- Keep the Fastly service version available under a correct name,
  `x-ts-fastly-version`.
- Make the value follow the Wasm binary. A Fastly rollback to an earlier
  version must report that version's git version, with no extra step.
- Accept the value from the deploy pipeline, and fall back to local git so
  local and ad-hoc builds still report something useful.
- Send `x-ts-version` from every adapter, since it does not depend on the
  platform, including on the Fastly `GET /health` fast path that deploy health
  checks probe right after a deploy.

## Non-goals

- Do not change `x-ts-env` or how staging is detected.
- Do not mark dirty working trees. The local fallback reports the tag or branch
  even when the tree has uncommitted changes, so a local build of a modified
  `v1.3.0` checkout reports `v1.3.0`. CI checkouts are always clean; a local
  header is not proof of a clean release build.
- Do not normalize tag names (for example to semver). Report the ref as
  deployed, subject only to the header-safety rule in §2.
- Do not add a runtime or versionless source for the version, such as the
  `edgezero_runtime_env` Config Store or a config-blob field.
- Do not change the precedence of operator `settings.response_headers`. Whether
  managed `x-ts-*` headers should be protected from operator overrides is a
  separate decision (see Open Questions).
- Do not add `x-ts-fastly-version` to the Fastly `/health` fast path. It stays
  minimal and only gains the compiled-in `x-ts-version`.
- Do not implement a production release gate or the pipeline-side version
  resolution here. Those belong to the deploy pipeline; this repository only
  defines the `TRUSTED_SERVER_GIT_VERSION` build input.

## Current behavior

`crates/trusted-server-core/src/constants.rs` defines `HEADER_X_TS_VERSION`
(`x-ts-version`), `HEADER_X_TS_ENV`, and the env names
`ENV_FASTLY_SERVICE_VERSION` and `ENV_FASTLY_IS_STAGING`.

`apply_finalize_headers` in
`crates/trusted-server-adapter-fastly/src/middleware.rs` reads
`FASTLY_SERVICE_VERSION` from the Compute runtime env and inserts it as
`x-ts-version`. It skips an invalid value with a warning. Two doc comments on the
header write order describe this (on `FinalizeResponseMiddleware` and on
`apply_finalize_headers`).

`health_response` in `crates/trusted-server-adapter-fastly/src/main.rs` answers
`GET /health` before logging, settings, app construction, and middleware, so it
carries no `x-ts-*` header at all.

The Axum, Cloudflare, and Spin `apply_finalize_headers` functions do not send
any version header. The Axum middleware doc comment calls `X-TS-Version`
Fastly-specific. Axum and Spin register `/health` as router routes, so their
`FinalizeResponseMiddleware` already runs on it. Cloudflare has no `/health`
route; the path falls through to the publisher fallback, which also runs
through its middleware.

`crates/trusted-server-core/build.rs` only prints `rerun-if-changed=build.rs`.
The workspace compiles in no git metadata.

`docs/guide/first-party-proxy.md` shows `X-TS-Version = "1.0"` as an example
`[response_headers]` entry. Operator headers apply last, so copying that example
overwrites the managed value.

## Design

### 1. The value is compiled in

Fastly Compute only exposes platform variables (`FASTLY_SERVICE_VERSION`,
`FASTLY_IS_STAGING`, and so on) through the process env at runtime. There is no
way to add a deploy-time variable. The only EdgeZero channel for runtime values
is the `edgezero_runtime_env` Config Store. It is versionless and holds only
`EDGEZERO__*` store mappings, so a Fastly rollback would leave the newer
deploy's version in it.

So the git version is compiled into the binary. A compiled-in value is
immutable per Fastly version, so it always matches the code being served,
including after a rollback.

### 2. Resolution order

`crates/trusted-server-core/build.rs` resolves the version once per build and
emits it as the rustc env `TS_GIT_VERSION`:

1. `TRUSTED_SERVER_GIT_VERSION` from the build environment, if it is usable
   (see below). The deploy pipeline sets this.
2. Otherwise the exact tag at `HEAD`, from `git describe --tags --exact-match`.
3. Otherwise the current branch, from `git symbolic-ref --short -q HEAD`.
4. Otherwise the first 6 characters of `git rev-parse HEAD`. Take exactly 6
   characters. Do not use `--short`, which may lengthen the hash to keep it
   unique.
5. Otherwise leave it unset.

A candidate is **usable** when, after trimming surrounding whitespace, it is
non-empty and consists only of visible ASCII (bytes `0x21`–`0x7E`). An
unusable candidate is skipped and the next step is tried.

This is stricter than git. `git check-ref-format` forbids control characters,
space, and `~^:?*[\`, but it allows non-ASCII UTF-8, so a tag such as `v1-été`
is a valid ref, and a pipeline may pass such a ref through. Under this rule it
is skipped: `build.rs` prints a `cargo:warning` naming
`TRUSTED_SERVER_GIT_VERSION`, and resolution falls back to local git. In a CI
checkout that yields the 6-char hash, which still identifies the code. Falling
back was preferred over omitting the header, because a missing header is harder
to diagnose. The visible-ASCII rule keeps every compiled-in value a valid
`HeaderValue` whose `to_str()` succeeds, so consumers never see opaque bytes.

The deploy pipeline must supply the value because CI checkouts are shallow and
detached (`actions/checkout`, `fetch-depth: 1`, no tags). In such a checkout
steps 2 and 3 always fail, and only the hash is available. The pipeline knows
the ref the operator asked for and can resolve the tag or branch against
`origin` before the build.

The choice between these candidates is a pure function in
`crates/trusted-server-core/build_support/git_version.rs`. `build.rs` and an
integration test both include it through `#[path]`, so the rule is unit-tested
without running a build script.

### 3. Build caching

`build.rs` prints `cargo:rerun-if-env-changed=TRUSTED_SERVER_GIT_VERSION`.
CI deploys commonly restore a cached `target/` (EdgeZero's `deploy-fastly`
action does). Without this line a warm cache could keep the previous deploy's
version.

It also prints `cargo:rerun-if-changed` for `build.rs` and
`build_support/git_version.rs`, so editing the resolver re-runs the script.

When it uses the local git fallback, `build.rs` also prints `rerun-if-changed`
for the paths that move when the checkout does, so local branch switches and
new tags show up without `cargo clean`:

- `<git-dir>/HEAD`, from `git rev-parse --absolute-git-dir`;
- the current branch's ref file, `<common-dir>/<git symbolic-ref -q HEAD>` (for
  example `refs/heads/feature/x`), only when `HEAD` is on a branch;
- `<common-dir>/refs/tags` and `<common-dir>/packed-refs`, from
  `git rev-parse --git-common-dir`.

The current branch's ref matters because a new commit on a tagged branch must
switch the result from the tag to the branch name, so a commit on the current
branch re-runs the script and recompiles core in local builds. That is
inherent to exact-tag detection. Commits on other branches and `refs/remotes`
are deliberately not watched: they never change the result, and every
`git fetch` rewrites `refs/remotes`.

The two directories differ in a linked worktree: `HEAD` lives in
`.git/worktrees/<name>`, while refs live in the main `.git`. In a plain clone
they are the same directory.

Only paths that exist are printed. Cargo treats a missing `rerun-if-changed`
path as changed, so printing an absent `packed-refs` would re-run the script
and recompile core on every build. The accepted cost: a `packed-refs` file
created after the last run is not noticed until something else triggers a
re-run. Watching the branch ref and `refs/tags` still catches new loose refs. Likewise, if the current branch's ref exists only in `packed-refs` (after `git pack-refs`), the loose ref written by the next commit is not watched. The tag-to-branch switch after committing on a tagged commit then waits for the next re-run.

### 4. Headers

Constants in `trusted-server-core::constants`:

- `HEADER_X_TS_VERSION` keeps the name `x-ts-version`, which now means the git
  version.
- Add `HEADER_X_TS_FASTLY_VERSION`, named `x-ts-fastly-version`.
- Add `TS_GIT_VERSION: Option<&str> = option_env!("TS_GIT_VERSION")`.

A new module, `trusted-server-core::version_header`, owns the value and the
write:

- `git_version_header_value() -> Option<HeaderValue>` converts
  `TS_GIT_VERSION`. It returns `None` when no version is known, or when the
  value is not a valid header value (logged at `warn`). §2 makes the latter
  unreachable for values from `build.rs`, but the check stays as a backstop.
- `apply_git_version_header(response)` inserts `x-ts-version` from
  `git_version_header_value()`.
- Testable variants take the version as an argument.

Keeping this in core means all four adapters, and the Fastly health fast path,
use the same rule.

Fastly `apply_finalize_headers` calls `apply_git_version_header` and writes
`FASTLY_SERVICE_VERSION` to `x-ts-fastly-version` instead of `x-ts-version`.
Axum, Cloudflare, and Spin `apply_finalize_headers` call
`apply_git_version_header` right after the geo-availability header. In every
adapter the call runs before `apply_response_headers_with_cache_privacy`, so the
existing order is kept: operator headers still apply last.

Fastly `health_response` sets `x-ts-version` from `git_version_header_value()`.
The `fastly` crate's `set_header` accepts the `http` 1.x `HeaderValue` that
`edgezero_core::http` re-exports, so no conversion is needed and nothing can
panic. The fast path stays free of settings, logging, and app construction.

A Fastly production response from release `v1.3.0`, served as Fastly version 42:

```text
x-ts-version: v1.3.0
x-ts-fastly-version: 42
```

A staged deploy of branch `feature/x`:

```text
x-ts-version: feature/x
x-ts-fastly-version: 43
x-ts-env: staging
```

`GET /health` on either:

```text
x-ts-version: v1.3.0
```

## Error Handling

`build.rs` never fails the build over version metadata. If git is missing, the
directory is not a repository (for example an exported source tree), or git
fails, the build leaves `TS_GIT_VERSION` unset and `x-ts-version` is omitted.
Sending a placeholder such as `unknown` was rejected: a missing header is easier
to tell apart from a real ref.

An unusable `TRUSTED_SERVER_GIT_VERSION` (§2) produces a `cargo:warning` and
falls back to local git. It does not fail the build.

No new public error type is introduced.

## Compatibility and Rollout

This is a **breaking change** for anything that reads `x-ts-version` as the
Fastly version number. Dashboards, monitors, and scripts must switch to
`x-ts-fastly-version`. The CHANGELOG entry calls this out.

The rollout does not depend on the deploy pipeline:

- A pipeline that does not set `TRUSTED_SERVER_GIT_VERSION`: CI deploys report
  the 6-char hash from the local fallback. This is still correct, just less
  readable.
- A pipeline that sets it before this change ships: the variable is in the
  build env but not read, so behavior is unchanged.

Rollback needs nothing extra. Reactivating an earlier Fastly version serves that
version's compiled-in value.

## Alternatives and Decision

- **`edgezero_runtime_env` Config Store.** Rejected. It is versionless, so it is
  wrong after a rollback. It holds only `EDGEZERO__*` mappings, and writing it
  per deploy would need EdgeZero changes.
- **Operator `[response_headers]` in the pushed config blob.** Rejected. Config
  push is versionless and not reverted by rollback, and it depends on every
  operator keeping it in sync.
- **Local git only, in `build.rs`.** Rejected as the only source. CI checkouts
  are shallow and detached, so it would always produce the hash and never the
  tag or branch. It stays as the fallback.
- **A new header, keeping `x-ts-version` as the Fastly version.** Rejected. The
  existing name is the one people expect to carry the Trusted Server version,
  and keeping it wrong would keep misleading them.
- **Omitting the header on an unusable override.** Rejected in favor of falling
  back to local git (§2).

Decision: compile in the pipeline-supplied value, with a local git fallback.

## Testing

Unit and integration tests follow red-green-refactor and cover:

- resolution: override wins; tag beats branch; branch when untagged; exactly 6
  characters of the commit; blank candidates skipped; a non-ASCII override
  (`v1-été`) and one with an inner space are skipped in favor of local git;
  `None` when nothing is known;
- `version_header`: value from a version; `None` when unknown; `None` for an
  invalid value; the header is set, omitted, or skipped accordingly; the
  default path reports the compiled-in `TS_GIT_VERSION`;
- Fastly `apply_finalize_headers`: `x-ts-version` equals `TS_GIT_VERSION`, and
  `x-ts-fastly-version` equals `FASTLY_SERVICE_VERSION` when set;
- Fastly `health_response`: `x-ts-version` equals `TS_GIT_VERSION`, and
  `x-ts-fastly-version` is absent;
- Axum, Cloudflare, and Spin `apply_finalize_headers`: `x-ts-version` equals
  `TS_GIT_VERSION`;
- Axum and Spin `GET /health` through the router: `x-ts-version` equals
  `TS_GIT_VERSION`.

End-to-end, offline, with Viceroy and no live Fastly service:

1. Build the release Wasm with `TRUSTED_SERVER_GIT_VERSION=v9.9.9-test`, run it
   under `fastly compute serve --file <wasm>`, and `curl -sI` both `GET /health`
   and a route that passes through `FinalizeResponseMiddleware`, with a local
   `trusted_server_config` pushed as `scripts/smoke-fastly.sh` does so the route
   loads settings. Assert the status codes (`200` for both) so the check cannot
   pass on an error page. Both carry
   `x-ts-version: v9.9.9-test`. The finalized route carries
   `x-ts-fastly-version` with whatever Viceroy reports, and no response carries
   a numeric `x-ts-version`.
2. Warm-cache rebuild, without cleaning, with `v9.9.9-test2`. The build script
   re-runs (visible in `cargo build -vv`), and the header changes.
3. Rebuild with the variable unset, on a branch and then on a detached `HEAD`.
   The header reports the branch, then the 6-char hash.
4. `git archive` the tree into a scratch directory outside any repository and
   build there with the variable unset. The build succeeds and `x-ts-version`
   is absent.

Final verification runs every CI gate in `CLAUDE.md`: `cargo fmt --all --
--check`; all eight clippy aliases; the Fastly, Axum, Cloudflare, and Spin test
aliases; the parity integration test; and the JS and docs format checks. The
handoff reports the commands run and their observed results.

## Documentation

- In `docs/guide/first-party-proxy.md`, replace the `X-TS-Version = "1.0"`
  `[response_headers]` example with a header that is not managed
  (`X-Debug-Build = "canary"`).
- Add a breaking-change entry to `CHANGELOG.md` under `## [Unreleased]` →
  `### Changed`.
- Update the header write-order doc comments in the Fastly middleware, and the
  Axum middleware comment that calls `X-TS-Version` Fastly-specific.

## Acceptance Criteria

- A Fastly deploy built with `TRUSTED_SERVER_GIT_VERSION=v1.3.0` returns
  `x-ts-version: v1.3.0` and `x-ts-fastly-version: <service version>` on
  finalized responses, and `x-ts-version: v1.3.0` on `GET /health`.
- A staging deploy of branch `feature/x` returns `x-ts-version: feature/x`. A
  deploy of a bare SHA returns the first 6 characters of that commit.
- A local build without the override reports the local tag, branch, or 6-char
  hash, in that order of preference, including from a linked worktree.
- A build with neither the override nor git omits `x-ts-version` and still
  succeeds.
- An override that is not visible ASCII produces a `cargo:warning` and the
  local git fallback.
- Changing `TRUSTED_SERVER_GIT_VERSION` between builds changes the header even
  with a warm `target/` cache.
- Axum, Cloudflare, and Spin responses carry `x-ts-version`.
- No response carries the Fastly service version as `x-ts-version`.

## Open Questions

- Should operator `settings.response_headers` be able to override
  `x-ts-version` and `x-ts-fastly-version`? This spec keeps current precedence.
  Protecting managed diagnostic headers, as cache-control is already protected
  on uncacheable responses, can follow separately.

## Expected Files

- `crates/trusted-server-core/build.rs`
- `crates/trusted-server-core/build_support/git_version.rs`
- `crates/trusted-server-core/tests/git_version_resolve.rs`
- `crates/trusted-server-core/src/constants.rs`
- `crates/trusted-server-core/src/version_header.rs`
- `crates/trusted-server-core/src/lib.rs`
- `crates/trusted-server-adapter-fastly/src/main.rs`
- `crates/trusted-server-adapter-fastly/src/middleware.rs`
- `crates/trusted-server-adapter-axum/src/middleware.rs`
- `crates/trusted-server-adapter-axum/tests/routes.rs`
- `crates/trusted-server-adapter-cloudflare/src/middleware.rs`
- `crates/trusted-server-adapter-spin/src/middleware.rs`
- `crates/trusted-server-adapter-spin/src/app.rs`
- `docs/guide/first-party-proxy.md`
- `CHANGELOG.md`
- `docs/superpowers/specs/2026-09-25-git-version-header-design.md`
- `docs/superpowers/plans/2026-09-25-git-version-header.md`

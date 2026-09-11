# Documentation Refresh Implementation Plan

- Status: executed on `spec-docs-refresh`
- PR: https://github.com/IABTechLab/trusted-server/pull/1049
- Base: `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`
- Revised: 2026-09-11 after reviewer scope correction

## Goal

Refresh the maintained documentation against shipped code, preserve the
publisher setup journey, and verify every supported runtime without introducing
a separate documentation tooling platform.

## Delivery rules

- Commit every package to PR #1049; create no individual PRs.
- Keep the public documentation and ordinary regression coverage.
- Do not add a standalone analysis workspace, generator manifests, scheduled
  issue writers, or dependency-submission automation.
- Use exact action release tags.
- Delegate workflow logic longer than one line to repository scripts.
- Keep the complete CI command matrix only in `CLAUDE.md`.
- Bind historical evidence only to immutable commit SHAs; never describe a
  moving branch head as exact.

## Package 1: Publishing and information architecture

Files:

- `docs/.vitepress/config.mts`
- `docs/guide/index.md`
- `docs/internal/onboarding.md`
- `docs/business-use-cases.md`
- `docs/superpowers/archive/FAQ_POC.md`
- `docs/public/CNAME`

Work:

- [x] Retain product getting-started material on the public site.
- [x] Move maintainer onboarding to the internal source tree.
- [x] Exclude internal, archived, and unverified business-use-case material
      from VitePress inputs.
- [x] Remove the placeholder custom-domain file and retain the project-path
      deployment base.
- [x] Verify navigation and repository-relative onboarding links.

Verification:

```bash
cd docs
npm ci
npm run lint
npm run format
npm run build
```

## Package 2: Product and operator guides

Files include:

- `README.md`, `CONTRIBUTING.md`, `ProjectGovernance.md`, `TESTING.md`
- `docs/guide/{ad-serving,architecture,configuration,edgezero}.md`
- `docs/guide/{api-reference,cli,testing,telemetry,tsjs}.md`
- `docs/guide/{fastly,axum-dev,cloudflare,spin}.md`
- `docs/guide/integrations-overview.md` and per-integration pages
- `docs/guide/auction-testing.md` and `creative-processing.md`

Work:

- [x] Describe the portable core and four adapter boundaries.
- [x] Document configuration initialization, validation, secret references,
      push, adapter store mapping, and restart semantics.
- [x] Document normal and degraded routes by adapter.
- [x] Cover deploy IDs, integration activation, auction provider profiles,
      JavaScript loading, telemetry, and CLI workflows.
- [x] Use fictional examples and make maturity/limitations explicit.
- [x] Retire or tombstone stale public claims.

Verification:

```bash
cd docs
npm run lint
npm run format
npm run build
```

Review every command, flag, route, setting, and integration claim against its
implementation and focused tests.

## Package 3: Crate and API documentation

Files include each workspace crate README plus touched Rust and TypeScript
modules.

Work:

- [x] Add a concise role, target, entry-point, and test command to every crate
      README.
- [x] Add useful module and public-item documentation where warnings exposed
      missing context.
- [x] Keep code changes documentation-only unless a test or smoke reveals an
      actual defect.
- [x] Preserve target portability in core crates.

Verification:

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features \
  -p trusted-server-core -p trusted-server-js -p trusted-server-openrtb \
  --target wasm32-wasip1
cargo test --doc -p trusted-server-core
```

Run the remaining adapter and host rustdoc commands listed in
`CLAUDE.md#ci-gates`.

## Package 4: Documentation examples and route contracts

Files:

- `crates/trusted-server-integration-tests/tests/documentation_snippets.rs`
- adapter route tests
- example configuration tests

Work:

- [x] Compile representative guide snippets through production APIs.
- [x] Assert documented route availability and degraded startup behavior.
- [x] Keep tests independent of internal documentation records or manifests.
- [x] Keep the example-template uncomment helper syntax-aware so prose comments
      are not treated as configuration.

Verification:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --test parity
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --test documentation_snippets
```

## Package 5: Runtime smoke coverage

Files:

- `scripts/smoke-common.sh`
- `scripts/smoke-{axum,fastly,cloudflare,spin}.sh`
- adapter deployment guides
- integration workflow where supported

Work:

- [x] Use an owned temporary workspace and bounded ports for every adapter.
- [x] Prove missing config and each required-secret failure.
- [x] Require a real publisher success response, origin sentinel, and URL
      rewrite; do not accept health alone.
- [x] Capture Spin's real component stderr for startup failure.
- [x] Keep Spin's global logging slot untouched.
- [x] Copy Fastly manifests into a per-run project before config push or secret
      mutation.
- [x] Stop only owned processes and remove only validated temporary paths.

Verification:

```bash
bash -n scripts/smoke-common.sh scripts/smoke-axum.sh \
  scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh
shellcheck -x scripts/smoke-common.sh scripts/smoke-axum.sh \
  scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh
./scripts/smoke-axum.sh
./scripts/smoke-fastly.sh
./scripts/smoke-cloudflare.sh
./scripts/smoke-spin.sh
```

Run platform smokes only where the corresponding CLI is available; record omissions
without representing them as passes.

## Package 6: CI and dependency maintenance

Files:

- `.github/workflows/{format,test,integration-tests,codeql,deploy-docs}.yml`
- `.github/actions/setup-integration-test-env/action.yml`
- `.github/dependabot.yml`
- `.tool-versions`
- scripts invoked by workflow steps
- `CLAUDE.md`, `TESTING.md`, `docs/guide/testing.md`

Work:

- [x] Retain read-only pull-request permissions.
- [x] Build the VitePress site so internal links fail the docs job.
- [x] Run documentation snippets, adapter checks, CLI tests, and release builds
      in normal CI jobs.
- [x] Keep the aggregate rustdoc and doctest pass in the manual documentation
      workflow so it cannot become a required core or adapter check.
- [x] Cover actual dependency roots in Dependabot.
- [x] Read pinned tool versions through a shell script where workflow logic is
      multiline.
- [x] Use exact action release tags rather than commit hashes.
- [x] Keep one canonical gate matrix and link to it elsewhere.
- [x] Provide one source-preserving shell entry point for the existing
      documentation gates without adding a parser, generator, or manifest system.
- [x] Expose that entry point through a manual-only GitHub Actions workflow so
      it does not become a required core or adapter check.

Verification:

```bash
git diff --check
rg -n 'uses: [^ ]+@[0-9a-f]{40}' .github
rg -n 'run: *\\|' .github
cargo metadata --locked --no-deps --format-version 1
```

Any remaining multiline `run` block requires a focused review and a script if
it was introduced by this branch.

## Package 7: Reviewer scope remediation

Work:

- [x] Remove the rejected standalone documentation tooling subtree.
- [x] Remove its workflows, scripts, dependency root, inventories, markers, and
      generated/check claims.
- [x] Remove core test modules that read proposal-owned manifests.
- [x] Preserve ordinary documentation snippet and adapter regression tests.
- [x] Replace duplicated gate matrices with links to `CLAUDE.md`.
- [x] Rewrite the spec, plan, decisions, evidence, and release runbook to match
      the repository that will merge.
- [x] Replace the Spin global logger with one direct startup stderr write.
- [x] Isolate Fastly smoke manifests per run.

Structural verification:

```bash
test ! -e tools
! git grep -n 'tools/docs' -- crates/trusted-server-core
git diff --check
```

## Final verification and delivery

Run the complete matrix in `CLAUDE.md#ci-gates`, plus workflow and shell checks.
Inspect:

```bash
git status --short
git diff --stat 07dfc1c6...HEAD
git diff --check
```

Then:

1. commit with an imperative, sentence-case subject;
2. push `spec-docs-refresh` to the existing PR;
3. wait for checks on the exact pushed SHA;
4. reply to each reviewer with the concrete fix and verification;
5. resolve only threads whose finding is fully addressed; and
6. update the bounded PR acceptance summary after hosted checks are green.

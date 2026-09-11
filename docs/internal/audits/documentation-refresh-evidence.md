# Documentation Refresh Evidence

This ledger records reproducible evidence for the documentation refresh without
claiming that a moving branch name is immutable. Commands run against working
tree content are local verification; hosted PR checks are bound to the SHA
shown by GitHub.

## Immutable baseline

| Item                  | Value                                                  |
| --------------------- | ------------------------------------------------------ |
| Target branch         | `rc/202608`                                            |
| Audited base SHA      | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`             |
| Implementation branch | `spec-docs-refresh`                                    |
| Pull request          | https://github.com/IABTechLab/trusted-server/pull/1049 |

## Historical hosted checkpoint

The following receipt applies only to immutable commit
`51ad97aeaf24de29d46e3e749406de93a99a4cae`. GitHub reported every required
check green at that checkpoint. Review after that run identified scope and
runtime defects, so the green result is historical evidence, not final
acceptance.

The previous tool-specific evidence has been removed with the rejected proposal.
It cannot be used to claim coverage for the revised repository.

## Documentation surfaces reviewed

- public landing, setup, architecture, configuration, API, adapters,
  integrations, ad serving, auction testing, telemetry, JavaScript, CLI,
  creative processing, errors, and testing;
- internal onboarding and publishing containment;
- root policy and contributor documentation;
- all workspace crate READMEs;
- Rust API documentation and exported TypeScript documentation;
- four adapter route/startup contracts;
- adapter smoke scripts and supported CI targets.

## Local verification contract

### Structure

```bash
test ! -e tools
! git grep -n 'tools/docs' -- crates/trusted-server-core
git diff --check
cargo metadata --locked --no-deps --format-version 1
```

Expected: no rejected tooling or core receipt coupling, no whitespace errors,
and valid locked workspace metadata.

### Documentation and JavaScript

```bash
cd docs
npm ci
npm run lint
npm run format
npm run build

cd ../crates/trusted-server-js/lib
npm ci
npm run lint
npx vitest run
npm run format
npm run build
```

Expected: lint, formatting, tests, and production builds pass.

### Rust targets and documentation

Run the exact matrix in `CLAUDE.md#ci-gates`. It includes target-matched
formatting, Clippy, adapter tests, CLI/codegen tests, integration parity,
documentation snippets, release builds, rustdoc, and doctests.

### Shell and workflow structure

```bash
bash -n scripts/*.sh
shellcheck -x scripts/smoke-common.sh scripts/smoke-axum.sh \
  scripts/smoke-fastly.sh scripts/smoke-cloudflare.sh scripts/smoke-spin.sh
rg -n 'uses: [^ ]+@[0-9a-f]{40}' .github
rg -n 'run: *\\|' .github
```

Expected: shell syntax and lint pass; no action SHA pins; any multiline workflow
step introduced by this branch is delegated to a script.

### Runtime smoke

```bash
./scripts/smoke-axum.sh
./scripts/smoke-fastly.sh
./scripts/smoke-cloudflare.sh
./scripts/smoke-spin.sh
```

Each available runtime must prove missing config, missing secrets, and a real
publisher success path. A missing local CLI is recorded as not run, never pass.

## 2026-09-11 local verification

This receipt applies to the reviewed working-tree content before its final
commit. It is not a hosted-SHA receipt.

- `./scripts/check-documentation.sh` passed the site, TypeScript lint,
  documentation snippet, core doctest, and all rustdoc targets from a
  second-run state with VitePress-generated temporary output present.
- `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, and
  `cargo test-spin` passed. Fastly and Axum required normal host access to the
  certificate store and loopback sockets, respectively.
- Every target-matched Clippy command in `CLAUDE.md`, integration-parity tests,
  CLI tests including the ignored Chromium fixtures, OpenRTB codegen tests,
  JavaScript lint/format/test/build, and both WASM release builds passed.
- Axum, Cloudflare, Fastly, and Spin first-success smokes passed. Cloudflare ran
  with the repository-pinned Wrangler `4.129.0`. Spin ran with locally installed
  Spin `4.1.0`, which is newer than EdgeZero's verified major-version range but
  completed the SQLite handoff and runtime assertions.
- Two Fastly smokes ran concurrently on disjoint ports. Both passed, and SHA-1
  hashes of root `edgezero.toml` and `fastly.toml` were identical before and
  after the runs.

## Reviewer finding disposition

| Finding                                              | Disposition                                                                  |
| ---------------------------------------------------- | ---------------------------------------------------------------------------- |
| Standalone tooling subtree is outside approved scope | Removed with all direct CI, script, manifest, marker, and test consumers     |
| Dependency snapshot omitted GitHub's `scanned` field | Feature removed; no snapshot is generated or submitted                       |
| Core tests read proposal-owned manifests             | Receipt suites removed; ordinary production and regression tests remain      |
| Spin startup logger suppresses normal records        | Global logger removed; one startup diagnostic writes directly to Spin stderr |
| Fastly smoke mutates a shared tracked manifest       | Smoke uses copied manifests in a per-run temporary project                   |
| Gate matrix is duplicated                            | Full matrix retained only in `CLAUDE.md`                                     |
| Exact-head ledger claim became stale                 | Moving-head claim removed; historical receipts are SHA-bound                 |

## Final acceptance

The authoritative final receipt is PR #1049 at its exact pushed commit:

1. GitHub shows the intended `spec-docs-refresh` head SHA.
2. Every required check is green for that SHA.
3. Review threads are answered with the concrete code/documentation change.
4. The PR diff contains no rejected tooling or dependent surface.
5. Any local platform smoke not reproducible in hosted CI is named explicitly.

This section deliberately contains no branch-head SHA. Embedding the SHA in the
commit that defines the SHA would create a false or recursive receipt.

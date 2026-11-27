# Repository scripts

The four `smoke-*.sh` adapter scripts require Python 3.11 or newer; the Fastly fixture reads TOML with `tomllib`.

Run these scripts from the repository root. They fail on command errors unless
their documented orchestration handles a probe deliberately.

| Script                                    | Inputs and prerequisites                                                                    | Side effects and cleanup                                                                                                                                                                                                                                                                                                                                       |
| ----------------------------------------- | ------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `batch-sync.sh`                           | Endpoint, bearer token, EC ID, partner UID; `curl` and Python                               | Sends one authenticated batch-sync request. Its temporary response file is removed on exit.                                                                                                                                                                                                                                                                    |
| `benchmark.sh`                            | A running server; `curl`, `bc`, and optionally `hey`                                        | May install `hey` through Homebrew. `--save` writes under `benchmark-results/`; it does not stop the server.                                                                                                                                                                                                                                                   |
| `check-ci-node-pin.mjs`                   | Pinned Node; tests in `check-ci-node-pin.test.mjs`                                          | Read-only. Fails when a CI job compiles Rust before an unconditional `actions/setup-node` step pinned to `.tool-versions`; `--root` checks another checkout.                                                                                                                                                                                                   |
| `profile.sh`                              | Fastly CLI, Rust WASM target, `curl`; endpoint and request options                          | Builds and starts the Fastly app, stops the owned process, and retains a profile under `benchmark-results/profiles/`. `--open` launches the local viewer.                                                                                                                                                                                                      |
| `generate-integration-viceroy-configs.sh` | Rust toolchain; optional origin port, artifact directory, bidder origin                     | Builds the native generator and writes Viceroy config under `target/integration-test-artifacts/`; generated files persist. With `INTEGRATION_BIDDER_ORIGIN_URL` set, the generator replaces the scheme and authority of every `auction.providers.*.endpoint` with that origin.                                                                                 |
| `integration-tests.sh`                    | Docker, Viceroy, Rust WASM target, pinned Node; Spin CLI only for a named Spin runtime case | Builds WASM/native artifacts and two Docker images, generates Viceroy config, and runs native integration tests serially. Skips the trace probes and the Spin runtime cases, as CI's main job does, unless a test filter names one; naming a Spin runtime case also builds the production Spin component. Build products and images persist.                   |
| `integration-tests-browser.sh`            | The integration prerequisites plus npm and Playwright                                       | Installs package/browser dependencies, builds fixtures and images, runs both browser suites, and stops matching test containers on exit. Build and npm artifacts persist.                                                                                                                                                                                      |
| `smoke-axum.sh`                           | `cargo`, `curl`, `jq`, Python, and `pgrep`                                                  | Uses an isolated temporary config/store and stub origin. Each negative case must keep `/health` live while `/ready` and the publisher route answer 503, and log its diagnostic. Stops owned processes and removes its workspace.                                                                                                                               |
| `smoke-fastly.sh`                         | Fastly CLI, `cargo`, `curl`, Python, and `pgrep`                                            | Uses an isolated Fastly project and application config; stops owned processes and removes its workspace.                                                                                                                                                                                                                                                       |
| `smoke-cloudflare.sh`                     | Pinned Wrangler, `cargo`, `curl`, `jq`, Python, and `pgrep`                                 | Uses isolated Wrangler manifests, KV state, ports, and logs; stops owned processes and removes its workspace.                                                                                                                                                                                                                                                  |
| `smoke-spin.sh`                           | Spin CLI, `cargo`, `curl`, Python, and `pgrep`                                              | Uses an isolated Spin manifest and SQLite KV store; stops owned processes and removes its workspace.                                                                                                                                                                                                                                                           |
| `smoke-common.sh`                         | Sourced by the four smoke scripts                                                           | Defines bounded port, process, config, secret, and response assertions. Do not invoke it as a standalone smoke.                                                                                                                                                                                                                                                |
| `async-html-finalization-local-test.sh`   | Rust, pinned Node and selected runtime; repository-pinned Viceroy                           | Builds feature-gated fixtures, preserves compiler settings, retains diagnostics, and stops owned process groups on failure, SIGTERM or SIGINT.                                                                                                                                                                                                                 |
| `template-cache-local-test.sh`            | Viceroy, Node, Python, `curl`, `lsof`; optional ports and mode                              | Builds local artifacts, generates fixtures in a temporary directory, including a stub bidder over plain HTTP on `127.0.0.1`, stops owned servers, and removes the directory.                                                                                                                                                                                   |
| `fastly-response-diff.sh`                 | `--base` and `--head` revisions; Viceroy, WASM target, pinned Node, `curl`                  | Builds each revision in a detached worktree under `$TMPDIR`, serves both under Viceroy with a loopback origin, and diffs them through `fastly-response-diff.mjs` against `fastly-response-diff.expected.json`. Stops its servers and removes the worktrees with `git worktree remove` on exit.                                                                 |
| `test-cli.sh`                             | Rustup and the host toolchain; optional host triple                                         | May install the selected Rust target, then runs native CLI and browser-audit tests. Cargo artifacts persist.                                                                                                                                                                                                                                                   |
| `test-worker-auction-deadlines.mjs`       | Adapter, its runtime CLI, pinned Node, prebuilt example; `wasm-bindgen`                     | Serves the example, which sends request batches through EdgeZero's Cloudflare or Spin client, against delayed local origins. Stops the runtime and removes its temporary directory.                                                                                                                                                                            |
| `docs-proposal/propose.sh`                | Commit SHA on the branch, work dir, optional base SHA; Copilot CLI, Node, `npm`             | Runs Copilot CLI to edit `docs/guide/**` or `docs/index.md`, formats docs, and writes a patch, rationale, evidence, and base into the work dir. Leaves the edits staged.                                                                                                                                                                                       |
| `docs-proposal/validate.sh`               | Merge SHA, work dir from `propose.sh`; Node, `npm`; no write credentials                    | Applies the patch, rejects paths outside the allowlist, and runs the docs gates. Leaves the edits staged.                                                                                                                                                                                                                                                      |
| `docs-proposal/publish.sh`                | Merge SHA, work dir from `propose.sh`; `gh` with write `GH_TOKEN`                           | Re-checks the allowlist without building, pushes `docs/auto/<sha12>` (`docs/auto/<branch-hash12>/<sha12>` off `main`) unless a maintainer added to it, opens or edits its PR, and reviews each new commit per hunk. An empty rerun deletes the branch under a lease, then closes the PR; a failed branch lookup stops the run. Never writes the target branch. |
| `docs-proposal/test.sh`                   | Git, Node, and `jq`                                                                         | Tests the helpers and the propose, validate, and publish flows against a temporary repository with stubbed `copilot`, `gh`, and `npm`; removes it on exit.                                                                                                                                                                                                     |

## Health and startup smoke checks

Run `./scripts/smoke-axum.sh`, `./scripts/smoke-fastly.sh`,
`./scripts/smoke-cloudflare.sh`, or `./scripts/smoke-spin.sh` with current
production artifacts and the pinned runtime tools. The health/startup CI matrix
runs each adapter separately. See the canonical
[operational response contract](../docs/guide/api-reference.md#operational-probe-contract).

Positive application-start waits require `/ready` to return 200. Negative
startup fixtures wait for liveness or transport, then require `/health` to stay
200 and `/ready` plus ordinary publisher requests to return a generic 503.
The smoke checks exact GET bodies, empty HEAD bodies, fixed content types,
`Cache-Control: no-store`, and no cookie or authentication challenge headers.
Missing config, each required secret, a malformed envelope, and valid-envelope
invalid settings are separate controlled startup failures. Detailed diagnostics
belong in logs; public failures contain only `Service Unavailable\n`. The
malformed-envelope case injects a literal `{` at the runtime config boundary.
The invalid-settings case sets `publisher.domain` to the empty string and
reconstructs a valid-integrity envelope, separating settings validation from
envelope parsing.

Probes bypass catch-all authentication and configured response headers and do
not forward to the origin. An ordinary protected route must still challenge,
and the final publisher request must return the controlled origin sentinel and
expected URL rewrite. A green probe cannot replace that publisher control.

Set `SMOKE_ARTIFACT_DIR` to preserve synthetic fixture logs and HTTP response
captures before temporary-workspace cleanup. Config envelopes, secret fixture
files, and ambient environment files are excluded; default cleanup still runs.
The health/startup CI workflow uploads these captures on failure.

## HTML EOF runtime checks

Requires Rust, pinned Node and the selected runtime CLI. Run from the
repository root:

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime
./scripts/async-html-finalization-local-test.sh axum --auction
./scripts/async-html-finalization-local-test.sh cloudflare --release --auction
./scripts/async-html-finalization-local-test.sh spin --release --auction
```

**Fastly requires Viceroy 0.19 or newer** for the hung-backend fixture. Use the
repository-pinned version shared with ordinary tests:

```bash
cargo install viceroy --version "$(awk '$1 == "viceroy" { print $2 }' .tool-versions)" --locked --force
./scripts/async-html-finalization-local-test.sh fastly --release --auction
```

Workers requires matching `worker-build`/`wasm-bindgen` versions and Wrangler.
The harness builds fresh feature-gated artifacts, checks generated timeout,
permission and loopback configuration, preserves explicit compiler/profile settings
and `CARGO_TARGET_DIR`, selects build targets itself, and stops owned process groups on failure, SIGTERM or SIGINT.
Artifacts and diagnostics persist in the printed temporary directory for inspection.
CI runs native Rust helper tests and lint, Axum and Fastly EOF controls. See the
[testing guide](../docs/guide/testing.md#html-eof-runtime-checks) for coverage details.

The auction control deliberately uses an unreachable bidder and verifies empty-bid
script assembly after finalized output; it does not measure successful or delayed bids.

The four adapter smoke contracts are documented in the
[deployment guides](../docs/guide/integrations-overview.md#adapter-support).
Repository-wide verification policy lives in [TESTING.md](../TESTING.md).

The `Documentation proposal` workflow runs the propose, validate, and publish
scripts when a maintainer dispatches it from the Actions tab. "Use workflow
from" picks the branch to propose documentation for and to open the pull
request against, so that branch must contain the workflow. `since` is the
last commit already documented, usually the `sha` of the previous proposal;
the run covers every change after it up to `sha`, which defaults to the
branch head. A `since` that is not an earlier commit of `sha` fails the run.
Each run opens one pull request on `docs/auto/<sha12>` for `main`, or
`docs/auto/<branch-hash12>/<sha12>` for another branch, so runs that end at
different commits open separate proposals; choose `since` so their ranges do
not overlap. Its first run fails unless the organization Copilot policy "Allow
use of Copilot CLI billed to the organization" and the repository setting
"Allow GitHub Actions to create and approve pull requests" are already
enabled. Pull requests opened with `GITHUB_TOKEN` do not trigger other
workflows, so the workflow's validate job runs the docs gates itself, without
write credentials. The checks `main` requires never start on a proposal, so
close and reopen it, or push to its branch, before merging. To retry a run,
dispatch it again with the same `since` and `sha`, both recorded in the
proposal's body; publication refuses a retry whose range differs from an open
proposal's.

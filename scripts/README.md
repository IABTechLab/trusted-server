# Repository scripts

Run these scripts from the repository root. They fail on command errors unless
their documented orchestration handles a probe deliberately.

| Script                                    | Inputs and prerequisites                                                   | Side effects and cleanup                                                                                                                                                                                                                                                                           |
| ----------------------------------------- | -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `batch-sync.sh`                           | Endpoint, bearer token, EC ID, partner UID; `curl` and Python              | Sends one authenticated batch-sync request. Its temporary response file is removed on exit.                                                                                                                                                                                                        |
| `benchmark.sh`                            | A running server; `curl`, `bc`, and optionally `hey`                       | May install `hey` through Homebrew. `--save` writes under `benchmark-results/`; it does not stop the server.                                                                                                                                                                                       |
| `check-ci-node-pin.mjs`                   | Pinned Node; tests in `check-ci-node-pin.test.mjs`                         | Read-only. Fails when a CI job compiles Rust before an unconditional `actions/setup-node` step pinned to `.tool-versions`; `--root` checks another checkout.                                                                                                                                       |
| `profile.sh`                              | Fastly CLI, Rust WASM target, `curl`; endpoint and request options         | Builds and starts the Fastly app, stops the owned process, and retains a profile under `benchmark-results/profiles/`. `--open` launches the local viewer.                                                                                                                                          |
| `generate-integration-viceroy-configs.sh` | Rust toolchain; optional origin port and artifact directory                | Builds the native generator and writes Viceroy config under `target/integration-test-artifacts/`; generated files persist.                                                                                                                                                                         |
| `integration-tests.sh`                    | Docker, Viceroy, Rust WASM target, pinned Node                             | Builds WASM/native artifacts and two Docker images, generates Viceroy config, and runs native integration tests serially. Build products and images persist.                                                                                                                                       |
| `integration-tests-browser.sh`            | The integration prerequisites plus npm and Playwright                      | Installs package/browser dependencies, builds fixtures and images, runs both browser suites, and stops matching test containers on exit. Build and npm artifacts persist.                                                                                                                          |
| `smoke-axum.sh`                           | `cargo`, `curl`, Python                                                    | Uses an isolated temporary config/store and stub origin; stops owned processes and removes its workspace.                                                                                                                                                                                          |
| `smoke-fastly.sh`                         | Fastly CLI, `cargo`, `curl`, Python                                        | Uses an isolated Fastly project and application config; stops owned processes and removes its workspace.                                                                                                                                                                                           |
| `smoke-cloudflare.sh`                     | Pinned Wrangler, `cargo`, `curl`, `jq`, and Python                         | Uses isolated Wrangler manifests, KV state, ports, and logs; stops owned processes and removes its workspace.                                                                                                                                                                                      |
| `smoke-spin.sh`                           | Spin CLI, `cargo`, `curl`, and Python                                      | Uses an isolated Spin manifest and SQLite KV store; stops owned processes and removes its workspace.                                                                                                                                                                                               |
| `smoke-common.sh`                         | Sourced by the four smoke scripts                                          | Defines bounded port, process, config, secret, and response assertions. Do not invoke it as a standalone smoke.                                                                                                                                                                                    |
| `async-html-finalization-local-test.sh`   | Rust, pinned Node and selected runtime; repository-pinned Viceroy          | Builds feature-gated fixtures, preserves compiler settings, retains diagnostics, and stops owned process groups on failure, SIGTERM or SIGINT.                                                                                                                                                     |
| `template-cache-local-test.sh`            | Viceroy, Node, OpenSSL, `curl`, `lsof`; optional ports and mode            | Builds local artifacts, generates certificates and fixtures in a temporary directory, stops owned servers, and removes the directory.                                                                                                                                                              |
| `fastly-response-diff.sh`                 | `--base` and `--head` revisions; Viceroy, WASM target, pinned Node, `curl` | Builds each revision in a detached worktree under `$TMPDIR`, serves both under Viceroy with a loopback origin, and diffs them through `fastly-response-diff.mjs` against `fastly-response-diff.expected.json`. Stops its servers and removes the worktrees with `git worktree remove` on exit.     |
| `test-cli.sh`                             | Rustup and the host toolchain; optional host triple                        | May install the selected Rust target, then runs native CLI and browser-audit tests. Cargo artifacts persist.                                                                                                                                                                                       |
| `docs-proposal/propose.sh`                | Merge SHA on `main`, work dir, optional base SHA; Copilot CLI, Node, `npm` | Runs Copilot CLI to edit `docs/guide/**` or `docs/index.md`, formats docs, and writes a patch, rationale, evidence, and base into the work dir. Leaves the edits staged.                                                                                                                           |
| `docs-proposal/validate.sh`               | Merge SHA, work dir from `propose.sh`; Node, `npm`; no write credentials   | Applies the patch, rejects paths outside the allowlist, and runs the docs gates. Leaves the edits staged.                                                                                                                                                                                          |
| `docs-proposal/publish.sh`                | Merge SHA, work dir from `propose.sh`; `gh` with write `GH_TOKEN`          | Re-checks the allowlist without building, pushes `docs/auto/<sha12>` unless a maintainer added to it, opens or edits its PR, and reviews each new commit per hunk. An empty rerun deletes the branch under a lease, then closes the PR; a failed branch lookup stops the run. Never writes `main`. |
| `docs-proposal/test.sh`                   | Git, Node, and `jq`                                                        | Tests the helpers and the propose, validate, and publish flows against a temporary repository with stubbed `copilot`, `gh`, and `npm`; removes it on exit.                                                                                                                                         |

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

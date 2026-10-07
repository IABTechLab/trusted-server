# Repository scripts

Run these scripts from the repository root. They fail on command errors unless
their documented orchestration handles a probe deliberately.

| Script                                    | Inputs and prerequisites                                                   | Side effects and cleanup                                                                                                                                                                |
| ----------------------------------------- | -------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `batch-sync.sh`                           | Endpoint, bearer token, EC ID, partner UID; `curl` and Python              | Sends one authenticated batch-sync request. Its temporary response file is removed on exit.                                                                                             |
| `benchmark.sh`                            | A running server; `curl`, `bc`, and optionally `hey`                       | May install `hey` through Homebrew. `--save` writes under `benchmark-results/`; it does not stop the server.                                                                            |
| `profile.sh`                              | Fastly CLI, Rust WASM target, `curl`; endpoint and request options         | Builds and starts the Fastly app, stops the owned process, and retains a profile under `benchmark-results/profiles/`. `--open` launches the local viewer.                               |
| `generate-integration-viceroy-configs.sh` | Rust toolchain; optional origin port and artifact directory                | Builds the native generator and writes Viceroy config under `target/integration-test-artifacts/`; generated files persist.                                                              |
| `integration-tests.sh`                    | Docker, Viceroy, Rust WASM target, pinned Node                             | Builds WASM/native artifacts and two Docker images, generates Viceroy config, and runs native integration tests serially. Build products and images persist.                            |
| `integration-tests-browser.sh`            | The integration prerequisites plus npm and Playwright                      | Installs package/browser dependencies, builds fixtures and images, runs both browser suites, and stops matching test containers on exit. Build and npm artifacts persist.               |
| `smoke-axum.sh`                           | `cargo`, `curl`, Python                                                    | Uses an isolated temporary config/store and stub origin; stops owned processes and removes its workspace.                                                                               |
| `smoke-fastly.sh`                         | Fastly CLI, `cargo`, `curl`, Python                                        | Uses an isolated Fastly project and application config; stops owned processes and removes its workspace.                                                                                |
| `smoke-cloudflare.sh`                     | Pinned Wrangler, `cargo`, `curl`, `jq`, and Python                         | Uses isolated Wrangler manifests, KV state, ports, and logs; stops owned processes and removes its workspace.                                                                           |
| `smoke-spin.sh`                           | Spin CLI, `cargo`, `curl`, and Python                                      | Uses an isolated Spin manifest and SQLite KV store; stops owned processes and removes its workspace.                                                                                    |
| `smoke-common.sh`                         | Sourced by the four smoke scripts                                          | Defines bounded port, process, config, secret, and response assertions. Do not invoke it as a standalone smoke.                                                                         |
| `template-cache-local-test.sh`            | Viceroy, Node, OpenSSL, `curl`, `lsof`; optional ports and mode            | Builds local artifacts, generates certificates and fixtures in a temporary directory, stops owned servers, and removes the directory.                                                   |
| `test-cli.sh`                             | Rustup and the host toolchain; optional host triple                        | May install the selected Rust target, then runs native CLI and browser-audit tests. Cargo artifacts persist.                                                                            |
| `docs-proposal/propose.sh`                | Merge SHA on `main`, work dir, optional base SHA; Copilot CLI, Node, `npm` | Runs Copilot CLI to edit `docs/guide/**` or `docs/index.md`, formats docs, and writes a patch, rationale, and evidence into the work dir. Leaves the edits staged.                      |
| `docs-proposal/validate.sh`               | Merge SHA, work dir from `propose.sh`; Node, `npm`; no write credentials   | Applies the patch, rejects paths outside the allowlist, and runs the docs gates. Leaves the edits staged.                                                                               |
| `docs-proposal/publish.sh`                | Merge SHA, work dir from `propose.sh`; `gh` with write `GH_TOKEN`          | Re-checks the allowlist without building, pushes `docs/auto/<sha12>` unless a maintainer added to it, opens or edits its PR, and reviews each new commit per hunk. Never writes `main`. |
| `docs-proposal/test.sh`                   | Git and Node                                                               | Tests the helpers, validate flow, and publish flow against a temporary repository with stubbed `gh` and `npm`; removes it on exit.                                                      |

The four adapter smoke contracts are documented in the
[deployment guides](../docs/guide/integrations-overview.md#adapter-support).
Repository-wide verification policy lives in [TESTING.md](../TESTING.md).

The `Documentation proposal` workflow runs both proposal scripts after code
merges to `main` and opens one pull request per merge on `docs/auto/<sha12>`.
It needs the organization Copilot policy "Allow use of Copilot CLI billed to
the organization" and the repository setting "Allow GitHub Actions to create
and approve pull requests". Pull requests opened with `GITHUB_TOKEN` do not
trigger other workflows, so the workflow's validate job runs the docs gates
itself, without write credentials; push to the proposal branch to run regular
CI. Retry a merge from the workflow's manual dispatch with its full commit SHA;
the scripts always run from the workflow's revision, so any merge on `main` can
be retried.

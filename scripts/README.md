# Repository scripts

Run these scripts from the repository root. They fail on command errors unless
their documented orchestration handles a probe deliberately.

| Script                                    | Inputs and prerequisites                                           | Side effects and cleanup                                                                                                                                                  |
| ----------------------------------------- | ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `batch-sync.sh`                           | Endpoint, bearer token, EC ID, partner UID; `curl` and Python      | Sends one authenticated batch-sync request. Its temporary response file is removed on exit.                                                                               |
| `benchmark.sh`                            | A running server; `curl`, `bc`, and optionally `hey`               | May install `hey` through Homebrew. `--save` writes under `benchmark-results/`; it does not stop the server.                                                              |
| `profile.sh`                              | Fastly CLI, Rust WASM target, `curl`; endpoint and request options | Builds and starts the Fastly app, stops the owned process, and retains a profile under `benchmark-results/profiles/`. `--open` launches the local viewer.                 |
| `generate-integration-viceroy-configs.sh` | Rust toolchain; optional origin port and artifact directory        | Builds the native generator and writes Viceroy config under `target/integration-test-artifacts/`; generated files persist.                                                |
| `integration-tests.sh`                    | Docker, Viceroy, Rust WASM target, pinned Node                     | Builds WASM/native artifacts and two Docker images, generates Viceroy config, and runs native integration tests serially. Build products and images persist.              |
| `integration-tests-browser.sh`            | The integration prerequisites plus npm and Playwright              | Installs package/browser dependencies, builds fixtures and images, runs both browser suites, and stops matching test containers on exit. Build and npm artifacts persist. |
| `smoke-axum.sh`                           | `cargo`, `curl`, Python                                            | Uses an isolated temporary config/store and stub origin; stops owned processes and removes its workspace.                                                                 |
| `smoke-fastly.sh`                         | Fastly CLI, `cargo`, `curl`, Python                                | Temporarily updates local Fastly state, then restores `fastly.toml` and its EdgeZero lock exactly and removes its workspace.                                              |
| `smoke-cloudflare.sh`                     | Pinned Wrangler, `cargo`, `curl`, Python                           | Uses isolated Wrangler manifests, KV state, ports, and logs; stops owned processes and removes its workspace.                                                             |
| `smoke-spin.sh`                           | Pinned Spin CLI, `cargo`, `curl`, Python                           | Uses an isolated Spin manifest and SQLite KV store; stops owned processes and removes its workspace.                                                                      |
| `smoke-common.sh`                         | Sourced by the four smoke scripts                                  | Defines bounded port, process, config, secret, and response assertions. Do not invoke it as a standalone smoke.                                                           |
| `template-cache-local-test.sh`            | Viceroy, Node, OpenSSL, `curl`, `lsof`; optional ports and mode    | Builds local artifacts, generates certificates and fixtures in a temporary directory, stops owned servers, and removes the directory.                                     |
| `test-cli.sh`                             | Rustup and the host toolchain; optional host triple                | May install the selected Rust target, then runs native CLI and browser-audit tests. Cargo artifacts persist.                                                              |

The four adapter smoke contracts are documented in the
[deployment guides](../docs/guide/integrations-overview.md#adapter-support).
Repository-wide verification policy lives in [TESTING.md](../TESTING.md).

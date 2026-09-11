# Trusted Server

Trusted Server is an open-source publisher runtime for moving selected
advertising, identity, consent, and security work from third-party browser code
into a publisher-controlled edge service. A portable Rust core runs through
Fastly Compute, Cloudflare Workers, Fermyon Spin, or the native Axum development
adapter.

## Documentation

The [published guide](https://iabtechlab.github.io/trusted-server/) is the
reader-facing source of truth. Start with:

| Guide                                                                                                 | Purpose                                                            |
| ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| [Getting Started](https://iabtechlab.github.io/trusted-server/guide/getting-started)                  | Install prerequisites and create a working configuration.          |
| [Configuration](https://iabtechlab.github.io/trusted-server/guide/configuration)                      | Exact settings, defaults, validation, and secret handling.         |
| [CLI](https://iabtechlab.github.io/trusted-server/guide/cli)                                          | Host installation and the generated Linux/macOS command inventory. |
| [Deployment](https://iabtechlab.github.io/trusted-server/guide/integrations-overview#adapter-support) | Adapter support and first-success journeys.                        |
| [Integrations](https://iabtechlab.github.io/trusted-server/guide/integrations-overview)               | Server and browser capability inventory.                           |

## Fastest local proof

Install the Rust toolchain declared by `rust-toolchain.toml`; the smoke also
requires `curl` and Python. Then run:

```bash
./scripts/smoke-axum.sh
```

The script builds the host `ts` CLI and Axum adapter, creates isolated
configuration and secrets, proves the missing-config and missing-secret
failures, proxies a request through a local stub origin, and removes its
temporary processes and files. It does not require an edge account.

For another runtime, install the pinned tool from `.tool-versions` and use its
equivalent checked journey:

| Runtime            | Command                         | Guide                                  |
| ------------------ | ------------------------------- | -------------------------------------- |
| Fastly Compute     | `./scripts/smoke-fastly.sh`     | [Fastly](docs/guide/fastly.md)         |
| Cloudflare Workers | `./scripts/smoke-cloudflare.sh` | [Cloudflare](docs/guide/cloudflare.md) |
| Fermyon Spin       | `./scripts/smoke-spin.sh`       | [Spin](docs/guide/spin.md)             |

## Operator CLI

Install the native CLI and create a configuration from the repository root:

```bash
cargo install-cli
ts config init
ts config validate
```

Edit `trusted-server.toml` before validation. The generated template uses
intentional placeholders that fail closed until replaced. Read the
[CLI guide](docs/guide/cli.md) before pushing configuration or invoking a
platform lifecycle command.

## Development

This workspace has no global Cargo target because its packages span native,
`wasm32-wasip1`, and `wasm32-unknown-unknown`. Use the target-matched commands
in [TESTING.md](TESTING.md). Contribution, documentation, error-handling, and
commit conventions are in [CONTRIBUTING.md](CONTRIBUTING.md) and
[CLAUDE.md](CLAUDE.md).

## Governance and license

Project roles and decision responsibilities are described in
[ProjectGovernance.md](ProjectGovernance.md). Trusted Server is licensed under
the [Apache License 2.0](LICENSE).

# Trusted Server

Trusted Server is an open-source, cloud based orchestration framework and runtime for publishers. It moves code execution and operations that traditionally occurs in browsers (via 3rd party JS) to secure, zero-cold-start WASM binaries running in WASI supported environments.

Trusted Server is the new execution layer for the open-web, returning control of 1st party data, security, and overall user-experience back to publishers.


## Documentation

The guide in `docs/guide/` (published at the link below) is the source of truth for human-readable documentation. This README is a brief overview.

**[Read the full documentation →](https://iabtechlab.github.io/trusted-server/)**

| Guide                                                                                   | Description                                |
| --------------------------------------------------------------------------------------- | ------------------------------------------ |
| [Getting Started](https://iabtechlab.github.io/trusted-server/guide/getting-started)    | Installation and setup                     |
| [Architecture](https://iabtechlab.github.io/trusted-server/guide/architecture)          | System architecture overview               |
| [Configuration](https://iabtechlab.github.io/trusted-server/guide/configuration)        | Configuration reference                    |
| [Integrations](https://iabtechlab.github.io/trusted-server/guide/integrations-overview) | Partner integrations (Prebid, Lockr, etc.) |

## Quick Start

See the [Getting Started guide](https://iabtechlab.github.io/trusted-server/guide/getting-started) for installation and setup instructions.

```bash
# Build
cargo build

# Run tests
cargo test

# Start local server
fastly compute serve
```

## Development

```bash
# Format code
cargo fmt

# Lint
cargo clippy --all-targets --all-features --workspace --no-deps

# Run tests
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

This project is licensed under the Apache License 2.0 - see the [LICENSE](LICENSE) file for details.

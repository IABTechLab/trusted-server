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
# Create a starter config
cargo run --package trusted-server-cli --bin ts --target "$(rustc -vV | sed -n 's/^host: //p')" -- config init

# Validate local config
cargo run --package trusted-server-cli --bin ts --target "$(rustc -vV | sed -n 's/^host: //p')" -- config validate

# Start local Fastly development
cargo run --package trusted-server-cli --bin ts --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev -a fastly

# Audit a public page with a real Chromium browser
cargo run --package trusted-server-cli --bin ts --target "$(rustc -vV | sed -n 's/^host: //p')" -- audit https://example.com
```

## Development

```bash
# Format code
cargo fmt --all -- --check

# Lint runtime crates (wasm target)
cargo clippy --workspace --exclude trusted-server-cli --all-targets --all-features -- -D warnings

# Lint CLI crate (host target)
cargo clippy --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --all-targets -- -D warnings

# Run runtime crate tests (wasm target)
cargo test --workspace --exclude trusted-server-cli

# Run CLI tests (host target)
cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"
```

`ts audit` is host-only and currently expects a local Chrome/Chromium installation. It checks common PATH names and standard macOS app bundle locations.

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

This project is licensed under the Apache License 2.0 - see the [LICENSE](LICENSE) file for details.

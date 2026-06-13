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
# Build per adapter (target-matched aliases from .cargo/config.toml)
cargo build-fastly       # Fastly adapter + core (wasm32-wasip1)
cargo build-axum         # Axum dev server (native)
cargo build-cloudflare   # Cloudflare Workers (wasm32-unknown-unknown)

# Run tests (Fastly/WASM crates — requires Viceroy)
cargo test-fastly

# Run tests (Axum native adapter)
cargo test-axum

# Run tests (Cloudflare Workers adapter — native host)
cargo test-cloudflare

# Run tests (Spin adapter — native host)
cargo test-spin

# Start local server — Axum (no Fastly CLI or Viceroy required)
cargo run -p trusted-server-adapter-axum

# Start local server — Fastly (requires Fastly CLI + Viceroy)
fastly compute serve
```

## Development

```bash
# Format code
cargo fmt

# Lint — use target-matched aliases (workspace has multiple WASM runtimes;
# broad --all-features clippy is not a reliable gate across adapters)
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-spin-native
cargo clippy-spin-wasm

# Run all tests
cargo test-fastly      # Fastly/WASM (requires Viceroy)
cargo test-axum        # Axum native adapter
cargo test-cloudflare  # Cloudflare Workers adapter (native host)
cargo test-spin        # Spin adapter (native host)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

This project is licensed under the Apache License 2.0 - see the [LICENSE](LICENSE) file for details.

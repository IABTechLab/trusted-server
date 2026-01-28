# Onboarding Guide

Get up to speed with the Trusted Server project and start contributing effectively.

## Overview

Trusted Server is an **open-source, edge computing framework** developed by IAB Tech Lab that moves advertising operations from third-party JavaScript in browsers to secure WebAssembly (WASM) binaries running on edge platforms (currently Fastly Compute).

### The Problem It Solves

- **Privacy restrictions**: Browser privacy initiatives (3rd-party cookie deprecation, tracking prevention) limit traditional advertising
- **Third-party dependency**: Publishers have little control over third-party scripts on their pages
- **Performance**: Multiple third-party scripts slow down page load times
- **Data control**: Publishers need better control over data sharing

### Key Benefits

- **First-party context**: Serve ads and assets from the publisher's domain
- **Privacy compliance**: GDPR-compliant with built-in consent management
- **Better performance**: Server-side processing reduces client-side JavaScript
- **Data control**: Publishers control data sharing and user identification

::: warning
This is a **proof of concept (POC)** - not production-ready. The goal is to demonstrate technical feasibility and invite industry collaboration.
:::

## Architecture at a Glance

```
┌─────────────────────────────────────────────────────────────────────┐
│                         User's Browser                              │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      Fastly Edge (Trusted Server)                   │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │  Request Router (main.rs)                                    │    │
│  │  • Route matching                                            │    │
│  │  • Authentication                                            │    │
│  │  • Handler delegation                                        │    │
│  └─────────────────────────────────────────────────────────────┘    │
│                                  │                                   │
│         ┌────────────────────────┼────────────────────────┐         │
│         ▼                        ▼                        ▼         │
│  ┌─────────────┐         ┌─────────────┐         ┌─────────────┐   │
│  │   Proxy     │         │ Publisher   │         │ Integrations│   │
│  │  Handlers   │         │   Origin    │         │  (Prebid,   │   │
│  │             │         │   Handler   │         │   Lockr,    │   │
│  │ • /first-   │         │             │         │   etc.)     │   │
│  │   party/*   │         │ • Synthetic │         │             │   │
│  │ • Creative  │         │   ID inject │         │             │   │
│  │   rewriting │         │ • HTML      │         │             │   │
│  │             │         │   processing│         │             │   │
│  └─────────────┘         └─────────────┘         └─────────────┘   │
│                                                                      │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │  Storage Layer                                               │    │
│  │  • KV Stores (counters, domain mappings)                     │    │
│  │  • Config Stores (public keys, settings)                     │    │
│  │  • Secret Stores (private signing keys)                      │    │
│  └─────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                    ┌─────────────┴─────────────┐
                    ▼                           ▼
            ┌─────────────┐             ┌─────────────┐
            │  Publisher  │             │  Ad Server  │
            │   Origin    │             │  / Prebid   │
            └─────────────┘             └─────────────┘
```

### Technology Stack

| Layer          | Technology                  |
| -------------- | --------------------------- |
| Language       | Rust ({{RUST_VERSION}})     |
| Runtime        | WebAssembly (wasm32-wasip1) |
| Edge Platform  | Fastly Compute              |
| Client Library | TypeScript (TSJS)           |
| Build Tools    | Cargo, Vite                 |

## Development Environment Setup

### Prerequisites

Install the following tools using [asdf](https://asdf-vm.com/) for version management:

```bash
# Install asdf (macOS)
brew install asdf

# Add plugins
asdf plugin add rust
asdf plugin add nodejs
asdf plugin add fastly

# Install required versions (from .tool-versions)
asdf install rust {{RUST_VERSION}}
asdf install nodejs {{NODEJS_VERSION}}
asdf install fastly {{FASTLY_VERSION}}

# Reshim to ensure binaries are available
asdf reshim

# Add the WASM target for Fastly builds
rustup target add wasm32-wasip1
```

Add to your shell profile (`~/.zshrc` or `~/.bash_profile`):

```bash
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

Then restart your terminal or run `source ~/.zshrc`.

### Clone and Build

```bash
# Clone the repository
git clone git@github.com:IABTechLab/trusted-server.git
cd trusted-server

# Build the Rust project
cargo build

# Install viceroy (Fastly local simulator)
cargo install viceroy

# Run tests to verify setup
cargo test

# Build the TypeScript client library
cd crates/js/lib
npm install
npm run build
npm test
cd ../../..
```

### IDE Setup

We recommend **VS Code** with these extensions:

- rust-analyzer (Rust language support)
- Even Better TOML (TOML file support)
- CodeLLDB (debugging)

## Codebase Structure

```
trusted-server/
├── crates/                      # Rust workspace
│   ├── common/                  # Core library (shared code)
│   │   └── src/
│   │       ├── proxy.rs         # First-party proxy handlers
│   │       ├── publisher.rs     # Publisher origin handling
│   │       ├── creative.rs      # HTML/creative rewriting
│   │       ├── synthetic.rs     # Synthetic ID generation
│   │       ├── settings.rs      # Configuration management
│   │       ├── integrations/    # Partner integrations
│   │       │   ├── prebid.rs
│   │       │   ├── lockr.rs
│   │       │   └── ...
│   │       └── request_signing/ # Ed25519 signing
│   │
│   ├── fastly/                  # Fastly-specific implementation
│   │   └── src/
│   │       └── main.rs          # Entry point & routing
│   │
│   └── js/                      # TypeScript client library (TSJS)
│       └── src/
│
├── docs/                        # VitePress documentation
├── static/                      # Static assets
│
├── Cargo.toml                   # Workspace manifest
├── fastly.toml                  # Fastly service config
├── trusted-server.toml          # Application settings
├── rust-toolchain.toml          # Pinned Rust version
└── .tool-versions               # Tool versions (asdf)
```

### Key Files to Start With

| File                                         | Purpose                                   |
| -------------------------------------------- | ----------------------------------------- |
| `crates/fastly/src/main.rs`                  | Request routing entry point - start here! |
| `crates/common/src/publisher.rs`             | Publisher origin handling                 |
| `crates/common/src/proxy.rs`                 | First-party proxy implementation          |
| `crates/common/src/synthetic.rs`             | Synthetic ID generation                   |
| `crates/common/src/integrations/registry.rs` | Integration module pattern                |
| `trusted-server.toml`                        | Application configuration                 |

## Key Concepts

### First-Party Proxying

Instead of loading ad creatives from third-party domains, Trusted Server proxies them through first-party endpoints:

```
Before:  Browser → ad-server.com/creative.html
After:   Browser → publisher.com/first-party/proxy?tsurl=ad-server.com/creative.html
```

This keeps all requests under the publisher's domain, avoiding third-party cookie restrictions.

### Synthetic ID Generation

Privacy-preserving user identification using HMAC-SHA256:

```rust
// Configurable template combining signals
template: "{{ip}}-{{user_agent}}-{{secret}}"

// Produces deterministic, non-reversible ID
synthetic_id: "a1b2c3d4e5f6..."
```

The ID is:

- Deterministic (same inputs = same output)
- Non-reversible (can't extract original signals)
- Publisher-controlled (configurable template)

### Integration Modules

Extensible pattern for adding new partners:

```rust
pub struct PrebidIntegration { ... }

impl Integration for PrebidIntegration {
    fn handle_request(&self, req: Request) -> Response { ... }
}
```

### Request Signing

Ed25519 cryptographic signing for authenticated API requests:

- Public keys published at `/.well-known/trusted-server.json`
- Key rotation supported with graceful transitions
- Used for OpenRTB bid requests

## Development Workflow

### Building

```bash
# Development build
cargo build

# Production WASM build
cargo build --bin trusted-server-fastly --release --target wasm32-wasip1
```

### Running Locally

```bash
# Load environment variables
set -a
source .env.dev
set +a

# Use the shared origin for local testing
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://origin.getpurpose.ai

# Start local Fastly simulator
fastly compute serve

# Server runs at http://127.0.0.1:7676
```

### Local Origin Stub

For a fully local origin instead of `origin.getpurpose.ai`:

```bash
# Terminal 1: start a simple origin server
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:9090
mkdir -p /tmp/ts-origin
printf 'hello from origin\n' > /tmp/ts-origin/hello.txt
python3 -m http.server 9090 --directory /tmp/ts-origin
```

```bash
# Terminal 2: sign and proxy an asset
signed_path=$(
  curl -s "http://127.0.0.1:7676/first-party/sign?url=http://localhost:9090/hello.txt" \
  | python3 - <<'PY'
import json, sys
print(json.load(sys.stdin)["href"])
PY
)
curl -i "http://127.0.0.1:7676${signed_path}"
```

You should see `hello from origin` in the response body.

### Code Quality

Before committing, always run:

```bash
# Format code
cargo fmt

# Lint with clippy
cargo clippy --all-targets --all-features --workspace --no-deps

# Run tests
cargo test
```

### Making Changes

1. Create a feature branch from `main`
2. Make your changes
3. Run `cargo fmt`, `cargo clippy`, and `cargo test`
4. Commit following the guidelines in [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md)
5. Open a Pull Request

### Commit Message Format

Use sentence case and imperative mood. Do not use semantic prefixes (like `fix:` or `feat:`):

```
Short summary in 50 chars or less

Optional longer description explaining the "why"
not the "what" (the code shows that).

Resolves: #123
```

## Testing

### Running Tests

```bash
# Run all Rust tests
cargo test

# Run with more details
cargo test -- --nocapture

# Run TypeScript tests
cd crates/js/lib && npm test
```

::: warning
If a test fails, viceroy won't display the line number. Use `cargo test -- --nocapture` to see detailed output.
:::

### Writing Tests

Tests live alongside source code in `#[cfg(test)]` modules:

```rust
// src/synthetic.rs

pub fn generate_id(template: &str) -> String {
    // implementation
}

#[cfg(test)]
mod tests {
    use super::generate_id;

    #[test_log::test]
    fn generates_id_for_template() {
        let id = generate_id("test-template");
        assert!(!id.is_empty(), "should generate a non-empty ID");
    }
}
```

### Local Integration Testing

Use the Fastly local simulator to test full request flows:

```bash
fastly compute serve
# Then make requests to http://127.0.0.1:7676
```

## Common Tasks

### Adding a New Integration

1. Create a new file in `crates/common/src/integrations/`
2. Implement the integration trait
3. Register in `registry.rs`
4. Add configuration in `trusted-server.toml`
5. Write tests

See `crates/common/src/integrations/testlight.rs` for an example.

### Modifying Request Routing

Edit `crates/fastly/src/main.rs` to add new routes or modify existing ones.

### Updating Configuration

1. Add new settings to `crates/common/src/settings.rs`
2. Update `trusted-server.toml` with defaults
3. Document the new setting

### Deploying to Fastly

```bash
# Build and publish
fastly compute publish

# Or build separately
cargo build --bin trusted-server-fastly --release --target wasm32-wasip1
fastly compute publish --package pkg/trusted-server-fastly.tar.gz
```

## Debugging & Troubleshooting

### Viewing Logs

When running with `fastly compute serve`, logs print to stdout:

```rust
use log::{debug, error, info, warn};

info!("Processing request for path: {}", path);
debug!("Request headers: {:?}", headers);
```

Use `RUST_LOG=debug fastly compute serve` for verbose logging.

### Common Issues

| Issue                                 | Solution                                                |
| ------------------------------------- | ------------------------------------------------------- |
| `cargo test` fails with viceroy error | Run `cargo install viceroy`                             |
| `asdf` commands not found             | Ensure PATH is configured (see Prerequisites)           |
| `fastly compute serve` fails          | Check `.env.dev` exists and `fastly.toml` is configured |
| TypeScript build fails                | Run `npm install` in `crates/js/lib` first              |
| Tests pass locally but fail in CI     | Ensure `cargo fmt` and `cargo clippy` pass              |

## Team & Governance

The project follows IAB Tech Lab's open-source governance model:

- **Trusted Server Task Force**: Defines requirements and roadmap (meets biweekly)
- **Development Team**: Handles engineering implementation and releases

### Key Contacts

| Role         | GitHub Handle                                                |
| ------------ | ------------------------------------------------------------ |
| Project Lead | [@jevansnyc](https://github.com/jevansnyc)                   |
| Developer    | [@aram356](https://github.com/aram356)                       |
| Developer    | [@ChristianPavilonis](https://github.com/ChristianPavilonis) |

See [ProjectGovernance.md](https://github.com/IABTechLab/trusted-server/blob/main/ProjectGovernance.md) for full details.

## Resources & Getting Help

### Documentation

| Resource                                                                                  | Description             |
| ----------------------------------------------------------------------------------------- | ----------------------- |
| [README.md](https://github.com/IABTechLab/trusted-server/blob/main/README.md)             | Project overview        |
| [CONTRIBUTING.md](https://github.com/IABTechLab/trusted-server/blob/main/CONTRIBUTING.md) | Contribution guidelines |
| [AGENTS.md](https://github.com/IABTechLab/trusted-server/blob/main/AGENTS.md)             | AI assistant guidance   |
| [SEQUENCE.md](https://github.com/IABTechLab/trusted-server/blob/main/SEQUENCE.md)         | Request flow diagrams   |
| [FAQ_POC.md](https://github.com/IABTechLab/trusted-server/blob/main/FAQ_POC.md)           | FAQs                    |

### VitePress Documentation

Run the docs site locally:

```bash
cd docs
npm install
npm run dev
```

### Coding Standards

Review the standards in `.agents/rules/`:

- `rust-coding-style.mdc` - Naming, organization, patterns
- `rust-error-handling.mdc` - Error handling patterns
- `rust-testing-strategy.mdc` - Testing approach
- `git-commit-conventions.mdc` - Commit message format

### External Resources

- [Fastly Compute Documentation](https://developer.fastly.com/learning/compute/)
- [Rust Book](https://doc.rust-lang.org/book/)
- [WebAssembly Overview](https://webassembly.org/)
- [OpenRTB Specification](https://iabtechlab.com/standards/openrtb/)

## Onboarding Checklist

### Access & Accounts

- [ ] Get GitHub access to [IABTechLab/trusted-server](https://github.com/IABTechLab/trusted-server)
- [ ] Get access to the [Trusted Server project board](https://github.com/orgs/IABTechLab/projects/3)
- [ ] Create a [Fastly account](https://manage.fastly.com) and obtain an API token
- [ ] Join the Slack workspace and `#trusted-server-internal` channel
- [ ] Get calendar invites for Task Force and Development Team meetings

### Environment Setup

- [ ] Clone the repository and build successfully
- [ ] Run tests locally (`cargo test`)
- [ ] Start the local server (`fastly compute serve`)

### Codebase Exploration

- [ ] Read through `main.rs` to understand request routing
- [ ] Trace a request through `publisher.rs` and `proxy.rs`
- [ ] Understand synthetic ID generation in `synthetic.rs`
- [ ] Review an existing integration (e.g., `prebid.rs`)

### Documentation & Contribution

- [ ] Read `CONTRIBUTING.md` for PR guidelines
- [ ] Browse the documentation site guides
- [ ] Make a small contribution (fix a typo, add a test, etc.)

## Next Steps

- [What is Trusted Server?](/guide/what-is-trusted-server) - Understand the project vision
- [Architecture](/guide/architecture) - Deep dive into system design
- [Configuration](/guide/configuration) - Configure for your environment
- [Synthetic IDs](/guide/synthetic-ids) - Learn about privacy-preserving IDs
- [Integrations Overview](/guide/integrations-overview) - Explore partner integrations

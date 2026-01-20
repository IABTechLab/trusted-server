# New Engineer Onboarding Guide

Welcome to the Trusted Server project! This guide will help you get up to speed quickly and start contributing effectively.

## Table of Contents

1. [Project Overview](#project-overview)
2. [Architecture at a Glance](#architecture-at-a-glance)
3. [Development Environment Setup](#development-environment-setup)
4. [Codebase Structure](#codebase-structure)
5. [Key Concepts](#key-concepts)
6. [Development Workflow](#development-workflow)
7. [Testing](#testing)
8. [Common Tasks](#common-tasks)
9. [Debugging & Troubleshooting](#debugging--troubleshooting)
10. [Team & Governance](#team--governance)
11. [Resources & Getting Help](#resources--getting-help)
12. [Onboarding Checklist](#onboarding-checklist)

---

## Project Overview

### What is Trusted Server?

Trusted Server is an **open-source, edge computing framework** developed by IAB Tech Lab that moves advertising operations from third-party JavaScript in browsers to secure WebAssembly (WASM) binaries running on edge platforms (currently Fastly Compute).

### The Problem It Solves

- **Privacy restrictions**: Browser privacy initiatives (3rd-party cookie deprecation, tracking prevention) are limiting traditional advertising
- **Third-party dependency**: Publishers have little control over third-party scripts running on their pages
- **Performance**: Multiple third-party scripts slow down page load times
- **Data control**: Publishers need better control over how and with whom they share data

### Key Benefits

- **First-party context**: Serve ads and assets from the publisher's domain
- **Privacy compliance**: GDPR-compliant with built-in consent management
- **Better performance**: Server-side processing reduces client-side JavaScript
- **Data control**: Publishers control data sharing and user identification

### Current Status

This is a **proof of concept (POC)** - not production-ready. The goal is to demonstrate technical feasibility and invite industry collaboration to build toward an MVP.

---

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
| Language       | Rust (1.91.1)               |
| Runtime        | WebAssembly (wasm32-wasip1) |
| Edge Platform  | Fastly Compute              |
| Client Library | TypeScript (TSJS)           |
| Build Tools    | Cargo, Vite                 |

---

## Development Environment Setup

### Prerequisites

Install the following tools (we recommend using `asdf` for version management):

```bash
# Install asdf (if not already installed)
brew install asdf

# Add plugins
asdf plugin add rust
asdf plugin add nodejs
asdf plugin add fastly

# Install required versions (from .tool-versions)
asdf install rust 1.91.1
asdf install nodejs 24.10.0
asdf install fastly 13.1.0

# Reshim to ensure binaries are available
asdf reshim

# Add the WASM target for Fastly builds
rustup target add wasm32-wasip1
```

**Configure your shell PATH** (required for asdf to work):

For **Bash** (`~/.bash_profile`):

```bash
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

For **Zsh** (`~/.zshrc`):

```bash
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

Then restart your terminal or run `source ~/.zshrc` (or `~/.bash_profile`).

### Clone and Build

```bash
# Clone the repository
git clone git@github.com:IABTechLab/trusted-server.git
cd trusted-server

# Build the Rust project
cargo build

# Install viceroy (Fastly local simulator) for testing
cargo install viceroy

# Run Rust tests to verify setup
cargo test

# Build the TypeScript client library (TSJS)
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

---

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
│   │       │   ├── prebid.rs    # Prebid Server RTB
│   │       │   ├── lockr.rs     # Lockr ID solution
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

### Key Files to Understand First

| File                                                                                        | Purpose                                   |
| ------------------------------------------------------------------------------------------- | ----------------------------------------- |
| [crates/fastly/src/main.rs](../crates/fastly/src/main.rs)                                   | Request routing entry point - start here! |
| [crates/common/src/publisher.rs](../crates/common/src/publisher.rs)                         | Publisher origin handling                 |
| [crates/common/src/proxy.rs](../crates/common/src/proxy.rs)                                 | First-party proxy implementation          |
| [crates/common/src/synthetic.rs](../crates/common/src/synthetic.rs)                         | Synthetic ID generation                   |
| [crates/common/src/integrations/registry.rs](../crates/common/src/integrations/registry.rs) | Integration module pattern                |
| [trusted-server.toml](../trusted-server.toml)                                               | Application configuration                 |

---

## Key Concepts

### 1. First-Party Proxying

Instead of loading ad creatives directly from third-party domains, Trusted Server proxies them through first-party endpoints:

```
Before:  Browser → ad-server.com/creative.html
After:   Browser → publisher.com/first-party/proxy?tsurl=ad-server.com/creative.html
```

This keeps all requests under the publisher's domain, avoiding third-party cookie restrictions.

### 2. Synthetic ID Generation

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

### 3. Integration Modules

Extensible pattern for adding new partners (ad servers, ID solutions, consent providers):

```rust
// Each integration implements standard traits
pub struct PrebidIntegration { ... }

impl Integration for PrebidIntegration {
    fn handle_request(&self, req: Request) -> Response { ... }
}
```

### 4. Request Signing

Ed25519 cryptographic signing for authenticated API requests:

- Public keys published at `/.well-known/trusted-server.json`
- Key rotation supported with graceful transitions
- Used for OpenRTB bid requests

---

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

### Local Origin Stub and Smoke Test

If you want a fully local origin instead of `origin.getpurpose.ai`, set the origin URL to `http://localhost:9090` and use this stub to validate a first-party proxy flow end-to-end.

```bash
# Terminal 1: start a simple origin server on port 9090
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:9090
mkdir -p /tmp/ts-origin
printf 'hello from origin\n' > /tmp/ts-origin/hello.txt
python3 -m http.server 9090 --directory /tmp/ts-origin
```

```bash
# Terminal 2: with `fastly compute serve` running, sign and proxy the asset
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
4. Commit using the guidelines in [CONTRIBUTING.md](../CONTRIBUTING.md#memo-writing-commit-messages)
5. Open a Pull Request

### Commit Message Format

Follow the guidelines in [CONTRIBUTING.md](../CONTRIBUTING.md#memo-writing-commit-messages). In short:

- Use sentence case and imperative mood
- Do not use semantic prefixes or bracketed tags (examples: `fix:`, `[Docs]`)
- Keep PR state out of commit messages; use GitHub Draft PRs instead

```
Short summary in 50 chars or less

Optional longer description explaining the "why"
not the "what" (the code shows that).

Resolves: #123
```

---

## Testing

### Running Tests

```bash
# Run all Rust tests
cargo test

# Run with more details (useful when tests fail)
cargo test -- --nocapture

# Run TypeScript tests
cd crates/js/lib && npm test
```

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

Use the Fastly local simulator (viceroy) to test full request flows:

```bash
fastly compute serve
# Then make requests to http://127.0.0.1:7676
```

---

## Common Tasks

### Adding a New Integration

1. Create a new file in `crates/common/src/integrations/`
2. Implement the integration trait
3. Register in `registry.rs`
4. Add configuration in `trusted-server.toml`
5. Write tests

See [testlight.rs](../crates/common/src/integrations/testlight.rs) for an example.

### Modifying Request Routing

Edit [crates/fastly/src/main.rs](../crates/fastly/src/main.rs) to add new routes or modify existing ones.

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

---

## Debugging & Troubleshooting

### Viewing Logs Locally

When running with `fastly compute serve`, logs are printed to stdout. Use the `log` macros in Rust:

```rust
use log::{debug, error, info, warn};

info!("Processing request for path: {}", path);
debug!("Request headers: {:?}", headers);
```

### Common Issues

| Issue                                 | Solution                                                       |
| ------------------------------------- | -------------------------------------------------------------- |
| `cargo test` fails with viceroy error | Run `cargo install viceroy` to install/update the test runtime |
| `asdf` commands not found             | Ensure PATH is configured (see Prerequisites section)          |
| `fastly compute serve` fails          | Check that `.env.dev` exists and `fastly.toml` is configured   |
| TypeScript build fails                | Run `npm install` in `crates/js/lib` first                     |
| Tests pass locally but fail in CI     | Ensure `cargo fmt` and `cargo clippy` pass without warnings    |

### Debugging Tips

- Use `cargo test -- --nocapture` to see println/log output during tests
- For failed tests, viceroy doesn't show line numbers - check test output carefully
- Use `RUST_LOG=debug fastly compute serve` for verbose logging locally
- Check `.env.dev` for environment variable overrides

---

## Team & Governance

### Project Structure

The project follows IAB Tech Lab's open-source governance model:

- **Trusted Server Task Force**: Defines requirements and roadmap (meets biweekly)
- **Development Team**: Handles engineering implementation and releases

### Team Roles

| Role         | Responsibility                       |
| ------------ | ------------------------------------ |
| Project Lead | Overall project vision and direction |
| Developer    | Contributes code/docs                |

See [ProjectGovernance.md](../ProjectGovernance.md) for full details.

### Key Contacts

<!-- TODO: Update with current team members -->

| Role         | GitHub Handle                                                |
| ------------ | ------------------------------------------------------------ |
| Project Lead | [@jevansnyc](https://github.com/jevansnyc)                   |
| Developer    | [@aram356](https://github.com/aram356)                       |
| Developer    | [@ChristianPavilonis](https://github.com/ChristianPavilonis) |

### Meetings

<!-- TODO: Add actual meeting links and times -->

- **Task Force Meeting**: Biweekly (check calendar for schedule)
- **Development Team Standup**: Weekly (check calendar for schedule)

Ask your manager or onboarding buddy for calendar invites to relevant meetings.

---

## Resources & Getting Help

### Documentation

| Resource                              | Description                                   |
| ------------------------------------- | --------------------------------------------- |
| [README.md](../README.md)             | Project overview and setup                    |
| [CONTRIBUTING.md](../CONTRIBUTING.md) | Contribution guidelines                       |
| [AGENTS.md](../AGENTS.md)             | AI assistant guidance / architecture overview |
| [SEQUENCE.md](../SEQUENCE.md)         | Request flow diagrams                         |
| [FAQ_POC.md](../FAQ_POC.md)           | Frequently asked questions                    |

### VitePress Documentation Site

The `docs/` folder contains a full documentation site with detailed guides:

To run the docs site locally:

```bash
cd docs
npm install
npm run dev
```

See [docs/README.md](README.md) for deployment details.

| Guide                                           | Description                      |
| ----------------------------------------------- | -------------------------------- |
| [Getting Started](guide/getting-started.md)     | Quick start guide                |
| [Architecture](guide/architecture.md)           | System architecture overview     |
| [Configuration](guide/configuration.md)         | Configuration options            |
| [Synthetic IDs](guide/synthetic-ids.md)         | Privacy-preserving ID generation |
| [First-Party Proxy](guide/first-party-proxy.md) | Proxy endpoint documentation     |
| [Request Signing](guide/request-signing.md)     | Ed25519 signing setup            |
| [GDPR Compliance](guide/gdpr-compliance.md)     | Privacy and consent handling     |

### Integration Guides

| Integration                                  | Description                   |
| -------------------------------------------- | ----------------------------- |
| [Prebid](guide/integrations/prebid.md)       | Prebid Server RTB integration |
| [Lockr](guide/integrations/lockr.md)         | Lockr ID solution             |
| [Didomi](guide/integrations/didomi.md)       | Didomi consent management     |
| [Permutive](guide/integrations/permutive.md) | Permutive audience segments   |
| [Next.js](guide/integrations/nextjs.md)      | Next.js RSC integration       |
| [GAM](guide/integrations/gam.md)             | Google Ad Manager             |
| [APS](guide/integrations/aps.md)             | Amazon Publisher Services     |

### Coding Standards

Review the coding standards in `.agents/rules/`:

- `rust-coding-style.mdc` - Naming, organization, patterns
- `rust-error-handling.mdc` - Error handling patterns
- `rust-testing-strategy.mdc` - Testing approach
- `git-commit-conventions.mdc` - Commit message format

### Getting Help

- **GitHub Issues**: For bugs, feature requests, and questions
- **Task Force Meetings**: Biweekly meetings for roadmap discussions
- **Code Review**: Submit PRs for feedback from maintainers

### External Resources

- [Fastly Compute Documentation](https://developer.fastly.com/learning/compute/)
- [Rust Book](https://doc.rust-lang.org/book/)
- [WebAssembly Overview](https://webassembly.org/)
- [OpenRTB Specification](https://iabtechlab.com/standards/openrtb/)

---

## Onboarding Checklist

Use this checklist to track your onboarding progress:

### Access & Accounts

- [ ] Get GitHub access to [IABTechLab/trusted-server](https://github.com/IABTechLab/trusted-server)
- [ ] Get access to the [Trusted Server project board](https://github.com/orgs/IABTechLab/projects/3)
- [ ] Create a [Fastly account](https://manage.fastly.com) and obtain an API token
- [ ] Join the Slack workspace and `#trusted-server` channel
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
- [ ] Browse the [documentation site guides](guide/index.md)
- [ ] Make a small contribution (fix a typo, add a test, etc.)

---

Welcome aboard! Don't hesitate to ask questions - we're here to help you succeed.

# AGENTS.md

Centralized guidelines for AI coding agents working in this repository. Review the
common setup steps before executing tasks, then follow any assistant-specific
instructions.

## Common Setup

- Always read the full coding standards at the start of a session:

  ```bash
  cat .agents/rules/*
  ```

- Use `cargo fmt`, `cargo clippy`, and `cargo test` where appropriate before
  delivering code changes.
- When work touches Fastly behavior or runtime configuration, review
  `fastly.toml`, `trusted-server.toml`, and `.env.dev`.

## Claude Code

This section consolidates the guidance previously stored in `CLAUDE.md` for
Claude Code (claude.ai/code).

### Project Overview

Rust-based edge computing application targeting Fastly Compute. Handles
privacy-preserving synthetic ID generation, ad serving with GDPR compliance, and
real-time bidding integration.

### Key Commands

#### Build & Development

```bash
# Standard build
cargo build

# Production build for Fastly
cargo build --bin trusted-server-fastly --release --target wasm32-wasip1

# Run locally with Fastly simulator
fastly compute serve

# Deploy to Fastly
fastly compute publish
```

#### Testing & Quality

```bash
# Run tests (requires viceroy)
cargo test

# Install test runtime if needed
cargo install viceroy

# Format code
cargo fmt

# Lint with clippy
cargo clippy --all-targets --all-features

# Check compilation
cargo check
```

### Architecture Overview

#### Workspace Structure

- **trusted-server-common**: Core library with shared functionality
  - Synthetic ID generation (`src/synthetic.rs`)
  - Cookie handling (`src/cookies.rs`)
  - HTTP abstractions (`src/http_wrapper.rs`)
  - GDPR consent management (`src/gdpr.rs`)
- **trusted-server-fastly**: Fastly-specific implementation
  - Main application entry point
  - Fastly SDK integration
  - Request/response handling

### Key Design Patterns

1. **RequestWrapper Trait**: Abstracts HTTP request handling to support different
   backends.
2. **Settings-Driven Config**: External configuration via `trusted-server.toml`.
3. **Privacy-First**: All tracking requires GDPR consent checks.
4. **HMAC-Based IDs**: Synthetic IDs generated using HMAC with configurable templates.

### Configuration Files

- `fastly.toml`: Fastly service configuration and build settings.
- `trusted-server.toml`: Application settings (ad servers, KV stores, ID templates).
- `rust-toolchain.toml`: Pins Rust version to 1.90.0.

### Key Functionality Areas

1. Synthetic ID generation using HMAC-based templates.
2. Ad serving integrations (currently Equativ).
3. GDPR consent handling and validation.
4. Geolocation utilities (DMA code extraction).
5. Prebid integration for real-time bidding flows.
6. KV store usage for counters and domain mappings.

### Testing Approach

- Unit tests reside alongside source files under `#[cfg(test)]` modules.
- Uses Viceroy for local Fastly Compute simulation.
- GitHub Actions CI runs format and test workflows.

### Important Notes

- Target platform is WebAssembly (`wasm32-wasip1`).
- Relies on Fastly KV stores for persistence.
- Uses Handlebars templates for dynamic responses.
- Emits detailed logs for edge debugging.
- Follow conventional commit format (see `CONTRIBUTING.md`).

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust-based edge computing application designed for Fastly Compute. It handles privacy-preserving synthetic ID generation, ad serving with GDPR compliance, and real-time bidding integration.

## Key Commands

### Build & Development
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

### Testing & Quality
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

## Architecture Overview

### Workspace Structure
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
1. **RequestWrapper Trait**: Abstracts HTTP request handling to support different backends
2. **Settings-Driven Config**: External configuration via `trusted-server.toml`
3. **Privacy-First**: All tracking requires GDPR consent checks
4. **HMAC-Based IDs**: Synthetic IDs generated using HMAC with configurable templates

### Configuration Files
- `fastly.toml`: Fastly service configuration and build settings
- `trusted-server.toml`: Application settings (ad servers, KV stores, ID templates)
- `rust-toolchain.toml`: Pins Rust version to 1.87.0

### Key Functionality Areas
1. **Synthetic ID Generation**: Privacy-preserving user identification using HMAC
2. **Ad Serving**: Integration with ad partners (currently Equativ)
3. **GDPR Compliance**: Consent management via TCF strings
4. **Geolocation**: DMA code extraction for targeted advertising
5. **Prebid Integration**: Real-time bidding support
6. **KV Store Usage**: Persistent storage for counters and domain mappings

### Testing Approach
- Unit tests embedded in source files using `#[cfg(test)]` modules
- Uses Viceroy for local Fastly Compute simulation
- GitHub Actions CI with test and format workflows

### Important Notes
- Target platform is WebAssembly (wasm32-wasip1)
- Uses Fastly KV stores for persistence
- Handlebars templating for dynamic responses
- Comprehensive logging for edge debugging
- Follow conventional commits format (see CONTRIBUTING.md)

## Coding Standards Reference

**IMPORTANT: I must ALWAYS read the full coding standards at the beginning of EACH session using this command:**

```bash
cat .cursor/rules/*
```

The rules in `.cursor/rules/*` contain detailed guidelines for:

- Git commit conventions
- Rust coding style (type system, async patterns, function arguments, etc.)
- Documentation practices (structure, links, error documentation, etc.)
- Error handling with error-stack
- Testing strategy and assertions
- Tracing and instrumentation

These full guidelines contain critical nuances and details that cannot be summarized. Reading the complete rules is essential for ensuring code quality and consistency.

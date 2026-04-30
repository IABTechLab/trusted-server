# PR 10 Logging Initialization Design

**Goal:** Keep logging backend initialization adapter-owned so `trusted-server-core` remains platform-agnostic while Fastly continues to initialize its own `log-fastly` backend.

## Problem

`trusted-server-core` still declares a `log-fastly` dependency even though log
backend setup already happens in the Fastly adapter entrypoint. That keeps a
Fastly-specific crate in the core dependency graph and weakens the migration
boundary needed for future EdgeZero adapters such as Cloudflare, Spin, and
Axum.

## Design

### Responsibility split

- `trusted-server-core` emits logs only through `log` macros.
- Each adapter crate owns logger initialization and backend wiring.
- Fastly-specific logger setup moves behind an adapter-local module boundary.

This keeps core free of platform logging backends while establishing a clean
pattern future adapters can mirror without forcing a shared abstraction too
early.

### Fastly adapter shape

Create an adapter-local logging module in
`crates/trusted-server-adapter-fastly/src/logging.rs` that exposes a small
`init_logger()` function. The implementation stays Fastly-specific and can keep
using `log-fastly`, `fern`, and the existing formatting choices.

`crates/trusted-server-adapter-fastly/src/main.rs` should only import that
module and call `logging::init_logger()` during startup.

### Core crate shape

Remove `log-fastly` from
`crates/trusted-server-core/Cargo.toml`. No production code in core should
change unless compilation reveals an unexpected dependency. The intended end
state is that core depends on `log` only.

## File impact

- Create: `crates/trusted-server-adapter-fastly/src/logging.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-core/Cargo.toml`
- Modify: `Cargo.lock`

## Testing and verification

- Add or update small adapter-local tests only if needed for the extracted
  logging module.
- Run the standard project gates:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace`

## Out of scope

- Introducing a cross-adapter logging trait in core
- Changing log formatting semantics beyond what is needed to extract the module
- Adding logging implementations for non-Fastly adapters

## Acceptance

- `log-fastly` exists only in the Fastly adapter dependency graph
- Core uses `log` macros without any Fastly-specific logging backend dependency
- Fastly adapter still initializes logging at startup
- Workspace verification gates pass

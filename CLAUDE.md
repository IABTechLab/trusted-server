# CLAUDE.md

> Single source of truth for all AI coding agents (Claude Code, Codex, Cursor,
> etc.). If you're reading `AGENTS.md`, it redirects here.

## Project Overview

Rust-based edge computing application targeting **Fastly Compute**. Handles
privacy-preserving synthetic ID generation, ad serving with GDPR compliance,
real-time bidding integration, and publisher-side JavaScript injection.

## Workspace Layout

```
crates/
  common/        # Core library — shared logic, integrations, HTML processing
  fastly/        # Fastly Compute entry point (wasm32-wasip1 binary)
  js/            # TypeScript/JS build — per-integration IIFE bundles
    lib/         # TS source, Vitest tests, esbuild pipeline
```

Supporting files: `fastly.toml`, `trusted-server.toml`, `.env.dev`,
`rust-toolchain.toml`, `CONTRIBUTING.md`.

## Toolchain

| Tool        | Version / Target                         |
| ----------- | ---------------------------------------- |
| Rust        | 1.91.1 (pinned in `rust-toolchain.toml`) |
| WASM target | `wasm32-wasip1`                          |
| Node        | LTS (for JS build)                       |
| Viceroy     | Latest (Fastly local simulator)          |

---

## Build & Test Commands

### Rust

```bash
# Build
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
# Run all Rust tests (uses viceroy)
cargo test --workspace

# Format
cargo fmt --all -- --check

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Check compilation
cargo check

# JS tests
cd crates/js/lib && npx vitest run

# JS build
cd crates/js/lib && node build-all.mjs
```

### Install prerequisites

```bash
cargo install viceroy          # Fastly local test runtime
```

---

## Coding Conventions

### Rust Style

- Use the **2024 edition** of Rust.
- Prefer `derive_more` over manual trait implementations.
- Use `#[allow(lint)]` to suppress lints when necessary.
- Use `rustfmt` to format code.
- Invoke clippy with `--all-targets --all-features -- -D warnings`.
- Use `cargo doc --no-deps --all-features` for checking documentation.

### Type System

- Create strong types with **newtype patterns** for domain entities.
- Consider visibility carefully — avoid unnecessary `pub`.

```rust
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, derive_more::Display)]
pub struct UserId(Uuid);
```

### Function Arguments

- Functions should **never** take more than 7 arguments — use a struct instead.
- Take references for immutable access, mutable references for modification. Only take ownership when the function consumes the value.
- Prefer `&[T]` instead of `&Vec<T>`.
- Never use `impl Into<Option<_>>` — it hides that `None` can be passed.

### `From` / `Into`

- Prefer `From` implementations over `Into` — the compiler derives `Into` automatically.
- Prefer `Type::from(value)` over `value.into()` for clarity.
- For wrapper types (`Cow`, `Arc`, `Rc`, `Box`), use explicit constructors.

### Allocations

- Minimize allocations — reuse buffers, prefer borrowed data.
- Balance performance and readability.

### Naming Conventions

- Avoid abbreviations unless widely recognized (e.g., `Http`, `Json` — not `Ctx`).
- Do not suffix names with their types (`users` not `usersList`).

### Import Style

- No local imports within functions or blocks.
- `use super::*` is acceptable in `#[cfg(test)]` modules only.
- Never use a prelude `use crate::prelude::*`.
- Use `use Trait as _` when you only need trait methods, not the trait name.

### Comments and Assertions

- Place comments on separate lines **above** the code, never inline.
- All `expect()` messages should follow the format `"should ..."`.
- Use descriptive assertion messages: `assert_eq!(result, expected, "should match expected output")`.

### Crate Preferences

- `log` (with `log-fastly`) for instrumentation.

---

## Error Handling

- Use `error-stack` (`Report<MyError>`) — not anyhow or eyre.
- Use `Box<dyn Error>` only in tests or prototyping.
- Use concrete error types with `Report<E>`.
- Use `ensure!()` / `bail!()` macros for early returns.
- Import `Error` from `core::error::` instead of `std::error::`.
- Use `change_context()` to map error types, `attach()` / `attach_with()` for debug info.
- Define errors with `derive_more::Display` (not thiserror):

```rust
#[derive(Debug, derive_more::Display)]
pub enum MyError {
    #[display("Resource `{id}` not found")]
    NotFound { id: String },
    #[display("Operation timed out after {seconds}s")]
    Timeout { seconds: u64 },
}

impl core::error::Error for MyError {}
```

---

## Testing Strategy

- Unit tests live alongside source files under `#[cfg(test)]` modules.
- Uses **Viceroy** for local Fastly Compute simulation.
- GitHub Actions CI runs format and test workflows.
- Structure tests with **Arrange-Act-Assert** pattern.
- Test both happy paths and error conditions.
- Use `expect()` / `expect_err()` with `"should ..."` messages instead of `unwrap()`.
- Use `json!` macro instead of raw JSON strings.
- Follow the same code quality standards in test code as production code.
- For JS tests: use `vi.hoisted()` for mock definitions referenced in `vi.mock()` factories.

---

## Documentation Standards

- Each public item must have a doc comment.
- Begin with a single-line summary, blank line, then details.
- Always use intra-doc links (`[`Item`]`) for referenced types.
- Document errors with `# Errors` section for all fallible functions.
- Document panics with `# Panics` section.
- Add `# Examples` sections for public API functions.
- Add `# Performance` sections for performance-critical functions.
- Skip documentation for standard trait implementations unless behavior is unique.
- Use `cargo doc --no-deps --all-features` to verify.

---

## Logging Practices

- Use `log` crate level-specific macros: `log::info!`, `log::debug!`, `log::trace!`, `log::warn!`, `log::error!`.
- Provide context in log messages with format strings.
- Format messages with present-tense verbs.
- Use `log-fastly` as the backend for Fastly Compute.

---

## Git Commit Conventions

- Be descriptive and concise.
- Use sentence case (capitalize first word).
- Imperative, present-tense style.
- No semantic prefixes (`fix:`, `feat:`, `chore:`).
- No bracketed tags (`[Docs]`, `[Fix]`).
- Follow the detailed guidelines in `CONTRIBUTING.md`.

Good: `"Add feature flags to Real type tests that require serde"`
Bad: `"fix: added feature flags"`

---

## Integration System

Integrations register in Rust via:

```rust
IntegrationRegistration::builder(ID)
    .with_proxy()
    .with_attribute_rewriter()
    .with_head_injector()
    .build()
```

- Integration IDs match JS directory names: `prebid`, `lockr`, `permutive`, `datadome`, `didomi`, `testlight`.
- `creative` is JS-only (no Rust registration); `nextjs`, `aps`, `adserver_mock` are Rust-only.
- `IntegrationRegistry::js_module_ids()` maps registered integrations to JS module names.

## JS Build Pipeline

- `build-all.mjs` discovers `src/integrations/*/index.ts` and builds each as a separate IIFE.
- Output: `dist/tsjs-core.js`, `dist/tsjs-{integration}.js`.
- `build.rs` auto-generates `tsjs_modules.rs` with `include_str!()` for each discovered file.
- `bundle.rs` provides `concatenate_modules(ids)` and `concatenated_hash(ids)` APIs.
- Runtime: Rust server concatenates core + enabled integration JS files at request time.

---

## Configuration Files

| File                  | Purpose                                                    |
| --------------------- | ---------------------------------------------------------- |
| `fastly.toml`         | Fastly service configuration and build settings            |
| `trusted-server.toml` | Application settings (ad servers, KV stores, ID templates) |
| `rust-toolchain.toml` | Pins Rust version to 1.91.1                                |
| `.env.dev`            | Local development environment variables                    |

---

## CI Gates

Every PR must pass:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --workspace`
4. JS build and test (`cd crates/js/lib && npx vitest run`)

---

## Standard Workflow

1. **Read & plan** — understand the request, explore relevant code.
2. **Get approval** — for non-trivial changes, present a plan first.
3. **Implement incrementally** — small, testable changes. Every change should
   impact as little code as possible.
4. **Test after every change** — `cargo test --workspace`.
5. **Explain as you go** — describe what you changed and why.
6. **If blocked** — explain what's blocking and why.

## Verification & Quality

- **Verify, don't assume**: after implementing a change, prove it works. Run
  tests, check clippy, and compare behavior against `main` when relevant.
  Don't say "it works" without evidence.
- **Plan review**: for complex tasks, review your own plan as a staff engineer
  would before implementing. Ask: is this the simplest approach? Does it touch
  too many files? Are there edge cases?
- **Escape hatch**: if an implementation is going sideways after multiple
  iterations, step back and reconsider. Scrap the approach and implement the
  simpler solution rather than patching a flawed design.
- **Minimal changes**: every change should impact as little code as possible.
  Avoid unnecessary refactoring, docstrings on untouched code, or premature
  abstractions.

---

## Subagents

For complex tasks, use specialized subagents (`.claude/agents/`):

| Agent             | When to use                                                          |
| ----------------- | -------------------------------------------------------------------- |
| `build-validator` | Validate build across native + wasm32-wasip1 targets                 |
| `code-architect`  | Analyze architecture, suggest improvements                           |
| `code-simplifier` | Find and simplify overly complex code                                |
| `verify-app`      | Full verification pipeline (build + test + lint)                     |
| `pr-creator`      | Create well-structured pull requests                                 |
| `pr-reviewer`     | Staff-engineer PR review with inline GitHub comments (user approves) |
| `issue-creator`   | Create GitHub issues with proper types via GraphQL                   |
| `repo-explorer`   | Explore and answer questions about the codebase                      |

### Multi-Phase Workflow (for complex tasks)

1. **Phase 1 — Investigation** (read-only): launch subagents with
   non-overlapping scopes. Each must return concrete findings with file paths.
2. **Phase 2 — Solution proposals** (no edits): propose minimal fix strategies.
   Compare tradeoffs before coding.
3. **Phase 3 — Implementation**: pick one plan (smallest safe change) and
   implement centrally.
4. **Phase 4 — Verification**: run `verify-app` or `build-validator`.
5. **Phase 5 — Ship**: use `pr-creator` to create the PR.

**Default trigger**: use this workflow when work touches 2+ crates or includes
both runtime behavior and build/tooling changes.

### Selection Matrix

| Situation            | Use first         | Optional follow-up                  | Expected output                    |
| -------------------- | ----------------- | ----------------------------------- | ---------------------------------- |
| Unfamiliar code area | `repo-explorer`   | `code-architect`                    | File map and risk hotspots         |
| Multi-crate change   | `repo-explorer`   | `code-architect`, `build-validator` | Change plan and validation scope   |
| CI/build failures    | `build-validator` | `repo-explorer`                     | Failing combos and fault area      |
| Design/API proposal  | `code-architect`  | `repo-explorer`                     | Architecture concerns and options  |
| Cleanup/refactor     | `code-simplifier` | `build-validator`                   | Simplification summary and checks  |
| Pre-PR readiness     | `build-validator` | `verify-app`, `pr-creator`          | Pass/fail report and PR draft      |
| PR review            | `pr-reviewer`     | `code-architect`, `repo-explorer`   | Inline GitHub review with findings |

---

## Slash Commands

| Command           | Purpose                                   |
| ----------------- | ----------------------------------------- |
| `/test-all`       | Run full test suite (Rust + JS)           |
| `/check-ci`       | Run all CI checks locally                 |
| `/verify`         | Full verification: build, test, lint      |
| `/review-changes` | Review staged/unstaged changes for issues |
| `/test-crate`     | Test a specific crate by name             |

---

## Key Files

| File                                         | Purpose                                           |
| -------------------------------------------- | ------------------------------------------------- |
| `crates/common/src/integrations/registry.rs` | IntegrationRegistry, `js_module_ids()`            |
| `crates/common/src/tsjs.rs`                  | Script tag generation with module IDs             |
| `crates/common/src/html_processor.rs`        | Injects `<script>` at `<head>` start              |
| `crates/common/src/publisher.rs`             | `/static/tsjs=` handler, concatenates modules     |
| `crates/common/src/synthetic.rs`             | Synthetic ID generation                           |
| `crates/common/src/cookies.rs`               | Cookie handling                                   |
| `crates/common/src/gdpr.rs`                  | GDPR consent management                           |
| `crates/common/src/http_wrapper.rs`          | HTTP abstractions                                 |
| `crates/js/build.rs`                         | Discovers dist files, generates `tsjs_modules.rs` |
| `crates/js/src/bundle.rs`                    | Module map, concatenation, hashing                |

---

## What NOT to Do

- Do not add unnecessary dependencies without justification.
- Do not use `println!` / `eprintln!` — use `log` macros.
- Do not use `unwrap()` in production code — use `expect("should ...")`.
- Do not use thiserror — use `derive_more::Display` + `impl Error`.
- Do not use wildcard imports (except `use super::*` in test modules).
- Do not commit `.env` files or secrets.
- Do not make large refactors without approval.
- Always run tests and linting before committing.

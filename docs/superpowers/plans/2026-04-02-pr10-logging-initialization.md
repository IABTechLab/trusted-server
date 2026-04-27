# PR 10 Logging Initialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep logging backend initialization adapter-owned by extracting Fastly logging setup into an adapter-local module and removing `log-fastly` from `trusted-server-core`.

**Architecture:** `trusted-server-core` continues to emit logs only through `log` macros and has no platform logging backend dependency. `trusted-server-adapter-fastly` owns Fastly-specific logger initialization behind a local `logging.rs` module, and `main.rs` just calls into that adapter-local entrypoint.

**Tech Stack:** Rust 2024 edition conventions, `log`, `log-fastly`, `fern`, `chrono`

---

## File Structure

- Create: `crates/trusted-server-adapter-fastly/src/logging.rs`
  - Own Fastly-specific logger setup and any small formatting helpers needed for unit testing.
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
  - Stop carrying logger implementation details directly; import the adapter-local module and call `logging::init_logger()`.
- Modify: `crates/trusted-server-core/Cargo.toml`
  - Remove `log-fastly` from core dependencies.
- Modify: `Cargo.lock`
  - Lockfile update after dependency removal.

The plan intentionally avoids any core logging trait or shared abstraction. Future adapters can mirror the same adapter-local module shape without forcing a premature common interface.

## Tasks

### Task 1: Extract Fastly logger helper and initializer into an adapter-local module

**Files:**

- Create: `crates/trusted-server-adapter-fastly/src/logging.rs`

- [ ] **Step 1: Write a failing unit test for a non-allocating formatting helper**

Create `crates/trusted-server-adapter-fastly/src/logging.rs` with a test-first skeleton. Add a helper test for the target-label extraction logic without trying to install a global logger:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_label_uses_last_target_segment() {
        assert_eq!(
            target_label("trusted_server_adapter_fastly::proxy"),
            "proxy",
            "should use the final target segment"
        );
    }
}
```

Also add a production skeleton so the file compiles but the test fails:

```rust
fn target_label(target: &str) -> &str {
    target
}
```

- [ ] **Step 2: Run the adapter test to verify it fails**

Run:

```bash
cargo test --package trusted-server-adapter-fastly logging -- --nocapture
```

Expected: FAIL because `target_label()` returns the full target instead of the final segment.

- [ ] **Step 3: Implement the minimal helper and adapter logger initializer**

Replace the skeleton with the real adapter-local module:

```rust
use chrono::{SecondsFormat, Utc};
use log_fastly::Logger;

fn target_label(target: &str) -> &str {
    match target.rsplit_once("::") {
        Some((head, "")) => head,
        Some((_, last)) => last,
        None => target,
    }
}

pub(crate) fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Info)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                record.level(),
                target_label(record.target()),
                message
            ));
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("should initialize logger");
}
```

Keep the logic semantically equivalent to the current `main.rs` formatting and avoid introducing a new heap allocation on each log call.

- [ ] **Step 4: Run the adapter test to verify it passes**

Run:

```bash
cargo test --package trusted-server-adapter-fastly logging -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit the extracted logging module**

```bash
git add crates/trusted-server-adapter-fastly/src/logging.rs
git commit -m "Extract Fastly logging initialization into adapter module"
```

---

### Task 2: Wire `main.rs` to the adapter-local logging module

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Write a failing compile-time integration step for the new module wiring**

Update `main.rs` to reference `logging::init_logger()` before the module exists in the file:

```rust
mod logging;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    logging::init_logger();
    // ...
}
```

Delete the old inline `init_logger()` function and remove imports that only it used:

- `use log_fastly::Logger;`
- any `chrono`/`fern` imports that are no longer needed in `main.rs`

- [ ] **Step 2: Run the adapter package tests to verify the extraction is wired correctly**

Run:

```bash
cargo test --package trusted-server-adapter-fastly -- --nocapture
```

Expected: PASS. If compilation fails, fix the module imports and remaining references in `main.rs`.

- [ ] **Step 3: Commit the adapter wiring cleanup**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Wire Fastly main.rs to adapter-local logging module"
```

---

### Task 3: Remove `log-fastly` from core

**Files:**

- Modify: `crates/trusted-server-core/Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Verify core does not reference `log-fastly` directly**

Run:

```bash
rg -n "log_fastly|Logger::builder|Logger::from_env" crates/trusted-server-core
```

Expected: no matches.

- [ ] **Step 2: Remove `log-fastly` from core dependencies**

In `crates/trusted-server-core/Cargo.toml`, delete:

```toml
log-fastly = { workspace = true }
```

Do not remove `log = { workspace = true }`.

- [ ] **Step 3: Update the lockfile**

Run:

```bash
cargo test --package trusted-server-core --lib --no-run
```

Expected: `Cargo.lock` updates only as needed for the dependency graph while core still compiles.

- [ ] **Step 4: Confirm `log-fastly` remains adapter-only**

Run:

```bash
rg -n "log-fastly" crates
```

Expected: match only in `crates/trusted-server-adapter-fastly/Cargo.toml`.

- [ ] **Step 5: Commit the dependency cleanup**

```bash
git add crates/trusted-server-core/Cargo.toml Cargo.lock
git commit -m "Remove log-fastly from trusted-server-core"
```

---

### Task 4: Run project verification gates

**Files:**

- Verify the whole workspace after the logging extraction and dependency cleanup

- [ ] **Step 1: Format check**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS. If it fails, run `cargo fmt --all` and re-run the check.

- [ ] **Step 2: Clippy**

Run:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 3: Full workspace tests**

Run:

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 4: Commit any formatting fallout**

Only if `cargo fmt --all` changed files:

```bash
git add -A
git commit -m "Fix formatting after logging extraction"
```

---

## Acceptance Checklist

- [ ] `crates/trusted-server-adapter-fastly/src/logging.rs` exists
- [ ] `crates/trusted-server-adapter-fastly/src/main.rs` no longer contains the inline Fastly logger implementation
- [ ] `crates/trusted-server-core/Cargo.toml` no longer depends on `log-fastly`
- [ ] `rg -n "log-fastly" crates` reports only the Fastly adapter crate
- [ ] `trusted-server-core` still uses `log` macros and compiles without a Fastly-specific logging backend dependency
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --workspace` passes

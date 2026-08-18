# Pre-navigation Cookie Installation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow domain/path-scoped operator cookies to be installed before the audit's first navigation.

**Architecture:** Add one browser-level cookie installation helper beside `host_cookie`, and call it before creating each audit page. Preserve explicit host-only and root-path scope while avoiding `Page::set_cookie`'s `about:blank` validation.

**Tech Stack:** Rust, chromiumoxide/CDP, Tokio, Cargo tests

---

### Task 1: Reproduce the pre-navigation failure

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`

- [ ] Add a Chrome-backed test that installs a host-only cookie before navigating away from `about:blank` and asserts it reaches the first document.
- [ ] Exercise the existing `BrowserCollector` end-to-end against a local HTTP fixture, supplying the cookie through `BrowserCollectRequest`, so the RED test compiles before the fix exists.
- [ ] Run `cargo test_cli_macos commands::audit::browser::tests::supplied_cookie_reaches_first_navigation -- --ignored --exact --nocapture` and confirm it fails with `Blank page can not have cookie`.
- [ ] Add a Chrome-backed error test against the wished-for `set_browser_cookies` API, using an invalid cookie name, and assert the error contains the name but not the secret value.
- [ ] Run that error test and confirm RED because `set_browser_cookies` does not exist yet.

### Task 2: Install cookies at browser scope

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`

- [ ] Add `set_browser_cookies(&Browser, &[(String, String)], &Url) -> Result<(), String>` beside `host_cookie`; install one cookie per browser call so failures retain name-only context without exposing values.
- [ ] Invoke it before page creation in both collectors and remove page-level cookie installation.
- [ ] Run `cargo test_cli_macos commands::audit::browser::tests::supplied_cookie_reaches_first_navigation -- --ignored --exact --nocapture` and confirm it passes.
- [ ] Run the focused error test and confirm it passes.

### Task 3: Verify the change

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`

- [ ] Run `cargo test_cli_macos commands::audit::browser::tests`.
- [ ] Run `cargo test_cli_macos commands::audit::generate::browser_collector::tests`.
- [ ] Run `./scripts/test-cli.sh` to exercise the portable host-target suite and ignored browser fixtures.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets -- -D warnings`.
- [ ] Inspect the diff to confirm no cookie values are logged and `fastly.toml` remains untouched.

# Trusted Server: `x-ts-version` = git version, `x-ts-fastly-version` = Fastly version — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Responses carry `x-ts-version` = the deployed git tag, else branch, else
6-char commit hash. The Fastly service version moves to `x-ts-fastly-version`.

**Architecture:** `trusted-server-core/build.rs` compiles the version in as
`TS_GIT_VERSION`. It takes the value from the build-time env
`TRUSTED_SERVER_GIT_VERSION` (set by the deploy pipeline), or falls back to local `git`.
The choice itself is a pure function in `build_support/git_version.rs`, shared by
`build.rs` and an integration test through `#[path]`. A core module
`version_header` owns the header value and the write. Every adapter's
`apply_finalize_headers` calls it before operator `response_headers` are
applied, and the Fastly `/health` fast path sets it directly.

**Tech Stack:** Rust 2024, Cargo build scripts, `edgezero_core::http`, `fastly` 0.12, Viceroy (`cargo test-fastly`).

**Spec:** [`docs/superpowers/specs/2026-09-25-git-version-header-design.md`](../specs/2026-09-25-git-version-header-design.md)

## Global Constraints

- Build env var: exactly `TRUSTED_SERVER_GIT_VERSION`. Compiled-in rustc env: exactly `TS_GIT_VERSION`.
- Resolution order: usable env override → exact tag → branch → first **6** chars of the commit → none.
- Usable = trimmed, non-empty, and only bytes `0x21`–`0x7E`. An unusable override prints a `cargo:warning` and falls back to local git.
- No version known: **omit** `x-ts-version`. Never emit `unknown`.
- `build.rs` must never fail the build over version metadata.
- `build.rs` must print `cargo:rerun-if-env-changed=TRUSTED_SERVER_GIT_VERSION`, and `rerun-if-changed` only for git paths that exist (`<git-dir>/HEAD`, the current branch's ref file under `<common-dir>`, `<common-dir>/refs/tags`, `<common-dir>/packed-refs`).
- Header names: `x-ts-version` (git), `x-ts-fastly-version` (the `FASTLY_SERVICE_VERSION` value). Fastly `/health` gets `x-ts-version` only.
- Operator `settings.response_headers` still apply last.
- Test messages use the repo's `"should …"` style. Run the relevant `cargo test-*` / `cargo clippy-*` alias per task.
- Commits: sentence case, imperative, no prefixes.

---

### Task 1: Version resolution in `build.rs`

**Files:**

- Create: `crates/trusted-server-core/build_support/git_version.rs`
- Modify: `crates/trusted-server-core/build.rs`
- Test: `crates/trusted-server-core/tests/git_version_resolve.rs`

**Interfaces:**

- Produces: `git_version::is_usable(value: &str) -> bool` and
  `git_version::resolve_git_version(candidates: &Candidates<'_>) -> Option<String>`,
  where `Candidates { override_value, exact_tag, branch, commit }` are all `Option<&str>`.
- Produces: the compile-time env `TS_GIT_VERSION`, set only when a version is known.

- [ ] **Step 1: Write the failing test**

`crates/trusted-server-core/tests/git_version_resolve.rs`:

```rust
//! The version-resolution rule `build.rs` uses for `TS_GIT_VERSION`.

#[path = "../build_support/git_version.rs"]
mod git_version;

use git_version::{Candidates, resolve_git_version};

const COMMIT: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn local(exact_tag: Option<&'static str>, branch: Option<&'static str>) -> Candidates<'static> {
    Candidates {
        override_value: None,
        exact_tag,
        branch,
        commit: Some(COMMIT),
    }
}

#[test]
fn override_wins_over_local_git() {
    let candidates = Candidates {
        override_value: Some("v1.2.3"),
        ..local(Some("v9.9.9"), Some("main"))
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("v1.2.3"),
        "should prefer the pipeline-supplied TRUSTED_SERVER_GIT_VERSION"
    );
}

#[test]
fn override_is_trimmed() {
    let candidates = Candidates {
        override_value: Some("  feature/x\n"),
        ..local(None, None)
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("feature/x"),
        "should trim surrounding whitespace from the override"
    );
}

#[test]
fn tag_wins_over_branch() {
    assert_eq!(
        resolve_git_version(&local(Some("v1.2.3"), Some("main"))).as_deref(),
        Some("v1.2.3"),
        "should prefer an exact tag over the branch"
    );
}

#[test]
fn branch_when_no_tag() {
    assert_eq!(
        resolve_git_version(&local(None, Some("feature/x"))).as_deref(),
        Some("feature/x"),
        "should use the branch when HEAD is not tagged"
    );
}

#[test]
fn six_char_hash_when_no_tag_or_branch() {
    assert_eq!(
        resolve_git_version(&local(None, None)).as_deref(),
        Some("abcdef"),
        "should use exactly the first 6 characters of the commit"
    );
}

#[test]
fn blank_candidates_are_skipped() {
    let candidates = Candidates {
        override_value: Some("  "),
        exact_tag: Some(""),
        branch: Some(" "),
        commit: Some(COMMIT),
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("abcdef"),
        "should skip empty or whitespace-only candidates"
    );
}

#[test]
fn non_ascii_override_falls_back_to_local_git() {
    let candidates = Candidates {
        override_value: Some("v1-été"),
        ..local(None, None)
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("abcdef"),
        "should skip a non-ASCII override and fall back to local git"
    );
}

#[test]
fn override_with_inner_space_falls_back_to_local_git() {
    let candidates = Candidates {
        override_value: Some("v1 2"),
        ..local(None, Some("main"))
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("main"),
        "should skip an override containing a space"
    );
}

#[test]
fn none_when_nothing_is_known() {
    let candidates = Candidates {
        override_value: None,
        exact_tag: None,
        branch: None,
        commit: None,
    };
    assert_eq!(
        resolve_git_version(&candidates),
        None,
        "should leave the version unset so the header is omitted"
    );
}

#[test]
fn is_usable_accepts_only_visible_ascii() {
    assert!(git_version::is_usable("v1.2.3"), "should accept a tag");
    assert!(git_version::is_usable(" main "), "should accept after trimming");
    assert!(!git_version::is_usable(""), "should reject empty");
    assert!(!git_version::is_usable("a\tb"), "should reject a control character");
    assert!(!git_version::is_usable("v1-été"), "should reject non-ASCII");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p trusted-server-core --test git_version_resolve --target wasm32-wasip1`
Expected: FAIL to compile, with `couldn't read …/build_support/git_version.rs`.

- [ ] **Step 3: Implement the pure function**

`crates/trusted-server-core/build_support/git_version.rs`:

```rust
//! Resolution rule for the deployed git version reported in `x-ts-version`.
//!
//! Shared by `build.rs` and `tests/git_version_resolve.rs` via `#[path]`, so it
//! must stay dependency-free.

/// Raw candidates for the deployed git version, in priority order.
pub struct Candidates<'a> {
    /// `TRUSTED_SERVER_GIT_VERSION`, supplied by the deploy pipeline.
    pub override_value: Option<&'a str>,
    /// `git describe --tags --exact-match`.
    pub exact_tag: Option<&'a str>,
    /// `git symbolic-ref --short -q HEAD`.
    pub branch: Option<&'a str>,
    /// `git rev-parse HEAD`.
    pub commit: Option<&'a str>,
}

/// Whether `value`, once trimmed, is non-empty visible ASCII (`0x21`–`0x7E`).
///
/// Stricter than git, which also allows non-ASCII UTF-8 in ref names. Such a
/// ref is skipped so every compiled-in value is a `to_str()`-able header value.
pub fn is_usable(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

/// Picks the first usable candidate: override, tag, branch, then the first 6
/// characters of the commit. `None` when nothing usable is known.
pub fn resolve_git_version(candidates: &Candidates<'_>) -> Option<String> {
    let usable = |value: Option<&str>| value.filter(|v| is_usable(v)).map(str::trim);

    usable(candidates.override_value)
        .or_else(|| usable(candidates.exact_tag))
        .or_else(|| usable(candidates.branch))
        .map(str::to_owned)
        .or_else(|| usable(candidates.commit).map(|c| c.chars().take(6).collect()))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p trusted-server-core --test git_version_resolve --target wasm32-wasip1`
Expected: 10 passed.

- [ ] **Step 5: Wire up `build.rs`**

Replace `crates/trusted-server-core/build.rs` with:

```rust
//! Compiles the deployed git version into `trusted-server-core` as `TS_GIT_VERSION`.

#[path = "build_support/git_version.rs"]
mod git_version;

use std::path::Path;
use std::process::Command;

use git_version::{Candidates, is_usable, resolve_git_version};

/// Set by the deploy pipeline. Its CI checkout is shallow and detached, so local git
/// cannot see the tag or branch there.
const OVERRIDE_ENV: &str = "TRUSTED_SERVER_GIT_VERSION";

/// Runs `git` in the crate directory; `None` if git is missing or fails.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Re-runs this script when `path` changes. Skips missing paths: Cargo treats
/// them as always changed, which would rebuild core on every build.
fn rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Resolves the version from local git, watching the paths that move with it.
fn resolve_from_local_git() -> Option<String> {
    // HEAD is per-worktree; refs are shared in the common dir. They differ in a
    // linked worktree and coincide in a plain clone.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        rerun_if_exists(&Path::new(&git_dir).join("HEAD"));
    }
    if let Some(common_dir) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common_dir = Path::new(&common_dir);
        if let Some(branch_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            rerun_if_exists(&common_dir.join(branch_ref));
        }
        rerun_if_exists(&common_dir.join("refs/tags"));
        rerun_if_exists(&common_dir.join("packed-refs"));
    }

    let exact_tag = git(&["describe", "--tags", "--exact-match"]);
    let branch = git(&["symbolic-ref", "--short", "-q", "HEAD"]);
    let commit = git(&["rev-parse", "HEAD"]);
    resolve_git_version(&Candidates {
        override_value: None,
        exact_tag: exact_tag.as_deref(),
        branch: branch.as_deref(),
        commit: commit.as_deref(),
    })
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/git_version.rs");
    println!("cargo:rerun-if-env-changed={OVERRIDE_ENV}");

    let override_value = std::env::var(OVERRIDE_ENV).ok();
    let resolved = match override_value.as_deref() {
        Some(value) if is_usable(value) => resolve_git_version(&Candidates {
            override_value: Some(value),
            exact_tag: None,
            branch: None,
            commit: None,
        }),
        Some(value) => {
            if !value.trim().is_empty() {
                println!(
                    "cargo:warning={OVERRIDE_ENV}={value:?} is not visible ASCII; \
                     falling back to local git for x-ts-version"
                );
            }
            resolve_from_local_git()
        }
        None => resolve_from_local_git(),
    };

    if let Some(version) = resolved {
        println!("cargo:rustc-env=TS_GIT_VERSION={version}");
    }
}
```

- [ ] **Step 6: Verify the override reaches the compile and re-runs on change**

Run:
`TRUSTED_SERVER_GIT_VERSION=v0.0.0-check cargo build -p trusted-server-core --target wasm32-wasip1 -vv 2>&1 | grep -F 'TS_GIT_VERSION=v0.0.0-check'`
Expected: one matching `cargo:rustc-env=TS_GIT_VERSION=v0.0.0-check` line.

Run the same command again with `v0.0.0-check2`. Expected: the build script
re-runs and prints `…check2`.

Run: `cargo build -p trusted-server-core --target wasm32-wasip1 -vv 2>&1 | grep -F 'cargo:rerun-if-changed='`
Expected: `…/.git/worktrees/<name>/HEAD` and `…/.git/refs` (when in a linked
worktree), no `packed-refs` line unless that file exists.

- [ ] **Step 7: Clippy and commit**

Run: `cargo clippy-fastly`
Expected: no warnings.

```bash
git add crates/trusted-server-core/build.rs crates/trusted-server-core/build_support/git_version.rs crates/trusted-server-core/tests/git_version_resolve.rs
git commit --signoff -S -m "Compile deployed git version into trusted-server-core as TS_GIT_VERSION"
```

---

### Task 2: Core constants and the `version_header` module

**Files:**

- Modify: `crates/trusted-server-core/src/constants.rs` (the `// Staging / version identification headers` block)
- Create: `crates/trusted-server-core/src/version_header.rs`
- Modify: `crates/trusted-server-core/src/lib.rs` (add `pub mod version_header;` after `pub mod tsjs;`)

**Interfaces:**

- Consumes: `TS_GIT_VERSION` compile-time env (Task 1).
- Produces: `constants::HEADER_X_TS_FASTLY_VERSION: HeaderName`, `constants::TS_GIT_VERSION: Option<&'static str>`.
- Produces: `version_header::git_version_header_value() -> Option<HeaderValue>`,
  `version_header::header_value_from(version: Option<&str>) -> Option<HeaderValue>`,
  `version_header::apply_git_version_header(response: &mut Response)`,
  `version_header::apply_git_version_header_from(version: Option<&str>, response: &mut Response)`.
  `HeaderValue`/`Response` are `edgezero_core::http`'s.

- [ ] **Step 1: Write the failing tests**

In `constants.rs`, replace the block under `// Staging / version identification headers` with:

```rust
// Staging / version identification headers
/// Deployed git version: tag, else branch, else 6-char commit (see [`TS_GIT_VERSION`]).
pub const HEADER_X_TS_VERSION: HeaderName = HeaderName::from_static("x-ts-version");
/// Fastly service version (`FASTLY_SERVICE_VERSION`), formerly sent as `x-ts-version`.
pub const HEADER_X_TS_FASTLY_VERSION: HeaderName = HeaderName::from_static("x-ts-fastly-version");
pub const HEADER_X_TS_ENV: HeaderName = HeaderName::from_static("x-ts-env");

/// Deployed git version compiled in by `build.rs`, from the deploy pipeline's
/// `TRUSTED_SERVER_GIT_VERSION` or local git. `None` when unknown.
pub const TS_GIT_VERSION: Option<&str> = option_env!("TS_GIT_VERSION");
```

Create `crates/trusted-server-core/src/version_header.rs`:

```rust
//! `x-ts-version`: the deployed git version compiled in by `build.rs`.

use edgezero_core::http::{HeaderValue, Response};

use crate::constants::{HEADER_X_TS_VERSION, TS_GIT_VERSION};

/// Returns the compiled-in git version as a header value.
///
/// `None` when no version is known or the value is not a valid header value.
#[must_use]
pub fn git_version_header_value() -> Option<HeaderValue> {
    header_value_from(TS_GIT_VERSION)
}

/// Converts `version` to a header value, logging and returning `None` when it
/// is not a valid header value.
#[must_use]
pub fn header_value_from(version: Option<&str>) -> Option<HeaderValue> {
    todo!()
}

/// Sets `x-ts-version` to the compiled-in git version, if one is known.
pub fn apply_git_version_header(response: &mut Response) {
    apply_git_version_header_from(TS_GIT_VERSION, response);
}

/// Sets `x-ts-version` to `version`, or leaves it unset when `version` is
/// unknown or invalid.
pub fn apply_git_version_header_from(version: Option<&str>, response: &mut Response) {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body;
    use edgezero_core::http::response_builder;

    fn empty_response() -> Response {
        response_builder()
            .body(Body::empty())
            .expect("should build empty test response")
    }

    fn version_of(response: &Response) -> Option<&str> {
        response
            .headers()
            .get(HEADER_X_TS_VERSION)
            .and_then(|v| v.to_str().ok())
    }

    #[test]
    fn header_value_from_valid_version() {
        assert_eq!(
            header_value_from(Some("v1.2.3")),
            Some(HeaderValue::from_static("v1.2.3")),
            "should convert a valid version"
        );
    }

    #[test]
    fn header_value_from_unknown_or_invalid_version() {
        assert_eq!(header_value_from(None), None, "should be None when unknown");
        assert_eq!(
            header_value_from(Some("bad\nvalue")),
            None,
            "should be None for a non-header-safe version"
        );
    }

    #[test]
    fn git_version_header_value_matches_compiled_in_version() {
        assert_eq!(
            git_version_header_value()
                .as_ref()
                .and_then(|v| v.to_str().ok()),
            TS_GIT_VERSION,
            "should convert the compiled-in TS_GIT_VERSION"
        );
    }

    #[test]
    fn sets_header_from_version() {
        let mut response = empty_response();
        apply_git_version_header_from(Some("v1.2.3"), &mut response);
        assert_eq!(version_of(&response), Some("v1.2.3"), "should set x-ts-version");
    }

    #[test]
    fn omits_header_when_version_unknown() {
        let mut response = empty_response();
        apply_git_version_header_from(None, &mut response);
        assert_eq!(version_of(&response), None, "should omit x-ts-version when unknown");
    }

    #[test]
    fn skips_invalid_header_value() {
        let mut response = empty_response();
        apply_git_version_header_from(Some("bad\nvalue"), &mut response);
        assert_eq!(version_of(&response), None, "should skip a non-header-safe version");
    }

    #[test]
    fn default_uses_compiled_in_version() {
        let mut response = empty_response();
        apply_git_version_header(&mut response);
        assert_eq!(
            version_of(&response),
            TS_GIT_VERSION,
            "should report the compiled-in TS_GIT_VERSION"
        );
    }
}
```

Add `pub mod version_header;` to `lib.rs` after `pub mod tsjs;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p trusted-server-core --lib --target wasm32-wasip1 version_header`
Expected: the 7 tests FAIL, panicking with `not yet implemented`.

- [ ] **Step 3: Implement**

Replace the two `todo!()` bodies:

```rust
pub fn header_value_from(version: Option<&str>) -> Option<HeaderValue> {
    let version = version?;
    match HeaderValue::from_str(version) {
        Ok(value) => Some(value),
        Err(_) => {
            log::warn!("Skipping invalid TS_GIT_VERSION response header value");
            None
        }
    }
}
```

```rust
pub fn apply_git_version_header_from(version: Option<&str>, response: &mut Response) {
    if let Some(value) = header_value_from(version) {
        response.headers_mut().insert(HEADER_X_TS_VERSION, value);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p trusted-server-core --lib --target wasm32-wasip1 version_header`
Expected: 7 passed.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy-fastly`
Expected: no warnings.

```bash
git add crates/trusted-server-core/src/constants.rs crates/trusted-server-core/src/version_header.rs crates/trusted-server-core/src/lib.rs
git commit --signoff -S -m "Add x-ts-fastly-version constant and git version header helpers"
```

---

### Task 3: Fastly adapter: split the headers and cover `/health`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/middleware.rs` (imports, the two header-order doc comments, the `FASTLY_SERVICE_VERSION` block in `apply_finalize_headers`, tests)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (`health_response`, tests)

**Interfaces:**

- Consumes: `HEADER_X_TS_FASTLY_VERSION`, `HEADER_X_TS_VERSION`, `TS_GIT_VERSION`,
  `version_header::{apply_git_version_header, git_version_header_value}` (Task 2).

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `middleware.rs`:

```rust
    #[test]
    fn version_headers_split_git_and_fastly_versions() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, None, &mut response);

        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok());
        assert_eq!(
            header("x-ts-version"),
            trusted_server_core::constants::TS_GIT_VERSION,
            "should report the compiled-in git version as x-ts-version"
        );
        assert_eq!(
            header("x-ts-fastly-version"),
            std::env::var(ENV_FASTLY_SERVICE_VERSION).ok().as_deref(),
            "should report FASTLY_SERVICE_VERSION as x-ts-fastly-version"
        );
    }
```

Add to `mod tests` in `main.rs`:

```rust
    #[test]
    fn health_response_reports_git_version_only() {
        let req = FastlyRequest::get("https://example.com/health");

        let response = health_response(&req).expect("should build health response");

        assert_eq!(
            response.get_header_str("x-ts-version"),
            trusted_server_core::constants::TS_GIT_VERSION,
            "should report the compiled-in git version on /health"
        );
        assert!(
            response.get_header("x-ts-fastly-version").is_none(),
            "should keep x-ts-fastly-version off the /health fast path"
        );
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test-fastly version_headers_split_git_and_fastly_versions health_response_reports_git_version_only`
Expected: both FAIL on the `x-ts-version` assertion. Local builds have git, so
`TS_GIT_VERSION` is `Some(…)`, but neither path sets it yet.

If `cargo test` rejects two filters in this toolchain, run each name separately.

- [ ] **Step 3: Implement**

`middleware.rs` imports:

```rust
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_FASTLY_VERSION,
};
use trusted_server_core::version_header::apply_git_version_header;
```

Replace the `FASTLY_SERVICE_VERSION` block in `apply_finalize_headers` with:

```rust
    apply_git_version_header(response);

    if let Ok(v) = std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        if let Ok(value) = HeaderValue::from_str(&v) {
            response
                .headers_mut()
                .insert(HEADER_X_TS_FASTLY_VERSION, value);
        } else {
            log::warn!("Skipping invalid FASTLY_SERVICE_VERSION response header value");
        }
    }
```

In **both** header-order doc comments (on `FinalizeResponseMiddleware` and on
`apply_finalize_headers`), replace
``2. `X-TS-Version` from `FASTLY_SERVICE_VERSION` env var`` with:

```rust
/// 2. `X-TS-Version` from the compiled-in git version (`TS_GIT_VERSION`), and
///    `X-TS-Fastly-Version` from the `FASTLY_SERVICE_VERSION` env var
```

`main.rs` — add imports:

```rust
use trusted_server_core::constants::HEADER_X_TS_VERSION;
use trusted_server_core::version_header::git_version_header_value;
```

and replace `health_response` with:

```rust
fn health_response(req: &FastlyRequest) -> Option<FastlyResponse> {
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        let mut response = FastlyResponse::from_status(200).with_body_text_plain("ok");
        // Compiled-in constant: keeps the probe free of settings and app construction.
        if let Some(version) = git_version_header_value() {
            response.set_header(HEADER_X_TS_VERSION, version);
        }
        return Some(response);
    }

    None
}
```

- [ ] **Step 4: Run the adapter tests and clippy**

Run: `cargo test-fastly && cargo clippy-fastly`
Expected: all tests pass. Clippy reports no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/middleware.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit --signoff -S -m "Send git version as x-ts-version and Fastly version as x-ts-fastly-version on Fastly"
```

---

### Task 4: Axum, Cloudflare, Spin: emit `x-ts-version`

**Files:**

- Modify: `crates/trusted-server-adapter-axum/src/middleware.rs` (the `FinalizeResponseMiddleware` and `apply_finalize_headers` doc comments, `apply_finalize_headers`, tests)
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs` (new `/health` test)
- Modify: `crates/trusted-server-adapter-cloudflare/src/middleware.rs` (`apply_finalize_headers`, tests)
- Modify: `crates/trusted-server-adapter-spin/src/middleware.rs` (`apply_finalize_headers`, tests)
- Modify: `crates/trusted-server-adapter-spin/src/app.rs` (`startup_error_router_answers_health_with_200`)

**Interfaces:**

- Consumes: `version_header::apply_git_version_header`, `constants::TS_GIT_VERSION` (Task 2).

- [ ] **Step 1: Write the failing tests**

Axum `middleware.rs` `mod tests` (signature `apply_finalize_headers(&Settings, &mut Response)`):

```rust
    #[test]
    fn emits_git_version_header() {
        let mut response = empty_response();
        apply_finalize_headers(&settings_with_response_headers(vec![]), &mut response);
        assert_eq!(
            response.headers().get("x-ts-version").and_then(|v| v.to_str().ok()),
            trusted_server_core::constants::TS_GIT_VERSION,
            "should report the compiled-in git version as x-ts-version"
        );
    }
```

Cloudflare and Spin `middleware.rs` `mod tests` (signature
`apply_finalize_headers(&Settings, bool, &mut Response)`), in each:

```rust
    #[test]
    fn emits_git_version_header() {
        let mut response = empty_response();
        apply_finalize_headers(&settings_with_response_headers(vec![]), false, &mut response);
        assert_eq!(
            response.headers().get("x-ts-version").and_then(|v| v.to_str().ok()),
            trusted_server_core::constants::TS_GIT_VERSION,
            "should report the compiled-in git version as x-ts-version"
        );
    }
```

Axum `tests/routes.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_reports_git_version() {
    let mut service = make_service();
    let request = Request::builder()
        .method("GET")
        .uri("/health")
        .body(AxumBody::empty())
        .expect("should build health request");

    let response = service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should serve health");

    assert_eq!(response.status().as_u16(), 200, "should return 200 on /health");
    assert_eq!(
        response
            .headers()
            .get("x-ts-version")
            .and_then(|v| v.to_str().ok()),
        trusted_server_core::constants::TS_GIT_VERSION,
        "should report the compiled-in git version on /health"
    );
}
```

Spin `app.rs`, at the end of `startup_error_router_answers_health_with_200`,
capture the header before `resp.into_body()` consumes the response. Insert
directly after the status assertion:

```rust
        assert_eq!(
            resp.headers()
                .get("x-ts-version")
                .and_then(|v| v.to_str().ok()),
            trusted_server_core::constants::TS_GIT_VERSION,
            "startup-fallback /health should report the compiled-in git version"
        );
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test-axum emits_git_version_header; cargo test-axum --test routes health_reports_git_version; cargo test-cloudflare emits_git_version_header; cargo test-spin emits_git_version_header; cargo test-spin startup_error_router_answers_health_with_200`
Expected: each FAILs, with left `None` and right `Some(…)`.

- [ ] **Step 3: Implement**

In each of the three `apply_finalize_headers` functions, directly after the
`HEADER_X_GEO_INFO_AVAILABLE` insert and before
`apply_response_headers_with_cache_privacy`, add:

```rust
    trusted_server_core::version_header::apply_git_version_header(response);
```

In the Axum `FinalizeResponseMiddleware` doc comment, replace
``is always emitted. Fastly-specific headers (`X-TS-Version`, `X-TS-ENV`) are``
and the following line with:

```rust
/// is always emitted. `X-TS-Version` carries the compiled-in git version. The
/// Fastly-specific headers (`X-TS-Fastly-Version`, `X-TS-ENV`) are skipped
/// because the corresponding env vars are not set in a local dev context.
```

In the Axum `apply_finalize_headers` doc comment, replace
`is unconditionally emitted. Fastly-specific headers are omitted.` with:

```rust
/// is unconditionally emitted, followed by the compiled-in `X-TS-Version`.
/// Fastly-specific headers are omitted.
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm`
Expected: all pass, with no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-axum crates/trusted-server-adapter-cloudflare/src/middleware.rs crates/trusted-server-adapter-spin/src
git commit --signoff -S -m "Emit x-ts-version git version on Axum, Cloudflare, and Spin"
```

---

### Task 5: Docs and changelog

**Files:**

- Modify: `docs/guide/first-party-proxy.md` (`[response_headers]` example, around line 719)
- Modify: `CHANGELOG.md` (`## [Unreleased]` → `### Changed`)

- [ ] **Step 1: Stop the docs example from overriding the managed header**

In `docs/guide/first-party-proxy.md`, replace `X-TS-Version = "1.0"` with:

```toml
X-Debug-Build = "canary"
```

- [ ] **Step 2: Changelog entry**

Add as the first bullet under `## [Unreleased]` → `### Changed`:

```markdown
- **Breaking:** `x-ts-version` now reports the deployed git version — the release tag, else the branch, else the first 6 characters of the commit — compiled in from the build-time `TRUSTED_SERVER_GIT_VERSION` (set by the deploy pipeline) or local git, and is sent by every adapter, including on the Fastly `GET /health` probe. The Fastly service version it previously carried is now `x-ts-fastly-version`; update dashboards, monitors, and scripts that read `x-ts-version` as the Fastly version number. Builds with neither the override nor git omit the header.
```

- [ ] **Step 3: Format checks**

Run: `cd docs && npm run format` then `git diff --stat` to confirm only intended files changed.
Expected: no unrelated reformatting.

- [ ] **Step 4: Commit**

```bash
git add docs/guide/first-party-proxy.md CHANGELOG.md
git commit --signoff -S -m "Document the x-ts-version and x-ts-fastly-version split"
```

---

### Task 6: Offline end-to-end verification under Viceroy

No repository changes. Record every command and its observed output for the
PR test plan. Work in the scratchpad directory (`$SCRATCH` below).

- [ ] **Step 1: Override build, served under Viceroy**

```bash
TRUSTED_SERVER_GIT_VERSION=v9.9.9-test cargo build --release -p trusted-server-adapter-fastly --target wasm32-wasip1
```

Serve it with a pushed local config, reusing the setup in
`scripts/smoke-fastly.sh` (`smoke-common.sh`: stub origin,
`smoke_initialize_config`, `ts config push --adapter fastly --local`, the three
`ts_secrets` entries, then `fastly compute serve --dir <project> --file <wasm>`).
Then:

```bash
curl -s -o /dev/null -D - http://127.0.0.1:$PORT/health | grep -iE '^HTTP|^x-ts-'
curl -s -o /dev/null -D - http://127.0.0.1:$PORT/ | grep -iE '^HTTP|^x-ts-|^x-geo-info'
```

Expected: both `200`. `/health` shows only `x-ts-version: v9.9.9-test`. `/`
shows `x-ts-version: v9.9.9-test`, `x-ts-fastly-version: <Viceroy's value>`, and
`x-geo-info-available` (proof it went through `apply_finalize_headers`). Record
Viceroy's `FASTLY_SERVICE_VERSION`. No `x-ts-version` is numeric.

- [ ] **Step 2: Warm-cache rebuild with a changed value**

```bash
TRUSTED_SERVER_GIT_VERSION=v9.9.9-test2 cargo build --release -p trusted-server-adapter-fastly --target wasm32-wasip1 -vv 2>&1 | grep -F 'TS_GIT_VERSION='
```

Expected: `cargo:rustc-env=TS_GIT_VERSION=v9.9.9-test2`. Re-serve and curl
`/health`: `x-ts-version: v9.9.9-test2`.

- [ ] **Step 3: No-env fallback**

Rebuild with `TRUSTED_SERVER_GIT_VERSION` unset on branch
`feat/git-version-header`: `/health` shows `x-ts-version: feat/git-version-header`.
Then `git switch --detach` and rebuild: `x-ts-version` is the first 6 chars of
`git rev-parse HEAD`. Switch back to the branch afterwards.

- [ ] **Step 4: Non-git build**

```bash
git archive --format=tar HEAD | (mkdir -p "$SCRATCH/nogit" && tar -x -C "$SCRATCH/nogit")
cd "$SCRATCH/nogit" && env -u TRUSTED_SERVER_GIT_VERSION cargo build --release -p trusted-server-adapter-fastly --target wasm32-wasip1
```

Confirm `$SCRATCH` is not inside a git repository first
(`git -C "$SCRATCH/nogit" rev-parse` fails). Expected: the build succeeds;
serving it, `/health` has no `x-ts-version`.

---

### Task 7: Full CI gate

- [ ] **Step 1: Run every gate from `CLAUDE.md`**

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo clippy-cli && cargo clippy-codegen
cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
(cd crates/trusted-server-js/lib && npx vitest run && npm run format)
(cd docs && npm run format)
```

Expected: all green, and `git status` clean afterwards (format scripts may
rewrite files; commit any intended changes).

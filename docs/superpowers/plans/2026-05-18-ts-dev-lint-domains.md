# `ts dev lint domains` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `ts dev lint domains` and `ts dev install-hooks` as new subcommands of the Trusted Server CLI, with a pre-commit hook integration that prevents commits from introducing non-allowlisted URL hosts in source, config, and documentation files.

**Architecture:** Add a `dev/` module directory to `trusted-server-cli` that hosts: (a) the existing dev-server behavior, renamed to `ts dev serve`; (b) `ts dev install-hooks` for the one-time hook installer; (c) `ts dev lint domains` for the actual linter. All git operations use the `gix` / `gix-config` crates — no subprocess. URL extraction uses the standard `regex` crate (no lookahead) with three allowlists (`EXACT_HOSTS`, `SUBDOMAIN_HOSTS`, `REFERENCE_HOSTS`). Pre-commit-only enforcement in v1; CI gate is a documented Stage 2 follow-up.

**Tech Stack:** Rust 2024 edition, `clap` (existing), `regex` (existing), `gix` + `gix-config` (new — versions pinned during the Phase 2 spike), `tempfile` + `assert_cmd` for tests. `error-stack` for error plumbing, `derive_more::Display` per project convention.

**Spec:** `docs/superpowers/specs/2026-05-18-check-domains-design.md` — every implementation decision below is grounded in a numbered section there. When a task says "per spec §X" it means "open the spec and read section X before implementing this step."

**Branch base:** `feature/check-domains-spec` (stacked on `origin/feature/ts-cli` / PR #669). All commits land on this branch.

---

## Pre-flight (Phase 0)

### Task 0.1: Verify prerequisite state

- [ ] **Step 1: Confirm the branch base**

Run: `git rev-list --count HEAD ^origin/feature/ts-cli`
Expected: a small positive integer (the existing spec commits on this branch). If `git` complains the ref is unknown, run `git fetch origin feature/ts-cli` first.

- [ ] **Step 2: Confirm the CLI surface is present**

Run: `ls crates/trusted-server-cli/src/`
Expected output includes: `audit.rs  audit  config.rs  dev.rs  error.rs  fastly  lib.rs  main.rs  output.rs`. If `dev.rs` is missing, the rebase onto `feature/ts-cli` did not land — stop and re-establish the branch base.

- [ ] **Step 3: Confirm the workspace builds clean before any edits**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS with no errors.

If this fails, the issue is upstream (PR #669 conflict or the workspace is broken); do not start the refactor on a broken base.

### Task 0.2: Capture the `ts dev` baseline before refactoring

- [ ] **Step 1: Capture `ts dev --help` output**

Run: `cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev --help 2>&1 | tee /tmp/ts-dev-help-before.txt`
Expected: clap help text listing `--adapter`, `--config`, `--env`, and a trailing-args mention. The file is the byte-for-byte baseline for the Phase 1 verification.

- [ ] **Step 2: Capture today's `dev.rs` public API surface**

Run: `grep -n '^pub ' crates/trusted-server-cli/src/dev.rs > /tmp/ts-dev-pub-api-before.txt && cat /tmp/ts-dev-pub-api-before.txt`
Expected output:

```
14:pub enum Adapter {
19:pub fn render_local_fastly_manifest(template: &str, canonical_toml: &str) -> String {
30:pub fn write_local_fastly_manifest(
46:pub fn run_fastly_dev(
102:pub fn run_dev_command(
```

These five public items must remain importable from `crate::dev::*` after the refactor (`pub use` re-exports if needed).

---

## Phase 1: Refactor `ts dev` → `ts dev serve`

Spec §"Why `ts dev` as the parent?" and §"Crate Layout" — `ts dev serve` must preserve every flag and behavior of today's `ts dev` leaf.

### Task 1.1: Create `dev/` module skeleton, move `dev.rs` body to `dev/serve.rs`

**Files:**
- Create: `crates/trusted-server-cli/src/dev/mod.rs`
- Create: `crates/trusted-server-cli/src/dev/serve.rs`
- Delete: `crates/trusted-server-cli/src/dev.rs`

- [ ] **Step 1: Create `dev/serve.rs` with the existing `dev.rs` body**

Move the contents of `crates/trusted-server-cli/src/dev.rs` verbatim into `crates/trusted-server-cli/src/dev/serve.rs`. The five `pub` items (`Adapter`, `render_local_fastly_manifest`, `write_local_fastly_manifest`, `run_fastly_dev`, `run_dev_command`) stay public.

- [ ] **Step 2: Create `dev/mod.rs` as the subcommand-group dispatcher**

Write:

```rust
//! `ts dev` subcommand group: developer-workflow commands.
//!
//! Subcommands:
//! - `serve`: launches the local dev server (formerly `ts dev`).
//! - `lint domains`: URL-host linter (Phase 2+).
//! - `install-hooks`: pre-commit hook installer (Phase 6).

pub mod serve;

// Re-export the public surface so existing imports
// `crate::dev::{Adapter, run_dev_command, ...}` continue to work.
pub use serve::{
    Adapter, render_local_fastly_manifest, run_dev_command, run_fastly_dev,
    write_local_fastly_manifest, FASTLY_LOCAL_MANIFEST,
};
```

- [ ] **Step 3: Delete the old `dev.rs` file**

Run: `git rm crates/trusted-server-cli/src/dev.rs`

- [ ] **Step 4: Verify the workspace still builds**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS. If the build fails, an import in `lib.rs` or elsewhere needs adjusting; do not proceed until clean.

- [ ] **Step 5: Run the existing `dev` tests**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::`
Expected: the three tests in `dev/serve.rs` (`rendered_manifest_embeds_runtime_config_store`, `cargo_target_dir_defaults_to_project_target`, `cargo_target_dir_honors_environment_override`) all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/dev/ crates/trusted-server-cli/src/dev.rs
git commit -m "Refactor ts dev into dev/ module with serve.rs

Move the existing dev-server function body verbatim into dev/serve.rs;
add dev/mod.rs that re-exports the public surface so existing
crate::dev::{...} imports keep working. This is the first half of
splitting ts dev from a leaf command into a subcommand group; the
clap-side change lands in the next commit."
```

### Task 1.2: Introduce `DevCommand` enum with `Serve` variant; rewire `lib.rs`

**Files:**
- Modify: `crates/trusted-server-cli/src/lib.rs` (lines around 40, 89, 184, 281)
- Modify: `crates/trusted-server-cli/src/dev/mod.rs`

- [ ] **Step 1: Add the `DevCommand` enum in `dev/mod.rs`**

Append to `crates/trusted-server-cli/src/dev/mod.rs`:

```rust
use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Subcommands under `ts dev`.
#[derive(Debug, Subcommand)]
pub enum DevCommand {
    /// Launch the local dev server (formerly `ts dev`).
    Serve(ServeArgs),
}

/// Arguments for `ts dev serve`. **Must preserve byte-for-byte the
/// flags of today's `ts dev` leaf** — see spec §"This PR must make
/// the CLI-surface change".
#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long, short = 'a', default_value = "fastly")]
    pub adapter: Adapter,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, default_value = "local")]
    pub env: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub passthrough: Vec<String>,
}
```

- [ ] **Step 2: Update `lib.rs` to use `DevCommand`**

In `crates/trusted-server-cli/src/lib.rs`:

Find:
```rust
    Dev(DevArgs),
```
Change to:
```rust
    Dev {
        #[command(subcommand)]
        command: dev::DevCommand,
    },
```

Find and delete the entire `struct DevArgs { ... }` block (lines ~89-99).

Find:
```rust
        Command::Dev(args) => run_dev(&args),
```
Change to:
```rust
        Command::Dev { command } => run_dev(command),
```

Find:
```rust
fn run_dev(args: &DevArgs) -> Result<(), Report<CliError>> {
```
Change the entire function body to:

```rust
fn run_dev(command: dev::DevCommand) -> Result<(), Report<CliError>> {
    match command {
        dev::DevCommand::Serve(args) => run_dev_serve(&args),
    }
}

fn run_dev_serve(args: &dev::ServeArgs) -> Result<(), Report<CliError>> {
    let validated = config::load_validated_config(args.config.as_deref())?;
    let status = dev::run_dev_command(args.adapter, &validated, &args.env, &args.passthrough)?;
    if status.success() {
        Ok(())
    } else {
        Err(Report::new(CliError::Development).attach(format!(
            "`fastly compute serve` exited with status {status}"
        )))
    }
}
```

(The body of `run_dev_serve` is literally the body of the old `run_dev` with `args.*` references unchanged. Verify by diffing against the old `run_dev` block.)

- [ ] **Step 3: Verify the workspace builds**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS.

- [ ] **Step 4: Verify the `dev serve --help` output preserves the flag contract**

A byte-for-byte diff against the captured baseline is too brittle —
clap may legitimately reformat headings or the `Usage:` line when
the command moves from a leaf to a child of a subcommand group.
The contract we care about is **flag preservation**, not
help-text identity. Capture the new help text and assert on each
required surface:

```sh
cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" \
  -- dev serve --help > /tmp/ts-dev-serve-help-after.txt 2>&1

# Each flag from the baseline must still be advertised, with the
# same default value where applicable.
grep -q -- '--adapter' /tmp/ts-dev-serve-help-after.txt
grep -q -- '-a' /tmp/ts-dev-serve-help-after.txt
grep -q -E 'default[^]]*fastly' /tmp/ts-dev-serve-help-after.txt
grep -q -- '--config' /tmp/ts-dev-serve-help-after.txt
grep -q -- '--env' /tmp/ts-dev-serve-help-after.txt
grep -q -E 'default[^]]*local' /tmp/ts-dev-serve-help-after.txt
# Trailing passthrough is usually rendered as '[PASSTHROUGH]...' or
# similar; the presence of an ellipsis after the positional name is
# the contract:
grep -q -E '\[.*\]\.\.\.' /tmp/ts-dev-serve-help-after.txt
```

All seven greps must exit 0. If any fail, the refactor lost a flag
— fix `ServeArgs` before continuing. Keep the captured baseline
(`/tmp/ts-dev-help-before.txt`) around so you can eyeball-diff if a
grep fails.

Functional verification (more important than help-text shape):

```sh
# Trailing args still reach the runner. Use --skip-build so the
# runner doesn't actually try to launch fastly; the failure mode
# should be the documented "no Wasm binary" message, not a
# clap-parse error.
cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" \
  -- dev serve --adapter=fastly --env=local -- --skip-build 2>&1 \
  | grep -q -- '--skip-build was passed'
```

Expected: the grep finds the runner's diagnostic, proving the
passthrough arg reached `run_fastly_dev`. If clap rejects the args
or the passthrough is lost, the refactor is broken.

- [ ] **Step 5: Verify `ts dev --help` now shows a subcommand list**

Run: `cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev --help`
Expected: clap help text listing `serve` as a subcommand (other subcommands `lint`, `install-hooks` arrive in later phases). No flags listed at the `ts dev` level itself.

- [ ] **Step 6: Run existing tests**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: all existing tests PASS (no behavior change yet, only structural rename).

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-cli/src/lib.rs crates/trusted-server-cli/src/dev/mod.rs
git commit -m "Promote ts dev to subcommand group with serve as the first child

ts dev is no longer a leaf; today's behavior is now ts dev serve,
preserving --adapter, --config, --env, and the trailing passthrough
args byte-for-byte. Verified via diff of --help output against the
captured baseline. Required by spec §'This PR must make the
CLI-surface change' so that ts dev lint domains and ts dev
install-hooks can be added in subsequent commits."
```

---

## Phase 2: gix feasibility spike

Spec §"Implementation Readiness" step 1 and §"Cargo dependencies". The spike's deliverables are: (a) pinned matched `gix` + `gix-config` versions; (b) three working integration tests proving the conceptual operations; (c) updates to the spec replacing the `<pin-during-spike>` placeholders.

### Task 2.1: Add the gix dependencies with provisional versions

**Files:**
- Modify: `crates/trusted-server-cli/Cargo.toml`

- [ ] **Step 1: Find a matched release-family pair**

Run: `cargo search gix --limit 5` and `cargo search gix-config --limit 5`
Note the latest `gix` version (e.g., `0.66.x`) and look at its release notes (on crates.io / docs.rs) for the corresponding `gix-config` version. **They must come from the same release family** — see spec note "the `gix 0.66` release line shipped with `gix-config 0.39.x`, not `0.40`". Write the chosen pair to `/tmp/gix-pins.txt` in the form `gix=0.x.y\ngix-config=0.a.b`.

- [ ] **Step 2: Add to `Cargo.toml`**

In `crates/trusted-server-cli/Cargo.toml` under `[dependencies]`, add:

```toml
gix = { version = "<your-pin>", default-features = false, features = [
    "blob-diff",
    "index",
    "revision",
] }
gix-config = "<your-matched-pin>"
```

Replace `<your-pin>` and `<your-matched-pin>` with the values from step 1.

- [ ] **Step 3: Resolve and verify no duplicate versions**

Run: `cargo update --package gix --package gix-config && cargo tree -p gix -p gix-config 2>&1 | head -40`

Expected: each crate appears exactly once at the top level. No `(*)` markers indicating duplicate-version entries elsewhere in the tree. If duplicates appear, adjust the version pins until they don't.

- [ ] **Step 4: Build to confirm the deps compile in this workspace**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/Cargo.toml Cargo.lock
git commit -m "Add gix + gix-config deps for ts dev lint domains spike

Pinned to a matched release-family pair (verified with
cargo tree -p gix -p gix-config that no duplicate versions land
in the lock). Features limited to blob-diff, index, revision per
spec §'Cargo dependencies'. Feasibility spike tests follow."
```

### Task 2.2: Spike test 1 — staged blob diff with new-side line numbers

**All spike-test commit helpers must use a fixed author/committer
signature**, not rely on the host's `user.name` / `user.email` git
config. A clean CI runner or fresh dev machine without global git
identity would otherwise fail the spike with "please tell me who
you are." The Phase 4 `test_support` module (Task 4.0) documents
the same requirement and pins a `test_signature()` helper; the
spike helpers in Tasks 2.2 / 2.3 should pin an equivalent fixed
signature locally. When the spike succeeds, the same constant can
be reused from `test_support` once that module exists in Phase 4.


**Files:**
- Create: `crates/trusted-server-cli/tests/spike_gix_staged_diff.rs`

- [ ] **Step 1: Write the failing test**

Create the file with:

```rust
//! Spike: prove that gix can give us per-blob hunk information for
//! files staged in the index against the HEAD tree, with new-side
//! line numbers. Once this test passes the chosen entry points are
//! pinned for the staged_added_lines() implementation in Phase 4.

use std::fs;

use gix::ObjectId;
use tempfile::tempdir;

#[test]
fn staged_blob_diff_yields_new_side_line_numbers() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let repo = gix::init(repo_path).expect("should init gix repo");

    // Commit 1: a file with three lines.
    let file = repo_path.join("a.txt");
    fs::write(&file, "alpha\nbeta\ngamma\n").expect("should write initial file");
    let commit1 = gix_test_util::commit_all(&repo, "initial");

    // Stage a modification adding a new line at position 2.
    fs::write(&file, "alpha\nNEW LINE\nbeta\ngamma\n").expect("should write modification");
    gix_test_util::stage_all(&repo);

    // Call the conceptual operation: enumerate index-vs-HEAD changes,
    // and for each modified blob produce hunks with new-side line numbers.
    let hunks = gix_test_util::staged_blob_hunks(&repo).expect("should collect staged hunks");

    // We expect exactly one added line at new-side line 2 with content "NEW LINE".
    let added: Vec<(String, usize, String)> = hunks
        .into_iter()
        .flat_map(|(path, hunks)| {
            hunks.into_iter().flat_map(move |h| {
                h.added_lines
                    .into_iter()
                    .map(|(ln, c)| (path.clone(), ln, c))
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    assert_eq!(added.len(), 1, "should have one added line: {added:?}");
    assert_eq!(added[0].0, "a.txt", "path");
    assert_eq!(added[0].1, 2, "new-side line number");
    assert_eq!(added[0].2, "NEW LINE", "content");

    let _ = commit1; // keep variable name visible in failure context
}

mod gix_test_util {
    //! Helpers that pin the specific gix entry points used by the
    //! production code in Phase 4. The signatures here are stable;
    //! the bodies use whatever gix APIs work in the pinned version.

    use super::*;

    pub fn commit_all(_repo: &gix::Repository, _msg: &str) -> ObjectId {
        unimplemented!("call into gix to stage everything and commit; \
                        return the new commit id")
    }

    pub fn stage_all(_repo: &gix::Repository) {
        unimplemented!("call into gix to update the index from working tree")
    }

    pub struct Hunk {
        pub added_lines: Vec<(usize, String)>,
    }

    pub fn staged_blob_hunks(
        _repo: &gix::Repository,
    ) -> Result<Vec<(String, Vec<Hunk>)>, Box<dyn std::error::Error>> {
        unimplemented!("compare HEAD tree vs index; for each modified entry, \
                        load old + new blobs and run a line diff; return hunks")
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --test spike_gix_staged_diff`
Expected: FAIL with `unimplemented!()` panic.

- [ ] **Step 3: Implement the three `gix_test_util` helpers using the pinned gix version**

Replace the `unimplemented!()` bodies with real calls. Start with `commit_all` (gix exposes commit-creation via `repo.commit("HEAD", msg, tree, parents)` or equivalent in the pinned version). Then `stage_all` (write the working tree to the index). Finally `staged_blob_hunks` — the most involved:

1. Open the HEAD tree via `repo.head_commit()?.tree()?`.
2. Read the index via `repo.index()?`.
3. Walk index-vs-tree changes. In the pinned gix version, this lives under one of: `gix::diff::tree_with_rewrites`, `gix::object::tree::diff::Platform`, or `gix::index::diff_against_tree` — pick the one that exists and produces `(path, old_blob_id, new_blob_id)` triples for modified/added entries.
4. For each changed entry, load the old blob (or empty for additions) and the new blob.
5. Run a blob line diff. In gix this is `gix_diff::blob::diff` driven by `imara_diff`. Collect `(post_image_line_no, content)` for each insertion.

When the test passes, **document the exact entry-point names you used** in `/tmp/gix-api-pins.txt` — these get copy-pasted into the spec in Task 2.5.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --test spike_gix_staged_diff`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/tests/spike_gix_staged_diff.rs
git commit -m "Spike: staged-diff gix entry points pinned

Proves we can enumerate index-vs-HEAD changes, load the old and new
blobs per changed entry, and produce blob-diff hunks with new-side
line numbers and content — the contract Phase 4's
staged_added_lines() relies on. The exact gix entry points used will
be reflected in the spec's prototype-required callout once the spike
batch is complete."
```

### Task 2.3: Spike test 2 — merge-base + tree-vs-tree blob diff

**Files:**
- Create: `crates/trusted-server-cli/tests/spike_gix_changed_vs.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! Spike: prove that gix can compute a merge-base between two refs
//! and then run a tree-vs-tree diff with the same blob-diff hunks
//! used by the staged path. Locks in the API for
//! changed_vs_added_lines() in Phase 4.

use std::fs;

use tempfile::tempdir;

#[test]
fn merge_base_then_tree_diff_yields_added_lines() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let repo = gix::init(repo_path).expect("should init gix repo");

    // main: commit a single line on a branch named "main".
    let file = repo_path.join("a.txt");
    fs::write(&file, "one\n").expect("should write base file");
    let _base = spike_helpers::commit_all_as_branch(&repo, "main", "first");

    // feature: branch off main, add another line.
    spike_helpers::create_and_checkout_branch(&repo, "feature");
    fs::write(&file, "one\ntwo\n").expect("should write feature-branch change");
    let _head = spike_helpers::commit_all(&repo, "second");

    // Conceptual operation: merge-base("main", HEAD) then diff the
    // merge-base tree against HEAD tree.
    let added = spike_helpers::changed_vs_ref(&repo, "main")
        .expect("should compute changed-vs added lines");

    assert_eq!(
        added,
        vec![("a.txt".to_string(), 2usize, "two".to_string())],
        "should report only the line added by the feature branch"
    );
}

mod spike_helpers {
    use super::*;
    use gix::ObjectId;

    pub fn commit_all_as_branch(_r: &gix::Repository, _b: &str, _m: &str) -> ObjectId {
        unimplemented!("stage + commit on the given branch ref")
    }

    pub fn create_and_checkout_branch(_r: &gix::Repository, _b: &str) {
        unimplemented!("create branch ref pointing at HEAD; move HEAD to it")
    }

    pub fn commit_all(_r: &gix::Repository, _m: &str) -> ObjectId {
        unimplemented!("stage + commit on current ref")
    }

    pub fn changed_vs_ref(
        _r: &gix::Repository,
        _ref_name: &str,
    ) -> Result<Vec<(String, usize, String)>, Box<dyn std::error::Error>> {
        unimplemented!(
            "resolve ref via the four-fallback order (see spec \
             §'Base-ref resolution order'), compute merge-base with \
             HEAD, diff base-tree vs HEAD-tree, return (path, \
             new-side line, content) for each added line"
        )
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --test spike_gix_changed_vs`
Expected: FAIL with `unimplemented!()`.

- [ ] **Step 3: Implement the helpers**

`changed_vs_ref` is the load-bearing one:

1. Resolve `_ref_name` per the spec's four-fallback order: `<ref>`, `refs/heads/<ref>`, `refs/remotes/origin/<ref>`, `refs/tags/<ref>`. Return the first that resolves to an object id.
2. Compute merge-base via `repo.merge_base(base_id, head_id)`.
3. Get the trees: `repo.find_commit(merge_base)?.tree()?` and `repo.find_commit(head_id)?.tree()?`.
4. Run tree-vs-tree diff via the same primitives used in Task 2.2.
5. For each changed blob, run the blob diff and collect `(path, new_line_no, content)` for insertions.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --test spike_gix_changed_vs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/tests/spike_gix_changed_vs.rs
git commit -m "Spike: merge-base and tree-vs-tree gix entry points pinned

Drives the conceptual operation for --changed-vs <ref> mode: resolve
the ref via the spec's four-fallback order, compute merge-base with
HEAD, diff the merge-base tree against HEAD tree, and yield added-line
hunks with new-side line numbers. Same blob-diff primitive as the
staged spike."
```

### Task 2.4: Spike test 3 — durable `core.hooksPath` write via `gix-config::File`

**Files:**
- Create: `crates/trusted-server-cli/tests/spike_gix_config_write.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! Spike: prove that gix-config::File can read and write
//! <repo>/.git/config so that ts dev install-hooks can persist
//! core.hooksPath without subprocess. Locks the read/write APIs
//! for Phase 6.

use std::fs;
use tempfile::tempdir;

#[test]
fn write_core_hooks_path_via_gix_config_persists_to_disk() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let _repo = gix::init(repo_path).expect("should init gix repo");

    spike_helpers::set_local_config_value(
        repo_path,
        "core",
        None,
        "hooksPath",
        ".githooks",
    )
    .expect("should write core.hooksPath via gix-config");

    // Read via gix-config and confirm.
    let value = spike_helpers::read_local_config_value(
        repo_path,
        "core",
        None,
        "hooksPath",
    )
    .expect("should read core.hooksPath back");
    assert_eq!(value.as_deref(), Some(".githooks"));

    // Sanity: reading directly off disk should show the section
    // and key in canonical format.
    let on_disk = fs::read_to_string(repo_path.join(".git/config"))
        .expect("should read .git/config from disk");
    assert!(
        on_disk.contains("[core]") && on_disk.contains("hooksPath"),
        "should contain core/hooksPath: {on_disk:?}"
    );
}

#[test]
fn read_local_config_value_returns_none_when_unset() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let _repo = gix::init(repo_path).expect("should init gix repo");

    let value = spike_helpers::read_local_config_value(
        repo_path,
        "core",
        None,
        "hooksPath",
    )
    .expect("should read core.hooksPath (returning None)");
    assert!(value.is_none(), "unset value reads as None: {value:?}");
}

mod spike_helpers {
    use std::path::Path;

    pub fn set_local_config_value(
        _repo_path: &Path,
        _section: &str,
        _subsection: Option<&str>,
        _key: &str,
        _value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!(
            "use gix_config::File::from_path_no_includes on \
             <repo>/.git/config (or default()), set_raw_value_by, \
             serialize, write atomically (temp + rename)"
        )
    }

    pub fn read_local_config_value(
        _repo_path: &Path,
        _section: &str,
        _subsection: Option<&str>,
        _key: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        unimplemented!(
            "gix_config::File::from_path_no_includes; raw_value_by; \
             return None if file or key absent"
        )
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --test spike_gix_config_write`
Expected: both tests FAIL with `unimplemented!()`.

- [ ] **Step 3: Implement the two helpers**

The set helper: read existing `.git/config` via `gix_config::File::from_path_no_includes(path, gix_config::Source::Local)`, fall back to `File::default()` if missing; call `set_raw_value_by(section, subsection, key, value.as_bytes())`; serialize via `to_bstring()`; write atomically (write to `config.tmp.<rand>`, then `rename` to `config`).

The read helper: same `from_path_no_includes`, then `raw_value_by(section, subsection, key)`. Return `Ok(None)` if the file is absent or the key is missing.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --test spike_gix_config_write`
Expected: both tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/tests/spike_gix_config_write.rs
git commit -m "Spike: gix-config File read/write entry points pinned

Drives the conceptual operations for ts dev install-hooks:
set_local_config_value (atomic write to <repo>/.git/config via
gix_config::File) and read_local_config_value (returns None for
unset, used by the core.hooksPath preflight). Atomic write uses
temp file + rename so a partial write never lands."
```

### Task 2.5: Update the spec with the pinned versions and entry points

**Files:**
- Modify: `docs/superpowers/specs/2026-05-18-check-domains-design.md`

- [ ] **Step 1: Replace the version placeholders**

In the Cargo dependencies block, change `<pin-during-spike>` and `<must-match-the-gix-release-family>` to the concrete versions from `/tmp/gix-pins.txt`. Add a trailing comment noting the release family (e.g., `# gix 0.66 release family`).

- [ ] **Step 2: Update Open Question 5 with the chosen gix API entry points**

In the Open Questions section, change Q5 from "prototype-required" to a RESOLVED list naming the concrete functions you used in the three spike tests (e.g., `gix::index::Platform::diff_against_tree`, `gix_diff::blob::diff` — whatever you actually used).

- [ ] **Step 3: Update Open Question 6 with the pinned versions**

Resolve Q6 with the chosen pair and a one-line note about why this pair.

- [ ] **Step 4: Update the prototype-required callout in the staged-mode section**

In the "Line collection: --staged mode (gitoxide)" section, change the "prototype-required" callout to a resolved one naming the entry points and pointing at `tests/spike_gix_staged_diff.rs` as the reference implementation.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-05-18-check-domains-design.md
git commit -m "Reflect gix feasibility spike outcomes in the spec

Replace <pin-during-spike> / <must-match-the-gix-release-family>
placeholders with the matched pair pinned in the spike commits.
Resolve Open Questions 5 and 6 with the concrete API entry points
used by tests/spike_gix_*.rs. Update the prototype-required
callout in the staged-mode section to name those entry points."
```

---

## Phase 3: URL extraction + allowlist + suppression (pure functions)

Spec §"Allowlist (Rust constants)", §"URL extraction (without lookahead)", §"Suppression marker regex", §"Allow check". This phase produces no CLI surface — only pure functions exercised by unit tests.

### Task 3.1: Create `dev/lint/` module skeleton + constants

**Files:**
- Create: `crates/trusted-server-cli/src/dev/lint/mod.rs`
- Create: `crates/trusted-server-cli/src/dev/lint/domains.rs`
- Modify: `crates/trusted-server-cli/src/dev/mod.rs`

- [ ] **Step 1: Create `dev/lint/mod.rs`**

```rust
//! `ts dev lint` subcommand group: linters for source/config/docs.
//!
//! Subcommands:
//! - `domains`: URL-host linter (this design).

pub mod domains;
```

- [ ] **Step 2: Create `dev/lint/domains.rs` with the three allowlist arrays and reserved TLDs**

Copy the verbatim lists from the spec (§"Exact-match hosts", §"Subdomain-permitting hosts", §"Reference / doc hosts"). Each entry gets a trailing `//`-comment naming the integration / category per the spec's maintenance policy.

Skeleton:

```rust
//! `ts dev lint domains` — URL-host linter.
//!
//! Design: docs/superpowers/specs/2026-05-18-check-domains-design.md

use core::error::Error;

use derive_more::Display;

/// Integration proxies and loopback hosts that must match exactly.
/// Subdomains are NOT allowed (e.g., `anything.api.privacy-center.org`
/// is disallowed). See spec §"Exact-match hosts" for the policy.
pub const EXACT_HOSTS: &[&str] = &[
    // Loopback
    "127.0.0.1",
    "::1",
    "localhost",
    // didomi
    "api.privacy-center.org",
    "sdk.privacy-center.org",
    // sourcepoint
    "cdn.privacy-mgmt.com",
    // lockr
    "aim.loc.kr",
    "identity.loc.kr",
    // datadome
    "js.datadome.co",
    "api-js.datadome.co",
    // aps / Amazon
    "aax.amazon-adsystem.com",
    "aax-events.amazon-adsystem.com",
    // permutive
    "api.permutive.com",
    "secure-signals.permutive.app",
    "cdn.permutive.com",
    // Google Tag Manager / Analytics
    "www.googletagmanager.com",
    "www.google-analytics.com",
    "analytics.google.com",
    // adserver mock
    "securepubads.g.doubleclick.net",
    "origin-mocktioneer.cdintel.com",
    // Prebid CDN
    "cdn.prebid.org",
    // Fastly platform
    "api.fastly.com",
];

/// Hosts where exact match AND any subdomain (`*.host`) is allowed.
/// See spec §"Subdomain-permitting hosts" and §"Allowlist
/// Maintenance Policy" for the bar to add an entry here.
pub const SUBDOMAIN_HOSTS: &[&str] = &[
    // IANA RFC 2606 reserved
    "example.com",
    "example.net",
    "example.org",
    // Permutive: runtime host is {organization_id}.edge.permutive.app
    "edge.permutive.app",
];

/// Well-known documentation and specification sources. Exact-match,
/// allowed in every scanned file. See spec §"Reference / doc hosts"
/// for the curated list (seeded from a sampling; expected to grow
/// during Stage 1 doc cleanup).
pub const REFERENCE_HOSTS: &[&str] = &[
    // Git / GitHub
    "github.com",
    "docs.github.com",
    "help.github.com",
    "token.actions.githubusercontent.com",
    // Git commit conventions
    "chris.beams.io",
    // Rust
    "docs.rs",
    "doc.rust-lang.org",
    "crates.io",
    // Web / W3C standards
    "www.w3.org",
    "schema.org",
    // Versioning / changelogs
    "semver.org",
    "keepachangelog.com",
    // IAB Tech Lab
    "iab.com",
    "iabtechlab.com",
    "iabtechlab.github.io",
    "iabeurope.github.io",
    // Specs (supply chain)
    "in-toto.io",
    "rslstandard.org",
    // Specs (other)
    "webassembly.org",
    // Fastly docs
    "www.fastly.com",
    "developer.fastly.com",
    "manage.fastly.com",
    // Cloudflare docs
    "developers.cloudflare.com",
    // Vendor docs
    "docs.datadome.co",
    "docs.prebid.org",
    // Tooling docs
    "vitepress.dev",
    "playwright.dev",
    "testcontainers.com",
    "grafana.com",
    "docsearch.algolia.com",
];

/// IANA RFC 2606 reserved TLDs. Any host ending in one of these is allowed.
pub const RESERVED_TLDS: &[&str] = &[".example", ".test", ".invalid", ".localhost"];

#[derive(Debug, Display)]
pub enum DomainsLintError {
    #[display("failed to open git repository")]
    OpenRepo,
    #[display("failed to read git index")]
    Index,
    #[display("failed to compute diff")]
    Diff,
    #[display("failed to resolve reference `{_0}`")]
    Reference(String),
    #[display("failed to compute merge-base of `{base}` and HEAD")]
    MergeBase { base: String },
    #[display("failed to read file `{_0}`")]
    ReadFile(std::path::PathBuf),
    #[display("path not found: `{_0}`")]
    PathNotFound(std::path::PathBuf),
    #[display("permission denied reading `{_0}`")]
    PermissionDenied(std::path::PathBuf),
    #[display("invalid mode combination")]
    InvalidMode,
    /// Failure writing a warning to stderr (broken pipe, etc.).
    /// Used by the in-module `warn` helper so collectors can call
    /// `crate::output::write_stderr_line` and still return
    /// `Report<DomainsLintError>` consistently.
    #[display("I/O error writing warning to stderr")]
    WriteWarning,
}
impl Error for DomainsLintError {}

/// In-module warning helper. Wraps the CLI's `write_stderr_line`
/// (which returns `Report<CliError>`) so that callers inside
/// `domains` can stay on `Report<DomainsLintError>` without
/// inventing custom `?` conversions at every call site.
fn warn(msg: impl Into<String>)
    -> Result<(), error_stack::Report<DomainsLintError>>
{
    use error_stack::ResultExt;
    crate::output::write_stderr_line(msg.into())
        .change_context(DomainsLintError::WriteWarning)
}
```

- [ ] **Step 3: Add `lint` to `dev/mod.rs`**

In `crates/trusted-server-cli/src/dev/mod.rs`, append:

```rust
pub mod lint;
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS (with a couple of "unused" warnings for the new constants — fine, they're consumed in subsequent tasks).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/ crates/trusted-server-cli/src/dev/mod.rs
git commit -m "Scaffold dev/lint/domains.rs with allowlist constants

EXACT_HOSTS, SUBDOMAIN_HOSTS, REFERENCE_HOSTS, RESERVED_TLDS, and
the DomainsLintError enum per spec §'Allowlist' sections. Pure
constants only; the allow check, URL extraction, and suppression
parsing arrive in subsequent commits."
```

### Task 3.2: Implement `normalise_host` (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing tests**

Append to `domains.rs`:

```rust
fn normalise_host(raw: &str) -> String {
    todo!("strip surrounding [ ] for bracketed IPv6; lowercase")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_lowercases() {
        assert_eq!(normalise_host("EXAMPLE.COM"), "example.com");
        assert_eq!(normalise_host("Foo.Example.Com"), "foo.example.com");
    }

    #[test]
    fn normalise_strips_ipv6_brackets() {
        assert_eq!(normalise_host("[::1]"), "::1");
        assert_eq!(normalise_host("[2001:DB8::1]"), "2001:db8::1");
    }

    #[test]
    fn normalise_passthrough_for_plain_hosts() {
        assert_eq!(normalise_host("test.com"), "test.com");
        assert_eq!(normalise_host("127.0.0.1"), "127.0.0.1");
    }
}
```

- [ ] **Step 2: Run to verify tests fail**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::tests::normalise`
Expected: 3 FAIL with `not yet implemented`.

- [ ] **Step 3: Implement**

Replace the `todo!()` body with:

```rust
fn normalise_host(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('[').trim_end_matches(']');
    trimmed.to_lowercase()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::tests::normalise`
Expected: 3 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Add normalise_host: bracket-strip + lowercase

Tested against IPv6 bracket forms (case-insensitive), regular
lowercase, and pass-through cases. Pure function; no I/O."
```

### Task 3.3: Implement `is_allowed` (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing tests**

Append:

```rust
use std::collections::HashSet;

fn is_allowed(host: &str, suppressed_on_line: &HashSet<String>) -> bool {
    todo!("see spec §'Allow check'")
}

#[cfg(test)]
mod allow_check_tests {
    use super::*;

    fn nothing_suppressed() -> HashSet<String> { HashSet::new() }

    #[test]
    fn exact_match_allows() {
        assert!(is_allowed("api.fastly.com", &nothing_suppressed()));
        assert!(is_allowed("127.0.0.1", &nothing_suppressed()));
    }

    #[test]
    fn exact_only_rejects_subdomain() {
        // api.fastly.com is exact-only; v2.api.fastly.com is allowed
        // by the subdomain rule on api.fastly.com (any subdomain of
        // an EXACT host is NOT allowed) — wait, re-read spec.
        // Per spec §"Worked examples": api.fastly.com EXACT-list
        // allows v2.api.fastly.com (subdomain rule applies to BOTH
        // arrays).  Re-confirm before changing.
        // Actually the spec says SUBDOMAIN_HOSTS adds the
        // subdomain rule; EXACT_HOSTS is exact-only.
        // So: api.fastly.com exact, v2.api.fastly.com NOT allowed.
        assert!(!is_allowed("v2.api.fastly.com", &nothing_suppressed()));
        assert!(!is_allowed("anything.api.privacy-center.org", &nothing_suppressed()));
    }

    #[test]
    fn subdomain_list_allows_apex_and_subdomains() {
        assert!(is_allowed("example.com", &nothing_suppressed()));
        assert!(is_allowed("foo.example.com", &nothing_suppressed()));
        assert!(is_allowed("a.b.example.com", &nothing_suppressed()));
        assert!(is_allowed("example.net", &nothing_suppressed()));
        assert!(is_allowed("assets.example.net", &nothing_suppressed()));
    }

    #[test]
    fn lookalike_attack_rejected() {
        // example.com.evil.com is not a subdomain of example.com.
        assert!(!is_allowed("example.com.evil.com", &nothing_suppressed()));
        assert!(!is_allowed("notexample.com", &nothing_suppressed()));
    }

    #[test]
    fn reserved_tld_allows() {
        assert!(is_allowed("testlight.example", &nothing_suppressed()));
        assert!(is_allowed("something.test", &nothing_suppressed()));
        assert!(is_allowed("thing.invalid", &nothing_suppressed()));
        assert!(is_allowed("my.localhost", &nothing_suppressed()));
    }

    #[test]
    fn reference_hosts_allowed_everywhere() {
        assert!(is_allowed("github.com", &nothing_suppressed()));
        assert!(is_allowed("docs.rs", &nothing_suppressed()));
        // But NOT subdomains of REFERENCE_HOSTS (exact-match).
        assert!(!is_allowed("other.github.com", &nothing_suppressed()));
    }

    #[test]
    fn suppression_set_allows() {
        let mut suppressed = HashSet::new();
        suppressed.insert("evil.com".to_string());
        assert!(is_allowed("evil.com", &suppressed));
    }

    #[test]
    fn rejects_unrelated_host() {
        assert!(!is_allowed("test.com", &nothing_suppressed()));
        assert!(!is_allowed("1.2.3.4", &nothing_suppressed()));
        assert!(!is_allowed("192.168.1.1", &nothing_suppressed()));
    }
}
```

- [ ] **Step 2: Run to verify tests fail**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::allow_check_tests`
Expected: 8 FAIL with `not yet implemented`.

- [ ] **Step 3: Implement**

Replace the `todo!()` body with:

```rust
fn is_allowed(host: &str, suppressed_on_line: &HashSet<String>) -> bool {
    if suppressed_on_line.contains(host) { return true; }
    if RESERVED_TLDS.iter().any(|t| host.ends_with(t)) { return true; }
    if EXACT_HOSTS.iter().any(|e| host == *e) { return true; }
    if REFERENCE_HOSTS.iter().any(|e| host == *e) { return true; }
    if SUBDOMAIN_HOSTS.iter().any(|e| {
        host == *e || host.ends_with(&format!(".{}", e))
    }) { return true; }
    false
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::allow_check_tests`
Expected: 8 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Add is_allowed implementing the three-array check

Pure function: suppressed-set short-circuit, reserved-TLD suffix,
exact-match against EXACT_HOSTS and REFERENCE_HOSTS, subdomain
rule against SUBDOMAIN_HOSTS. Eight tests cover the worked
examples from spec §'Matching summary'."
```

### Task 3.4: Implement absolute-URL extraction (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing tests**

Append:

```rust
use regex::Regex;
use std::sync::OnceLock;

fn absolute_url_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // (?i) case-insensitive; host must start with alphanumeric to
        // reject placeholders like https://...
        Regex::new(r"(?i)https?://(\[[0-9a-fA-F:]+\]|[A-Za-z0-9][A-Za-z0-9.\-]*)")
            .expect("should compile absolute URL regex")
    })
}

fn extract_absolute_hosts(line: &str) -> Vec<String> {
    todo!("apply absolute_url_regex, capture group 1, normalise each match")
}

#[cfg(test)]
mod absolute_url_tests {
    use super::*;

    #[test]
    fn extracts_plain() {
        assert_eq!(
            extract_absolute_hosts("see https://example.com/path here"),
            vec!["example.com"]
        );
    }

    #[test]
    fn extracts_bracketed_ipv6() {
        assert_eq!(
            extract_absolute_hosts("dial http://[::1]:8080/"),
            vec!["::1"]
        );
    }

    #[test]
    fn extracts_uppercase_normalised() {
        assert_eq!(
            extract_absolute_hosts("HTTPS://Example.COM/x"),
            vec!["example.com"]
        );
    }

    #[test]
    fn rejects_dots_only_placeholder() {
        assert!(extract_absolute_hosts("see https://... for an example").is_empty());
    }

    #[test]
    fn handles_punctuation_wrapping() {
        // The regex stops at any character not in [A-Za-z0-9.-];
        // wrapping punctuation falls outside the capture.
        for s in [
            "\"https://example.com\",",
            "(https://example.com)",
            "<https://example.com>",
        ] {
            assert_eq!(extract_absolute_hosts(s), vec!["example.com"], "input: {s}");
        }
    }

    #[test]
    fn extracts_multiple_per_line() {
        assert_eq!(
            extract_absolute_hosts(
                "see [a](https://github.com/x) and [b](https://example.com/y)"
            ),
            vec!["github.com", "example.com"]
        );
    }
}
```

- [ ] **Step 2: Run to verify tests fail**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::absolute_url_tests`
Expected: 6 FAIL.

- [ ] **Step 3: Implement**

Replace the `todo!()` body with:

```rust
fn extract_absolute_hosts(line: &str) -> Vec<String> {
    absolute_url_regex()
        .captures_iter(line)
        .filter_map(|c| c.get(1).map(|m| normalise_host(m.as_str())))
        .collect()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::absolute_url_tests`
Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Add extract_absolute_hosts using the no-lookahead regex

Standard regex crate; host must start with an alphanumeric to reject
https://... placeholder noise. Six tests cover plain, bracketed
IPv6, case-insensitive, punctuation wrapping, multi-per-line, and
the malformed-host rejection from spec test 20a."
```

### Task 3.5: Implement protocol-relative URL extraction (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing tests**

Append:

```rust
fn protocol_relative_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Boundary class: start-of-line, whitespace, quotes, paren,
        // =, <, >, {, [, ], comma, backtick. NOT colon (would
        // double-match absolute URLs).
        Regex::new(
            r"(?i)(?:^|[\s\"'(=<>{,\[\]`])//([A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,})",
        )
        .expect("should compile protocol-relative URL regex")
    })
}

fn extract_protocol_relative_hosts(line: &str) -> Vec<String> {
    todo!("apply protocol_relative_regex, capture group 1, normalise")
}

#[cfg(test)]
mod protocol_relative_tests {
    use super::*;

    #[test]
    fn extracts_after_quote() {
        assert_eq!(
            extract_protocol_relative_hosts("src=\"//www.googletagmanager.com/gtm.js\""),
            vec!["www.googletagmanager.com"]
        );
    }

    #[test]
    fn extracts_after_start_of_line() {
        assert_eq!(
            extract_protocol_relative_hosts("//cdn.example.evil/foo"),
            vec!["cdn.example.evil"]
        );
    }

    #[test]
    fn extracts_template_literal_backtick() {
        assert_eq!(
            extract_protocol_relative_hosts("`//cdn.example.evil/${path}`"),
            vec!["cdn.example.evil"]
        );
    }

    #[test]
    fn extracts_json_object_value() {
        assert_eq!(
            extract_protocol_relative_hosts("{\"src\": \"//cdn.example.evil/x\"}"),
            vec!["cdn.example.evil"]
        );
    }

    #[test]
    fn does_not_match_colon_prefix() {
        // http://foo.com — // is preceded by ':', NOT in the boundary class.
        assert!(extract_protocol_relative_hosts("http://foo.com/x").is_empty());
    }

    #[test]
    fn does_not_match_code_comment_divider() {
        // The trailing TLD-like constraint (.{2,}) filters this out;
        // "comment text" has no dotted-suffix.
        assert!(extract_protocol_relative_hosts("// comment text").is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::protocol_relative_tests`
Expected: 6 FAIL.

- [ ] **Step 3: Implement**

```rust
fn extract_protocol_relative_hosts(line: &str) -> Vec<String> {
    protocol_relative_regex()
        .captures_iter(line)
        .filter_map(|c| c.get(1).map(|m| normalise_host(m.as_str())))
        .collect()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::protocol_relative_tests`
Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Add extract_protocol_relative_hosts with boundary class

Boundary class includes start-of-line, whitespace, quotes, paren,
=, <, >, {, [, ], comma, backtick — covers HTML attribute values,
JS template literals, JSON object values. Deliberately excludes
':' to avoid double-matching absolute URLs (where '//' is preceded
by the scheme separator). Six tests cover the cases from spec
§'Protocol-relative URL regex'."
```

### Task 3.6: Implement suppression-marker parsing (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing tests**

Append:

```rust
fn suppression_marker_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?im)(?:^|\s)(?://|\#|<!--|\*\s)\s*allow-domain:\s*([A-Za-z0-9.\-:\[\],\s]+?)(?:-->|$)",
        )
        .expect("should compile suppression marker regex")
    })
}

/// Result of parsing a line for a suppression marker.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LineSuppression {
    /// Hosts listed in the marker (post-trim, lowercased).
    pub suppressed: HashSet<String>,
    /// Hosts listed but found nowhere on this line; emitted as a
    /// stderr warning later.
    pub _unused: Vec<String>,
}

fn parse_suppression_marker(line: &str) -> LineSuppression {
    todo!("apply regex, capture group 1, split on ',', trim, lowercase, drop empties")
}

#[cfg(test)]
mod suppression_tests {
    use super::*;

    fn parse(line: &str) -> HashSet<String> {
        parse_suppression_marker(line).suppressed
    }

    #[test]
    fn single_host_after_slash_comment() {
        let got = parse("let x = \"https://evil.com\"; // allow-domain: evil.com");
        let expected: HashSet<String> = ["evil.com".to_string()].into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn html_comment_form_with_trailing_space() {
        // Captured group includes trailing space before --> ; trim handles it.
        let got = parse("<!-- allow-domain: test.com   -->");
        let expected: HashSet<String> = ["test.com".to_string()].into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn hash_comment_form() {
        let got = parse("upstream = \"https://evil.com\"  # allow-domain: evil.com");
        let expected: HashSet<String> = ["evil.com".to_string()].into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn multi_host_with_whitespace() {
        let got = parse("// allow-domain: a.com ,  b.com , c.com");
        let expected: HashSet<String> = ["a.com", "b.com", "c.com"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn bypass_attempt_url_path_lookalike_not_suppressed() {
        // 'allow-domain' inside a URL path is NOT a comment.
        let got = parse("fetch(\"https://evil.com/allow-domain\")");
        assert!(got.is_empty(), "URL-path content must not suppress: {got:?}");
    }

    #[test]
    fn bypass_attempt_pathological_host_named_allow_domain() {
        // https://allow-domain:8080/path — the // is preceded by ':',
        // not whitespace/SOL, so the marker anchor fails.
        let got = parse("let x = \"https://allow-domain:8080/path\";");
        assert!(got.is_empty(), "pathological host must not suppress: {got:?}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::suppression_tests`
Expected: 6 FAIL.

- [ ] **Step 3: Implement**

```rust
fn parse_suppression_marker(line: &str) -> LineSuppression {
    let mut out = LineSuppression::default();
    let Some(caps) = suppression_marker_regex().captures(line) else { return out };
    let Some(m) = caps.get(1) else { return out };
    for host in m.as_str().split(',') {
        let host = host.trim();
        if !host.is_empty() {
            out.suppressed.insert(host.to_lowercase());
        }
    }
    out
}
```

(`_unused` is populated later by `scan_line` once it knows which hosts actually appeared.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::suppression_tests`
Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Add parse_suppression_marker with bypass-resistant anchor

Marker regex requires start-of-line or whitespace before the comment
introducer (//, #, <!--, '* '), then 'allow-domain:', then a
comma-separated host list. Captured group is split on comma and
trimmed (handles trailing space before --> in HTML form). Six tests
include the two documented bypass attempts (URL-path 'allow-domain'
substring; pathological host literally named 'allow-domain')."
```

### Task 3.7: Implement `scan_line` (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

`scan_line` returns **two** things: the violations and an
"unused suppression" report. Per spec §"Per-Line Suppression":
"Each host listed must actually match a violation on that line; if a
listed host does not appear among the line's violations, a warning
is emitted (stderr) but the suppression for matched hosts still
applies." The unused list is what the caller emits as the stderr
warning.

- [ ] **Step 1: Write failing tests**

Append:

```rust
/// One reported violation on a scanned line.
#[derive(Debug, PartialEq, Eq)]
pub struct LineViolation {
    pub host: String,
}

/// Result of scanning one source line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LineScanOutcome {
    pub violations: Vec<LineViolation>,
    /// Hosts that the line's `allow-domain:` marker listed but that
    /// did not appear among the extracted hosts. Caller emits these
    /// as a stderr warning ("listed in allow-domain marker but no
    /// matching host on the line").
    pub unused_suppressions: Vec<String>,
}

/// Scan one source line; return violations and any unused
/// suppression-marker entries.
pub fn scan_line(line: &str) -> LineScanOutcome {
    todo!("collect absolute + protocol-relative hosts, apply suppression, \
           filter via is_allowed, compute unused = listed - extracted")
}

#[cfg(test)]
mod scan_line_tests {
    use super::*;

    fn hosts(line: &str) -> Vec<String> {
        scan_line(line).violations.into_iter().map(|v| v.host).collect()
    }

    fn unused(line: &str) -> Vec<String> {
        let mut u = scan_line(line).unused_suppressions;
        u.sort();
        u
    }

    #[test]
    fn allowed_passes_clean() {
        for line in [
            "see https://example.com",
            "see https://foo.example.com",
            "see https://api.privacy-center.org",
            "dial http://127.0.0.1:8080/",
            "see https://github.com/x/y",
            "see https://testlight.example",
            "//www.googletagmanager.com/gtm.js",
        ] {
            assert!(hosts(line).is_empty(), "should be clean: {line}");
        }
    }

    #[test]
    fn disallowed_reports() {
        assert_eq!(hosts("see https://test.com"), vec!["test.com"]);
        assert_eq!(hosts("see https://partner.com"), vec!["partner.com"]);
    }

    #[test]
    fn suppression_with_correct_host_passes() {
        let out = scan_line("https://evil.com // allow-domain: evil.com");
        assert!(out.violations.is_empty());
        assert!(out.unused_suppressions.is_empty());
    }

    #[test]
    fn suppression_with_wrong_host_still_reports_and_warns() {
        let out = scan_line("https://evil.com // allow-domain: other.com");
        assert_eq!(
            out.violations.into_iter().map(|v| v.host).collect::<Vec<_>>(),
            vec!["evil.com"]
        );
        assert_eq!(
            out.unused_suppressions, vec!["other.com"],
            "other.com was listed but never appeared on the line"
        );
    }

    #[test]
    fn multi_host_suppression_applied_to_violations() {
        // Spec §"Per-line suppression" — multiple comma-separated
        // hosts; all are suppressed when they match extracted hosts.
        let out = scan_line(
            "x = \"https://evil.com\"; y = \"https://bad.org\"; \
             // allow-domain: evil.com, bad.org"
        );
        assert!(out.violations.is_empty(), "both hosts should be suppressed: {out:?}");
        assert!(out.unused_suppressions.is_empty());
    }

    #[test]
    fn multi_host_suppression_partial_match_warns_for_unused() {
        // evil.com matches; ghost.com does not appear on the line.
        let out = scan_line("\"https://evil.com\" // allow-domain: evil.com, ghost.com");
        assert!(out.violations.is_empty(), "evil.com should be suppressed");
        assert_eq!(out.unused_suppressions, vec!["ghost.com"]);
    }

    #[test]
    fn jsdoc_star_suppression_form() {
        // Spec §"Marker grammar" — '*' followed by whitespace is one
        // of the four supported comment-introducer branches.
        // Format: a jsdoc/block-comment continuation line where the
        // marker is adjacent to '* '.
        let out = scan_line(
            " * fetch(\"https://evil.com\") * allow-domain: evil.com"
        );
        assert!(out.violations.is_empty(), "jsdoc-style suppression should apply: {out:?}");
    }

    #[test]
    fn multiple_disallowed_on_one_line() {
        let got = hosts(
            "<a href=\"https://test.com\">x</a><a href=\"https://partner.com\">y</a>",
        );
        assert_eq!(got, vec!["test.com", "partner.com"]);
    }

    #[test]
    fn bypass_attempt_reports() {
        // fetch("https://evil.com/allow-domain") — substring inside URL,
        // not a comment, so suppression does NOT apply.
        assert_eq!(
            hosts("fetch(\"https://evil.com/allow-domain\")"),
            vec!["evil.com"]
        );
    }

    #[test]
    fn unused_warning_only_when_marker_present() {
        // No marker → no unused warning, even though "other.com" does
        // not appear in any line we scanned.
        let out = scan_line("see https://example.com");
        assert!(out.unused_suppressions.is_empty());
    }

    #[test]
    fn unused_warning_fires_for_already_allowed_listed_host() {
        // Spec §"Per-Line Suppression": listed host must match a
        // VIOLATION, not just an extracted host. example.com is
        // extracted but is already allowed → would never have been
        // a violation → the marker entry was unnecessary → warn.
        let out = scan_line("see https://example.com // allow-domain: example.com");
        assert!(out.violations.is_empty(), "example.com is already allowed");
        assert_eq!(
            out.unused_suppressions, vec!["example.com"],
            "marker listed an already-allowed host; it suppresses nothing"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::scan_line_tests`
Expected: 11 FAIL (one per `#[test]`).

- [ ] **Step 3: Implement**

```rust
pub fn scan_line(line: &str) -> LineScanOutcome {
    let suppression = parse_suppression_marker(line);
    let mut hosts = extract_absolute_hosts(line);
    hosts.extend(extract_protocol_relative_hosts(line));

    // Compute the set of hosts that WOULD be flagged WITHOUT any
    // suppression — i.e., extracted hosts that fail the allowlist
    // check when the suppression set is empty. Per spec
    // §"Per-Line Suppression": the allow-domain marker's job is to
    // suppress violations. A listed host that wasn't going to be a
    // violation anyway (already allowed, or not extracted at all)
    // is "unused" and warrants the stderr warning.
    let empty_suppression: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let disallowed_without_suppression: std::collections::HashSet<&String> = hosts
        .iter()
        .filter(|h| !is_allowed(h, &empty_suppression))
        .collect();

    let mut unused: Vec<String> = suppression
        .suppressed
        .iter()
        .filter(|listed| {
            !disallowed_without_suppression
                .iter()
                .any(|h| h.as_str() == listed.as_str())
        })
        .cloned()
        .collect();
    unused.sort();

    let violations = hosts
        .into_iter()
        .filter(|h| !is_allowed(h, &suppression.suppressed))
        .map(|host| LineViolation { host })
        .collect();

    LineScanOutcome {
        violations,
        unused_suppressions: unused,
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev::lint::domains::scan_line_tests`
Expected: 11 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Add scan_line returning violations + unused-suppression report

Composes parse_suppression_marker + extract_absolute_hosts +
extract_protocol_relative_hosts + is_allowed. The LineScanOutcome
struct carries both the violation list AND the 'unused suppression'
list per spec §'Per-Line Suppression' — listed hosts that would
not have been a violation in the first place (already allowed, or
not extracted at all) are surfaced for the caller to emit as
stderr warnings. Eleven tests cover: allowed-pass,
disallowed-report, single-host suppression match, wrong-host
warning, multi-host full-match, multi-host partial-match warning,
jsdoc/* form, multi-violation-per-line, URL-content bypass attempt,
no-marker-no-warning, and the already-allowed-host-listed case."
```

---

## Phase 4: Diff and path collectors

Spec §"Line collection: --staged mode", §"Line collection: --changed-vs", §"Line collection: full-repo", §"Line collection: explicit paths".

Each task in this phase pulls the gix entry points from the Phase 2 spike tests and wraps them in production helpers under `dev/lint/domains.rs`. Re-read the spike test bodies before implementing.

**Tests live as inline `#[cfg(test)] mod tests` blocks inside `dev/lint/domains.rs`, NOT as files under `crates/trusted-server-cli/tests/`.** Reason: `lib.rs` declares `mod dev;` (private), so integration tests under `tests/` cannot reach `trusted_server_cli::dev::lint::domains::staged_added_lines` or any other path inside the crate. Inline tests get full access to the private/`pub(crate)` items. End-to-end binary-level tests (Phase 7) belong in `tests/` because they call `Command::cargo_bin("ts")`.

A shared helper module for git-repo fixtures lives at `dev/lint/test_support.rs` and is gated `#[cfg(test)]`. Copy the `commit_all` / `stage_all` / branch helpers proven in the Phase 2 spike tests into it (the spike tests stay where they are; this file is the production-quality version of those helpers).

### Task 4.0: Extract git-fixture helpers into a shared `test_support` module

**Files:**
- Create: `crates/trusted-server-cli/src/dev/lint/test_support.rs`
- Modify: `crates/trusted-server-cli/src/dev/lint/mod.rs`

**Critical: helper commits MUST set explicit author/committer
signatures, not rely on ambient git config.** A clean test
environment (CI runner, container, fresh machine without
`user.name` / `user.email` set globally) will fail with "please tell
me who you are" or produce nondeterministic timestamps. Pin a fixed
signature in the helpers so tests are deterministic and don't depend
on the host's git config.

- [ ] **Step 1: Create `dev/lint/test_support.rs`**

Lift the helper functions from `tests/spike_gix_staged_diff.rs` and `tests/spike_gix_changed_vs.rs` (the production-quality versions, not the `unimplemented!()` shells). Signatures:

```rust
#![cfg(test)]

use std::path::Path;

use gix::ObjectId;

/// Fixed test signature used for all helper commits — avoids
/// dependence on ambient `user.name` / `user.email` config and
/// keeps commit hashes stable across runs.
pub(crate) fn test_signature() -> gix::actor::SignatureRef<'static> {
    gix::actor::SignatureRef {
        name: "ts dev lint tests".into(),
        email: "tests@example.com".into(),
        time: gix::date::Time::new(1_700_000_000, 0).into(),
    }
}

pub(crate) fn init_repo(path: &Path) -> gix::Repository { /* ... */ }
pub(crate) fn commit_all(repo: &gix::Repository, msg: &str) -> ObjectId { /* ... */ }
pub(crate) fn stage_all(repo: &gix::Repository) { /* ... */ }
pub(crate) fn create_and_checkout_branch(repo: &gix::Repository, branch: &str) { /* ... */ }
pub(crate) fn commit_all_as_branch(repo: &gix::Repository, branch: &str, msg: &str) -> ObjectId { /* ... */ }
```

`commit_all` and `commit_all_as_branch` MUST pass `test_signature()`
(or equivalent) as both author and committer when calling gix's
commit-creation API — do not let gix fall back to environment /
git-config lookups. If the pinned gix version's exact SignatureRef
shape differs from the sketch above, adjust the helper to whatever
the pinned API requires, but the fixed-signature principle is
non-negotiable.

- [ ] **Step 2: Wire the module**

In `dev/lint/mod.rs`, add:

```rust
#[cfg(test)]
pub(crate) mod test_support;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/test_support.rs crates/trusted-server-cli/src/dev/lint/mod.rs
git commit -m "Add dev/lint/test_support: shared git fixtures for module tests

Lifts the working gix helper bodies from tests/spike_gix_*.rs into
a #[cfg(test)] pub(crate) module that the inline #[cfg(test)] mod
tests blocks in domains.rs (Phase 4) can use. The spike tests
themselves stay in tests/ and continue to drive their unimplemented
stubs through the pinned implementations."
```

### Task 4.1: `staged_added_lines` (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

**Path representation for staged diffs.** `gix` returns diff entry
paths as `BString` (byte strings). `DiffLine::path` is a `PathBuf`,
which on Unix is an `OsString` byte container — so byte sequences
that are not valid UTF-8 are still valid paths there. The
implementation must:

- For valid UTF-8 paths: convert directly via `std::str::from_utf8`
  → `PathBuf`. Normal path.
- For non-UTF-8 paths in `--staged` mode (per spec test 25 and
  spec §"Note on non-UTF-8 paths"): **report normally with a stderr
  warning that the path is being displayed lossy-UTF-8.** This
  intentionally differs from full-repo mode (case 4 in spec
  §"Handling tracked-but-missing files and symlinks"), which
  skips non-UTF-8 entries. Construct the `PathBuf` via
  `String::from_utf8_lossy` (replacement chars in the display name
  are acceptable — host extraction runs against blob content, not
  the path) and emit a stderr warning via
  `crate::output::write_stderr_line` naming the lossy path.

This applies to `--changed-vs` mode as well (same blob-content
scanning model). Full-repo mode is the only place we skip — see
Task 4.3.

- [ ] **Step 1: Write a failing inline test inside `dev/lint/domains.rs`**

In the existing `#[cfg(test)] mod tests` block (the same one with the URL extraction and scan_line tests), append:

```rust
mod staged_added_lines_tests {
    use super::*;
    use crate::dev::lint::test_support;

    #[test]
    fn reports_added_line_with_new_side_line_number() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        std::fs::write(temp.path().join("a.txt"), "alpha\nbeta\ngamma\n")
            .expect("should write initial file");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "initial");

        std::fs::write(temp.path().join("a.txt"), "alpha\nNEW LINE\nbeta\ngamma\n")
            .expect("should write modification");
        test_support::stage_all(&repo);

        let lines = staged_added_lines(temp.path()).expect("should collect staged lines");
        let added: Vec<_> = lines
            .iter()
            .map(|l| (l.path.to_string_lossy().into_owned(), l.line_no, l.content.clone()))
            .collect();

        assert_eq!(added, vec![("a.txt".to_string(), 2, "NEW LINE".to_string())]);
    }

    /// Spec test case 25: staged scan must NOT skip non-UTF-8 paths
    /// (full-repo mode skips them; staged reports lossy + warning).
    #[cfg(unix)]
    #[test]
    fn reports_non_utf8_staged_path_lossy() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());

        // Initial commit so HEAD exists.
        std::fs::write(temp.path().join("readme.txt"), "hi\n")
            .expect("should write readme");
        test_support::stage_all(&repo);
        test_support::commit_all(&repo, "initial");

        // Add a file with a non-UTF-8 component, containing a
        // disallowed URL.
        let non_utf8_name = std::ffi::OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f, 0x2e, 0x72, 0x73]); // f, o, 0xff, o, ., r, s
        let bad_file = temp.path().join(non_utf8_name);
        std::fs::write(&bad_file, "let x = \"https://test.com\";\n")
            .expect("should write non-utf8-named file");
        test_support::stage_all(&repo);

        let lines = staged_added_lines(temp.path())
            .expect("should collect staged lines even with non-UTF-8 path");
        // Expect exactly one DiffLine for the bad file's added line.
        // The path displays with a replacement char, but the line is
        // reported (NOT skipped).
        let added_lines: Vec<_> = lines.iter().collect();
        assert!(
            !added_lines.is_empty(),
            "non-UTF-8 staged paths must be reported, not skipped"
        );
        // The content must be the original added line, byte-faithful.
        assert!(
            added_lines.iter().any(|l| l.content.contains("https://test.com")),
            "must surface the URL for scanning: {added_lines:?}"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure** (function doesn't exist yet)

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- staged_added_lines_tests`
Expected: FAIL with `cannot find function staged_added_lines in this scope`.

- [ ] **Step 3: Implement `staged_added_lines` in `dev/lint/domains.rs`**

Function signature:

```rust
#[derive(Debug)]
pub(crate) struct DiffLine {
    /// Path for display and reporting. Built via `String::from_utf8_lossy`
    /// for non-UTF-8 sources (see Task 4.1 notes on path representation).
    pub path: std::path::PathBuf,
    pub line_no: usize,
    pub content: String,
}

pub(crate) fn staged_added_lines(
    repo_path: &std::path::Path,
) -> Result<Vec<DiffLine>, error_stack::Report<DomainsLintError>>
```

Body: open repo, get HEAD tree, get index, run index-vs-tree diff using the entry points pinned in Phase 2 step 2.3, filter changed paths through `path_is_scanned()` (Task 4.5 dependency — define a stub returning `true` for now and refine later), run blob diff per changed entry, collect added-line hunks.

Path conversion: for each gix `BString` entry path,

```rust
let (path, was_lossy) = match std::str::from_utf8(raw_bytes) {
    Ok(s) => (std::path::PathBuf::from(s), false),
    Err(_) => {
        let lossy = String::from_utf8_lossy(raw_bytes).into_owned();
        (std::path::PathBuf::from(&lossy), true)
    }
};
if was_lossy {
    // `warn` is the in-module helper defined alongside
    // DomainsLintError; it returns Report<DomainsLintError> so the
    // `?` here flows correctly out of staged_added_lines.
    warn(format!(
        "warning: staged path is not valid UTF-8; displaying lossy: {}",
        path.display()
    ))?;
}
```

`pub(crate)` (not `pub`) is appropriate — the function is exercised through inline tests and the in-crate `domains::run` caller; no external API surface.

- [ ] **Step 4: Run to verify pass.**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- staged_added_lines_tests`
Expected: PASS (both the normal case and the non-UTF-8 case).

- [ ] **Step 5: Commit.**

### Task 4.2: `changed_vs_added_lines` with base-ref resolution (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing inline tests**

In the same module-level test mod, append a new `mod changed_vs_tests { ... }` with two cases:

1. Two-branch fixture (`main` with base commit, `feature` with an additional commit adding `https://test.com` to a file). Assert `changed_vs_added_lines(repo_path, "main")` returns exactly one `DiffLine` with the new content.
2. Ref-resolution fallback: rename the local `main` ref to `refs/remotes/origin/main` (use gix to manipulate refs in the fixture) and assert `changed_vs_added_lines(repo_path, "main")` still resolves and returns the same result via the fallback chain.

Use `tempfile::tempdir().expect("should create tempdir")` and the `test_support` helpers; every `expect()` message follows the `should ...` convention.

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement `changed_vs_added_lines`** in `dev/lint/domains.rs`. Pull merge-base + tree-vs-tree from Phase 2 step 2.3. Include the `resolve_base_ref` helper that tries the four candidates from the spec (`<ref>`, `refs/heads/<ref>`, `refs/remotes/origin/<ref>`, `refs/tags/<ref>`) in order and returns the first match.

Signature: `pub(crate) fn changed_vs_added_lines(repo_path: &Path, reference: &str) -> Result<Vec<DiffLine>, Report<DomainsLintError>>`

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 4.3: `full_repo_lines` with edge-case handling (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing inline tests** (`mod full_repo_tests`) for each of the five edge cases in spec §"Handling tracked-but-missing files and symlinks":
  1. Tracked-but-missing file → warns and skips.
  2. Symlink → warns and skips ("symlink not followed").
  3. Non-regular file (`#[cfg(unix)]` — mkfifo via `nix` or shell-equivalent; if too painful, gate this case behind `#[cfg(feature = "fifo-test")]` and skip in CI).
  4. Non-UTF-8 path component (Unix-only — create via `std::os::unix::ffi::OsStrExt::from_bytes(&[0xff, 0xfe])`).
  5. Binary file (`.json` with embedded NUL — write `b"{\"x\": \0null}"`).

Each test asserts the audit proceeds to the next entry; the function returns `Ok(Vec<DiffLine>)` with no entries for the skipped file. (Test the stderr warning indirectly by ensuring no violation is reported for the problematic path; full stderr-capture tests happen in Phase 7 via `assert_cmd`.)

Use `expect("should ...")` throughout.

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement `full_repo_lines`** per the spec pseudocode. The `warn_skip(path, reason)` / `warn_skip_bytes(bytes, reason)` helpers wrap the in-module `warn` helper (defined alongside `DomainsLintError`), which itself wraps `crate::output::write_stderr_line` with `change_context(DomainsLintError::WriteWarning)`. Do NOT call `write_stderr_line` directly — the type would not unify with `Report<DomainsLintError>` and the `?` operator would fail to compile.

Signature: `pub(crate) fn full_repo_lines(repo_path: &Path) -> Result<Vec<DiffLine>, Report<DomainsLintError>>`

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 4.4: `explicit_path_lines` with the soft/hard split (TDD)

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Write failing inline tests** (`mod explicit_path_tests`):
  1. Existing valid file → reports violations from it normally.
  2. Path with an excluded extension (`.html`) → warns and skips, returns empty `Vec`.
  3. Path under `node_modules/` → warns and skips.
  4. Symlink → warns and skips.
  5. Missing path (typo) → returns `Err(...)` whose `current_context()` is `DomainsLintError::PathNotFound`.
  6. Permission-denied path (`#[cfg(unix)]` only — use `chmod 000` on a tempfile) → returns `Err(DomainsLintError::PermissionDenied)`.

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement `explicit_path_lines`** per the spec pseudocode. Policy filters use `warn_skip`; access failures return `Err`. Map `io::ErrorKind::NotFound` → `DomainsLintError::PathNotFound`, `io::ErrorKind::PermissionDenied` → `DomainsLintError::PermissionDenied`.

Signature: `pub(crate) fn explicit_path_lines(paths: &[PathBuf]) -> Result<Vec<DiffLine>, Report<DomainsLintError>>`

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 4.5: `path_is_scanned` policy helper (TDD)

- [ ] **Step 1: Write failing tests** for the extension and path-exclusion filter:
  - `foo.rs` → scanned.
  - `foo.html` → **scanned** (extension list now includes `.html`).
  - `foo.css` → **scanned** (extension list now includes `.css`).
  - `Dockerfile` → **scanned** (matched by exact basename).
  - `Dockerfile.prod` → **scanned** (matched by `Dockerfile.*` pattern).
  - `crates/trusted-server-core/src/integrations/nextjs/fixtures/inlined-data-escaped.html` → **NOT scanned** (publisher-fixture path exclusion — spec §"Always excluded (paths)").
  - `crates/trusted-server-core/src/integrations/google_tag_manager/fixtures/captured.html` → **NOT scanned** (same publisher-fixture rule, different integration).
  - `crates/trusted-server-core/src/html_processor.test.html` → **scanned** (NOT under a `/fixtures/` directory; this is our own test fixture, not a publisher capture).
  - `crates/js/lib/src/core/templates/iframe.html` → **scanned** (our own template).
  - `node_modules/foo.js` → not scanned (path exclusion).
  - `.worktrees/x/y.rs` → not scanned.
  - `package-lock.json` → not scanned.
  - `pnpm-lock.yaml` → not scanned (exact basename match).
  - `Cargo.lock` → not scanned.
  - `.env.dev` → scanned (matches `.env*`).
  - `crates/integration-tests/fixtures/frameworks/nextjs/app/page.tsx` → scanned (proves the **/fixtures/** blanket exclusion was removed; only the narrow `crates/trusted-server-core/src/integrations/**/fixtures/**` path is excluded).
  - `crates/integration-tests/fixtures/frameworks/nextjs/Dockerfile` → **scanned** (Dockerfile matched by basename; this fixture path is NOT the excluded publisher-capture path).
  - `crates/integration-tests/fixtures/frameworks/wordpress/Dockerfile` → **scanned** (same reasoning).
  - `crates/trusted-server-cli/src/dev/lint/domains.rs` → NOT scanned (self-exclude).
  - **Markdown coverage (spec §"File extensions scanned" mandates `.md` is in scope):**
    - `README.md` → scanned.
    - `CHANGELOG.md` → scanned.
    - `CONTRIBUTING.md` → scanned.
    - `docs/guide/onboarding.md` → scanned.
    - `docs/superpowers/specs/2026-05-18-check-domains-design.md` → scanned (spec itself is in scope).
    - `foo.markdown` → NOT scanned (only `.md` is in the extension list, not `.markdown`).
    - `foo.MD` → NOT scanned (case-sensitive extension match per Rust conventions; if a contributor uses uppercase, they get a warning at scan time, not a silent skip — document this as a known limitation if `.MD` files appear in real PRs).

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement `path_is_scanned(rel_path: &[u8]) -> bool`** with the constants from spec §"File extensions scanned" and §"Always excluded (paths)".

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

---

## Phase 5: CLI exit-code wiring + `dev lint domains` subcommand

Spec §"CLI Surface" and §"Required change to existing CLI exit-code mapping".

### Task 5.1: Extend `CliError` with `EnvironmentError` and `ViolationsFound`

**Files:**
- Modify: `crates/trusted-server-cli/src/error.rs`

- [ ] **Step 1: Add the two variants**

Add to the enum in `error.rs`:

```rust
    #[display("environment error")]
    EnvironmentError,
    #[display("found {count} disallowed host(s)")]
    ViolationsFound { count: usize },
```

- [ ] **Step 2: Update `lib.rs::run()` to map them**

The existing implementation prints `format_report(&error)` for
EVERY error, then maps the exit code. That model collapses two
different user experiences: a real failure (`EnvironmentError`,
`Configuration`, etc.) deserves the error-stack dump, but
`ViolationsFound` and `Cancelled` should not — the violation
report itself is already on stdout (or JSON), and Cancelled is a
benign user signal. Printing `format_report` for `ViolationsFound`
would write the linter's normal output AND an error-stack message
on stderr, doubling the noise.

Replace the existing `match` body in `run()` with:

```rust
#[must_use]
pub fn run() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => match error.current_context() {
            CliError::Cancelled => ExitCode::from(130),
            CliError::ViolationsFound { .. } => ExitCode::from(1),
            CliError::EnvironmentError => {
                let _ = write_stderr_line(format_report(&error));
                ExitCode::from(2)
            }
            _ => {
                let _ = write_stderr_line(format_report(&error));
                ExitCode::from(1)
            }
        }
    }
}
```

Only the "real failure" branches print the error-stack report;
`ViolationsFound` and `Cancelled` exit silently (the violation
list and the cancellation are conveyed elsewhere). Matches the
spec's Output Format section, which shows the violation report
itself as the user-visible output.

- [ ] **Step 3: Build and verify existing tests still pass**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-cli/src/error.rs crates/trusted-server-cli/src/lib.rs
git commit -m "Add CliError::EnvironmentError and ViolationsFound; map exit codes

Required by spec §'Required change to existing CLI exit-code mapping'.
run() now maps Cancelled -> 130, ViolationsFound -> 1, EnvironmentError
-> 2, everything else -> 1 (unchanged). Distinguishes 'found a real
violation' from 'could not even run the scan' in CI logs."
```

### Task 5.2: Add `DevCommand::Lint` and `LintCommand::Domains` clap surface

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/mod.rs`
- Modify: `crates/trusted-server-cli/src/dev/lint/mod.rs`

- [ ] **Step 1: Add the nested clap types**

In `dev/lint/mod.rs`:

```rust
use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum LintCommand {
    /// Lint URL hosts in source/config/docs.
    Domains(DomainsArgs),
}

#[derive(Debug, Args)]
pub struct DomainsArgs {
    /// Pre-commit mode: scan only staged-added lines.
    #[arg(long, conflicts_with_all = ["changed_vs", "paths"])]
    pub staged: bool,

    /// CI/PR mode: scan only lines added relative to merge-base(<ref>, HEAD).
    #[arg(long, value_name = "REF", conflicts_with_all = ["staged", "paths"])]
    pub changed_vs: Option<String>,

    /// Explicit paths to scan (full file). Mutually exclusive with --staged / --changed-vs.
    #[arg(value_name = "PATH", conflicts_with_all = ["staged", "changed_vs"])]
    pub paths: Vec<PathBuf>,

    /// Output format. Default: human.
    #[arg(long, value_enum, default_value = "human")]
    pub format: OutputFormat,

    /// Verbose: print per-file scan progress on stderr (number of
    /// lines scanned per file, number of suppressed hosts per line).
    /// Off by default; useful for debugging "why was X not flagged"
    /// or "is this file being scanned at all". Has no effect on
    /// exit code or violation count.
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}
```

In `dev/mod.rs`, extend `DevCommand`:

```rust
pub enum DevCommand {
    Serve(ServeArgs),
    /// Linters for source/config/docs.
    Lint {
        #[command(subcommand)]
        command: lint::LintCommand,
    },
}
```

- [ ] **Step 2: Wire dispatch in `lib.rs`**

Update `run_dev`:

```rust
fn run_dev(command: dev::DevCommand) -> Result<(), Report<CliError>> {
    match command {
        dev::DevCommand::Serve(args) => run_dev_serve(&args),
        dev::DevCommand::Lint { command } => dev::lint::run(command),
    }
}
```

In `dev/lint/mod.rs`, add:

```rust
pub fn run(command: LintCommand) -> Result<(), error_stack::Report<crate::error::CliError>> {
    match command {
        LintCommand::Domains(args) => domains::run(args),
    }
}
```

In `dev/lint/domains.rs`, add the entry-point function:

```rust
pub fn run(args: crate::dev::lint::DomainsArgs)
    -> Result<(), error_stack::Report<crate::error::CliError>>
{
    todo!("dispatch on mode (staged | changed_vs | paths | full-repo); \
           call the appropriate collector; scan each line; emit report; \
           return Err(ViolationsFound) on violations, Err(EnvironmentError) on env errors")
}
```

- [ ] **Step 3: Verify build and `--help` surfaces are correct**

Run: `cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev lint --help`
Expected: lists `domains` as a subcommand.

Run: `cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev lint domains --help`
Expected: lists `--staged`, `--changed-vs`, `--format`, `--verbose`, plus the trailing `[PATH]...` arg.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-cli/src/dev/ crates/trusted-server-cli/src/lib.rs
git commit -m "Wire ts dev lint domains clap surface and dispatch

Adds DevCommand::Lint, LintCommand::Domains, DomainsArgs (with the
four mutually-exclusive mode flags). Body of domains::run is a
todo! to be replaced in the next commit; this commit just lands
the CLI scaffolding so --help works end-to-end."
```

### Task 5.3: Implement `domains::run` mode dispatch + reporting

**Files:**
- Modify: `crates/trusted-server-cli/src/dev/lint/domains.rs`

- [ ] **Step 1: Implement `domains::run`**

Replace the `todo!()` body with:

```rust
pub fn run(args: crate::dev::lint::DomainsArgs)
    -> Result<(), error_stack::Report<crate::error::CliError>>
{
    use error_stack::ResultExt;
    use crate::error::CliError;

    let cwd = std::env::current_dir().change_context(CliError::EnvironmentError)?;
    let lines: Vec<DiffLine> = if args.staged {
        staged_added_lines(&cwd).change_context(CliError::EnvironmentError)?
    } else if let Some(ref reference) = args.changed_vs {
        changed_vs_added_lines(&cwd, reference).change_context(CliError::EnvironmentError)?
    } else if !args.paths.is_empty() {
        explicit_path_lines(&args.paths).change_context(CliError::EnvironmentError)?
    } else {
        full_repo_lines(&cwd).change_context(CliError::EnvironmentError)?
    };

    let mut violations: Vec<FileViolation> = Vec::new();
    let mut last_verbose_path: Option<std::path::PathBuf> = None;
    let mut verbose_line_count: usize = 0;
    for line in lines {
        if args.verbose {
            // Tally per-file line counts for the end-of-file summary.
            match &last_verbose_path {
                Some(prev) if prev == &line.path => verbose_line_count += 1,
                _ => {
                    if let Some(prev) = last_verbose_path.take() {
                        crate::output::write_stderr_line(format!(
                            "scanned {} lines in {}",
                            verbose_line_count, prev.display()
                        ))?;
                    }
                    last_verbose_path = Some(line.path.clone());
                    verbose_line_count = 1;
                }
            }
        }
        let outcome = scan_line(&line.content);
        for unused in outcome.unused_suppressions {
            crate::output::write_stderr_line(format!(
                "warning: {}:{}: allow-domain marker listed `{}` but it does not appear on the line",
                line.path.display(), line.line_no, unused
            ))?;
        }
        for v in outcome.violations {
            violations.push(FileViolation {
                path: line.path.clone(),
                line: line.line_no,
                host: v.host,
                url_excerpt: line.content.clone(),
            });
        }
    }
    if let Some(prev) = last_verbose_path {
        // Flush the last file's tally.
        crate::output::write_stderr_line(format!(
            "scanned {} lines in {}",
            verbose_line_count, prev.display()
        ))?;
    }

    match args.format {
        crate::dev::lint::OutputFormat::Human => emit_human(&violations)?,
        crate::dev::lint::OutputFormat::Json => emit_json(&violations)?,
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(error_stack::Report::new(CliError::ViolationsFound {
            count: violations.len(),
        }))
    }
}

#[derive(Debug, serde::Serialize)]
pub struct FileViolation {
    pub path: std::path::PathBuf,
    pub line: usize,
    pub host: String,
    #[serde(rename = "url")]
    pub url_excerpt: String,
}

fn emit_human(violations: &[FileViolation])
    -> Result<(), error_stack::Report<crate::error::CliError>>
{
    use crate::output::write_stdout_line;

    for v in violations {
        write_stdout_line(format!(
            "{}:{}: disallowed host {}",
            v.path.display(), v.line, v.host
        ))?;
    }
    if !violations.is_empty() {
        let files: std::collections::BTreeSet<_> = violations.iter().map(|v| &v.path).collect();
        write_stdout_line("")?;
        write_stdout_line(format!(
            "{} disallowed host(s) found in {} file(s).",
            violations.len(),
            files.len()
        ))?;
        write_stdout_line(
            "To allow a new integration proxy, add it to EXACT_HOSTS in \
             crates/trusted-server-cli/src/dev/lint/domains.rs."
        )?;
        write_stdout_line(
            "To suppress one line (e.g., security tests), append \
             `// allow-domain: <host>` in a comment."
        )?;
        write_stdout_line("Run `ts dev lint domains` (no args) for a full-repo audit.")?;
    }
    Ok(())
}

fn emit_json(violations: &[FileViolation])
    -> Result<(), error_stack::Report<crate::error::CliError>>
{
    use crate::output::write_json;

    let files_affected: std::collections::BTreeSet<_> =
        violations.iter().map(|v| &v.path).collect();
    let report = serde_json::json!({
        "violations": violations,
        "count": violations.len(),
        "files_affected": files_affected.len(),
    });
    write_json(&report)
}
```

**No raw `println!` / `eprintln!` in production code.** The workspace
lints under `-D warnings` may not flag `println!` directly, but the
CLI's convention (see `crates/trusted-server-cli/src/config.rs`) is
to route all stdout through `crate::output::write_stdout_line` /
`write_json` and stderr through `write_stderr_line`. In
`domains::run` the return type is `Report<CliError>` so
`write_stderr_line(...)?` works directly. In the Phase 4
collectors (which return `Report<DomainsLintError>`), use the
in-module `warn(msg)` helper instead — it wraps
`write_stderr_line` with `change_context(DomainsLintError::WriteWarning)`
so the `?` operator type-checks.

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo check --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"`
Expected: PASS.

- [ ] **Step 3: Smoke-test in a throwaway tempdir, NOT the working repo**

Building and running `ts dev lint domains --staged` directly in the
working checkout would (a) require staging a `https://test.com`
fixture file in this repo — easy to forget to revert — and (b)
report on the existing Stage 1 doc violations, drowning the
smoke-test output in noise. Use a throwaway tempdir instead:

```sh
TMPREPO="$(mktemp -d)"
( cd "$TMPREPO" && git init -q && \
  git config user.name 'smoke' && git config user.email 'smoke@example.com' && \
  echo 'fn ok() {}' > ok.rs && git add ok.rs && git commit -q -m initial && \
  echo 'let bad = "https://test.com";' > bad.rs && git add bad.rs )
TS_BIN="$(cargo build --quiet --package trusted-server-cli \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --message-format=json 2>/dev/null \
  | jq -r 'select(.executable != null and (.target.name == "ts")) | .executable' | tail -1)"
( cd "$TMPREPO" && "$TS_BIN" dev lint domains --staged ) ; rc=$?
echo "exit: $rc"
rm -rf "$TMPREPO"
```

Expected: prints `bad.rs:1: disallowed host test.com` (and the
summary lines) to stdout, then `exit: 1`. Clean exit code, no
artifacts left in the working repo.

If `jq` is unavailable, run `ts dev lint domains --staged` from the
already-installed `ts` binary (post `cargo install_cli`) instead of
extracting the path from `cargo build --message-format=json`.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-cli/src/dev/lint/domains.rs
git commit -m "Implement domains::run mode dispatch + human/JSON reporting

Routes --staged, --changed-vs, explicit paths, and full-repo to the
matching collector; scans each returned line via scan_line; emits a
human or JSON report; returns Err(ViolationsFound { count }) on
violations, Err(EnvironmentError) on collector failures. Exit codes
flow through the run() match arm added in the previous CliError
extension."
```

---

## Phase 6: `ts dev install-hooks`

Spec §"Pre-commit hook", §"Hook installer (Rust subcommand)", and §"Persisting `core.hooksPath`".

### Task 6.1: `shell_quote` helper (TDD)

- [ ] **Step 1: Write failing tests** for: simple path, path with spaces, path with a single quote, path with `$`, path with backticks, path with backslashes. Each test asserts the output is wrappable by `bash -c "<output>"` without misbehaving (verify via a temp bash invocation).

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement** per the spec snippet (POSIX single-quote escaping).

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 6.2: `render_hook` + `is_managed` (TDD)

- [ ] **Step 1: Write failing tests:**
  - `render_hook(Path::new("/Users/Alice Q/.cargo/bin/ts"))` produces a string containing `exec '/Users/Alice Q/.cargo/bin/ts' dev lint domains --staged` and the `# ts-install-hooks: managed` marker line.
  - `is_managed` returns `true` on a file containing the marker line in its first 10 lines, `false` otherwise.

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement** both functions per spec.

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 6.3: `write_atomic` helper (TDD)

- [ ] **Step 1: Write failing test:** in a tempdir, call `write_atomic(path, b"hello")`; assert `fs::read(path).expect("should read written file") == b"hello"`; assert no `path.tmp.*` file remains in the directory. **Do not use `.unwrap()`** — workspace clippy denies `unwrap_used`.

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement:** write to `path.with_extension("tmp.{rand}")`, then `rename` to `path`. Use a small random suffix from `std::time::SystemTime` or `process::id()` to avoid collision on parallel installs.

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 6.4: `set_local_config_value` + `read_local_config_value` (production versions)

- [ ] **Step 1: Lift the spike helpers from `tests/spike_gix_config_write.rs`** into `crates/trusted-server-cli/src/dev/install_hooks.rs` (new file). Adjust signatures to take `&gix::Repository` and return `error_stack::Report<InstallHooksError>` per the spec sketch.

- [ ] **Step 2: Define the `InstallHooksError` enum** with variants `OpenRepo`, `NoWorkdir`, `CurrentExe`, `WriteHook`, `ConfigWrite`, `WouldClobber { path }`, `ForeignHooksPath { current, proposed }`.

- [ ] **Step 3: Write unit tests** for both helpers using a tempdir repo. Assert read returns `None` when unset, returns `Some(value)` after a write, and the on-disk `.git/config` contains a `[core]` section with `hooksPath` after the write.

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 6.5: `install_hooks` main function with preflight + clobber detection (TDD)

- [ ] **Step 1: Write failing end-to-end tests:**
  - Fresh repo, no `.githooks/`, no `core.hooksPath`: `install_hooks(force=false)` writes the hook, sets `core.hooksPath = .githooks`, succeeds.
  - Re-run on the same repo: idempotent, succeeds.
  - Pre-existing `.githooks/pre-commit` with the managed marker: silently overwritten, succeeds.
  - Pre-existing `.githooks/pre-commit` WITHOUT the marker: `install_hooks(force=false)` returns `Err(WouldClobber)`.
  - Same as above with `force=true`: backs up to `.githooks/pre-commit.bak.<timestamp>`, succeeds.
  - Pre-existing `core.hooksPath = hooks` (foreign): `install_hooks(force=false)` returns `Err(ForeignHooksPath)`.
  - Same as above with `force=true`: succeeds, prints the displaced value with the restore command.

- [ ] **Step 2: Verify failure.**

- [ ] **Step 3: Implement `install_hooks`** per the spec pseudocode.

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Commit.**

### Task 6.6: Wire `dev install-hooks` into the CLI

- [ ] **Step 1: Add the clap variant**

In `dev/mod.rs`:

```rust
pub enum DevCommand {
    Serve(ServeArgs),
    Lint { #[command(subcommand)] command: lint::LintCommand },
    /// Install the pre-commit hook into this repo (one-time setup).
    InstallHooks(InstallHooksArgs),
}

#[derive(Debug, Args)]
pub struct InstallHooksArgs {
    /// Overwrite an existing unmanaged hook or non-default core.hooksPath.
    #[arg(long)]
    pub force: bool,
}
```

- [ ] **Step 2: Wire dispatch in `lib.rs`**

Add to `run_dev`:

```rust
dev::DevCommand::InstallHooks(args) => dev::install_hooks::run(&args),
```

- [ ] **Step 3: Add `install_hooks::run` wrapper** that maps `InstallHooksError` → `CliError` (`ForeignHooksPath` and `WouldClobber` map to `CliError::EnvironmentError`; other variants map to `CliError::EnvironmentError` too — every install-hooks failure is by definition an env-config issue).

- [ ] **Step 4: Verify build and `--help`**

Run: `cargo run --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" -- dev install-hooks --help`
Expected: shows `--force`.

- [ ] **Step 5: Smoke-test in a tempdir repo end-to-end**

Run:

```sh
mkdir -p /tmp/ts-install-hooks-smoke && cd /tmp/ts-install-hooks-smoke
git init
ts dev install-hooks
test -x .githooks/pre-commit && grep -q 'ts-install-hooks: managed' .githooks/pre-commit
grep -A1 'hooksPath' .git/config
```

Expected: hook file exists, is executable, contains the
`# ts-install-hooks: managed` marker; `.git/config` shows
`hooksPath = .githooks` under `[core]`. (`git init` is intentional —
`gix` is a Rust crate dependency, not a shell command the
contributor can rely on having installed.)

- [ ] **Step 6: Commit.**

---

## Phase 7: End-to-end CLI tests via `assert_cmd`

Spec §"Testing Strategy" enumerates 47 cases. Phases 3, 4, and 6 covered the unit-level cases. This phase covers the remaining `assert_cmd` end-to-end cases — those that exercise the binary as a whole.

### Task 7.1: Add `assert_cmd` and `predicates` dev-dependencies

- [ ] **Step 1: Add to `[dev-dependencies]` in `crates/trusted-server-cli/Cargo.toml`:**

```toml
assert_cmd = "2"
predicates = "3"
```

- [ ] **Step 2: Commit.**

### Task 7.2: End-to-end tests for `--staged` mode (spec cases 21–26)

- [ ] Implement each case as a `#[test]` in `crates/trusted-server-cli/tests/lint_domains_cli.rs`. Each test builds a tempdir repo, invokes `Command::cargo_bin("ts").args(["dev", "lint", "domains", "--staged"]).current_dir(&tempdir)`, asserts on exit code + stdout + stderr.

- [ ] Each case gets its own task step: write failing test → verify failure → confirm production code already passes it → commit.

- [ ] **Spec case 25 (non-UTF-8 staged path) requires an explicit stderr assertion** in addition to the exit-code and stdout checks. The inline Task 4.1 test proves the path is not skipped; the Phase 7 E2E test must additionally assert that stderr contains the lossy-path warning string (`"staged path is not valid UTF-8; displaying lossy:"` or whatever exact phrasing Task 4.1's implementation lands on). Example assertion using `predicates`:

  ```rust
  use predicates::prelude::*;
  // ... build a tempdir repo, stage a file with a 0xff byte in the
  // name containing https://test.com ...
  Command::cargo_bin("ts")
      .expect("should find ts binary")
      .args(["dev", "lint", "domains", "--staged"])
      .current_dir(&tempdir)
      .assert()
      .code(1)
      .stdout(predicate::str::contains("disallowed host test.com"))
      .stderr(predicate::str::contains("not valid UTF-8"));
  ```

  This locks the staged non-UTF-8 reporting contract at the E2E layer so a future refactor cannot silently start skipping these paths.

### Task 7.3: End-to-end tests for `--changed-vs` mode (spec cases 27–29)

- [ ] Same pattern as 7.2, with two-commit branch fixtures.

### Task 7.4: End-to-end tests for path-exclusion (spec cases 30–34) and markdown (35–43)

- [ ] Same pattern. Markdown cases use `.md` fixtures with the various forms (allowed/disallowed link, autolink, HTML comment suppression, fenced block, reference list, image link).

### Task 7.5: End-to-end environment cases (spec 44–47)

- [ ] Test 44: run outside a git repo → exit 2 with `EnvironmentError`.
- [ ] Test 45: bare repo → exit 2.
- [ ] Test 46: run under `env -i PATH=""` → still works (proves no `git` binary needed). On non-Unix CI lanes this test is `#[cfg(unix)]`.
- [ ] Test 47: run the full test suite via `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"` — already covered by the host-target CI lane introduced in PR #669.

- [ ] Final commit for Phase 7.

---

## Phase 8: Documentation

### Task 8.1: Update `CONTRIBUTING.md` with the install steps

- [ ] **Step 1: Add a "Local setup" subsection** documenting:

```markdown
### Pre-commit URL-host linter (`ts dev lint domains`)

One-time setup after cloning:

```bash
cargo install_cli      # builds and installs the `ts` binary
ts dev install-hooks   # installs the pre-commit hook into .githooks/
```

After that, every `git commit` runs the linter against staged
changes. If you have an existing `core.hooksPath` (husky,
lefthook, etc.), `ts dev install-hooks` refuses to overwrite it
without `--force`. See `docs/superpowers/specs/2026-05-18-check-domains-design.md`
for the full design.

To bypass the hook for a single commit: `git commit --no-verify`.
```

- [ ] **Step 2: Commit.**

### Task 8.2: Update `README.md` with a brief mention

- [ ] **Step 1: Under any "Development" section in the project README**, add a one-line mention pointing at `CONTRIBUTING.md` for the linter setup.

- [ ] **Step 2: Commit.**

---

## Phase 9: Final verification

### Task 9.1: Run all CI gates locally

CLAUDE.md splits clippy and test into separate wasm-runtime and
host-target CLI lanes (per PR #669's CI changes). Use the split
commands; **do NOT use the older single `cargo clippy --workspace`
form** — it doesn't match what CI runs and will give a misleading
green when the host-target CLI has warnings.

- [ ] `cargo fmt --all -- --check` → PASS
- [ ] `cargo clippy --workspace --exclude trusted-server-cli --all-targets --all-features -- -D warnings` → PASS (wasm-runtime lane)
- [ ] `cargo clippy --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')" --all-targets -- -D warnings` → PASS (host-target CLI lane)
- [ ] `cargo test --workspace --exclude trusted-server-cli` → PASS (wasm-runtime lane)
- [ ] `cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"` → PASS (host-target lane, including the new lint module + spike + end-to-end tests)
- [ ] `cd crates/js/lib && npx vitest run` → PASS (unchanged)
- [ ] `cd crates/js/lib && npm run format` → PASS (unchanged)
- [ ] `cd docs && npm run format` → PASS (no doc changes that would fail formatting)

### Task 9.2: Self-dogfood the linter

**Exit-code expectations.** The linter is designed to find existing
violations in this repo (the Stage 1 cleanup target). Both commands
below are **expected to exit `1`** — this is not a failure of the
linter, it is the linter doing its job. Do not abort the
verification step on a non-zero exit here. The commands below are
written defensively for `set -e` / `pipefail` shells.

- [ ] **Step 1: Run `ts dev lint domains` against this very branch**

Run:

```sh
ts dev lint domains || rc=$?
echo "exit code: ${rc:-0}"
```

Expected: a list of existing violations on stdout, and `exit code: 1` printed at the end. **`exit 1` is the success condition for this step.** The output should look reasonable (well-formed `path:line:` lines). The violations themselves go into the Stage 1 Doc Cleanup Plan, not into this PR.

- [ ] **Step 2: Run the frequency report from the spec**

The JSON pipeline below uses `|| true` on the linter so the pipe
doesn't abort under `set -e` / `pipefail` when the linter exits 1
(by design — see Step 1).

```sh
(ts dev lint domains --format json || true) \
  | jq -r '.violations[].host' \
  | sort | uniq -c | sort -rn | head -30
```

Expected: a host-frequency table, top entries first. File the top entries into the Stage 1 Doc Cleanup Plan as a follow-up issue.

If `jq` is not installed, use the python3 alternative from spec §"Stage 1 Doc Cleanup Plan" — same `(... || true) | …` wrapping applies.

### Task 9.3: Push and open the PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feature/check-domains-spec
```

- [ ] **Step 2: Open the PR** with a title like "Add `ts dev lint domains` and `ts dev install-hooks`" and a body summarizing:
  - What it does (one paragraph)
  - Link to the design doc
  - Test plan checklist (the items from Task 9.1 + a manual `ts dev install-hooks` smoke test in a tempdir)
  - Note that the Stage 1 doc cleanup is a separate follow-up workstream

---

## Notes for the implementer

- Each phase's spec references are intentional — open the spec for the relevant section before writing code. The spec contains *why* in places where the plan only has *what*.
- The Phase 2 spike is the riskiest part. If it fails — e.g., the chosen `gix` version doesn't expose a stable tree-vs-tree diff entry point — stop and re-pin against a different release before proceeding. The downstream phases all depend on those API choices.
- `error-stack` usage follows the existing crate convention: `Report<CliError>` at the boundary, `change_context()` to map module-level errors. See PR #669's `config.rs` / `audit.rs` for examples.
- Commit early and often. Each task step that says "commit" is a real commit; don't batch.
- If a step's "expected" output doesn't match what you see, STOP. Don't ratchet through the failure — investigate and either fix the implementation or update the plan with a note about what the spec/spike missed.

# docs-parity

`docs-parity` is the repository-local host tool for deterministic documentation
records. It is an independent Cargo workspace and does not participate in the
repository's target-specific root workspace.

## Commands

Run the tool from any directory inside a Git worktree:

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- update \
  --tracked-paths-record path/to/record.txt
```

`check` discovers the worktree root, enumerates Git-tracked paths, and validates
the repository boundary. With `--tracked-paths-record`, it compares the named
repository-relative file with the deterministic, lexically sorted path record.
It never creates directories, creates files, or changes existing bytes.

`update` requires `--tracked-paths-record` and replaces that file atomically.
It writes and syncs a sibling stage file before a same-directory rename, then
syncs the containing directory. A failed or interrupted stage leaves the last
complete target unchanged. A stale stage file causes the update to fail closed
instead of overwriting an uncertain concurrent or interrupted update.

The exit-code contract is stable:

- `0`: the check is clean or the update completed;
- `1`: check mode found a missing or stale generated record;
- `2`: command syntax, repository safety, Git, or file I/O failed.

## Repository boundary

Generated paths must be normalized, non-empty relative paths. Absolute paths,
parent traversal, redundant components, platform-prefix shapes, and non-UTF-8
paths are rejected. Existing output symlinks are never replaced. Git-tracked
symlinks may resolve only to regular files inside the repository. Tracked files
and generation directories must not be group- or world-writable on Unix.

Tracked paths come exclusively from `git ls-files -z`, are validated before
use, and are sorted lexically before rendering. A second update with unchanged
Git index state therefore produces identical bytes.

## Development gates

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml --test cli
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
```

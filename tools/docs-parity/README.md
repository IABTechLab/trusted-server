# docs-parity

`docs-parity` is the repository-local host tool for deterministic documentation
records. It is an independent Cargo workspace and does not participate in the
repository's target-specific root workspace.

## Commands

The Cargo manifest path below is relative to the repository root, so run these
development commands from that root:

```bash
cargo run --manifest-path tools/docs-parity/Cargo.toml -- check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- update \
  --tracked-paths-record path/to/record.txt
cargo run --manifest-path tools/docs-parity/Cargo.toml -- classify --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- classify --update
cargo run --manifest-path tools/docs-parity/Cargo.toml -- scan --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- scan --bootstrap
```

The tool may also be launched from any nested worktree directory. Derive and
quote the exact Git root so Cargo receives an absolute manifest path:

```bash
repository_root="$(git rev-parse --show-toplevel)"
cargo run --manifest-path "$repository_root/tools/docs-parity/Cargo.toml" -- check
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

## Classification and privacy review

`classify --check` validates every `git ls-files -z` path against
`manifests/tracked-files.toml` and every expected-text path against
`manifests/maintained-sources.toml`. Text/binary classification is manifest
authority: invalid UTF-8 or oversized declared text fails rather than being
reclassified. Whole-file sources carry an include or typed exclude. Operational
sources carry a grammar and every extracted comment has an exact byte-range
selector, SHA-256 content fingerprint, and disposition.

`classify --update` is a candidate-generation workflow. It preserves only exact
reviewed records, adds new or moved paths/comments as unreviewed candidates,
and drops stale records. Review all differences before changing both manifests'
explicit `reviewed` field to true. Missing or false attestation fails closed.

`scan --check` scans every tracked text file, printable binary strings,
supported image metadata, and structured lockfile URL fields. Domains use the
compiled public-suffix dataset shipped by the standalone dependency, so checks
remain offline and reproducible. It also enforces the hashed retired identifier
and access-phrase records in `manifests/retired-identifiers.toml`. That manifest
also requires explicit review.

Every permitted finding has one occurrence record in
`manifests/sensitive-allowlist.toml`: a narrow exception class, exact path and
byte selector, content fingerprint, owner, rationale, and expiry. Check mode
fails at the expiry instant, on stale/moved records, on unsupported structured
data, or on findings without an exact exception. `scan --bootstrap` regenerates
candidates, preserves only exact reviewed matches, and reopens the complete set
for review when findings change. It never approves candidates. The five allowed
exception classes do not assert that mechanical detection is complete for
human semantic sensitivity.

## Repository boundary

Generated paths must be normalized, non-empty relative paths. Absolute paths,
parent traversal, redundant components, controls, backslashes, Windows-invalid
characters, trailing dots or spaces, reserved Windows device names, and
non-UTF-8 paths are rejected. A `.git` path component is rejected
case-insensitively; names such as `.gitignore` and `.gitmodules` remain valid.
Existing output symlinks, including dangling symlinks, are never replaced.
Git-tracked symlinks may resolve only to regular files inside the repository.
Tracked files and generation directories must not be group- or world-writable.

Tracked paths come exclusively from `git ls-files -z`, are validated before
use, and are sorted lexically before rendering. A second update with unchanged
Git index state therefore produces identical bytes.

## Supported hosts

The tool compiles only on Linux and macOS, matching the native CLI-capture and
enforcement matrix. Its atomic replacement and unsafe-mode contract depends on
same-directory Unix rename and Unix permission semantics. Other targets fail at
compile time; there is no non-atomic or mode-blind fallback.

## Development gates

```bash
cargo test --manifest-path tools/docs-parity/Cargo.toml --test cli
cargo test --manifest-path tools/docs-parity/Cargo.toml --test classification
cargo test --manifest-path tools/docs-parity/Cargo.toml --test scanner
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
```

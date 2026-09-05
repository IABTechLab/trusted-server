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
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check
cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --update
cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --local --check
# Scheduled or manual only; this command performs bounded network requests.
cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --external --check
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

## Generated regions and links

`generate --check` renders the typed records in `manifests/pages.toml` and
returns exit code `1` when a named Markdown region differs. Check mode performs
no writes. `generate --update` validates every target first, then atomically
replaces only drifted region bodies; exact bytes outside the paired markers are
preserved. Region names, rows, ownership markers, paths, file modes, sizes, and
marker placement all fail closed. A second update is byte-identical.

`links --local --check` parses CommonMark events with source offsets over all
maintained public, internal, and repository source sets. It checks relative
files, images, references, autolinks, HTML `href`/`id`/`name` attributes,
VitePress routes, queries, strict single-pass percent decoding, and anchors.
Public headings use the VitePress 1.6.4 `@mdit-vue/shared` slug contract;
repository and maintained-internal headings use GitHub slugs. Code and HTML
comments do not create destinations. The check also rejects links to excluded
sources, validates the exact live-page and typed-tombstone inventories against
navigation and orphan records, proves live-page reachability, and requires an
owned prose heading for every exact semantic Mermaid fence. All three
publication manifests and every Markdown input are bounded to 4 MiB. The
command is offline and is suitable for pull-request validation.

`links --external --check` is the only command that performs network I/O. It is
reserved for scheduled or explicit manual execution. Requests require HTTPS,
reject URL credentials, follow at most five redirects, use HEAD with GET only
for unsupported HEAD responses, and make at most three attempts for 429/5xx.
The production curl process cannot follow redirects itself; the shared checker
validates each relative or absolute redirect before issuing the next request.
Transport arguments allow only credential-free HTTPS and HEAD/GET with bounded
connect and total time. Stdout is bounded while read, then independently
validated for status, header count, header line/name/value/total bytes, and
body bytes. Exact exceptions require an owner, reason, and unexpired timestamp.

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
cargo test --manifest-path tools/docs-parity/Cargo.toml --test markdown
cargo test --manifest-path tools/docs-parity/Cargo.toml --test links
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
```

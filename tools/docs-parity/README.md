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
cargo run --manifest-path tools/docs-parity/Cargo.toml -- settings --check
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
It owns a unique sibling stage file, writes and syncs it before a
same-directory rename, then syncs the containing directory. A failed or
interrupted operation cleans only its owned stage and leaves the last complete
target unchanged; an unrelated peer stage is never deleted.

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
for review when findings change. It never approves candidates. Six narrow
exception classes are supported; the settings-schema class applies only to the
three non-value secret-disposition identifiers in the checked companion
manifest. These classes do not assert that mechanical detection is complete
for human semantic sensitivity.

## Generated regions and links

`generate --check` renders the typed records in `manifests/pages.toml` and
returns exit code `1` when a named Markdown region differs. Check mode performs
no writes. `generate --update` validates every target first, then atomically
replaces only drifted region bodies; exact bytes outside the paired markers are
preserved. Region names, rows, ownership markers, paths, file modes, sizes, and
marker placement all fail closed. Atomic replacement explicitly restores the
original safe mode on the staged inode, fsyncs the staged file and parent
directory, and compares the expected bytes, file identity, type, and mode
immediately before rename. A change observed by that precommit validation
aborts without replacing the target. Portable rename cannot conditionally bind
that observation to the rename syscall, so a noncooperating write in the final
syscall window can still be replaced. Failed operations clean only their unique
owned stages. A second update is byte-identical.

`links --local --check` parses CommonMark events with source offsets over all
maintained public, internal, and repository source sets. It checks relative
files, images, references, autolinks, HTML `href`/`id`/`name` attributes,
VitePress routes, queries, strict single-pass path/query/fragment percent
decoding, and anchors. Public-page YAML frontmatter is bounded to 64 KiB,
decoded by the maintained `yaml_serde` compatibility package, and checked for
typed hero image/action and feature link/icon targets. Hero images and feature
icons accept the pinned VitePress string, `src`, or complete `light`/`dark`
shapes; both theme targets enter the same checker. Root assets resolve only
through the configured literal `publicDir` or VitePress's `docs/public`
default and must be regular, non-symlink repository files. Public headings use
the VitePress 1.6.4 `@mdit-vue/shared` slug contract, including contextual
Unicode lowercase conversion and the exact ECMAScript whitespace set. Their
title extraction ignores image alt text plus soft and hard break events, and
colliding explicit IDs fail instead of receiving an automatic suffix.
Repository and maintained-internal headings use GitHub title and slug behavior.
Code and HTML comments do not create destinations. The check also rejects links to excluded
sources, validates the exact live-page and typed-tombstone inventories against
navigation and orphan records, proves live-page reachability, and requires an
owned prose heading for every exact semantic Mermaid fence. All three
publication manifests and every Markdown input are bounded to 4 MiB. Each
diagram record binds its path, selector, prose anchor, owner, and exact semantic
fence-content SHA-256, so edits or order swaps require renewed review. The
command is offline and is suitable for pull-request validation.

`links --external --check` is the only command that performs network I/O. It is
reserved for scheduled or explicit manual execution. Requests require HTTPS,
reject URL credentials, follow at most five redirects, use HEAD with GET only
for unsupported HEAD responses, and make at most three attempts for 429/5xx.
The production curl process uses the fixed `/usr/bin/curl` executable and
starts with `--disable`, so `PATH`, proxy, TLS, curl-home, and ambient curl
configuration cannot change its behavior. Its environment is cleared except
for a deterministic C locale, and curl cannot follow redirects itself; the
shared checker validates each relative or absolute redirect before issuing the
next request.
Transport arguments allow only credential-free HTTPS and HEAD/GET with bounded
connect and total time. GET writes headers once through `--dump-header`; HEAD
omits that option because `--head --output -` already writes its header block.
Stdout is read concurrently under the same wall-clock bound as the request,
then independently validated for status, raw header count,
line/name/value/total bytes, and body bytes. Every intermediate proxy or 1xx
HTTP block is validated and only the final response drives policy. Legal
repeated fields remain distinct; duplicate Location, Retry-After, and
Content-Length fields fail closed as policy singletons. The stricter security
policy also rejects repeated Transfer-Encoding and Content-Encoding rather
than coalescing their otherwise list-shaped field values.
The wall-clock deadline is fixed before the stdout reader starts. Every
post-spawn outcome polls the child, kills it when still alive, waits regardless
of a kill failure, and joins the reader; primary, kill, wait, and join
diagnostics are retained. Exact exceptions require an owner, reason, and
unexpired timestamp.

## Settings semantics

`settings --check` parses the four checked Rust settings/profile sources with
`syn` and a closed Serde/validator attribute grammar. Shape-changing attributes
outside that grammar fail closed. Nonliteral defaults, custom deserializers,
and custom validators require reviewed companion records whose named positive
and negative probes compile in the owning production module's test target.

The command checks the exact 17-field `Settings` root, independent lifecycle,
key-identity, serialization, runtime, and secret-handling dispositions, the 11
store-resolved paths, the deliberately-inline and accepted-discarded paths,
deprecated normalized-away selectors, and aliases. It also validates the
source template's three placeholder paths, 14 deploy-validated integration
IDs, three profile IDs, three secret-key literals, and all six literal or
include-only consumers. The source template and every checked source are
bounded to 4 MiB and decoded as strict UTF-8.

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
cargo test --manifest-path tools/docs-parity/Cargo.toml --test settings
cargo test --manifest-path tools/docs-parity/Cargo.toml
cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check
cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets -- -D warnings
```

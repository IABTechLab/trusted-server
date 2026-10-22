# JS composition producer interface

`collect-js.mjs` reuses the ordinary build pipeline with a read-only Rollup
observer. Its version-1 output follows the closed Task 5 root/module schema.
It requires the existing `TRUSTED_SERVER__RELEASE_BUILD=1` control and a full
`TRUSTED_SERVER__GIT_VERSION`. Any skip presence or invalid release control fails.
Expected entry points come from the selected full commit’s Git tree, independently
of the current index. The collector builds only its own checkout, so imported
build/tool code cannot be measured as a different checkout’s installation. Source-byte immutability remains preparation’s check.
The release options disable Vite configuration and `.env` loading and select
production mode; Vitest uses the same release controls. These options do not
remove inherited Node/npm/Vite environment variables: release preflight owns
that rejection.

## Contribution semantics

Contributions with positive Rollup `renderedLength` carry
`stage: "rollup-rendered"` and `rendered_length`. Type-only and wholly eliminated
modules are excluded. Later esbuild minification can remove additional code.
These records describe conservative post-Rollup reachability, not final function
reachability or attribution of final bundle bytes. Final module sizes and SHA-256
are measured from files after Vite/esbuild finishes.

Source contributions contain repository-relative `path`, original-file `sha256`
and `transform` (`identity`, `raw` or `inline`). npm contributions additionally
contain exact `lock_path`, `package_name`, `package_version` and archive
`integrity`; their paths are relative to the owning installed package. Only the selected registry’s canonical HTTPS tarball sources are supported;
links, missing sources and file/git sources fail. Parent/root aliases are
normalized before nearest-owner lookup; package-root aliases and escapes fail. Unsupported
queries, unmapped owners, symlink escapes and ambiguous identities fail.

Generated contributions contain `id`, `plugin`, `owner_lock_path` and
`generated_sha256` of the actual transformed virtual code. The reviewed mappings
are Vite's bundled CommonJS helper, CommonJS ES-import wrappers and Vite's preload
helper. Wrappers additionally record the underlying file's `input` origin.
Unknown virtual producers fail. Wrapper IDs use normalized input paths;
transformed-code hashes still measure actual code, which can include checkout
paths. Contribution inventories are not a claim that different checkout paths
produce identical virtual-code hashes. The bundle-byte comparison is separate.

## Checked preparation input

The producer accepts a bounded strict UTF-8 JSON document with exactly:

- `schema_version: 1`;
- `packages`: the full measured installed `npm-package` material set;
- `distributions`: records with exactly `material` and `path`;
- `tools`: records with `name`, `version`, `role`, `material_id`;
- `lifecycle_events`: observed executions, including executions with no output;
- `lifecycle_outputs`: observed output files.

A package material contains `kind: "npm-package"`, `id: "npm:<lock_path>"`,
`lock_path`, `name`, `version`, `integrity` and `installed_tree_sha256`.
Only installed locked locations are recorded; optional absent platform packages
are not claimed installed. The selected native esbuild and Rollup packages are
mandatory through the actual-tool checks.

Each distribution material contains `kind: "tool-distribution"`, `id`, `name`,
`version`, `sha256`. `path` selects the actual measured tool entry file or native
executable, never a command to run. Required tools are Node, npm, Vite, Rollup,
esbuild, Vitest and the selected esbuild/Rollup native executables. Paths, versions,
roles and file digests MUST match the producer's actual tool selection. Whole
locked npm package trees are recorded separately. Tool entry/native file hashes
are measured identities; they are not authenticated distribution acquisition or
qualified host evidence. Task 8 supplies that boundary.

Lifecycle events contain `kind: "lifecycle-event"`, unique `id`,
`package_material_id`, `event` and `script_sha256`. The script digest is SHA-256
of the UTF-8 command string in the owning installed package's
`package.json.scripts[event]`. Outputs contain `kind: "lifecycle-output"`, unique
`id`, the same event/script/package join, installation-relative `path`, `size`
and `sha256`. Each output MUST remain inside its owning package and match an
explicit observed event. Registry-install coverage requires exactly the installed packages’ declared
preinstall, install and postinstall hooks, including hooks producing no files.
Registry dependency prepare scripts do not run in the selected npm ci recipe.
Implicit binding.gyp installs without explicit reviewed hooks are rejected. Neither array is inferred from lockfile flags. Task 7 MUST produce
these records from actual preparation execution; semantic checks here cannot
prove that an execution occurred or that the observations are exhaustive.
Synthetic fixture observations exercise joins but do not claim genuine execution
history. Missing required events cannot produce an inventory.

## Installed tree hash

`installed_tree_sha256` hashes the UTF-8 domain separator
`trusted-server-installed-tree-v1\n`, then entries in recursive sorted pathname
order. A directory contributes compact JSON `[relative_path,"directory"]` plus
newline. A regular file contributes compact JSON
`[relative_path,"file",byte_length,is_executable]`, newline, actual bytes and
newline; `is_executable` means any executable bit is set. A symlink contributes
compact JSON `[relative_path,"symlink",relative_target]` plus newline and MUST
resolve inside its package. Separately locked nested package roots and their
contents are excluded from the parent's hash. Timestamps, owners and
non-executable permission bits are excluded. Files are read through held
regular-file descriptors with mutation checks and bounded buffers.

JSON and final inventory are limited to 1 MiB; source and final bundle reads are
limited to 32 MiB. Duplicate keys, invalid UTF-8, BOM, unpaired surrogates,
unsafe integers, non-finite numbers and more than 64 nested containers fail.
The inventory writer sorts module/material/tool records and appends one newline.
The inventory MUST live outside the bundle directory. This task does not perform
fresh installation, network confinement, A/B comparison, signing or publication.

## Independent Rust preparation recipe

The private Rust preparation and comparison recipe uses the retained Node collector.
`build-js.sh` remains a thin POSIX adapter to the independently approved-source
OS-native `trusted-server-release build-js` command. Until its authenticated Rust
bootstrap and qualified source-owned selections are implemented, the wrapper
refuses before creating work directories. No public fixture bypass, unchecked
interpreter or colocated self-authored SHA file establishes bootstrap authority.
The host-only tool has no core/CLI/JS crate dependency; it retains this Node
collector and composition pipeline. See the [migration addendum](../../docs/superpowers/specs/2026-10-14-release-tooling-rust-migration.md).

The recipe lives in private `materials`, `process`, `boundary`,
`preparation` and `javascript` modules. Each installation owns a fresh copy of
approved tracked source bytes, cache, node_modules, HOME, TMP, lifecycle observations
and bundles. The parent authenticates the approved Git objects and derives the
complete module set; it supplies that set and the source commit through a protected
read-only source-context document. JS working copies contain no inherited `.git`,
configuration or history. Git material snapshots for Cargo remain separate.
Original sources/object stores and shared authenticated archives/tools are read-only;
sibling data/metadata, shared writes and network sockets are denied. The shared JS
execution path selects Linux containers with owned descendant cleanup; macOS native
recipes require their own lifecycle containment and qualification. Current installed-tool
rehashing is separate from independently reviewed all-cohort hosted evidence.
Local macOS 26.6.2 diagnostics cannot qualify macOS 15 ARM/Intel or GNU.

Archive filenames remain locked SHA-512 hex digests followed by `.tgz`. Authenticate
original bytes before bounded decompression/TAR inspection. Preserve the source-owned
exception for three exact locked npm archives containing identical
`package/./dist/index.js` and `package/dist/index.js` entries: agent-base 7.1.4,
http-proxy-agent 7.0.2 and https-proxy-agent 7.0.6. Resolve their original locked
SHA-512 identities and require matching regular type, size, mode and bytes;
materialize the canonical file once. Other duplicates, dot aliases, traversal,
links, devices and privileged modes fail. Eight exact locked type-package
versions use reviewed legacy roots: `@types/chai` 5.2.3 (`chai`),
`@types/deep-eql` 4.0.2 (`deep-eql`), `@types/estree` 1.0.8 (`estree`),
`@types/jsdom` 27.0.0 (`jsdom`), `@types/json-schema` 7.0.15 (`json-schema`),
`@types/json5` 0.0.29 (`json5`), `@types/node` 24.10.13 (`node v24.10`),
and `@types/tough-cookie` 4.0.5 (`tough-cookie`). Seven of these originals
(all except json5) may use the observed node-tar timestamp layout: empty
130-byte prefix, a zero separator, two eleven-digit octal timestamps with
NUL terminators, and zero remaining reserved bytes. No timestamp propagates
to extracted files. Other hidden name/prefix bytes fail.

Only locked `gulp-wrap` 0.15.0 may carry local PAX metadata. Each bounded
record uses the reviewed metadata-key allowlist; duplicate, unknown and
malformed fields fail. Its path and size must exactly match the immediately
following raw regular-file header, and its own name must be
`PaxHeader/<that-path>`. Global, consecutive and dangling records and any
path/size override fail. These exceptions apply only after original-byte
authentication and exact name/version selection; they are not general
extension support.

Registry archive SHA-256/exact-root and
Git revision/whole-tree validation remain available to Cargo preparation. Query
selected Git object identity/type/size before body acquisition: reject objects
over 32 MiB or cumulative bodies over 512 MiB, then rejoin exact body headers.
This reader assumes the sanitized trusted Git implementation and an immutable
object store, rather than claiming adversarial race-proof streaming.

The Rust dependency archive inspector limits each authenticated compressed
original to 64 MiB, decoded TAR bytes to 512 MiB, each member to 128 MiB,
entries to 100,000, path depth to 64 and path text to 4,096 bytes. It accepts
ASCII regular-file and directory names under the exact locked root, rejects
case aliases and unsupported extension records (including unreviewed PAX,
GNU long-name and sparse records), and requires a single complete gzip member with complete
TAR zero terminators and no hidden trailing data. These are dependency archive
limits, separate from toolchain distributions and Git object limits. The
128 MiB member limit accommodates the current locked optional OpenBSD Rollup
binary expands to 110,275,248 bytes. Hashing and extraction remain streamed;
this does not increase the compressed-original or total decoded-byte limits. Extraction
owns a fresh private directory and preserves original compressed bytes. File
permissions become owner read/write plus the archive's executable bits; archive
ownership, timestamps and other permission bits do not propagate. This stage
does not establish an installed dependency, executed hook or completed JS build.

Seed each private cache through selected npm's public `cache add` API with only
verified archives, offline and scripts disabled. Run fresh pinned npm 11.19.0
`ci --offline --include=dev --no-audit --no-fund` with hooks in the foreground
under the qualified lifecycle denial boundary. Reject root install/prepare hooks
and implicit binding.gyp installs. Preserve the two exact reviewed dependency
hooks and actual starts/completions/failures/script hashes/changed-owned-files,
including no-output hooks and swallowed optional failures. Bind bare `node` to
selected Node 24.20.0 despite package-bin shadowing. Missing prepared native
Vite/esbuild tools MUST fail without fetching substitutes. Observed lifecycle
records cannot be inferred from lockfile flags or successful npm exit alone.

The source-owned declaration policy in the Rust material reviewer permits only
these exact dependency install-hook tuples. It checks the name/version against
the approved lock, the manifest bytes against the authenticated extraction, and
the command bytes against this policy:

| Package   | Version  | Event         | Exact command                                       |
| --------- | -------- | ------------- | --------------------------------------------------- |
| `core-js` | `3.48.0` | `postinstall` | `node -e "try{require('./postinstall')}catch(e){}"` |
| `esbuild` | `0.27.3` | `postinstall` | `node install.js`                                   |

Any added hook or command/version change requires a source policy review.
Root review rejects all ten `npm ci` lifecycle hooks: `preinstall`, `install`,
`postinstall`, `prepublish`, `preprepare`, `prepare`, `postprepare`,
`predependencies`, `dependencies` and `postdependencies`. The last three also run
through pinned npm's Arborist when installation changes the dependency tree. These
results describe declarations only. They grant no execution authority and supply
no starts, completions, failures or changed-file observations. Full installation
tree mutation checks and actual lifecycle observations remain separate gates.

`observe-npm.mjs` adapts the selected npm 11.19.0 lifecycle entry before npm loads
its dependency graph. It records real starts, fulfilled/rejected execution,
exit status and changed owned-file facts in memory. This includes hooks with no
output and optional hook failures that npm would otherwise swallow. Root calls
for absent scripts remain no-ops; root commands and unreviewed dependency hooks
fail. Selected Node replaces the reviewed command's bare `node` and leads the
child PATH after npm prepends package bins. Lifecycle children receive no parent
stdin or additional inherited descriptors.

The Linux supervisor loads the authenticated native non-dumpable-process guard
before giving the observer its private request key. The guard and the process
memory syscall denials prevent lifecycle children from reading or modifying the
trusted npm process. Observations and the checked preparation document are sealed
together with HMAC-SHA-256 over their exact original JSON bytes. The supervisor
joins the nonce, source, workspace, cache, guard and native esbuild expectation,
requires successful npm exit and complete successful events, and independently
checks the retained outputs. A child's printed prefix or writable JSON file does
not establish an observation. This adapter provides no macOS completion path and
does not itself authenticate Node/npm distributions or qualify a hosted runner.

The reviewed esbuild 0.27.3 postinstall can hard-link its selected native package
binary to `node_modules/esbuild/bin/esbuild`. To retain the boundary's global
single-link policy, the observer detaches only this exact pair after execution.
Both paths must identify the same regular inode with exactly two links and match
the parent-derived authenticated native archive's digest, size and expected mode.
The observer copies the owned esbuild path into a fresh file and atomically
replaces it, then verifies both files have one link and unchanged executable
bytes. It records `esbuild-hardlink-copy-v1` separately in the sealed observations;
the changed owned executable remains a lifecycle output. Other hard links,
additional aliases, mismatched bytes/modes or unsafe paths fail. This
normalization changes inode ownership of the bytes, not executable content.

Run collector and unconditional full Vitest/type checks under that same native
boundary, checking tracked source and unexpected files before/after each stage.
Preserve release Vitest's literal host and narrowly scoped native `node:` import
resolver; npm imports remain unmodified. `configFile: false` collector builds
remain separate from that test plugin. Existing ordinary development behavior
remains supported. Both actual A/B Vitest processes MUST unconditionally receive
`TRUSTED_SERVER__RELEASE_JS_TESTS=1` and run the full registered suite/type checks,
with no developer opt-out. Require all tracked core/integration final files
compared byte-for-byte. Ordinary development skips only the two slow Git
collector fixtures by default; CI enables the full suite. B inventory virtual-code hashes may differ because paths differ.

After all actual preparation/lifecycle/test/build/control/comparison stages pass,
construct a private typed completion bound to approved source, authenticated tools,
settings, raw inventory digest and retained A module bytes. A parsed JSON boolean,
source label, arbitrary checked inventory or asserted path/hash cannot construct
that state. Atomically retain A's `bundles/` plus sibling `tsjs-inventory.json`,
with only JS files in `bundles/`; preserve A's original inventory encoding and
closed root/module shape, exact integer sizes and final digests. Rehash at handoff.
Use bounded child pipes/deadlines and kill/reap descendants on cancellation,
timeout or oversize output. Clean private profiles/resources and Docker containers
on success and failure with bounded stop/remove operations.

Once Rust modules/tests exist, derive `HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')`
and run `cargo test -p trusted-server-release --target "$HOST_TARGET" javascript::tests`
using the [primary plan's nonzero-selection guard](../../docs/superpowers/plans/2026-10-13-attested-release-implementation.md#focused-rust-test-selection-guard).
Private fixtures belong alongside their registered modules; public process tests
use `tests/commands.rs`. Full local two-installation/native-probe execution MUST be
selected explicitly through reviewed test paths and cannot waive hosted gates.

The committed Node collector/composition fixtures are current local interface checks; utility fixtures do not construct producer completion. Task 7's real two-installation orchestration passed its full Linux execution test in ordinary PR CI; see the [two-build evidence report](qualification/2026-10-15-js-two-builds.md). That candidate run is not release qualification: Task 8's selected hosted/native qualification, authenticated bootstrap and tool pins remain pending.

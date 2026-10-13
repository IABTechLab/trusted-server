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
roles and file digests must match the producer's actual tool selection. Whole
locked npm package trees are recorded separately. Tool entry/native file hashes
are measured identities; they are not authenticated distribution acquisition or
qualified host evidence. Task 8 supplies that boundary.

Lifecycle events contain `kind: "lifecycle-event"`, unique `id`,
`package_material_id`, `event` and `script_sha256`. The script digest is SHA-256
of the UTF-8 command string in the owning installed package's
`package.json.scripts[event]`. Outputs contain `kind: "lifecycle-output"`, unique
`id`, the same event/script/package join, installation-relative `path`, `size`
and `sha256`. Each output must remain inside its owning package and match an
explicit observed event. Registry-install coverage requires exactly the installed packages’ declared
preinstall, install and postinstall hooks, including hooks producing no files.
Registry dependency prepare scripts do not run in the selected npm ci recipe.
Implicit binding.gyp installs without explicit reviewed hooks are rejected. Neither array is inferred from lockfile flags. Task 7 must produce
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
compact JSON `[relative_path,"symlink",relative_target]` plus newline and must
resolve inside its package. Separately locked nested package roots and their
contents are excluded from the parent's hash. Timestamps, owners and
non-executable permission bits are excluded. Files are read through held
regular-file descriptors with mutation checks and bounded buffers.

JSON and final inventory are limited to 1 MiB; source and final bundle reads are
limited to 32 MiB. Duplicate keys, invalid UTF-8, BOM, unpaired surrogates,
unsafe integers, non-finite numbers and more than 64 nested containers fail.
The inventory writer sorts module/material/tool records and appends one newline.
The inventory must live outside the bundle directory. This task does not perform
fresh installation, network confinement, A/B comparison, signing or publication.

## Independent preparation recipe

`build-js.sh` runs the isolated Python entry in `prepare.py`. The official entry
requires qualified acquisitions and hosted denial evidence; it currently refuses
before creating work directories. There is no command-line fixture bypass.
Task 8 owns the source-owned authenticated tool and boundary selection.

The separate `local_js_pair` Python API exercises the actual full recipe using
macOS sandbox-exec and returns `local-unqualified`. It cannot qualify a release.
Each installation has its own tracked-object snapshot, sanitized private Git
object store, cache, node_modules, HOME, TMP, observations and bundles. Shared
archive/tool paths permit reads only; sibling workspace data and metadata, shared
writes, network sockets and arbitrary Mach services are denied. Local system
runtime and developer-tool directories are explicitly readable. These local path
measurements do not authenticate an image, SDK or distribution acquisition.

Archive filenames are locked SHA-512 hexadecimal digests followed by `.tgz`.
Preparation authenticates raw bytes before bounded decompression/tar inspection.
Three pinned npm archives contain the identical raw pair
`package/./dist/index.js` and `package/dist/index.js`: agent-base 7.1.4,
http-proxy-agent 7.0.2 and https-proxy-agent 7.0.6. Their exact locked SHA-512
identities are source-owned exceptions in `NPM_ALIAS_ARCHIVES`. Both regular
entries must have identical size, bytes, type and mode, and their canonical file
is materialized once. Other duplicates, dot aliases, traversal, links, devices
and privileged modes fail. SHA-256/exact-root registry validation and exact Git
revision/whole-tree validation are also reusable by later Cargo preparation.

Each private cache is seeded via pinned npm's public `cache add` API using only
verified archives, offline and with scripts disabled. Fresh pinned npm ci runs
offline with dev dependencies included, audit/fund disabled and hooks in the
foreground. Root install/prepare hooks and implicit binding.gyp installs fail.
Only the two exact reviewed dependency hooks are enabled. A recipe-owned script
shell records actual starts, completions, failures, command hashes and changed
owned files, including hooks with no outputs; optional failures cannot disappear
behind npm's success status. Its effective PATH binds bare `node` to the selected
runtime. Task 6 measures the full installed locked trees and actual executables.

Collector and Vitest execute under the same denial policy, with source checks
covering all tracked inputs and unexpected files before and after each stage.
Vitest is unconditional and uses a literal host in release mode to avoid
localhost DNS resolution; it retains its middleware mode without a network
server. Its release-only test plugin preserves native `node:` imports that Vite
otherwise replaces with browser stubs in production mode; npm imports are not
externalized by this plugin. Collector builds use `configFile: false`, so the
test plugin never affects published IIFEs. Ordinary development retains its
existing config. Only final bundle bytes must match: all tracked integrations and core
are compared. A's bundles and inventory are published together only after
equality, in one atomic parent with `bundles/` and sibling `tsjs-inventory.json`.
B's inventory can legitimately contain different virtual-code hashes.

Run ordinary fixtures with `python3 -B -m unittest discover -s
scripts/release/tests -p 'test_*.py' -v`. On macOS, explicitly enable actual local
boundary probes with `TS_RELEASE_BOUNDARY_TESTS=1`, and the full two-installation
probe with `TS_RELEASE_FULL_JS_TESTS=1`. Select Node 24.12.0 on the fixture PATH;
the exact npm 11.6.2 is checked. The full local test authenticates original archive
bytes read from the local npm cache and never copies cache indexes or installed
trees. These probes require sandbox-exec outside an enclosing application
sandbox. Hosted qualification remains pending regardless of their result.

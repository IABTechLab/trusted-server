# trusted-server-js

Rust wrapper and TypeScript build for Trusted Server's browser runtime.

The build script discovers the checked TypeScript entries, invokes the Node
build, and embeds the resulting IIFE bundles in Rust. The Rust API returns
individual bundles, deterministic concatenations, and content hashes used by
adapter responses. Browser behavior lives under `lib/src`. Cargo builds the
bundles into a private directory under its `OUT_DIR` and fails if any expected
bundle is missing or empty; the `dist` directory is written only by a manual
`npm run build` (for browser tests) and is never embedded unless you pass it
explicitly through `TSJS_PREBUILT_DIR`. The external Prebid.js artifact is
built separately and is not the deferred Trusted Server Prebid shim.

With `TRUSTED_SERVER__RELEASE_BUILD=1`, Rust builds require
`TRUSTED_SERVER__GIT_VERSION` as the approved 40-character lowercase commit,
plus absolute `TSJS_PREBUILT_DIR` and
`TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY` paths. They run no npm. Any presence
of `TSJS_SKIP_BUILD` fails in Rust Cargo builds, including empty, `0` and `1`;
Node development builds and Vitest allow it when release control is absent.
Development still supports fresh Node builds and prebuilt bundles without an
inventory. When an inventory is supplied, its source commit syntax and bytes
are checked; matching the approved source is required only in release mode.
The development override/tag/branch display label is not compared with the
inventory commit.

The version-1 inventory has exactly `schema_version`, `source_commit`,
`modules`, `tools`, and `materials`. Each module has exactly `id`, `filename`,
`size`, `sha256`, and `contributions`. Modules are sorted by safe ASCII ID and
must match the discovered set; filenames are `tsjs-<id>.js`, sizes are positive
integers, and digests are lowercase SHA-256. The inventory must be outside
the bundle directory, is capped at 1 MiB, and uses strict UTF-8 JSON with
unique keys, finite numbers, and at most 64 container levels. Integer-valued
numbers, including exponent notation and numbers inside opaque arrays, must
stay within the JavaScript safe-integer range; lexical integer overflow also
fails. Producers must honor the 32 MiB input limit per module. The required contribution, tool, and
material arrays contain strictly parsed, opaque JSON here; composition
validation belongs to the release producer.

Inventory-backed copying checks the exact source set through held file handles,
rejects links, hard links and nonregular files, and checks source identities and
private copied bytes before generating Rust. Inventory-absent development
keeps the existing prebuilt path, including unrelated non-bundle extras and
supported directory/file aliases. Cargo build-script messages expose
`TRUSTED_SERVER__TSJS_INVENTORY_SHA256` through `cargo:rustc-env`, hashing the exact raw
inventory bytes for later build evidence. These are prepared input checks;
they do not establish which modules survive linking into a final executable.

Run the JavaScript checks from `crates/trusted-server-js/lib`:

```bash
npm ci
npm run lint
npx vitest run
npm run format
npm run build
```

The default development suite skips only the two slow Git collector fixtures.
Run the full registered suite and type checks, as CI does, with
`TRUSTED_SERVER__RELEASE_JS_TESTS=1 npx vitest run`. Both future release JS A/B
test processes must receive that opt-in unconditionally.

`cargo test-fastly` covers WASM tests but selects zero native prebuilt tests.
Run those checks from the repository root with the actual host target:

```bash
set -eu
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
test -n "$HOST_TARGET"
TEST_LIST=$(mktemp)
trap 'rm -f "$TEST_LIST"' EXIT
cargo test -p trusted-server-js --target "$HOST_TARGET" --test release_prebuilt -- --list > "$TEST_LIST"
awk '/: test$/ { count++ } END { if (count == 0) { print "No native prebuilt tests selected"; exit 1 } }' "$TEST_LIST"
cargo test -p trusted-server-js --target "$HOST_TARGET" --test release_prebuilt
cargo clippy -p trusted-server-js --target "$HOST_TARGET" --all-targets --all-features -- -D warnings
```

These source-built tests do not establish release qualification. See
[Trusted Server JavaScript](../../docs/guide/tsjs.md) for the 12 integration
entries, 13 emitted bundles, and immediate, deferred, and standalone loading
modes.

# Prebid fork source for the external bundle CLI

**Date:** 2026-08-20
**Status:** Proposed
**Related:** `docs/superpowers/specs/2026-06-17-prebid-bundle-cli-design.md`

## Problem

`ts prebid bundle` builds the external Prebid bundle from the `prebid.js`
npm package pinned in `crates/trusted-server-js/lib/package.json`. The
generator (`build-prebid-external.mjs`) hardcodes
`node_modules/prebid.js` as the package root and resolves every
`prebid.js/...` import through it.

Operators who maintain a Prebid.js fork (for example, a patched bid
adapter) cannot use it without npm-level surgery: building the fork,
packing a tarball, and editing `package.json` to a `file:` or registry
override. That workflow is manual, is not recorded anywhere a teammate
can reproduce, and mutates a source-controlled file for what is an
operator-local decision.

The desired behavior is that `ts prebid bundle` accepts a fork source,
either a git URL (with optional ref) or a local path to a built fork
checkout, builds the bundle from it, and records exactly which fork
commit produced the hosted artifact.

## Goals

- Accept a fork source as a git URL with optional `#ref`, or as a local
  filesystem path.
- Declare the source in `trusted-server.toml` under
  `integrations.prebid.bundle.source`, with a `--prebid-source` CLI flag
  that overrides it.
- For git sources, clone and build the fork automatically in a
  CLI-owned cache, keyed by URL, and skip rebuilds when the resolved
  commit has not changed.
- For path sources, use the operator's checkout as-is and never mutate
  it.
- Accept any git ref (branch, tag, or commit SHA), resolve it to the
  exact commit at build time, and record that commit in the manifest.
- Record source provenance (`npm`, `path`, or `git` plus URL, ref,
  commit, or path) in the generated `manifest.json`.
- Preserve current behavior byte-for-byte when no source is configured.
- Keep the existing failure guarantee: `trusted-server.toml` is never
  patched when generation fails.

## Non-goals

- Do not change `crates/trusted-server-js/lib/package.json` or the
  pinned `prebid.js` dependency; the default bundle path is unchanged.
- Do not change how the server serves or validates
  `external_bundle_url`, `external_bundle_sha256`, or
  `external_bundle_sri`.
- Do not build a fork checkout given as a local path; the CLI validates
  it and errors with the exact build commands if `dist/` is missing.
- Do not regenerate Prebid module metadata (`gulp update-metadata`
  requires a headless browser); forks are expected to keep
  `metadata/modules/*.json` checked in, as upstream does.
- Do not add authentication handling for private git remotes beyond
  what the operator's ambient `git` credentials already provide.
- Do not validate or reject local-path sources at `ts config push`
  time; git URLs are the shareable form by convention and the docs say
  so.

## Confirmed decisions

1. **Source home:** config key `integrations.prebid.bundle.source` plus
   a `--prebid-source` CLI override. CLI wins over config; absent both,
   the bundled npm package is used.
2. **Ref pinning:** any ref is accepted; the CLI always resolves it to
   a commit SHA and stamps that SHA into the manifest.
3. **Build owner:** cloned forks live in a CLI-owned cache and are
   built there automatically (`npm ci` then `npx gulp build`), skipped
   when the stamped last-built SHA matches. Local paths must already be
   built.
4. **Flag shape:** one flag/key with classification by shape
   (`https://`, `git@`, or `ssh://` means git; anything else is a
   path), not separate `--prebid-dir` and `--prebid-repo` options.
5. **Provenance:** `manifest.json` gains a `source` object; the
   existing `prebidVersion` field is unchanged and may still report the
   upstream version string for a fork.

## Current architecture

### Rust CLI

`crates/trusted-server-cli/src/prebid_bundle.rs` loads
`[integrations.prebid.bundle]` from the config, locates
`crates/trusted-server-js/lib`, checks prerequisites (npm on PATH,
`package.json`, `build-prebid-external.mjs`, `node_modules`), and shells
out through the `PrebidBundleGenerator` trait to
`npm run build:prebid-external -- --adapters ... --out ...`. It then
reads back `manifest.json`, validates `sha256`/`sri`/`filename`, and
atomically patches `external_bundle_sha256` and `external_bundle_sri`
into the config with `toml_edit`.

### Generator

`crates/trusted-server-js/lib/build-prebid-external.mjs` derives every
Prebid path from the constant
`PREBID_PACKAGE_DIR = node_modules/prebid.js`:

- adapter existence checks against `modules/<name>BidAdapter.js`;
- generated imports using bare `prebid.js/modules/*.js` specifiers,
  which Node and Vite resolve through the published package `exports`
  map (`./modules/*.js` maps to `./dist/src/public/*.js`);
- bidder-code metadata reads from `metadata/modules/*.json`;
- hardcoded `dist/src/...` paths for the LiveIntent standard module,
  `prebidGlobal`, `adapterManager`, and `adRendering` Vite aliases;
- the version stamped into the manifest from the package
  `package.json`.

### Fork build reality

The published npm package ships a prebuilt `dist/` tree that a raw git
checkout does not have, and the published `package.json` has no
`prepare` script, so git checkouts must be built explicitly. On the
`10.26.0` tag, `npx gulp build` (clean, babel precompile, webpack,
`setupDist`) produces the same `dist/` layout the npm tarball ships,
and `metadata/modules/*.json` is checked into the repo.

## Design

### 1. Source declaration and precedence

Add an optional string to the bundle config:

```toml
[integrations.prebid.bundle]
adapters = ["rubicon", "kargo"]
source = "https://github.com/example-org/Prebid.js#my-adapter-fix"
```

Add an optional `--prebid-source <path|url>` argument to
`PrebidBundleArgs`. Effective source is: CLI flag if present, else
config key if present, else the default npm package.

`load_bundle_config` gains an optional `source` field. An empty or
whitespace-only string is a config error.

### 2. Source classification and the resolver seam

Introduce a `PrebidSource` enum in `prebid_bundle.rs`:

- `Npm` (default),
- `Path(PathBuf)`,
- `Git { url: String, reference: Option<String> }`.

Classification: strings starting with `https://`, `git@`, or `ssh://`
parse as git, with an optional trailing `#<ref>` split off; everything
else is a path. `http://` is rejected with an error suggesting
`https://`.

Resolution runs behind a new trait (mirroring the existing
`PrebidBundleGenerator` seam so tests can fake it):

```rust
pub(crate) trait PrebidSourceResolver {
    fn resolve(
        &mut self,
        source: &PrebidSource,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> CliResult<ResolvedPrebidSource>;
}
```

`ResolvedPrebidSource` carries the package directory to hand to the
generator (`None` for `Npm`) and the provenance to stamp into the
manifest. The production implementation shells out to `git`, `npm`, and
`npx` (the CLI already shells out to npm; no libgit2 dependency).

### 3. Path sources: validate, never mutate

For `Path`, canonicalize and validate that the directory is a built
package:

- `package.json` exists;
- `dist/src/` exists;
- `metadata/modules/` exists.

A missing `dist/src/` fails with guidance naming the exact commands
(`npm ci && npx gulp build` inside the checkout). The CLI never runs
installs or builds in a path source.

### 4. Git sources: clone, build, cache

For `Git`:

1. Compute the cache directory:
   `~/.cache/trusted-server/prebid-src/<hash-of-url>` (honoring
   `XDG_CACHE_HOME` when set).
2. Clone the URL there if absent; otherwise `git fetch`.
3. Check out the requested ref (default: the remote default branch) and
   resolve it to a commit SHA via `git rev-parse`.
4. Read the stamp file `.ts-prebid-build-stamp` in the checkout. If it
   names the same SHA and `dist/src/` exists, skip the build.
5. Otherwise run `npm ci` then `npx gulp build` in the checkout,
   forwarding stdout/stderr like the generator does, and write the
   stamp file on success.
6. Validate the result with the same checks as a path source, then
   treat the checkout directory as the package directory.

Build failures surface the underlying exit status and never touch the
operator's config. The first build of a fork takes minutes (Prebid's
`npm ci` plus gulp); subsequent runs at an unchanged SHA are instant.

### 5. Generator: `--prebid-dir` and self-resolved specifiers

`build-prebid-external.mjs` accepts an optional `--prebid-dir <abs
path>`. When absent, behavior is unchanged. When present:

- `PREBID_PACKAGE_DIR` and every path derived from it (LiveIntent
  standard module, `prebidGlobal`, metadata directory, version read)
  come from the given directory.
- Adapter existence is checked against the resolved
  `dist/src/public/<name>BidAdapter.js` rather than the untranspiled
  `modules/` folder, because `dist/` is what actually gets bundled.
- The generator resolves `prebid.js/...` specifiers itself instead of
  relying on node_modules resolution, replicating the published
  `exports` layout:
  - `prebid.js` resolves to `<dir>/dist/src/src/prebid.public.js`;
  - `prebid.js/modules/<name>.js` resolves to
    `<dir>/dist/src/public/<name>.js`.
- Generated modules (`_adapters.generated.ts`,
  `_user_ids.generated.ts`, the entry file) emit absolute file paths,
  and the existing Vite aliases (`adapterManager`, `adRendering`,
  LiveIntent, `prebidGlobal`) point into the given directory. A plain
  directory alias is explicitly avoided because it would bypass the
  `exports` map and import untranspiled source.
- User ID registry entries in `user_id_modules.json` whose
  `importPath` starts with `prebid.js/` go through the same resolver;
  validation uses file existence at the resolved path instead of
  `require.resolve`.

Bare dependencies imported by the fork's `dist/` files resolve from the
fork's own `node_modules`, which exist because the CLI (or the
operator) just ran `npm ci` there.

### 6. Provenance in the manifest

The CLI passes the resolved provenance to the generator as a single
`--source <json>` argument. The generator records it verbatim under a
new `source` key in `manifest.json`:

```json
{
  "source": {
    "kind": "git",
    "url": "https://github.com/example-org/Prebid.js",
    "ref": "my-adapter-fix",
    "commit": "0123456789abcdef0123456789abcdef01234567"
  }
}
```

For a path source: `{ "kind": "path", "path": "/abs/path" }`. When no
`--source` is given the generator writes `{ "kind": "npm" }`. The Rust
manifest deserializer ignores unknown fields, so no compatibility work
is needed there; the CLI additionally prints the resolved source in its
summary output.

## Testing plan

### Rust CLI tests (`prebid_bundle.rs`, `run.rs`)

- Arg parsing: `--prebid-source` accepted, absent by default.
- Config parsing: `source` read from `[integrations.prebid.bundle]`,
  CLI flag wins over config, empty string rejected.
- Classification: `https://...`, `git@...`, `ssh://...` parse as git
  with and without `#ref`; other strings parse as paths; `http://` is
  rejected.
- Path validation: missing `dist/src/` fails with build-command
  guidance; a valid built layout passes; the source directory is never
  written to.
- Resolver flows with a fake `PrebidSourceResolver`: resolved directory
  and provenance reach the generator arguments; resolver failure leaves
  `trusted-server.toml` unpatched.
- Existing default-flow tests continue to pass unchanged.

### Generator tests (`test/build-prebid-external.test.mjs`)

- `parseArgs` accepts `--prebid-dir` and `--source`; both default to
  absent.
- Specifier resolution maps `prebid.js` and `prebid.js/modules/*.js`
  to the expected `dist/` paths under a fixture directory.
- Generated modules contain absolute imports when `--prebid-dir` is
  set and bare specifiers when it is not.
- Manifest contains the passed `source` object, and `{ "kind": "npm" }`
  when none is passed.
- Adapter existence errors name the missing `dist/src/public` file for
  a custom directory.

### Out of scope for automated tests

Cloning and building a real Prebid fork stays manual (network and
multi-minute build); the git/npm orchestration is covered through the
resolver trait with fakes.

## Documentation changes

- `docs/guide/cli.md`: document `--prebid-source`, the config key,
  precedence, ref resolution, the cache location, and the built-package
  requirement for path sources.
- `docs/guide/integrations/prebid.md`: document fork workflows, that
  git URLs are the shareable form for teams while local paths are for
  local iteration, and the `source` manifest field.
- `trusted-server.example.toml`: add a commented `source` line under
  `[integrations.prebid.bundle]` using an `example.com`/`example-org`
  placeholder.

## Files expected to change

- `crates/trusted-server-cli/src/prebid_bundle.rs`
  - `source` config field, `PrebidSource`, `PrebidSourceResolver`,
    path validation, git clone/build orchestration, generator arg and
    summary-output changes, tests.
- `crates/trusted-server-cli/src/run.rs`
  - `--prebid-source` flag wiring and parsing tests.
- `crates/trusted-server-js/lib/build-prebid-external.mjs`
  - `--prebid-dir` and `--source` arguments, specifier resolver,
    manifest `source` field.
- `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`
  - generator coverage above.
- `docs/guide/cli.md`, `docs/guide/integrations/prebid.md`,
  `trusted-server.example.toml`
  - documentation and template updates.

## Verification

```bash
cargo fmt --all -- --check
./scripts/test-cli.sh
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm
cd crates/trusted-server-js/lib && npx vitest run && npm run format
cd docs && npm run format
```

Manual verification for the git path: run `ts prebid bundle` against a
public fork URL twice and confirm the second run skips the clone/build,
the manifest records the resolved commit, and the produced bundle loads
in the existing external-bundle flow.

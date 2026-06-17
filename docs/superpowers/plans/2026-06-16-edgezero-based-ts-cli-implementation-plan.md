# EdgeZero-Based Trusted Server CLI Implementation Plan

**Date:** 2026-06-16
**Status:** Draft implementation plan
**Spec:** `docs/superpowers/specs/2026-06-16-edgezero-based-ts-cli-design.md`

## Decisions locked for this plan

- Start by moving this repository to the target EdgeZero PR #269 branch/rev; do
  not build the TS CLI against the older pinned EdgeZero rev.
- Keep platform lifecycle and platform writes inside EdgeZero. Trusted Server may
  transform app config, but it must not implement Fastly/Wrangler/Spin writes.
- For v1, literal secrets that still live in `Settings` are allowed to be written
  as flattened config-store entries. Secret-store write primitives are a future
  EdgeZero coordination item.
- Flattened keys escape path segments before joining: `\` -> `\\`, `.` -> `\.`.
- CLI validation must reject unknown fields throughout the typed settings schema,
  except for intentional dynamic map fields.
- Delegate commands support passthrough args after `--` and forward them
  verbatim to EdgeZero.
- `ts config init` may create a placeholder-filled config; `ts config validate`
  and `ts config push` must fail until required placeholders/secrets are
  replaced.

## Definition of done

- `ts` binary exists and implements the spec command surface.
- `ts config init`, `validate`, and `push` behave exactly as specified.
- Lifecycle commands are thin EdgeZero delegates and are covered by fake-delegate
  tests.
- Flatten/hash output is deterministic, escaped, and covered by known-vector
  tests.
- `trusted-server.toml` is operator-owned, ignored, and no longer compiled into
  runtime artifacts once the adjacent runtime-config-store migration lands.
- No Trusted Server code performs direct platform provisioning or config-store
  writes.
- Repository docs and verification commands are updated.

## Stage 0 — EdgeZero PR #269 baseline

1. Update root `Cargo.toml` EdgeZero git dependencies from the current pinned rev
   to the target PR #269 branch/rev.
2. Add any new EdgeZero crates needed by the CLI, likely including the library
   crate that exposes CLI command handlers and config-push primitives.
3. Run `cargo update` for the EdgeZero crates and inspect the resulting
   `Cargo.lock` diff.
4. Audit the target EdgeZero APIs for:
   - auth login/status/logout delegation;
   - provision delegation;
   - serve/build/deploy delegation;
   - manifest loading and adapter resolution;
   - logical config-store resolution;
   - caller-supplied flattened config-entry push;
   - `--local`, `--dry-run`, and `--runtime-config` support;
   - passthrough-arg support.
5. If a required EdgeZero API is missing, add it upstream on the EdgeZero branch
   first or pause. Do not add TS-owned platform write logic as a workaround.
6. Run an initial compile check after the bump to surface dependency/API fallout.

## Stage 1 — CLI crate and host-target test strategy

1. Add `crates/trusted-server-cli` with binary name `ts`.
2. Keep the implementation internal/testable; do not commit to a public reusable
   `trusted-server-cli` library API.
3. Decide and implement the workspace strategy before adding substantial code:
   - preferred: keep the crate as a workspace member, but target-gate the real
     CLI implementation to host targets and provide a tiny wasm-compatible stub
     so existing `cargo test --workspace` wasm gates keep working;
   - add explicit host commands for real CLI tests, for example
     `cargo test --package trusted-server-cli --target <host-triple>`;
   - document this in `CLAUDE.md` and/or `.cargo/config.toml` aliases.
4. Add dependencies only as needed: `clap`, `error-stack`, `derive_more`,
   `serde`, `serde_json`, `sha2`, `hex`, `toml`, `trusted-server-core`, and the
   EdgeZero CLI/delegate crate from Stage 0. Add `tempfile` as a justified
   dev-dependency for filesystem command tests if needed.
5. Implement internal modules:
   - `args` — clap command tree;
   - `run` — testable command dispatcher with injectable stdout/stderr writers;
   - `edgezero_delegate` — production EdgeZero wrapper plus fake test delegate;
   - `config_command` — init/validate/push orchestration.
6. Avoid `println!`/`eprintln!`; write to injected `Write` handles so clippy's
   print lints remain clean.
7. Add parser tests for every command shape, including passthrough args after
   `--`.

## Stage 2 — EdgeZero manifest and config template files

1. Add `edgezero.toml` using the target EdgeZero PR #269 manifest schema:
   - `[app] name = "trusted-server"`;
   - config store logical ID `app_config`;
   - secrets store logical ID `secrets`;
   - adapter command metadata for the supported initial adapter(s).
2. Create `trusted-server.example.toml` from the current tracked config, keeping
   only example/placeholder values and example domains.
3. Keep `trusted-server.example.toml` parseable as `Settings`, even though it is
   expected to fail placeholder-secret validation until an operator edits it.
4. Do not remove tracked `trusted-server.toml` until Stage 8 removes build-time
   embedding; otherwise current workspace builds will break.

## Stage 3 — Strict `Settings` schema validation

1. Audit every struct reachable from `Settings` in
   `crates/trusted-server-core/src/settings.rs` and related config modules.
2. Add `#[serde(deny_unknown_fields)]` to concrete non-map config structs.
3. Do not add `deny_unknown_fields` to intentional dynamic map wrappers or
   structs using `#[serde(flatten)]` as extension points.
4. Keep explicit dynamic maps for integrations, response headers, image profiles,
   and similar keyed config.
5. Add tests for:
   - unknown top-level fields;
   - unknown nested fields;
   - dynamic map keys still accepted;
   - current example config still parses before placeholder rejection.
6. Verify both `Settings::from_toml` and any remaining build/runtime parsing path
   still behave intentionally.

## Stage 4 — Deterministic config payload module

1. Put shared transformation logic in `trusted-server-core`, not only in the CLI,
   so the future runtime-config-store loader can reuse the same escaping and hash
   semantics.
2. Add a small public core module, for example `config_payload`, with documented
   APIs such as:
   - `escape_key_segment`;
   - `split_escaped_key` / inverse unescape helper;
   - `flatten_settings_value`;
   - `build_config_payload(&Settings)`.
3. Load and validate config for CLI use with:
   - UTF-8 file read;
   - TOML parse;
   - `Settings::from_toml` with no `TRUSTED_SERVER__` env overlay;
   - `Settings::reject_placeholder_secrets`.
4. Convert validated settings to `serde_json::Value` and flatten into
   `BTreeMap<String, String>`.
5. Flattening rules:
   - object keys are escaped path segments;
   - object entries recurse;
   - leaf values are stored as canonical JSON text so reconstruction is lossless;
   - strings are JSON-quoted strings;
   - booleans/numbers use JSON scalar text;
   - arrays use canonical minified JSON with recursively sorted object keys;
   - nulls are skipped;
   - final settings keys beginning with `ts-config-` are rejected.
6. Compute metadata:
   - `ts-config-keys` = minified sorted JSON array of settings-only keys;
   - `ts-config-hash` = `sha256:<hex>` over the canonical settings-only entry
     map JSON bytes;
   - hash excludes metadata entries.
7. Add known-vector tests covering:
   - nested flattening;
   - `.` and `\` key escaping;
   - arrays and canonical object ordering inside arrays;
   - null skipping;
   - lexicographic ordering by escaped key;
   - metadata exclusion from hash;
   - stable hash for reordered TOML input;
   - dynamic map stability.

## Stage 5 — `ts config init` and `ts config validate`

1. Implement `ts config init [--config <path>] [--force]`:
   - use the source-controlled example template as the copy source, embedded at
     build time or otherwise available independent of an operator-owned config;
   - create parent directories;
   - refuse overwrite without `--force`;
   - do not read `edgezero.toml`;
   - do not contact EdgeZero/platforms;
   - print only `Initialized config at <path>` on success.
2. Implement `ts config validate [--config <path>] [--json]`:
   - run the Stage 4 loader/payload pipeline;
   - produce human output on success;
   - produce JSON success/failure shape exactly as specified;
   - on `--json` failure, write JSON to stdout and exit non-zero;
   - on human failure, write errors and hints to stderr;
   - never print config values or secrets.
3. Add command tests for:
   - default/custom config paths;
   - missing file hint;
   - malformed TOML;
   - unknown fields;
   - semantic validation errors;
   - placeholder rejection;
   - JSON success/failure validity;
   - `config init` output failing validation until placeholders are replaced.

## Stage 6 — EdgeZero lifecycle delegation

1. Implement the production `EdgeZeroDelegate` wrapper around the Stage 0
   EdgeZero APIs.
2. Support:
   - `ts auth login/status/logout --adapter <adapter> [-- ...]`;
   - `ts provision --adapter <adapter> [-- ...]`;
   - `ts serve --adapter <adapter> [-- ...]`;
   - `ts build --adapter <adapter> [-- ...]`;
   - `ts deploy --adapter <adapter> [-- ...]`.
3. Forward adapter and passthrough args verbatim.
4. Do not read, validate, flatten, or push `trusted-server.toml` in these
   lifecycle commands unless EdgeZero itself requires manifest context.
5. Surface EdgeZero adapter/manifest errors without converting them into
   TS-owned platform logic.
6. Add fake-delegate tests proving each command calls the expected EdgeZero
   method with the selected adapter and passthrough args.

## Stage 7 — `ts config push`

1. Implement `ts config push` after Stage 4 payload generation and Stage 6
   EdgeZero delegation are in place.
2. Parse:
   - required `--adapter`;
   - `--config`, default `trusted-server.toml`;
   - `--manifest`, default `edgezero.toml`;
   - `--store`, default `app_config`;
   - `--local`;
   - `--dry-run`;
   - `--runtime-config`.
3. Run the exact same validation/flatten/hash path as `config validate`.
4. Build the push entry map with settings entries plus `ts-config-keys` and
   `ts-config-hash`.
5. Call EdgeZero's caller-supplied-entry config push API with adapter, manifest,
   logical store, local/dry-run/runtime-config options, and entries.
6. Ensure `--dry-run` does not mutate local or remote adapter state. TS output
   should show key names, entry count, and hash, never full values.
7. Add fake-push tests for:
   - validation happens before push;
   - metadata entries are included;
   - default store is `app_config`;
   - all flags/options are forwarded;
   - dry-run reaches the delegate as dry-run;
   - secret-store writes are never requested;
   - no full config values appear in output.

## Stage 8 — Runtime/file-ownership alignment

This spec does not define runtime loading details, but the repository is not
fully compliant with the file ownership model until build-time config embedding
is removed.

1. Land or implement the runtime-config-store spec that reads flattened
   `app_config` entries at runtime, uses the same escaping/hash helpers, and
   fails closed when runtime config is invalid.
2. Remove the current build-time `trusted-server.toml` embedding path:
   - stop `build.rs` from reading `../../trusted-server.toml`;
   - remove or replace `settings_data.rs` embedded bytes usage;
   - remove `TRUSTED_SERVER__` build-time app-settings env overlay.
3. Move the source-controlled app config to `trusted-server.example.toml` only.
4. Add `trusted-server.toml` to `.gitignore` and remove it from git tracking.
5. Keep local dev/test fixtures explicit so tests do not depend on an
   operator-owned root `trusted-server.toml`.

## Stage 9 — Documentation and verification

1. Update operator docs with the minimal workflow:

   ```bash
   ts config init
   ts config validate
   ts auth login --adapter fastly
   ts provision --adapter fastly
   ts config push --adapter fastly
   ts serve --adapter fastly
   ts deploy --adapter fastly
   ```

2. Update `CLAUDE.md` for:
   - the new CLI crate;
   - host-target CLI test command;
   - `edgezero.toml` and `trusted-server.example.toml` ownership;
   - removal of `trusted-server.toml` as a tracked/build-time file.
3. Update `CONTRIBUTING.md` if developer workflow or verification commands
   change.
4. Run verification:
   - `cargo fmt --all -- --check`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace`;
   - host-target CLI tests, e.g. `cargo test --package trusted-server-cli --target <host-triple>`;
   - `cargo build --package trusted-server-cli --target <host-triple>`;
   - `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`;
   - JS/docs checks only if those areas are touched.

## Risks and watch points

- The exact EdgeZero PR #269 API shape may differ from the spec assumptions.
  Resolve that upstream before adding TS-owned workarounds.
- Host-only CLI testing must not break existing wasm-default workspace gates.
- `deny_unknown_fields` can uncover previously accepted config typos; update
  tests and examples deliberately.
- Arrays stored as JSON values need canonical serialization to keep hashes
  stable.
- Runtime reconstruction of flattened entries is owned by the runtime-config
  spec; share escaping/hash helpers now to avoid divergent behavior later.
- Literal secrets in config-store entries are accepted for v1 but must never be
  logged or printed.

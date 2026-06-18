# Trusted Server CLI — Prebid Bundle Generation

**Date:** 2026-06-17
**Status:** Implemented
**Scope:** `ts prebid bundle` local external Prebid bundle generation
**Related context:**

- `docs/superpowers/specs/2026-05-28-external-prebid-first-party-proxy-design.md`
- `docs/superpowers/specs/2026-06-16-edgezero-based-ts-cli-design.md`
- `crates/trusted-server-js/lib/build-prebid-external.mjs`
- `crates/trusted-server-js/lib/package.json`

---

## 1. Goal

Add a Trusted Server-specific CLI command for generating the external Prebid
browser bundle used by the first-party Prebid proxy flow:

```bash
ts prebid bundle
```

The command should make the existing external bundle generation path ergonomic for
operators by reading bundle selections from `trusted-server.toml`, running the
existing JS/Vite generator, writing local build artifacts to `dist/prebid` by
default, and updating `trusted-server.toml` with the generated bundle integrity
metadata.

The command is intentionally a Trusted Server product command, not an EdgeZero
lifecycle command. It does not require `--adapter`, does not upload assets, and
does not provision or deploy platform resources.

The external Prebid runtime model remains the one defined by the first-party
proxy spec:

1. Prebid is not embedded in the Trusted Server WASM/TSJS bundle.
2. A generated external Prebid bundle is hosted by the operator.
3. Trusted Server injects `/integrations/prebid/bundle.js[?v=<sha256>]`.
4. Trusted Server proxies that first-party URL to `integrations.prebid.external_bundle_url`.

---

## 2. Non-goals

The initial `ts prebid bundle` command does **not** do any of the following:

- upload generated bundles to an asset host or CDN;
- infer or construct the public `external_bundle_url`;
- accept an asset base URL and derive hosted URLs;
- call EdgeZero adapter lifecycle commands;
- require or accept `--adapter`;
- push `trusted-server.toml` to a config store;
- run `npm install`, `npm ci`, or otherwise mutate JS dependencies;
- port the Prebid bundler from Node/Vite into Rust;
- change the generated `manifest.json` schema;
- change the first-party proxy runtime behavior;
- generate arbitrary Prebid runtime module choices at the edge;
- support remote or platform-hosted bundling.

---

## 3. Command surface

```bash
ts prebid bundle [--config <path>] [--out <dir>]
```

Defaults:

| Option     | Default               | Description                                    |
| ---------- | --------------------- | ---------------------------------------------- |
| `--config` | `trusted-server.toml` | Trusted Server app config to read and update   |
| `--out`    | `dist/prebid`         | Local output directory for generated artifacts |

Examples:

```bash
# Generate from trusted-server.toml into dist/prebid
ts prebid bundle

# Generate from a draft config
ts prebid bundle --config ./publisher-a.trusted-server.toml

# Generate into a custom local directory
ts prebid bundle --out ./build/prebid
```

Successful output should be concise and actionable, for example:

```text
Built Prebid bundle: dist/prebid/trusted-prebid-<sha256>.js
Manifest: dist/prebid/manifest.json
Updated config: trusted-server.toml
Next: upload the bundle and set integrations.prebid.external_bundle_url to its HTTPS URL if needed.
```

The default output directory `dist/prebid` is a local generated-artifact path and
must be git-ignored at the repository root.

---

## 4. Trusted Server config schema

Bundle-generation selections live in `trusted-server.toml` under the Prebid
integration block:

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com/openrtb2/auction"
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid.js"

[integrations.prebid.bundle]
adapters = ["rubicon", "kargo"]
user_id_modules = ["sharedIdSystem", "uid2IdSystem"]
```

### 4.1 Field semantics

| Field                                        | Required | Description                                                                                                                  |
| -------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `integrations.prebid.bundle.adapters`        | Yes      | Prebid bidder adapter module names passed to the external bundle generator, e.g. `rubicon` -> `rubiconBidAdapter.js`         |
| `integrations.prebid.bundle.user_id_modules` | No       | Prebid User ID module names passed to the external bundle generator. When omitted, the JS generator's default preset is used |

The bundle config is intentionally separate from existing runtime fields:

- `integrations.prebid.bidders` controls server-side bidders routed through the
  Trusted Server / Prebid Server auction flow.
- `integrations.prebid.client_side_bidders` controls browser-side bidder behavior
  in the injected Prebid client config.
- `integrations.prebid.bundle.adapters` controls which native Prebid.js adapter
  modules are statically imported into the generated external browser bundle.

Operators may choose to keep `client_side_bidders` and `bundle.adapters` aligned,
but the CLI should not infer one from the other in this initial design.

### 4.2 Config compatibility

The existing runtime fields remain unchanged:

```toml
[integrations.prebid]
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid.js"
external_bundle_sha256 = "..."
external_bundle_sri = "sha384-..."
```

`external_bundle_url` remains manually authored by the operator. The CLI does not
infer it from `--out` and does not know where the operator will upload the local
bundle.

---

## 5. Config update behavior

After a successful local bundle build, `ts prebid bundle` must read the generated
`manifest.json` and update the same `trusted-server.toml` file with:

```toml
[integrations.prebid]
external_bundle_sha256 = "<manifest.sha256>"
external_bundle_sri = "<manifest.sri>"
```

The command must preserve `external_bundle_url` as-is. It must not overwrite,
derive, or remove the URL.

If `external_bundle_url` is absent, the command may still generate the bundle and
write `external_bundle_sha256` / `external_bundle_sri`, but it must report a clear
next step telling the operator to set `integrations.prebid.external_bundle_url` to
the hosted HTTPS URL before validation/push/deploy.

If `external_bundle_url` points at an old content-addressed filename, the command
must not guess the replacement. It should print the generated filename and remind
the operator to update `external_bundle_url` manually after upload.

Config writes should be atomic: write to a temporary file next to the config and
rename into place after serialization succeeds. The implementation should prefer
a TOML editing library such as `toml_edit` so comments, ordering, and unrelated
formatting are preserved as much as practical.

---

## 6. Local build behavior

The Rust CLI should shell out to the existing JS bundler instead of reimplementing
Prebid/Vite bundling in Rust.

Expected invocation model:

1. Locate `crates/trusted-server-js/lib/package.json` and `crates/trusted-server-js/lib/build-prebid-external.mjs`
   relative to the repository root/current working tree.
2. Read `[integrations.prebid.bundle]` from the selected config file.
3. Convert configured lists to the existing generator's CSV arguments.
4. Run the existing npm script from `crates/trusted-server-js/lib`:

```bash
npm run build:prebid-external -- \
  --adapters rubicon,kargo \
  --user-id-modules sharedIdSystem,uid2IdSystem \
  --out <absolute-or-repo-relative-output-dir>
```

If `user_id_modules` is omitted, the CLI should omit `--user-id-modules` so the
JS generator uses its existing default preset.

The generated output remains the current JS generator output:

```text
dist/prebid/
  trusted-prebid-<sha256>.js
  manifest.json
```

The manifest schema remains unchanged:

```json
{
  "prebidVersion": "10.26.0",
  "adapters": ["rubicon", "kargo"],
  "userIdModules": ["sharedIdSystem", "uid2IdSystem"],
  "sha256": "abc123...",
  "sri": "sha384-...",
  "filename": "trusted-prebid-abc123.js"
}
```

---

## 7. Dependency and environment handling

`ts prebid bundle` should fail fast with actionable diagnostics when local JS
build prerequisites are missing.

Minimum checks before shelling out:

- `npm` is available on `PATH`;
- `crates/trusted-server-js/lib/package.json` exists;
- `crates/trusted-server-js/lib/build-prebid-external.mjs` exists;
- `crates/trusted-server-js/lib/node_modules` exists.

If `node_modules` is missing, the command must not run dependency installation.
It should fail with an instruction like:

```text
Prebid bundling dependencies are missing. Run `cd crates/trusted-server-js/lib && npm ci`, then retry `ts prebid bundle`.
```

Errors from the JS generator, including unknown adapter names or unknown User ID
module names, should be surfaced without hiding the original generator message.
The CLI may add a short Trusted Server context prefix, but should preserve stdout
and stderr enough for debugging.

---

## 8. Config loading and validation

`ts prebid bundle` should not require full production config validity. It is a
local artifact-generation command, and operators may run it before the config is
ready for `ts config validate` or `ts config push`.

The command should perform focused validation only for the fields it needs:

- selected config file exists and parses as TOML;
- `[integrations.prebid]` exists;
- `[integrations.prebid.bundle]` exists;
- `bundle.adapters` is a non-empty array of non-empty strings;
- `bundle.user_id_modules`, when present, is an array of non-empty strings;
- `--out` resolves to a writable local directory path.

The JS generator remains responsible for validating that adapter and User ID
module names correspond to available Prebid package modules.

After the command updates hash/SRI metadata, `ts config validate` remains the
source of truth for full deployment readiness, including `external_bundle_url`
requirements, placeholder secret rejection, and runtime config validation.

---

## 9. Integration with existing CLI design

This spec extends the `ts` product CLI command surface with a new Trusted
Server-specific command group:

```text
ts prebid bundle
```

The resulting CLI command enum should conceptually become:

```text
ts audit ...
ts config ...
ts prebid bundle ...
ts auth ...
ts provision ...
ts serve ...
ts build ...
ts deploy ...
```

`ts prebid bundle` is similar to `ts audit` and `ts config` in that it owns
Trusted Server behavior directly. It is unlike `ts build` / `ts deploy`, which
are EdgeZero lifecycle delegates.

---

## 10. Required code changes

### CLI argument parsing

- Add `Command::Prebid(PrebidArgs)`.
- Add `PrebidCommand::Bundle(PrebidBundleArgs)`.
- Add options:
  - `--config <path>` defaulting to `trusted-server.toml`;
  - `--out <dir>` defaulting to `dist/prebid`.
- Add parser tests for defaults and custom paths.
- Reject `--adapter` for `ts prebid bundle`.

### CLI implementation

- Add a Prebid bundle command module, for example
  `crates/trusted-server-cli/src/prebid_bundle.rs`.
- Parse focused bundle config from TOML.
- Check local JS dependency prerequisites.
- Shell out to `npm run build:prebid-external -- ...` in `crates/trusted-server-js/lib`.
- Read generated `manifest.json`.
- Atomically update `external_bundle_sha256` and `external_bundle_sri` in the
  selected config file.
- Print concise success output and next steps.

### JS tooling

No manifest format change is required.

The existing `crates/trusted-server-js/lib/build-prebid-external.mjs` should remain the source
of truth for generating the bundle, validating adapter module files, validating
User ID module names, hashing bundle bytes, and writing `manifest.json`.

### Git ignore

- Add `/dist/prebid/` to the repository root `.gitignore`.

---

## 11. Test plan

### CLI parser tests

- `ts prebid bundle` parses with defaults:
  - config: `trusted-server.toml`
  - out: `dist/prebid`
- `ts prebid bundle --config publisher.toml --out build/prebid` parses custom paths.
- `ts prebid bundle --adapter fastly` is rejected.

### Unit tests

- Bundle config loader accepts valid `[integrations.prebid.bundle]` settings.
- Bundle config loader rejects missing Prebid block.
- Bundle config loader rejects missing bundle block.
- Bundle config loader rejects empty or malformed adapter arrays.
- Config patcher writes `external_bundle_sha256` and `external_bundle_sri`.
- Config patcher preserves existing `external_bundle_url`.
- Config patcher creates missing `external_bundle_sha256` / `external_bundle_sri`
  fields when absent.
- Missing `node_modules` fails with an instruction to run `cd crates/trusted-server-js/lib && npm ci`.

### Integration-style CLI tests

- With a fake shell delegate/process runner, the command invokes:
  - program: `npm`
  - cwd: `crates/trusted-server-js/lib`
  - args: `run build:prebid-external -- --adapters ... --out ...`
- When `user_id_modules` is omitted, `--user-id-modules` is not passed.
- When the fake generator writes `manifest.json`, the selected config is patched
  from that manifest.
- Generator failure returns a CLI error and does not update config.

### Manual smoke test

```bash
cd crates/trusted-server-js/lib
npm ci
cd ../../..

ts prebid bundle
ls dist/prebid
rg 'external_bundle_sha256|external_bundle_sri' trusted-server.toml
```

Then upload the generated JS file manually, set or verify
`integrations.prebid.external_bundle_url`, and run:

```bash
ts config validate
```

---

## 12. Open follow-up work

These are intentionally outside the initial local-only command, but the design
should not preclude them later:

- optional upload support through EdgeZero/platform asset primitives;
- optional asset URL/base URL handling;
- manifest generator metadata such as Trusted Server CLI version or source
  revision;
- stronger checks that `external_bundle_url` corresponds to the generated
  content-addressed filename;
- richer JSON output for CI automation.

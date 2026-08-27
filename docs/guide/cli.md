# Trusted Server CLI

The Trusted Server CLI binary is `ts`. It is a host-target operator tool for
configuration, page audits, and EdgeZero-backed lifecycle commands.

## Install from source

From the repository root, install the `ts` binary with the workspace Cargo alias:

```bash
cargo install-cli
```

The alias runs `cargo install --path crates/trusted-server-cli --bin ts --locked --force`.
Because it does not pass an explicit `--target`, Cargo builds the CLI for your
current host platform. The binary is installed into Cargo's bin directory,
usually `~/.cargo/bin`; make sure that directory is on your `PATH`.

For example, add Cargo's bin directory to your current shell session:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Verify the install:

```bash
ts --help
```

## Common workflow

```bash
ts config init
# Edit trusted-server.toml
ts config validate
ts auth login --adapter fastly
ts provision --adapter fastly
ts config push --adapter fastly
ts serve --adapter fastly
```

## Configuration commands

Create a starter Trusted Server config:

```bash
ts config init
```

`config init` accepts `--app-config <path>` and the compatibility alias
`--config <path>`.

Validate a local config before pushing it to platform storage:

```bash
ts config validate
```

Push Trusted Server config through EdgeZero:

```bash
ts config push --adapter fastly
```

`config validate`, `config diff`, and `config push` use EdgeZero's typed
app-config loader. By default that loader applies `TRUSTED_SERVER__...`
environment overlays before validation, comparison, and blob creation. The
overlay only overrides leaves already present in the TOML; add newly introduced
fields to existing configs before relying on their overrides. Pass `--no-env`
for file-only operation. See [Configuration](/guide/configuration#environment-variable-overrides-typed-cli)
for migration and rollback guidance.

`config push` publishes a single EdgeZero `BlobEnvelope` containing the validated
Trusted Server settings JSON. This blob model is intentional because full
Trusted Server configs can exceed Fastly limits when split into one config-store
entry per setting.

Reclaim orphaned chunk entries leaked from prior oversized pushes:

```bash
ts config gc --adapter fastly
```

Without `--yes`, `config gc` only previews: it reports what it would delete and
deletes nothing. `--dry-run` states that intent explicitly and conflicts with
`--yes`. To actually delete, pass `--yes` together with `--older-than <window>`
(`s`/`m`/`h`/`d` suffixes, e.g. `7d`; a bare number means seconds):

```bash
ts config gc --adapter fastly --yes --older-than 7d
```

`config gc` sweeps every root in the selected physical store, so `--older-than`
is a safety assertion about the whole store: nothing in it changed within the
window and no writer is targeting it. Unlike the other `config` subcommands,
`gc` never loads the typed app config; its `--no-env` flag instead ignores
`EDGEZERO__STORES__CONFIG__<ID>__NAME` when resolving which physical store to
sweep, and `--store <id>` overrides the manifest's config-store id outright.
Both change which store gets swept, so on a destructive run check the store id
`gc` reports before passing `--yes`.

## Lifecycle commands

Lifecycle commands delegate to the selected EdgeZero adapter:

```bash
ts auth login --adapter fastly
ts build --adapter fastly
ts provision --adapter fastly
ts deploy --adapter fastly
ts serve --adapter fastly
```

`ts deploy` accepts `--staging` (Fastly only) to build and upload a staged
draft version cloned from the active one instead of activating a production
deploy. Adapter passthrough arguments must now follow a `--` separator; unknown
flags before `--` (including the renamed-away `--stage`) are rejected at parse
time rather than forwarded. This is a change: passthrough args previously
worked without the separator, so existing runbooks and CI jobs that pass
adapter flags directly need the `--` added:

```bash
ts deploy --adapter fastly --service-id <service-id> --staging
ts deploy --adapter fastly -- --comment "release"
```

A staged deploy only redirects the staged version's config selector at the
`<logical-store-id>_staging` key — it does not copy the production config blob
there. Push the staged config before probing the staged version:

```bash
ts config push --adapter fastly --staging
ts config diff --adapter fastly --staging
```

> **Known limitation:** Trusted Server's Fastly entry point does not yet read
> the version-linked `edgezero_runtime_env` selectors, so a staged version
> currently loads the **production** config blob rather than the staged one —
> a staging healthcheck exercises the new binary against production config.
> Selector resolution for custom entry points is tracked upstream in EdgeZero;
> until it lands, do not rely on `--staging` to validate a config change.

`--staging` on `config push` / `config diff` writes and compares the
`<logical-store-id>_staging` key in the same store. It is mutually exclusive
with `--key`: the staging key is derived from the store's logical id, so an
explicit key would be written where nothing reads it.

Inspect and verify deployments with the deploy lifecycle commands. All three are
Fastly-only — the axum, cloudflare, and spin adapters reject them:

```bash
# Capture the production rollback target BEFORE deploying: after a deploy this
# prints the NEW version, and Fastly keeps no record of which version was live
# before it, so the target is then unrecoverable.
ts active-version --adapter fastly --service-id <service-id>

# Probe a deployed version until it reports healthy. `<version>` is the version
# the deploy activated; pass `--service-id` to `ts deploy` and it emits that as
# a machine-readable `version=<N>` line.
ts healthcheck --adapter fastly --service-id <service-id> \
  --version <version> --domain edge.example

# Re-activate the version captured before the deploy
ts rollback --adapter fastly --service-id <service-id> \
  --version <bad-version> --rollback-to <previous-version>
```

`healthcheck` probes `/` by default (`--path` overrides) and makes 3 total
attempts — not 3 retries after a first try — with a 5 second delay between
attempts and a 10 second per-attempt timeout (`--retry`, `--retry-delay`,
`--timeout`). With `--staging` it resolves the staged version's IP from the
service id and probes that instead of the production endpoint.

`rollback` cannot infer the production rollback target: Fastly exposes no
metadata to tell a previously live version from a staged one, so pass the
version to re-activate via `--rollback-to`. With `--staging`, it deactivates
the staged `--version` instead and needs no `--rollback-to`.

## Audit a public page

`ts audit` loads a public page in a fresh headless Chrome/Chromium session,
collects rendered JavaScript asset evidence, detects known Trusted Server
integrations, and writes local draft artifacts.

Chrome or Chromium must be installed locally. The command checks common PATH
names and standard macOS/Linux install locations.

```bash
ts audit https://publisher.example
```

By default, the command writes:

| File                  | Purpose                                                                  |
| --------------------- | ------------------------------------------------------------------------ |
| `js-assets.toml`      | JavaScript asset inventory, detected integrations, counts, and warnings. |
| `trusted-server.toml` | Draft Trusted Server config based on the starter template and final URL. |

The generated config is a draft. Review it, replace placeholders/secrets, adjust
publisher-specific settings, then run:

```bash
ts config validate
```

If a config already exists, avoid overwriting it:

```bash
ts audit https://publisher.example --no-config
```

Use custom output paths when reviewing artifacts first:

```bash
ts audit https://publisher.example \
  --js-assets audit/js-assets.toml \
  --config audit/trusted-server.toml
```

Use `--force` only when replacing existing output files is intentional:

```bash
ts audit https://publisher.example --force
```

`ts audit` is not an EdgeZero adapter command. It has no `--adapter` option and
it does not provision resources, push config, build, deploy, or contact platform
APIs.

## Generate an external Prebid bundle

`ts prebid bundle` builds the local external Prebid browser bundle configured in
`trusted-server.toml`.

```toml
[integrations.prebid.bundle]
adapters = ["rubicon", "kargo"]
user_id_modules = ["sharedIdSystem"]
```

Run the command after installing JS dependencies:

```bash
cd crates/trusted-server-js/lib && npm ci
cd ../../..
ts prebid bundle
```

By default, generated artifacts are written to `dist/prebid/`, and the command
updates `integrations.prebid.external_bundle_sha256` and
`integrations.prebid.external_bundle_sri` in `trusted-server.toml`. Upload the
generated JavaScript file yourself, set `external_bundle_url` to its HTTPS
asset URL, and include that host (plus any redirect targets) in
`proxy.allowed_domains` before running `ts config validate` or `ts config push`.

Use custom paths when needed:

```bash
ts prebid bundle --config publisher-a.toml --out build/prebid
```

`ts prebid bundle` is local-only. It has no `--adapter` option and does not
upload, provision, deploy, or push config.

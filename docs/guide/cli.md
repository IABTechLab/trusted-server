# Trusted Server CLI

The Trusted Server CLI is the operator tool for local configuration, page audits, Fastly credentials, local development, and Fastly resource provisioning.

The binary name is `ts`. Use it when you want to validate `trusted-server.toml`, start a local Fastly Compute server with runtime configuration, inspect a publisher page for integrations, or provision Fastly stores and bindings for an existing Compute service.

## Requirements

Install these tools before using the CLI:

- Rust, pinned by this repository in `.tool-versions` and `rust-toolchain.toml`
- Fastly CLI, required by `ts dev` and Fastly deployments
- A Fastly API token, required by `ts provision fastly ...`
- Chrome or Chromium, required by `ts audit`

The CLI is a host-target binary. Do not build or run it for `wasm32-wasip1`.

## Run from source

From the repository root, use the Cargo aliases in `.cargo/config.toml` when you need to build, check, test, or install the host-target CLI. These aliases avoid Cargo's default workspace target, which is `wasm32-wasip1` for the runtime crates.

```bash
cargo build_cli
cargo check_cli
cargo test_cli
cargo install_cli
```

After installation, verify that the command is on your path:

```bash
ts --help
```

If you do not want to install the binary, run it directly with an explicit host target:

```bash
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo run --package trusted-server-cli --bin ts --target "$HOST_TARGET" -- --help
```

## Common workflow

A typical local workflow starts with a config file, validates it, and then starts Fastly Compute locally:

```bash
ts config init
# Edit trusted-server.toml
ts config validate
ts dev -a fastly
```

To create a draft config from a live publisher page, audit the page before you write the final config:

```bash
ts audit https://publisher.example
```

If you already have `trusted-server.toml`, avoid overwriting it during audit:

```bash
ts audit https://publisher.example --no-config
```

To provision Fastly resources for an existing Compute service:

```bash
ts auth fastly login
ts provision fastly plan --service-id svc_123
FASTLY_RUNTIME_API_KEY=your-runtime-token \
  ts provision fastly apply --service-id svc_123
```

## Paths and config files

Most commands use `trusted-server.toml` in the current working directory by default. Pass `--config <path>` to use a different file:

```bash
ts config validate --config config/publisher.toml
ts dev --config config/publisher.toml
ts provision fastly plan --service-id svc_123 --config config/publisher.toml
```

Relative paths resolve from the current working directory. Absolute paths are used as-is.

## Cargo aliases

This repository sets `wasm32-wasip1` as the default Cargo build target because the runtime deploys to Fastly Compute. The CLI is host-only, so CLI Cargo commands must override that default target.

Use these aliases from the repository root:

| Alias                | Expands to                                                                     | Purpose                                                                     |
| -------------------- | ------------------------------------------------------------------------------ | --------------------------------------------------------------------------- |
| `cargo build_cli`    | `cargo build --package trusted-server-cli --target aarch64-apple-darwin`       | Build the CLI for the configured host target.                               |
| `cargo check_cli`    | `cargo check --package trusted-server-cli --target aarch64-apple-darwin`       | Type-check the CLI for the configured host target.                          |
| `cargo test_cli`     | `cargo test --package trusted-server-cli --target aarch64-apple-darwin`        | Run CLI tests on the configured host target.                                |
| `cargo install_cli`  | `cargo install --path crates/trusted-server-cli --target aarch64-apple-darwin` | Install `ts` from the local checkout.                                       |
| `cargo test_details` | `cargo test --target aarch64-apple-darwin`                                     | Run tests for the configured host target when you need host-target details. |

The current aliases target `aarch64-apple-darwin`. If you are not on Apple Silicon macOS, use the explicit host-target form instead:

```bash
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo test --package trusted-server-cli --target "$HOST_TARGET"
```

## Command reference

### `ts config init`

Create a starter `trusted-server.toml` file from the repository example template.

```bash
ts config init [--config <path>] [--force]
```

| Option            | Description                                                               |
| ----------------- | ------------------------------------------------------------------------- |
| `--config <path>` | Write the starter config to this path. Defaults to `trusted-server.toml`. |
| `--force`         | Overwrite the target file if it already exists.                           |

By default, the command refuses to overwrite an existing file:

```text
refusing to overwrite existing file `trusted-server.toml`; re-run with --force
```

### `ts config validate`

Validate a Trusted Server config file and print the canonical config hash.

```bash
ts config validate [--config <path>] [--json]
```

| Option            | Description                                                   |
| ----------------- | ------------------------------------------------------------- |
| `--config <path>` | Validate this config file. Defaults to `trusted-server.toml`. |
| `--json`          | Write a machine-readable validation result to stdout.         |

Human-readable output includes the resolved path and config hash:

```text
Config valid: /path/to/trusted-server.toml
Config hash: 5f2c...
```

JSON output uses this shape:

```json
{
  "valid": true,
  "path": "/path/to/trusted-server.toml",
  "config_hash": "5f2c...",
  "errors": []
}
```

When validation fails with `--json`, the command still writes JSON to stdout, sets `valid` to `false`, puts formatted errors in `errors`, and exits with a non-zero status.

### `ts dev`

Validate local config, write a Fastly local manifest, and run `fastly compute serve`.

```bash
ts dev [--adapter fastly] [--config <path>] [passthrough args...]
```

| Option                 | Description                                                                                 |
| ---------------------- | ------------------------------------------------------------------------------------------- |
| `-a, --adapter fastly` | Select the runtime adapter. `fastly` is the only supported value.                           |
| `--config <path>`      | Use this config file. Defaults to `trusted-server.toml`.                                    |
| `passthrough args...`  | Pass extra arguments to `fastly compute serve`. Use `--` before Fastly options for clarity. |

The command writes `fastly.local.toml` in the current working directory. That file extends `fastly.toml` and embeds the canonical Trusted Server config in the local Fastly config store named `ts_config_store`, under item key `ts-config`.

Then the CLI runs:

```bash
fastly compute serve --dir <current-directory> --env=local
```

Pass Fastly CLI options after `--`:

```bash
ts dev -- --skip-build
ts dev -- --watch
ts dev -- --addr 127.0.0.1:7676
```

When `--skip-build` is passed without `--file`, the CLI looks for an existing Wasm binary at:

1. `target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm`
2. `target/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm`

If neither file exists, build the Fastly adapter first:

```bash
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
```

### `ts audit`

Audit a public URL with a real Chrome or Chromium browser session, then write draft assets and config files.

```bash
ts audit [options] <url>
```

| Option               | Description                                                                            |
| -------------------- | -------------------------------------------------------------------------------------- |
| `<url>`              | Public page URL to audit.                                                              |
| `--js-assets <path>` | Write the JS asset audit to this path. Defaults to `js-assets.toml`.                   |
| `--config <path>`    | Write the draft Trusted Server config to this path. Defaults to `trusted-server.toml`. |
| `--no-js-assets`     | Do not write the JS asset audit file.                                                  |
| `--no-config`        | Do not write the draft Trusted Server config.                                          |
| `--force`            | Overwrite existing output files.                                                       |

The audit collector loads the page in Chromium, reads script tags, records script network requests, and classifies assets as first-party or third-party by host relationship. It detects these integration IDs when there is matching URL or inline-script evidence:

- `google_tag_manager`
- `gpt`
- `didomi`
- `datadome`
- `permutive`
- `lockr`
- `prebid`

By default, `ts audit` writes two files:

| File                  | Purpose                                                                  |
| --------------------- | ------------------------------------------------------------------------ |
| `js-assets.toml`      | Audit artifact with detected assets, integrations, counts, and warnings. |
| `trusted-server.toml` | Draft config based on the starter template and the audited page host.    |

The draft config updates publisher host fields from the audited URL. It can enable GPT, Didomi, DataDome, and Google Tag Manager when those integrations are detected. Other detected integrations are added as comments that require manual review.

Use `--no-config` when you already have a config file:

```bash
ts audit https://publisher.example --no-config
```

Use custom output paths when you want to inspect the generated files before moving them into place:

```bash
ts audit https://publisher.example \
  --js-assets audit/js-assets.toml \
  --config audit/trusted-server.toml
```

Use `--force` only when you intend to replace existing output files:

```bash
ts audit https://publisher.example --force
```

The command exits with an argument error if both `--no-js-assets` and `--no-config` are set, since there would be no output to write.

### `ts auth fastly login`

Prompt for a Fastly API token and store it in the host secure credential store.

```bash
ts auth fastly login
```

Use this for local development. For CI and automation, set `FASTLY_API_KEY` instead of storing a credential on the machine.

### `ts auth fastly status`

Inspect Fastly credential availability.

```bash
ts auth fastly status [--json]
```

Human-readable output reports whether each source is present and which source is active:

```text
Environment credential: present
Stored credential: present
Effective source: environment
```

`FASTLY_API_KEY` takes precedence over secure storage. JSON output uses this shape:

```json
{
  "has_env_credential": true,
  "has_stored_credential": false,
  "effective_source": "environment"
}
```

`effective_source` is `environment`, `secure-storage`, or `null`.

### `ts auth fastly logout`

Remove the stored Fastly credential from secure storage.

```bash
ts auth fastly logout
```

This does not unset `FASTLY_API_KEY`. If the environment variable is set, it remains the effective credential source.

### `ts provision fastly plan`

Preview Fastly resources and bindings needed for the local config.

```bash
ts provision fastly plan --service-id <service-id> [--config <path>] [--json]
```

| Option                      | Description                                                  |
| --------------------------- | ------------------------------------------------------------ |
| `--service-id <service-id>` | Existing Fastly Compute service ID. Required.                |
| `--config <path>`           | Config file to provision. Defaults to `trusted-server.toml`. |
| `--json`                    | Write the plan as JSON.                                      |

The command uses `FASTLY_API_KEY` or the stored Fastly credential from `ts auth fastly login`. It does not modify the service.

Plan output includes:

- Service ID and config path
- Latest and target Fastly service version
- Whether cloning the service version is required
- Planned create, update, and bind actions
- Warnings, including request-signing bootstrap and locked service versions

JSON output uses this high-level shape:

```json
{
  "service_id": "svc_123",
  "config_path": "/path/to/trusted-server.toml",
  "service_version": {
    "latest_version": 4,
    "target_version": 4,
    "clone_required": false,
    "clone_source_version": null
  },
  "actions": [
    {
      "action": "create",
      "resource_kind": "config",
      "name": "ts_config_store",
      "detail": "create config store `ts_config_store`",
      "remote_id": null
    }
  ],
  "warnings": []
}
```

### `ts provision fastly apply`

Apply the Fastly provisioning plan.

```bash
ts provision fastly apply --service-id <service-id> [options]
```

| Option                       | Description                                                  |
| ---------------------------- | ------------------------------------------------------------ |
| `--service-id <service-id>`  | Existing Fastly Compute service ID. Required.                |
| `--config <path>`            | Config file to provision. Defaults to `trusted-server.toml`. |
| `--json`                     | Write apply results as JSON.                                 |
| `--yes`                      | Skip the interactive confirmation prompt.                    |
| `--runtime-api-key <token>`  | Runtime Fastly API token for request-signing provisioning.   |
| `--reuse-management-api-key` | Use the management Fastly API token as the runtime token.    |

`apply` prompts before making changes unless `--yes` is passed. If binding changes are required and the latest Fastly service version is locked, the CLI clones it first. When bindings are created or updated, the CLI activates the target service version.

`apply` provisions resources and bindings only. It does not deploy the Wasm package. Use `fastly compute publish` for deployment.

JSON output uses this high-level shape:

```json
{
  "service_id": "svc_123",
  "config_path": "/path/to/trusted-server.toml",
  "service_version": {
    "latest_version": 4,
    "target_version": 5,
    "clone_required": true,
    "clone_source_version": 4
  },
  "completed_actions": [],
  "warnings": [],
  "failed_action": null,
  "activated_version": true
}
```

## Fastly provisioning resources

Fastly provisioning is config-driven. The CLI reads the validated local config and plans the resources that runtime code expects.

| Resource                    | Type         | When used                                                                                            |
| --------------------------- | ------------ | ---------------------------------------------------------------------------------------------------- |
| `ts_config_store`           | Config store | Always. Stores canonical app config under `ts-config`.                                               |
| `jwks_store`                | Config store | When `request_signing.enabled = true`. Stores `current-kid`, `active-kids`, and public JWK entries.  |
| `signing_keys`              | Secret store | When `request_signing.enabled = true`. Stores private signing keys by key ID.                        |
| `api-keys`                  | Secret store | When `request_signing.enabled = true`. Stores runtime Fastly API token under `api_key` when missing. |
| Configured consent KV store | KV store     | When `[consent] consent_store = "..."` is set.                                                       |

When request signing is enabled and the signing stores are empty, `plan` warns that `apply` will bootstrap an initial Ed25519 keypair. `apply` writes the public JWK data to `jwks_store` and the private signing key to `signing_keys`.

Request signing also needs a runtime Fastly API token stored as `api-keys/api_key` so the running service can rotate keys. If that secret is missing, choose exactly one runtime token source:

```bash
FASTLY_RUNTIME_API_KEY=runtime-token ts provision fastly apply --service-id svc_123

ts provision fastly apply --service-id svc_123 --runtime-api-key runtime-token

ts provision fastly apply --service-id svc_123 --reuse-management-api-key
```

Prefer `FASTLY_RUNTIME_API_KEY` for local use and CI because it avoids putting the token in shell history. Use `--reuse-management-api-key` only when your management token is acceptable for runtime key rotation.

After provisioning request signing resources, update these config fields if the plan or apply output warns that the configured IDs differ from Fastly:

```toml
[request_signing]
config_store_id = "..."
secret_store_id = "..."
```

## Environment variables

| Variable                 | Used by                                            | Description                                                                            |
| ------------------------ | -------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `FASTLY_API_KEY`         | `ts auth fastly status`, `ts provision fastly ...` | Fastly management API token. Takes precedence over secure storage.                     |
| `FASTLY_RUNTIME_API_KEY` | `ts provision fastly apply`                        | Runtime Fastly API token used when request signing needs to create `api-keys/api_key`. |

## Exit codes

| Exit code | Meaning                                      |
| --------- | -------------------------------------------- |
| `0`       | Command completed successfully.              |
| `1`       | Command failed. Read stderr for the report.  |
| `130`     | Interactive apply was cancelled by the user. |

## Troubleshooting

### The CLI tries to build for Wasm

Use the CLI Cargo aliases or pass the host target explicitly:

```bash
cargo build_cli
cargo test_cli

HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo run --package trusted-server-cli --bin ts --target "$HOST_TARGET" -- --help
```

### `trusted-server.toml` already exists

`ts config init` and `ts audit` refuse to overwrite files by default. Use a custom output path, skip config output, or pass `--force` when replacement is intended:

```bash
ts config init --config draft/trusted-server.toml
ts audit https://publisher.example --no-config
ts audit https://publisher.example --force
```

### `ts dev -- --skip-build` cannot find a Wasm file

Build the Fastly adapter first:

```bash
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
```

Or pass an explicit Wasm file to Fastly:

```bash
ts dev -- --skip-build --file target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm
```

### Fastly provisioning cannot find credentials

Set `FASTLY_API_KEY` or store a local credential:

```bash
export FASTLY_API_KEY=your-token
# or
ts auth fastly login
```

Check which source is active:

```bash
ts auth fastly status
```

### Request-signing provisioning asks for a runtime token

Set exactly one runtime token source:

```bash
FASTLY_RUNTIME_API_KEY=runtime-token ts provision fastly apply --service-id svc_123
```

Do not combine `FASTLY_RUNTIME_API_KEY`, `--runtime-api-key`, and `--reuse-management-api-key` in the same command.

### `ts audit` cannot launch a browser

Install Chrome or Chromium on the host machine. The audit collector checks common PATH names and standard macOS app bundle locations.

## Related docs

- [Getting Started](/guide/getting-started)
- [Configuration](/guide/configuration)
- [Fastly Setup](/guide/fastly)
- [Request Signing](/guide/request-signing)
- [Testing](/guide/testing)

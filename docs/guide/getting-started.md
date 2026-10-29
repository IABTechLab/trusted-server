# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have the following installed (versions are pinned in `.tool-versions`):

- Rust {{RUST_VERSION}} (see `.tool-versions`)
- NodeJS {{NODEJS_VERSION}}
- Basic familiarity with Rust and WebAssembly

**For Fastly deployment** (optional for local dev):

- Fastly {{FASTLY_VERSION}} CLI installed
- Chrome or Chromium, required for `ts audit`
- A Fastly account and API key

## Installation

### Clone the Repository

```bash
git clone https://github.com/IABTechLab/trusted-server.git
cd trusted-server
```

### Install the CLI

Install the `ts` operator CLI for your current platform:

```bash
cargo install-cli

# If your shell cannot find `ts`, add Cargo's bin directory to PATH
export PATH="$HOME/.cargo/bin:$PATH"
ts --help
```

See [Trusted Server CLI](/guide/cli) for command details.

## Local Development

Trusted Server supports two local development modes:

### Option A — Fastly Compute via Viceroy

Simulates the full Fastly production environment locally.

Install and configure the Fastly CLI using the [Fastly setup guide](/guide/fastly), then install Viceroy:

```bash
cargo install viceroy --version 0.21.0 --locked --force
```

Create and push the starter config, then start the local Fastly simulator:

```bash
cp trusted-server.example.toml trusted-server.toml
set -a && source .env.dev && set +a

ts config push --adapter fastly --local --yes --no-diff
fastly compute serve
```

The local manifest must provide fictional values at the compiler-generated keys
for the starter config's `handler.password`, `publisher.proxy`, and
`ec.passphrase` contexts. Legacy key labels no longer work. Use
`./scripts/smoke-fastly.sh` for the isolated fixture path, and verify the checked-in
fixtures match your exact handler regex/username before serving. Do not reuse
local values outside development. Fastly `secrets plan` resolves provider
resources; it is not an offline Viceroy provisioning command. For local fictional
fixtures only, an offline Axum plan uses the same core keys:

```bash
ts secrets plan --adapter axum --app-config trusted-server.toml --no-env --out bindings-local.json
jq -r '.entries[].compiled.key' bindings-local.json
```

Use these keys for fictional values in the isolated Viceroy
`[local_server.secret_stores.trusted_server_secrets]` fixture. An Axum plan does
not authorize Fastly `set`, resolve Fastly resources, or replace a reviewed
Fastly plan. Do not copy real credentials into local fixtures.
The server will be available at `http://localhost:7676`.

### Option B — Axum dev server

No Fastly account, CLI, or Viceroy needed. Runs natively on your machine.

The Axum adapter reads the EdgeZero config blob and secret store from
environment variables — it does **not** auto-load `.env` files. You must export
the variables into your shell before starting the server.

```bash
# Create the local app config and apply the non-secret development overlay.
cp trusted-server.example.toml trusted-server.toml
set -a && source .env.dev && set +a

# Use the repository's default app-config store name and key for this quickstart.
unset EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME
unset EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY

# Create the local blob-backed config-store entry.
ts config push --adapter axum --local --yes
export TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG="$(
  jq -r '.trusted_server_config' .edgezero/local-config-trusted_server_config.json
)"

# Generate exact names for this config. Review every purpose/context first.
ts secrets plan --adapter axum --app-config trusted-server.toml --no-env --out bindings-axum.json
set +x
while IFS= read -r name; do
  # Fictional local values only. Use protected input for existing credentials.
  value="$(openssl rand -base64 32)"
  export "$name=$value"
  unset value
done < <(jq -r '.entries[].native.native_name' bindings-axum.json | sort -u)
ts secrets check --adapter axum --app-config trusted-server.toml --no-env --plan bindings-axum.json

# Build and start the dev server in the same shell.
cargo run -p trusted-server-adapter-axum
```

The server will be available at `http://localhost:8787`. Set `PORT=<port>` before
`cargo run` to bind the dev server to a different local port.

**Environment variable conventions used by the Axum adapter:**

| Purpose            | Pattern                               | Example                                                               |
| ------------------ | ------------------------------------- | --------------------------------------------------------------------- |
| Config store value | `TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` | `TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG=…` |
| Secret store value | `TRUSTED_SERVER_SECRET_{STORE}_{KEY}` | Generated `native_name` from the Axum plan                            |

The repeated `TRUSTED_SERVER_CONFIG` segments in the example are intentional:
`TRUSTED_SERVER_CONFIG_` is the adapter prefix, followed by the resolved store
name and blob key. Both default to `[stores.config].default` in `edgezero.toml`,
currently `trusted_server_config`. The commands above assume that repository
default and clear any name/key overrides left in the shell.

If you customize the defaults or use `EDGEZERO__STORES__CONFIG__<ID>__NAME` or
`__KEY`, adjust the exported variable's store/key segments and the `jq` key to
match. Pass a matching `--key` to `ts config push` when overriding the runtime
key. The local JSON filename still uses the logical store ID, even with a
physical-name override.

The config-store value is the verified app-config blob. Secret-store values are
looked up only at context-derived generated identities, never names from the
blob. The plan renderer supplies the actual native environment names. The quick-start exports
ephemeral secret-store values only into the current shell; do not put secret
values in the TOML config, config-store blob, or a source-controlled environment
file.

> **Dev server limitations:** The Axum adapter does not support KV store,
> geo lookup, config/secret-store writes, or admin key-management routes.
> See [Architecture](/guide/architecture) for the full list.

### Build the Project

```bash
# Axum dev server (native)
cargo build -p trusted-server-adapter-axum

# Fastly adapter (WASM)
cargo build -p trusted-server-adapter-fastly --target wasm32-wasip1
```

### Run Tests

```bash
# Fastly/WASM crates (requires Viceroy)
cargo test-fastly

# Axum native adapter
cargo test-axum
```

## Configuration

Create a starter Trusted Server config with the `ts` CLI:

```bash
ts config init
```

To bootstrap from a public publisher page, run an audit first:

```bash
ts audit generate https://publisher.example
```

The audit command writes `js-assets.toml` plus a draft `trusted-server.toml`.
The draft includes disabled JS Asset Proxy candidates for detected third-party
scripts. Review it, keep credential properties absent, provision generated bindings, and enable
only the asset proxy entries you want to serve or block. Then validate it.

Edit `trusted-server.toml` to configure:

- browser integrations under `[integrations.*]`;
- server auction providers under map-shaped `[auction.providers.<id>]`;
- server bidder routes under `[auction.bidders.<id>]`;
- KV store mappings;
- EC configuration;
- consent settings (`[gdpr]`); and
- a reviewed generated binding plan for the selected deployment.

Do not put a Prebid Server URL or server bidder list under
`[integrations.prebid]`, and do not put APS account/endpoint/timeout fields under
`[integrations.aps]`. Those server values belong to auction provider common
fields and `profile_config`.

Before the first push, provision the physical store mapped from logical
`trusted_server_secrets` with raw values at the generated identities from a
reviewed plan. Follow [Provision app credentials](./secrets); TS does not create
recipient-side tokens.
On Fastly, `ts_secrets` is the documented example physical name. Then validate
and push:

```bash
ts config validate
ts config push --adapter fastly
```

This command performs target-independent plan validation. Each adapter performs
mandatory target-aware fan-out and backend-name validation at startup. The
EdgeZero callback needed for target-aware pre-write push validation is not yet
available in this tree, so startup remains the final target gate.

Recreate state after secret rotation. New keys do not revoke retired contexts;
follow [migration and retirement](./secret-binding-migration). See
[Configuration](/guide/configuration) and [Trusted Server CLI](/guide/cli) for details.

## Deploy to Fastly

Trusted Server declares Config, KV, and Secret Stores, so deployment must go
through EdgeZero's managed path. That path links each selected physical store
to the service version under its logical ID; `fastly compute publish` clones
the active version's links instead and activates a version whose logical links
are missing.

```bash
TRUSTED_SERVER__GIT_VERSION=v1.3.0 \
  EDGEZERO_MANIFEST="<release-root>/edgezero.toml" \
  ts deploy --adapter fastly --service-id <service-id> --application-release "<release-root>"
```

See [the Fastly guide](/guide/fastly#upgrading-from-edgezero-v0-0-8) when
upgrading a service that was deployed before logical store links, and
[the CLI guide](/guide/cli#lifecycle-commands) for where `<release-root>`
comes from.

The build compiles `TRUSTED_SERVER__GIT_VERSION` into the binary and reports it
in the `x-ts-version` response header, so the variable must be set for the
build the deploy runs, not just for the activation step. The Fastly service
version is in `x-ts-fastly-version`. Without the variable, a build reports the
checked-out tag, else the branch, else the first 6 characters of the commit,
and a build outside a git checkout omits the header. The value is public on
every response, so pass a neutral value such as the commit hash when a branch
name is sensitive.

## Next Steps

- Learn about [Edge Cookies](/guide/edge-cookies)
- Follow the [EC Setup Guide](/guide/ec-setup-guide)
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

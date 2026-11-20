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

The local manifest provides public development-only values for the starter
config's three secret references. Do not reuse them outside local development.
The server will be available at `http://localhost:7676`.

### Option B — Axum server

No Fastly account, CLI, or Viceroy needed. Runs natively on your machine.

The Axum server reads its config envelope from a local config-store file in
`EDGEZERO__ADAPTER__CONFIG_DIR` and its secrets from `EDGEZERO__SECRETS__*`
variables. It does **not** auto-load `.env` files, so export the variables into
your shell before starting the server.

Copy `trusted-server.example.toml` to `trusted-server.toml`. In the copied
file, add `provider` to the existing storage table:

```toml
[ec.storage]
provider = "records"
store = "trusted_server_kv"
```

The starter's default provider is Fastly-only. Set the native provider explicitly
in TOML: environment overlays can replace existing keys but cannot add one.

```bash
# Apply the non-secret development overlay.
set -a && source .env.dev && set +a
# Axum listens on 8787, not the Viceroy port in .env.dev.
export TRUSTED_SERVER__PUBLISHER__PUBLIC_ORIGIN=http://localhost:8787

# Use the repository's default app-config store name and key for this quickstart.
unset EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME
unset EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY

# Write .edgezero/local-config-trusted_server_config.json.
ts config push --adapter axum --local --yes
export EDGEZERO__ADAPTER__CONFIG_DIR="$PWD/.edgezero"
export EDGEZERO__ADAPTER__DATA_DIR="$PWD/.edgezero"

# Populate the three secret references from the starter config for this shell.
# Use stable values only if you need existing proxy URLs or EC IDs to remain valid.
export EDGEZERO__SECRETS__HANDLER_PASSWORD="$(openssl rand -base64 32)"
export EDGEZERO__SECRETS__PUBLISHER_PROXY_SECRET="$(openssl rand -base64 32)"
export EDGEZERO__SECRETS__EC_PASSPHRASE="$(openssl rand -base64 32)"

# Build and start the server in the same shell.
cargo run -p trusted-server-adapter-axum
```

The server will be available at `http://localhost:8787`. To use a different port,
set `EDGEZERO__ADAPTER__PORT=<port>` before `cargo run` and set
`TRUSTED_SERVER__PUBLISHER__PUBLIC_ORIGIN=http://localhost:<port>` before
`ts config push` so rewritten creative URLs use the same listener.

Each secret variable is `EDGEZERO__SECRETS__` followed by the uppercased secret
reference name from `trusted-server.toml`: `handler_password`,
`publisher_proxy_secret` and `ec_passphrase` in the starter config. Startup
reads only the references the config uses. If one is missing, the process exits
before it listens and names the config field of the missing secret. Restart the process to pick
up a changed config or secret.

`TRUSTED_SERVER__*` variables are applied by `ts config push` when it builds
the envelope; the running server does not read them. The older
`TRUSTED_SERVER_CONFIG_*` and `TRUSTED_SERVER_SECRET_*` variables are ignored.
Do not put secret values in the TOML config, the config-store file, or a
source-controlled environment file.

> **Axum capabilities:** The shared identity app uses EdgeZero's prepared record
> service; the local provider stores records in `EDGEZERO__ADAPTER__DATA_DIR`.
> Protected geo is optional and off by default. Generic platform KV operations,
> config/secret-store writes and admin key-management mutations remain unavailable.
> See [Architecture](/guide/architecture) for the full list, and
> [Axum production startup](/guide/native-rc-demo) for AWS-backed stores.

### Build the Project

```bash
# Axum server (native)
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
scripts. Review it, replace placeholders with stable secret key names, and enable
only the asset proxy entries you want to serve or block. Then validate it.

Edit `trusted-server.toml` to configure:

- browser integrations under `[integrations.*]`;
- server auction providers under map-shaped `[auction.providers.<id>]`;
- server bidder routes under `[auction.bidders.<id>]`;
- KV store mappings;
- EC configuration;
- consent settings (`[gdpr]`); and
- stable key names for `trusted_server_secrets`.

Do not put a Prebid Server URL or server bidder list under
`[integrations.prebid]`, and do not put APS account/endpoint/timeout fields under
`[integrations.aps]`. Those server values belong to auction provider common
fields and `profile_config`.

Before the first push, provision the physical store mapped from logical
`trusted_server_secrets` with the credential values referenced by the config.
On Fastly, `ts_secrets` is the documented example physical name. Then validate
and push:

```bash
ts config validate
ts config push --adapter fastly
```

This command performs target-independent plan validation. Fastly startup also
checks that an enabled auction fits its bound of 160 provider backends; push
does not run that check.

Restart or redeploy instances after secret rotation. See
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

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
cargo install viceroy --version 0.17.0 --locked --force
```

Start the local Fastly simulator:

```bash
fastly compute serve
```

The server will be available at `http://localhost:7676`.

### Option B — Axum dev server

No Fastly account, CLI, or Viceroy needed. Runs natively on your machine.

The Axum adapter reads the EdgeZero config blob and secret store from
environment variables — it does **not** auto-load `.env` files. You must export
the variables into your shell before starting the server.

```bash
# Create the local app config and apply the non-secret development overlay.
cp trusted-server.example.toml trusted-server.toml
cp .env.dev .env
set -a && source .env && set +a

# Create the local blob-backed config-store entry.
ts config push --adapter axum --local --yes
export TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG="$(
  jq -r '.trusted_server_config' .edgezero/local-config-trusted_server_config.json
)"

# Populate the three secret references from the starter config for this shell.
# Use stable values only if you need existing proxy URLs or EC IDs to remain valid.
export TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET="$(openssl rand -base64 32)"
export TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE="$(openssl rand -base64 32)"
export TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD="$(openssl rand -base64 32)"

# Build and start the dev server in the same shell.
cargo run -p trusted-server-adapter-axum
```

The server will be available at `http://localhost:8787`. Set `PORT=<port>` before
`cargo run` to bind the dev server to a different local port.

**Environment variable conventions used by the Axum adapter:**

| Purpose            | Pattern                               | Example                                                               |
| ------------------ | ------------------------------------- | --------------------------------------------------------------------- |
| Config store value | `TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` | `TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG=…` |
| Secret store value | `TRUSTED_SERVER_SECRET_{STORE}_{KEY}` | `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PROXY_KEY=…`            |

The config-store value is the verified app-config blob. Secret-store values are
looked up by the key names in that blob. Store names and key names are uppercased
with hyphens and dots replaced by underscores. The quick-start exports ephemeral
secret-store values only into the current shell; do not put secret values in the
TOML config, config-store blob, or a source-controlled environment file.

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
Review the draft, replace placeholders with stable secret key names, then
validate it.

Edit `trusted-server.toml` to configure:

- browser integrations under `[integrations.*]`
- server auction providers under map-shaped `[auction.providers.<id>]`
- server bidder routes under `[auction.bidders.<id>]`
- ad server integrations
- KV store mappings
- EC configuration
- consent settings under `[gdpr]`
- stable key names for `trusted_server_secrets`

Do not put a Prebid Server URL or server bidder list under
`[integrations.prebid]`, and do not put APS account, endpoint, or timeout fields
under `[integrations.aps]`. Those server values belong to auction provider
common fields and `profile_config`.

Provision the physical store mapped from logical `trusted_server_secrets` with
the existing credential values before pushing a migrated config. On Fastly,
`ts_secrets` is the documented example physical name. Then validate and push:

```bash
ts config validate
ts config push --adapter fastly
```

The validation command performs target-independent plan validation. Each adapter
performs mandatory target-aware fan-out and backend-name validation at startup.
The EdgeZero callback needed for target-aware pre-write push validation is not
yet available in this tree, so startup remains the final target gate.

Restart or redeploy instances after secret rotation. See
[Configuration](/guide/configuration) and [Trusted Server CLI](/guide/cli) for details.

## Deploy to Fastly

```bash
fastly compute publish
```

## Next Steps

- Learn about [Edge Cookies](/guide/edge-cookies)
- Follow the [EC Setup Guide](/guide/ec-setup-guide)
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

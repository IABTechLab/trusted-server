# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have:

- Rust 1.91.1 (see `.tool-versions`)
- Basic familiarity with Rust and WebAssembly

**For Fastly deployment** (optional for local dev):

- Fastly CLI installed
- A Fastly account and API key

## Installation

### Clone the Repository

```bash
git clone https://github.com/IABTechLab/trusted-server.git
cd trusted-server
```

## Local Development

Trusted Server supports two local development modes:

### Option A — Fastly Compute via Viceroy

Simulates the full Fastly production environment locally.

Install and configure the Fastly CLI using the [Fastly setup guide](/guide/fastly), then install Viceroy:

```bash
cargo install viceroy
```

Start the local Fastly simulator:

```bash
fastly compute serve
```

The server will be available at `http://localhost:7676`.

### Option B — Axum dev server

No Fastly account, CLI, or Viceroy needed. Runs natively on your machine.

The Axum adapter reads configuration from environment variables — it does **not**
auto-load `.env` files. You must export the variables into your shell before starting
the server.

```bash
# Copy and edit the environment file
cp .env.dev .env

# Export the variables into your current shell session
set -a && source .env && set +a

# Build and start the dev server
cargo run -p trusted-server-adapter-axum
```

The server will be available at `http://localhost:8787`. Set `PORT=<port>` before
`cargo run` to bind the dev server to a different local port.

**Environment variable conventions used by the Axum adapter:**

| Purpose            | Pattern                               | Example                                                  |
| ------------------ | ------------------------------------- | -------------------------------------------------------- |
| Config store value | `TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` | `TRUSTED_SERVER_CONFIG_SETTINGS_AD_SERVER_URL=https://…` |
| Secret store value | `TRUSTED_SERVER_SECRET_{STORE}_{KEY}` | `TRUSTED_SERVER_SECRET_KEYS_SIGNING_KEY=abc123`          |

Store names and key names are uppercased with hyphens and dots replaced by underscores.

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

Edit `trusted-server.toml` to configure:

- Ad server integrations
- KV store mappings
- EC configuration
- GDPR settings

See [Configuration](/guide/configuration) for details.

## Deploy to Fastly

```bash
fastly compute publish
```

## Next Steps

- Learn about [Edge Cookies](/guide/edge-cookies)
- Follow the [EC Setup Guide](/guide/ec-setup-guide)
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

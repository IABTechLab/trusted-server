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

### Option A — Axum dev server (recommended for local development)

No Fastly account, CLI, or Viceroy needed. Runs natively on your machine.

```bash
# Copy and edit the environment file
cp .env.dev .env

# Build and start the dev server
cargo run -p trusted-server-adapter-axum
```

The server will be available at `http://localhost:8787`.

### Option B — Fastly Compute via Viceroy

Simulates the Fastly production environment locally.

Install and configure the Fastly CLI using the [Fastly setup guide](/guide/fastly), then install Viceroy:

```bash
cargo install viceroy
```

Start the local Fastly simulator:

```bash
fastly compute serve
```

The server will be available at `http://localhost:7676`.

### Build the Project

```bash
cargo build
```

### Run Tests

```bash
# Fastly/WASM crates (requires Viceroy)
cargo test --workspace --exclude trusted-server-adapter-axum --target wasm32-wasip1

# Axum native adapter
cargo test -p trusted-server-adapter-axum
```

## Configuration

Edit `trusted-server.toml` to configure:

- Ad server integrations
- KV store mappings
- Synthetic ID templates
- GDPR settings

See [Configuration](/guide/configuration) for details.

## Deploy to Fastly

```bash
fastly compute publish
```

## Next Steps

- Learn about [Synthetic IDs](/guide/synthetic-ids)
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

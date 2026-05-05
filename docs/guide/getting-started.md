# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have:

- Rust 1.91.1 (see `.tool-versions`)
- Fastly CLI installed
- A Fastly account and API key
- Basic familiarity with WebAssembly

## Installation

### Clone the Repository

```bash
git clone https://github.com/IABTechLab/trusted-server.git
cd trusted-server
```

### Fastly CLI Setup

Install and configure the Fastly CLI using the [Fastly setup guide](/guide/fastly).

### Install Viceroy (Test Runtime)

```bash
cargo install viceroy
```

### Install Trusted Server CLI

From the repository root, install the host-target `ts` CLI with the Cargo alias:

```bash
cargo install_cli
```

See [Trusted Server CLI](/guide/cli#cargo-aliases) if you need to run the CLI without installing it or if your host is not Apple Silicon macOS.

## Local Development

### Build the Project

```bash
cargo build
```

### Run Tests

```bash
cargo test --workspace --exclude trusted-server-cli
cargo test --package trusted-server-cli --target "$(rustc -vV | sed -n 's/^host: //p')"
```

### Initialize and Validate Configuration

```bash
ts config init
ts config validate
```

### Start Local Server

```bash
ts dev -a fastly
```

The server will be available at `http://localhost:7676`.

### Audit a Public URL

```bash
ts audit https://example.com
```

`ts audit` currently uses a real Chromium browser session and expects Chrome/Chromium to already be installed on the host machine. It checks common PATH names and standard macOS app bundle locations.

See [Trusted Server CLI](/guide/cli) for the full command reference.

## Configuration

Use `ts config init` to generate `trusted-server.toml`, then edit it to configure:

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
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Review the [Trusted Server CLI](/guide/cli)
- Configure [Ad Serving](/guide/ad-serving)

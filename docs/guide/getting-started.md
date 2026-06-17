# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have the following installed (versions are pinned in `.tool-versions`):

- Rust {{RUST_VERSION}}
- NodeJS {{NODEJS_VERSION}}
- Fastly {{FASTLY_VERSION}} CLI installed
- Chrome or Chromium, required for `ts audit`
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
cargo install viceroy --version 0.17.0 --locked --force
```

## Local Development

### Build the Project

```bash
cargo build
```

### Run Tests

```bash
cargo test
```

### Start Local Server

```bash
fastly compute serve
```

The server will be available at `http://localhost:7676`.

## Configuration

Create a starter Trusted Server config with the `ts` CLI:

```bash
ts config init
```

To bootstrap from a public publisher page, run an audit first:

```bash
ts audit https://publisher.example
```

The audit command writes `js-assets.toml` plus a draft `trusted-server.toml`.
Review the draft, replace placeholders/secrets, then validate it.

Edit `trusted-server.toml` to configure:

- Ad server integrations
- KV store mappings
- EC configuration
- GDPR settings

Validate the config before pushing it to platform storage:

```bash
ts config validate
```

See [Configuration](/guide/configuration) and [Trusted Server CLI](/guide/cli) for details.

## Deploy to Fastly

```bash
fastly compute publish
```

## Next Steps

- Learn about [Edge Cookies](/guide/edge-cookies)
- Follow the [EC Setup Guide](/guide/ec-setup-guide)
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

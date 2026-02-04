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

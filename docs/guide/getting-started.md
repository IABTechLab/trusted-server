# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have:

- Rust 1.91.1 or later
- Fastly CLI installed
- A Fastly account and API Key
- Basic familiarity with WebAssembly

## Installation

### Clone the Repository

```bash
git clone https://github.com/yourusername/trusted-server.git
cd trusted-server
```

### Install Fastly CLI

```bash
# macOS
brew install fastly/tap/fastly

# Or download from fastly.com/cli
```

### Set Up Fastly CLI

```bash
# Generate API Token in Faslty Web Portal
fastly profile create
# Follow interactive Prompts
# This will store your API token credential in a configuration file and remember it for subsequent commands.
# Set a FASTLY_API_TOKEN environment variable
```

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

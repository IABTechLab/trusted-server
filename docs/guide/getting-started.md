# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have:

- Rust 1.91.1 or later
- Fastly CLI installed
- A Fastly account
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

### Install Viceroy (Test Runtime)

```bash
cargo install viceroy
```

### Install Trusted Server CLI

#### OSX
```bash
cargo install --path crates/cli --target aarch64-apple-darwin
```

# Linux:

```bash
cargo install --path crates/cli --target x86_64-unknown-linux-gnu
```

This installs `tscli`, the CLI tool for configuration management.

## Local Development

### Build the Project

```bash
cargo build
```

### Run Tests

```bash
cargo test
```

### Configure Your Environment

Before running locally, customize your configuration using one of these approaches:

**Option 1: Edit the TOML file directly**

Edit `trusted-server.toml` with your publisher settings, origin URL, etc.

**Option 2: Use environment variables**

Override any config value with environment variables prefixed with `TRUSTED_SERVER__`:

```bash
export TRUSTED_SERVER__PUBLISHER__DOMAIN=my-publisher.com
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:3000
export TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=my-dev-secret
```

**Option 3: Combine both**

Use `trusted-server.toml` as a base and override specific values with environment variables.

### Generate Local Config Store

Generate the config store JSON (this merges TOML + environment variables):

```bash
tscli config local -f trusted-server.toml
```

This creates `target/trusted-server-config.json` which is used by the local Fastly server.

::: tip
After changing `trusted-server.toml` or environment variables, re-run `tscli config local` to regenerate the config store.
:::

### Start Local Server

```bash
fastly compute serve
```

The server will be available at `https://localhost:7676`.

## Configuration Reference

Edit `trusted-server.toml` to configure:

- Ad server integrations
- KV store mappings
- Synthetic ID templates
- GDPR settings

### Validate Configuration

```bash
tscli config validate -f trusted-server.toml
```

See [Configuration](/guide/configuration) for details.

## Deploy to Fastly

### Push Configuration to Config Store

First, push your configuration to the Fastly Config Store:

```bash
export FASTLY_API_TOKEN=your-api-token
tscli config push -f trusted-server.toml --store-id <config-store-id>
```

Preview what will be uploaded with `--dry-run`:

```bash
tscli config push -f trusted-server.toml --store-id <store-id> --dry-run
```

### Deploy the Service

```bash
fastly compute publish
```

### Verify Deployment

Compare local and deployed configurations:

```bash
tscli config diff -f trusted-server.toml --store-id <store-id>
```

## CLI Reference

The `tscli` command provides these subcommands:

| Command | Description |
|---------|-------------|
| `tscli config validate -f <file>` | Validate configuration file |
| `tscli config hash -f <file>` | Compute SHA-256 hash |
| `tscli config local -f <file>` | Generate local dev config store |
| `tscli config push -f <file> --store-id <id>` | Push config to Fastly |
| `tscli config pull --store-id <id> -o <file>` | Pull config from Fastly |
| `tscli config diff -f <file> --store-id <id>` | Compare local vs deployed |

Add `-v` for verbose output on any command.

## Next Steps

- Learn about [Synthetic IDs](/guide/synthetic-ids)
- Understand [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)

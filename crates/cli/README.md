# Trusted Server CLI (`tscli`)

CLI tool for Trusted Server configuration management.

## Installation

The workspace defaults to the wasm32 target, so you must specify the native target:

```bash
# macOS (Apple Silicon)
cargo install --path crates/cli --target aarch64-apple-darwin

# macOS (Intel)
cargo install --path crates/cli --target x86_64-apple-darwin

# Linux
cargo install --path crates/cli --target x86_64-unknown-linux-gnu
```

Or build directly:

```bash
cargo build -p cli --release --target aarch64-apple-darwin
```

## Commands

### Validate Configuration

Validate a TOML configuration file:

```bash
tscli config validate -f trusted-server.toml
tscli config validate -f trusted-server.toml -v  # verbose
```

### Compute Hash

Compute SHA-256 hash of a configuration file (after applying `TRUSTED_SERVER__` overrides by default):

```bash
tscli config hash -f trusted-server.toml
tscli config hash -f trusted-server.toml --format json
tscli config hash -f trusted-server.toml --raw  # hash the file as-is
```

### Local Development

Generate config store JSON for local development with `fastly compute serve`:

```bash
tscli config local -f trusted-server.toml
```

This outputs to `target/trusted-server-config.json` by default. The `fastly.toml` is already configured to read from this path.

To specify a custom output path:

```bash
tscli config local -f trusted-server.toml -o custom-path.json
```

### Push Configuration

Push configuration to Fastly Config Store:

```bash
export FASTLY_API_TOKEN=xxx
tscli config push -f trusted-server.toml --store-id <store-id>
```

Preview without uploading (dry run):

```bash
tscli config push -f trusted-server.toml --store-id <store-id> --dry-run
```

### Pull Configuration

Pull configuration from Fastly Config Store:

```bash
export FASTLY_API_TOKEN=xxx
tscli config pull --store-id <store-id> -o pulled-config.toml
```

### Compare Configurations

Compare local config with deployed config:

```bash
export FASTLY_API_TOKEN=xxx
tscli config diff -f trusted-server.toml --store-id <store-id>
tscli config diff -f trusted-server.toml --store-id <store-id> -v  # verbose diff
```

## Environment Variables

- `FASTLY_API_TOKEN` - Required for push, pull, and diff commands
- `TRUSTED_SERVER__*` - Config values can be overridden via environment variables (e.g., `TRUSTED_SERVER__PUBLISHER__DOMAIN=example.com`)

## Local Development Workflow

```bash
# 1. Generate config store JSON
tscli config local -f trusted-server.toml

# 2. Run local Fastly server
fastly compute serve
```

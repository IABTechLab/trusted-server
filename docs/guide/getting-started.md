# Getting Started

Get up and running with Trusted Server quickly.

## Prerequisites

Before you begin, ensure you have:

- [Node.js](https://nodejs.org/) {{NODEJS_VERSION}}
- [Rust](https://www.rust-lang.org/) {{RUST_VERSION}}
- [Fastly CLI](https://www.fastly.com/documentation/reference/tools/cli/) {{FASTLY_VERSION}}
- A [Fastly account](https://manage.fastly.com)

We recommend using [asdf](https://asdf-vm.com/) to manage Node.js and Rust versions:

```bash
# Install asdf (macOS)
brew install asdf

# Install Node.js
asdf plugin add nodejs
asdf install nodejs $(grep '^nodejs ' .tool-versions | awk '{print $2}')

# Install Rust
asdf plugin add rust
asdf install rust $(grep '^rust ' .tool-versions | awk '{print $2}')

asdf reshim
```

Add to your shell profile (`~/.zshrc` or `~/.bash_profile`):

```bash
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

## Installation

### Clone the Repository

```bash
git clone git@github.com:IABTechLab/trusted-server.git
cd trusted-server
```

### Install Fastly CLI

```bash
# macOS
brew install fastly/tap/fastly

# Verify version (should be {{FASTLY_VERSION}} or later)
fastly version

# Create profile with your API token
fastly profile create
```

### Install Viceroy (Test Runtime)

```bash
cargo install viceroy
```

## Fastly Account Setup

Before deploying, you'll need to set up your Fastly account, create an API token, and configure a Compute service.

See the [Fastly Setup Guide](/guide/fastly) for detailed instructions on:

- Creating a Fastly account and API token
- Setting up a Compute service
- Configuring origins for your ad integrations
- Setting up Config and Secret stores

## Configuration

Edit the following files for your setup:

- `fastly.toml` - Service ID, author, description, Config/Secret Store IDs
- `trusted-server.toml` - KV store ID names, request signing, integrations

See [Configuration](/guide/configuration) for details.

## Local Development

### Build the Project

```bash
cargo build
```

### Run Tests

```bash
cargo test
```

::: warning
If a test fails, `viceroy` will not display the line number. Rerun with `cargo test_details` for more details.
:::

### Start Local Server

Set up environment variables and start the local server:

```bash
# Load environment variables
export $(grep -v '^#' .env.dev | xargs -0)

# Start local server
fastly compute serve
```

The server will be available at `http://localhost:7676`.

### Additional Commands

| Command        | Description                                                  |
| -------------- | ------------------------------------------------------------ |
| `cargo fmt`    | Ensure uniform code formatting                               |
| `cargo clippy` | Ensure idiomatic code                                        |
| `cargo check`  | Verify compilation on Linux, macOS, Windows, and WebAssembly |
| `cargo bench`  | Run all benchmarks                                           |

## Deploy to Fastly

```bash
fastly compute publish
```

## Next Steps

- [Configuration](/guide/configuration) - Detailed configuration options
- [Synthetic IDs](/guide/synthetic-ids) - Learn about synthetic ID generation
- [GDPR Compliance](/guide/gdpr-compliance) - Privacy and consent management
- [Ad Serving](/guide/ad-serving) - Configure ad server integrations
- [Request Signing](/guide/request-signing) - Cryptographic request signing

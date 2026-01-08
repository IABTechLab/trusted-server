# Trusted Server

:information_source: Trusted Server is an open-source, cloud based orchestration framework and runtime for publishers. It moves code execution and operations that traditionally occurs in browsers (via 3rd party JS) to secure, zero-cold-start [WASM](https://webassembly.org) binaries running in [WASI](https://github.com/WebAssembly/WASI) supported environments. It importantly gives publishers benefits such as: dramatically increasing control over how and who they share their data with (while maintaining user-privacy compliance), increasing revenue from inventory inside cookie restricted or non-JS environments, ability to serve all assets under 1st party context, and provides secure cryptographic functions to ensure trust across the programmatic ad ecosystem.

Trusted Server is the new execution layer for the open-web, returning control of 1st party data, security, and overall user-experience back to publishers.

At this time, Trusted Server is designed to work with Fastly Compute. Follow these steps to configure Fastly Compute and deploy it.

## Getting Started: Edge-Cloud Support on Fastly

- Create account at Fastly if you don’t have one - manage.fastly.com
- Log in to the Fastly control panel.
  - Go to Account > API tokens > Personal tokens.
  - Click Create token
  - Name the Token
  - Choose User Token
  - Choose Global API Access
  - Choose what makes sense for your Org in terms of Service Access
  - Copy key to a secure location because you will not be able to see it again

- Create new Compute Service
  - Click Compute and Create Service
  - Click “Create Empty Service” (below main options)
  - Add your domain of the website you’ll be testing or using and click update
  - Click on “Origins” section and add your ad-server / SSP integration information as hostnames (note after you save this information you can select port numbers and TLS on/off)
  - IMPORTANT: when you enter the FQDN or IP ADDR information and click Add you need to enter a “Name” in the first field that will be referenced in your code so something like “my_ad_integration_1”
  -

:warning: With a dev account, Fastly gives you a test domain by default, but you’re also able to create a CNAME to your own domain when you’re ready, along with 2 free TLS certs (non-wildcard). Note that Fastly Compute ONLY accepts client traffic via TLS, though origins and backends can be non-TLS.

## CLI and OS Tools Installation

### Brew

:warning: Follow the prompts before and afterwards (to configure system path, etc)

#### Install Brew

```sh
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

### Fastly CLI

#### Install Fastly CLI

```sh
brew install fastly/tap/fastly
```

#### Verify Installation and Version

```sh
fastly version
```

:warning: fastly cli version should be at least v12.1.0

#### Create profile and follow interactive prompt for pasting your API Token created earlier:

```sh
fastly profile create
```

### Rust

#### Install Rust with asdf (our preference)

```sh
brew install asdf
asdf plugin add rust
asdf install rust $(grep '^rust ' .tool-versions | awk '{print $2}')
asdf reshim
```

### NodeJS

#### Install NodeJS with asdf

```sh
brew install asdf
asdf plugin add nodejs
asdf install nodejs $(grep '^nodejs ' .tool-versions | awk '{print $2}')
asdf reshim
```

#### Fix path for Bash

Edit ~/.bash_profile to add path for asdf shims:

```sh
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

#### Fix path for ZSH

Edit ~/.zshrc to add path for asdf shims:

```sh
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

#### Other shells

See https://asdf-vm.com/guide/getting-started.html#_2-configure-asdf

### Clone Trusted Server and Configure Build

#### Clone Project (assumes you have 'git' installed on your system)

```sh
git clone git@github.com:IABTechLab/trusted-server.git
```

### Configure

#### Edit configuration files

:information_source: Note that you'll have to edit the following files for your setup:

- fastly.toml (service ID, author, description, Config/Secret Store IDs for request signing)
- trusted-server.toml (KV store ID names - optional, request signing configuration)

### Build

```sh
cargo build
```

### Deploy to Fastly

```sh
fastly compute publish
```

## Devleopment

#### Install viceroy for running tests

```sh
cargo install viceroy
```

#### Run Fastly server locally

- Review configuration for [local_server](fastly.toml#L16)
- Review env variables overrides in [.env.dev](.env.dev)

```sh
export $(grep -v '^#' .env.dev | xargs -0)
```

```sh
fastly -i compute serve
```

#### Tests

```sh
cargo test
```

:warning: if test fails `viceroy` will not display line number of the failed test. Rerun it with `cargo test_details`.

#### Additional Rust Commands

- `cargo fmt`: Ensure uniform code formatting
- `cargo clippy`: Ensure idiomatic code
- `cargo check`: Ensure compilation succeeds on Linux, MacOS, Windows and WebAssembly
- `cargo bench`: Run all benchmarks

## Request Signing

Trusted Server supports cryptographic signing of OpenRTB requests and other API calls using Ed25519 keys.

### Configuration

Request signing requires Fastly Config Store and Secret Store for key management:

1. **Create Fastly Stores** (via Fastly Control Panel or CLI):
   - Config Store: `jwks_store` - stores public keys (JWKs) and key metadata
   - Secret Store: `signing_keys` - stores private signing keys

2. **Configure in trusted-server.toml**:

```toml
[request_signing]
enabled = true  # Set to true to enable request signing
config_store_id = "<your-fastly-config-store-id>"  # Config Store ID from Fastly
secret_store_id = "<your-fastly-secret-store-id>"  # Secret Store ID from Fastly
```

### Key Management Endpoints

Once configured, the following endpoints are available:

- **`GET /.well-known/ts.jwks.json`**: Returns active public keys in JWKS format for signature verification
- **`POST /verify-signature`**: Verifies a signature against a payload and key ID (useful for testing)
  - Request body: `{"payload": "...", "signature": "...", "kid": "..."}`
  - Response: `{"verified": true/false, "kid": "...", "message": "..."}`

#### Admin Endpoints (Key Rotation)

- **`POST /admin/keys/rotate`**: Generates and activates a new signing key
  - Optional body: `{"kid": "custom-key-id"}` (auto-generates date-based ID if omitted)
  - Response includes new key ID, previous key ID, and active keys list
- **`POST /admin/keys/deactivate`**: Deactivates or deletes a key
  - Request body: `{"kid": "key-to-deactivate", "delete": false}`
  - Set `delete: true` to permanently remove the key (also deactivates it)

:warning: Key rotation keeps both the new and previous key active to allow for graceful transitions. Deactivate old keys manually when no longer needed.

## First-Party Endpoints

- `/first-party/ad` (GET): returns HTML for a single slot (`slot`, `w`, `h` query params). The server inspects returned creative HTML and rewrites:
- All absolute images and iframes to `/first-party/proxy?tsurl=<base-url>&<original-query-params>&tstoken=<sig>` (1×1 pixels are detected server‑side heuristically for logging). The `tstoken` is derived from encrypting the full target URL and hashing it.
- `/auction` (POST): accepts tsjs ad units and runs the auction orchestrator.
- `/first-party/proxy` (GET): unified proxy for resources referenced by creatives.
  - Query params:
    - `tsurl`: Target URL without query (base URL) — required
    - Any original target query parameters are included at top level as-is (order preserved)
    - `tstoken`: Base64 URL‑safe (no padding) SHA‑256 digest of the encrypted full target URL — required
  - Behavior:
    - Reconstructs the full target URL from `tsurl` + provided parameters in order, computes `tstoken` by encrypting with XChaCha20‑Poly1305 (deterministic nonce) and hashing the bytes with SHA‑256, and validates it.
    - HTML responses: proxied and rewritten (images/iframes/pixels) via creative rewriter
    - Image responses: proxied; if content‑type is missing, sets `image/*`; logs likely 1×1 pixels via size/URL heuristics
    - Follows HTTP redirects (301/302/303/307/308) up to four hops, reapplying the forwarded synthetic ID and switching to `GET` after a 303; logs when the redirect limit is reached.
    - When forwarding to the target URL, no `tstoken` is included (it is not part of the target URL).
- Synthetic ID propagation: reads the trusted ID from the incoming cookie/header and appends `synthetic_id=<value>` to the target URL sent to the third-party origin while preserving existing query strings.
  - Redirect following re-applies the identifier on each hop so downstream origins see a consistent ID even when assets bounce through intermediate trackers.

- `/first-party/click` (GET): first‑party click redirect handler for anchors and clickable areas.
  - Query params: same as `/first-party/proxy` (uses `tsurl`, original params, `tstoken`).
  - Behavior:
    - Validates `tstoken` against the reconstructed full URL (same enc+SHA256 scheme).
    - Emits a `302 Found` with `Location: <reconstructed_target_url>` — content is not parsed or proxied.
    - If a synthetic identifier is available, appends `synthetic_id=<value>` to the redirect target.
    - Logs click metadata (tsurl, whether params are present, target URL, referer, user agent, and Trusted Server ID header) for observability.

- Publisher origin proxy (`handle_publisher_request`): retrieves/generates the synthetic ID, stamps the response with `X-Synthetic-*` headers, and sets the `synthetic_id` cookie (Secure, SameSite=Lax) when absent so subsequent creative and click proxies can propagate the identifier.

Notes

- Rewriting uses `lol_html`. Only absolute and protocol‑relative URLs are rewritten; relative URLs are left unchanged.
- For the proxy endpoint, the base URL is carried in `tsurl`, the original query parameters are preserved individually, and `tstoken` authenticates the reconstructed full URL.
- Synthetic identifiers are generated by `crates/common/src/synthetic.rs` and are surfaced in three places: publisher responses (headers + cookie), creative proxy target URLs (`synthetic_id` query param), and click redirect URLs. This ensures downstream integrations can correlate impressions and clicks without direct third-party cookies.

## Integration Modules

- See [`docs/integration_guide.md`](docs/integration_guide.md) for the full integration module guide, covering configuration, proxy routing, HTML shim hooks, and the `testlight` example implementation.

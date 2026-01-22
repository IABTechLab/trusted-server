# Configuration

Learn how to configure Trusted Server for your deployment.

## Overview

Trusted Server uses a flexible configuration system based on:

1. **TOML Files** - `trusted-server.toml` for base configuration
2. **Environment Variables** - Runtime overrides with `TRUSTED_SERVER__` prefix
3. **Fastly Stores** - KV/Config/Secret stores for runtime data

::: tip Complete Reference
See [Configuration Reference](/guide/configuration-reference) for detailed documentation of all configuration options.
:::

## Quick Start

### Basic Configuration

Create `trusted-server.toml` in your project root:

```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "your-secure-secret-here"

[synthetic]
counter_store = "counter_store"
opid_store = "opid_store"
secret_key = "your-hmac-secret"
template = "{{ client_ip }}:{{ user_agent }}"
```

### Environment Overrides

Override any setting with environment variables:

```bash
# Publisher settings
export TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://origin.publisher.com

# Synthetic ID settings
export TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=your-secret
export TRUSTED_SERVER__SYNTHETIC__TEMPLATE="{{ client_ip }}:{{ user_agent }}"
```

## Configuration Files

### `trusted-server.toml`

Main application configuration file.

**Location**: Project root directory

**Format**: TOML (Tom's Obvious, Minimal Language)

**Sections**:
- `[publisher]` - Publisher domain and origin settings
- `[synthetic]` - Synthetic ID generation
- `[request_signing]` - Request signing and JWKS
- `[response_headers]` - Custom response headers
- `[rewrite]` - URL rewriting rules
- `[[handlers]]` - Basic auth handlers
- `[integrations.*]` - Integration configs (Prebid, Next.js, etc.)

**Example**:
```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "change-me-to-secure-value"

[synthetic]
counter_store = "counter_store"
opid_store = "opid_store"
secret_key = "your-hmac-secret-key"
template = "{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}"

[response_headers]
X-Publisher-ID = "pub-12345"
X-Environment = "production"

[request_signing]
enabled = false
config_store_id = "<your-config-store-id>"
secret_store_id = "<your-secret-store-id>"

[integrations.prebid]
enabled = true
server_url = "https://prebid-server.com/openrtb2/auction"
timeout_ms = 1200
bidders = ["kargo", "rubicon", "appnexus"]
auto_configure = false
```

### `fastly.toml`

Fastly Compute service configuration.

**Purpose**: Build settings, local development, store links

**Example**:
```toml
manifest_version = 2
name = "trusted-server"
description = "Privacy-preserving ad serving"
authors = ["Your Team"]
language = "rust"

[local_server]
  [local_server.kv_stores.counter_store]
    file = "test-data/counter_store.json"
  
  [local_server.kv_stores.opid_store]
    file = "test-data/opid_store.json"

[setup]
  [setup.config_stores.jwks_store]
  [setup.secret_stores.signing_keys]
```

### `.env.*` Files

Environment-specific variable files.

**`.env.dev`** - Local development:
```bash
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:3000
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=dev-secret
LOG_LEVEL=debug
```

**`.env.staging`** - Staging environment:
```bash
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://staging.publisher.com
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=$(cat /run/secrets/synthetic_key_staging)
```

**`.env.production`** - Production (secrets from secure store):
```bash
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=$(cat /run/secrets/proxy_secret)
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=$(cat /run/secrets/synthetic_secret)
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true
```

## Environment Variables

### Format

```
TRUSTED_SERVER__SECTION__FIELD=value
```

**Rules**:
- Prefix: `TRUSTED_SERVER`
- Separator: `__` (double underscore)
- Case: UPPERCASE
- Nested: Use additional `__` for each level

### Examples

**Simple Field**:
```bash
TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
```

**Array (JSON)**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS='["kargo","rubicon"]'
```

**Array (Indexed)**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__0=kargo
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__1=rubicon
```

**Array (Comma-Separated)**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS=kargo,rubicon,appnexus
```

## Key Configuration Sections

### Publisher Settings

Core settings for your publisher domain and origin.

```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "secure-random-secret"
```

**Key Fields**:
- `domain` - Your publisher domain
- `cookie_domain` - Domain for synthetic ID cookies (use `.domain.com` for subdomains)
- `origin_url` - Backend origin server URL
- `proxy_secret` - Secret for signing proxy URLs (HMAC-SHA256)

::: warning Security
Generate `proxy_secret` with cryptographically random values:
```bash
openssl rand -base64 32
```
:::

### Synthetic IDs

Configure privacy-preserving ID generation.

```toml
[synthetic]
counter_store = "counter_store"
opid_store = "opid_store"
secret_key = "your-hmac-secret"
template = "{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}"
```

**Template Variables**:
- `client_ip` - Client IP address
- `user_agent` - User-Agent header
- `first_party_id` - Publisher-provided ID
- `auth_user_id` - Authenticated user ID
- `publisher_domain` - Publisher domain
- `accept_language` - Accept-Language header

See [Synthetic IDs](/guide/synthetic-ids) for details.

### Request Signing

Enable Ed25519 request signing and JWKS management.

```toml
[request_signing]
enabled = true
config_store_id = "01GXXX"  # From Fastly dashboard
secret_store_id = "01GYYY"  # From Fastly dashboard
```

**Setup**:
1. Create Fastly Config Store for JWKS
2. Create Fastly Secret Store for private keys
3. Copy store IDs to configuration
4. Enable request signing

See [Request Signing](/guide/request-signing) and [Key Rotation](/guide/key-rotation) for setup.

### Integrations

Configure built-in integrations.

**Prebid**:
```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.com/openrtb2/auction"
timeout_ms = 1200
bidders = ["kargo", "rubicon", "appnexus"]
auto_configure = false
```

**Next.js**:
```toml
[integrations.nextjs]
enabled = true
rewrite_attributes = ["href", "link", "url"]
```

**Permutive**:
```toml
[integrations.permutive]
enabled = true
organization_id = "org-12345"
workspace_id = "ws-67890"
project_id = "proj-abcde"
```

See [Integration Guide](/guide/integration-guide) for custom integrations.

## Fastly Store Setup

### KV Stores

Create stores for synthetic ID data:

```bash
# Create counter store
fastly kv-store create --name=counter_store

# Create publisher ID mapping store
fastly kv-store create --name=opid_store
```

**Link to Service** (`fastly.toml`):
```toml
[local_server.kv_stores.counter_store]
  file = "test-data/counter_store.json"

[local_server.kv_stores.opid_store]
  file = "test-data/opid_store.json"
```

### Config Stores

For JWKS public keys:

```bash
# Create config store
fastly config-store create --name=jwks_store

# Get store ID
fastly config-store list
```

### Secret Stores

For private signing keys:

```bash
# Create secret store
fastly secret-store create --name=signing_keys

# Get store ID
fastly secret-store list
```

## Validation

### Automatic Validation

Configuration is validated at application startup:

**Checks**:
- Required fields present
- Data types correct
- Regex patterns valid
- Secret keys not default values
- Integration configs valid

**Failure Behavior**: Application exits with error message.

### Manual Validation with CLI

Use `tscli` to validate configuration before deployment:

```bash
# Validate configuration file
tscli config validate -f trusted-server.toml

# Validate with verbose output (shows sections and integrations)
tscli config validate -f trusted-server.toml -v

# Compute configuration hash
tscli config hash -f trusted-server.toml
```

`tscli` applies `TRUSTED_SERVER__` environment overrides for validation, hashing, and push operations. Use `tscli config hash --raw` to hash the file without applying environment overrides.

### Generate Local Config Store

For local development with `fastly compute serve`:

```bash
# Generate config store JSON (outputs to target/trusted-server-config.json)
tscli config local -f trusted-server.toml

# Generate to custom path
tscli config local -f trusted-server.toml -o custom-path.json
```

### Push to Fastly Config Store

Deploy configuration to Fastly:

```bash
export FASTLY_API_TOKEN=your-token

# Push configuration
tscli config push -f trusted-server.toml --store-id <store-id>

# Dry run (preview without uploading)
tscli config push -f trusted-server.toml --store-id <store-id> --dry-run

# Pull current deployed config
tscli config pull --store-id <store-id> -o pulled-config.toml

# Compare local vs deployed
tscli config diff -f trusted-server.toml --store-id <store-id>
```

### Test with Local Server

```bash
# Generate local config first
tscli config local -f trusted-server.toml

# Then run local server
fastly compute serve
```

## Secrets Management

### Best Practices

**Development**:
```bash
# Use simple secrets for local dev
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=dev-secret
```

**Staging/Production**:
```bash
# Load from secure sources
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=$(cat /run/secrets/proxy_secret)
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=$(vault kv get -field=value secret/synthetic_key)
```

**Do**:
✅ Use environment variables for secrets  
✅ Generate cryptographically random values  
✅ Rotate secrets periodically  
✅ Store in Fastly Secret Store or Vault  
✅ Use different secrets per environment  

**Don't**:
❌ Commit secrets to version control  
❌ Use default placeholder values  
❌ Share secrets across environments  
❌ Log secret values  

### `.gitignore`

Protect secret files:

```
.env.production
.env.staging
.env.local
*.secret
trusted-server.production.toml
```

## Configuration Patterns

### Multi-Environment Setup

**Directory Structure**:
```
project/
├── trusted-server.toml           # Base config
├── trusted-server.dev.toml       # Development overrides
├── .env.development              # Dev environment vars
├── .env.staging                  # Staging environment vars
├── .env.production               # Production (not in git)
├── .env.example                  # Template (in git)
└── .gitignore
```

**Base Config** (`trusted-server.toml`):
```toml
# Shared across all environments
[synthetic]
template = "{{ client_ip }}:{{ user_agent }}"

[integrations.prebid]
timeout_ms = 1200
bidders = ["kargo", "rubicon"]
```

**Environment Overrides**:
```bash
# Development
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://localhost:3000

# Staging
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://staging.publisher.com

# Production
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://origin.publisher.com
```

### Dynamic Configuration

Use environment variables for runtime changes:

```bash
# Enable/disable features
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true

# Adjust timeouts
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1500

# Update endpoints
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL=https://new-prebid.com/auction
```

## Troubleshooting

### Common Issues

**"Failed to build configuration"**:
- Check TOML syntax (commas, quotes, brackets)
- Verify all required fields present
- Check environment variable format

**"Secret key is not valid"**:
- Cannot use `"secret-key"` placeholder
- Must be non-empty
- Change to secure random value

**"Invalid regex"**:
- Handler `path` must be valid regex
- Escape special characters: `\.`, `\$`
- Test with: `echo "pattern" | grep -E "pattern"`

**Environment variables not applied**:
- Verify prefix: `TRUSTED_SERVER__`
- Check separator: `__` (double underscore)
- Confirm exported: `echo $VAR_NAME`

### Debug Commands

**Check environment**:
```bash
env | grep TRUSTED_SERVER
```

**Validate TOML**:
```bash
cat trusted-server.toml | npx toml-cli validate
```

**Test local server**:
```bash
fastly compute serve --verbose
```

## Next Steps

- See [Configuration Reference](/guide/configuration-reference) for complete options
- Set up [Request Signing](/guide/request-signing) for secure API calls
- Configure [First-Party Proxy](/guide/first-party-proxy) for URL proxying
- Learn about [Integration Guide](/guide/integration-guide) for custom integrations
- Review [Testing](/guide/testing) for validation strategies

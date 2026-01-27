# Configuration Reference

Complete reference for all configuration options in Trusted Server.

## Overview

Trusted Server uses a TOML-based configuration system with environment variable overrides. Configuration is loaded from:

1. **`trusted-server.toml`** - Base configuration file
2. **Environment Variables** - Runtime overrides with `TRUSTED_SERVER__` prefix

### Quick Example

```toml
# trusted-server.toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "your-secure-secret-here"

[synthetic]
counter_store = "counter_store"
opid_store = "opid_store"
secret_key = "your-hmac-secret-here"
template = "{{ client_ip }}:{{ user_agent }}"
```

## Environment Variable Overrides

### Format

```
TRUSTED_SERVER__SECTION__SUBSECTION__FIELD
```

**Rules**:
- Prefix: `TRUSTED_SERVER`
- Separator: `__` (double underscore)
- Case: UPPERCASE
- Sections: Match TOML hierarchy

### Examples

**Simple Field**:
```bash
TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
```

**Nested Field**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL=https://prebid.example/auction
```

**Array Field (JSON)**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS='["kargo","rubicon"]'
```

**Array Field (Indexed)**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__0=kargo
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__1=rubicon
```

**Array Field (Comma-Separated)**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS=kargo,rubicon,appnexus
```

## Publisher Configuration

Core publisher settings for domain, origin, and proxy configuration.

### `[publisher]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `domain` | String | Yes | Publisher's domain name |
| `cookie_domain` | String | Yes | Domain for setting cookies (typically with leading dot) |
| `origin_url` | String | Yes | Full URL of publisher origin server |
| `proxy_secret` | String | Yes | Secret key for encrypting/signing proxy URLs |

**Example**:
```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"  # Includes subdomains
origin_url = "https://origin.publisher.com"
proxy_secret = "change-me-to-secure-random-value"
```

**Environment Override**:
```bash
TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=.publisher.com
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://origin.publisher.com
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=your-secret-here
```

### Field Details

#### `domain`

**Purpose**: Primary domain for the publisher.

**Usage**:
- Displayed in synthetic ID generation
- Used in template variables (`publisher_domain`)
- Part of request context

**Format**: Hostname without protocol or path
- ✅ `publisher.com`
- ✅ `www.publisher.com`  
- ❌ `https://publisher.com`
- ❌ `publisher.com/path`

#### `cookie_domain`

**Purpose**: Domain scope for synthetic ID cookies.

**Usage**:
- Set on `synthetic_id` cookie
- Controls cookie sharing across subdomains

**Format**: Domain with optional leading dot
- `.publisher.com` - Shares across all subdomains
- `publisher.com` - Exact domain only

**Best Practice**: Use leading dot (`.publisher.com`) for subdomain sharing.

#### `origin_url`

**Purpose**: Backend origin server URL for publisher content.

**Usage**:
- Fallback proxy target for non-integration requests
- HTML processing rewrites origin URLs to request host
- Base for relative URL resolution

**Format**: Full URL with protocol
- ✅ `https://origin.publisher.com`
- ✅ `https://origin.publisher.com:8080`
- ✅ `http://192.168.1.1:9000`
- ❌ `origin.publisher.com` (missing protocol)

**Port Handling**: Includes port if non-standard (not 80/443).

#### `proxy_secret`

**Purpose**: Secret key for HMAC-SHA256 signing of proxy URLs.

**Security**:
- Keep confidential and secure
- Rotate periodically (90 days recommended)
- Use cryptographically random values (32+ bytes)
- Never commit to version control

**Generation**:
```bash
# Generate secure random secret
openssl rand -base64 32
```

**Usage**:
- Signs `/first-party/proxy` URLs
- Signs `/first-party/click` URLs
- Validates incoming proxy requests
- Prevents URL tampering

::: danger Security Warning
Changing `proxy_secret` invalidates all existing signed URLs. Plan rotations carefully and use graceful transition periods.
:::

## Synthetic ID Configuration

Settings for generating privacy-preserving synthetic identifiers.

### `[synthetic]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `counter_store` | String | Yes | Fastly KV store name for counters |
| `opid_store` | String | Yes | Fastly KV store name for publisher ID mappings |
| `secret_key` | String | Yes | HMAC secret for ID generation |
| `template` | String | Yes | Handlebars template for ID composition |

**Example**:
```toml
[synthetic]
counter_store = "counter_store"
opid_store = "opid_store"
secret_key = "your-secure-hmac-secret"
template = "{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}"
```

**Environment Override**:
```bash
TRUSTED_SERVER__SYNTHETIC__COUNTER_STORE=counter_store
TRUSTED_SERVER__SYNTHETIC__OPID_STORE=opid_store
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=your-secret
TRUSTED_SERVER__SYNTHETIC__TEMPLATE="{{ client_ip }}:{{ user_agent }}"
```

### Field Details

#### `counter_store`

**Purpose**: Fastly KV store for synthetic ID counters.

**Usage**:
- Stores incrementing counters per domain
- Ensures ID uniqueness
- Accessed via Fastly KV Store API

**Setup**:
```bash
# Create KV store
fastly kv-store create --name=counter_store
```

**Data Format**:
```json
{
  "publisher.com": 12345,
  "another.com": 67890
}
```

#### `opid_store`

**Purpose**: Fastly KV store for publisher-provided ID mappings.

**Usage**:
- Maps publisher IDs to synthetic IDs
- Enables first-party ID integration
- Optional (used if publisher provides IDs)

**Setup**:
```bash
# Create KV store
fastly kv-store create --name=opid_store
```

**Data Format**:
```json
{
  "publisher-id-123": "synthetic-abc",
  "publisher-id-456": "synthetic-def"
}
```

#### `secret_key`

**Purpose**: HMAC secret for deterministic ID generation.

**Security**:
- Minimum 8 bytes (validation enforced)
- Cannot be `"secret-key"` (reserved/invalid)
- Rotate periodically for security
- Store securely (environment variable recommended)

**Generation**:
```bash
# Generate secure random key
openssl rand -hex 32
```

**Validation**: Application startup fails if:
- Empty string
- Exactly `"secret-key"` (default placeholder)
- Less than 1 character

#### `template`

**Purpose**: Handlebars template defining ID composition.

**Available Variables**:

| Variable | Description | Example |
|----------|-------------|---------|
| `client_ip` | Client IP address | `192.168.1.1` |
| `user_agent` | User-Agent header | `Mozilla/5.0...` |
| `first_party_id` | Publisher-provided ID | `user-123` |
| `auth_user_id` | Authenticated user ID | `auth-456` |
| `publisher_domain` | Publisher domain | `publisher.com` |
| `accept_language` | Accept-Language header | `en-US,en;q=0.9` |

**Template Examples**:

**Simple (IP + UA)**:
```toml
template = "{{ client_ip }}:{{ user_agent }}"
```

**With First-Party ID**:
```toml
template = "{{ first_party_id }}:{{ client_ip }}"
```

**Comprehensive**:
```toml
template = "{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}:{{ auth_user_id }}:{{ publisher_domain }}:{{ accept_language }}"
```

**Validation**: Must be non-empty string.

::: tip Template Design
Choose template variables based on your privacy and uniqueness requirements:
- **More variables** = More unique IDs, less privacy
- **Fewer variables** = More privacy, potential collisions
- **Include `first_party_id`** for publisher ID integration
:::

## Response Headers

Custom headers added to all responses.

### `[response_headers]`

**Purpose**: Add custom HTTP headers to every response.

**Format**: Key-value pairs

**Example**:
```toml
[response_headers]
X-Custom-Header = "custom value"
X-Publisher-ID = "pub-12345"
X-Environment = "production"
Cache-Control = "public, max-age=3600"
```

**Environment Override**:
```bash
TRUSTED_SERVER__RESPONSE_HEADERS__X_CUSTOM_HEADER="custom value"
```

**Use Cases**:
- Custom tracking headers
- Cache control overrides
- Debugging identifiers
- CORS headers (if needed)

::: warning Header Precedence
Custom headers may be overwritten by application logic. Standard headers (`Content-Type`, `Content-Length`) are controlled by the application.
:::

## Request Signing

Configuration for Ed25519 request signing and JWKS management.

### `[request_signing]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `enabled` | Boolean | No (default: false) | Enable request signing features |
| `config_store_id` | String | If enabled | Fastly Config Store ID for JWKS |
| `secret_store_id` | String | If enabled | Fastly Secret Store ID for private keys |

**Example**:
```toml
[request_signing]
enabled = true
config_store_id = "01GXXX"  # From Fastly dashboard
secret_store_id = "01GYYY"  # From Fastly dashboard
```

**Environment Override**:
```bash
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true
TRUSTED_SERVER__REQUEST_SIGNING__CONFIG_STORE_ID=01GXXX
TRUSTED_SERVER__REQUEST_SIGNING__SECRET_STORE_ID=01GYYY
```

### Store Setup

**Config Store** (for public keys):
```bash
# Create store
fastly config-store create --name=jwks_store

# Get store ID
fastly config-store list
```

**Secret Store** (for private keys):
```bash
# Create store
fastly secret-store create --name=signing_keys

# Get store ID
fastly secret-store list
```

**Link to Service** (`fastly.toml`):
```toml
[setup.config_stores.jwks_store]

[setup.secret_stores.signing_keys]
```

See [Request Signing](/guide/request-signing) and [Key Rotation](/guide/key-rotation) for usage.

## Basic Authentication Handlers

Path-based HTTP Basic Authentication.

### `[[handlers]]`

**Purpose**: Protect specific paths with username/password authentication.

**Format**: Array of handler objects

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `path` | String (Regex) | Yes | Regular expression matching paths |
| `username` | String | Yes | HTTP Basic Auth username |
| `password` | String | Yes | HTTP Basic Auth password |

**Example**:
```toml
# Single handler
[[handlers]]
path = "^/admin"
username = "admin"
password = "secure-password"

# Multiple handlers
[[handlers]]
path = "^/secure"
username = "user1"
password = "pass1"

[[handlers]]
path = "^/api/private"
username = "api-user"
password = "api-pass"
```

**Environment Override**:
```bash
# Handler 0
TRUSTED_SERVER__HANDLERS__0__PATH="^/admin"
TRUSTED_SERVER__HANDLERS__0__USERNAME="admin"
TRUSTED_SERVER__HANDLERS__0__PASSWORD="secure-password"

# Handler 1
TRUSTED_SERVER__HANDLERS__1__PATH="^/api/private"
TRUSTED_SERVER__HANDLERS__1__USERNAME="api-user"
TRUSTED_SERVER__HANDLERS__1__PASSWORD="api-pass"
```

### Path Patterns

**Regex Syntax**: Standard Rust regex patterns

**Examples**:

```toml
# Exact path
path = "^/admin$"  # Only /admin

# Prefix match
path = "^/admin"   # /admin, /admin/users, /admin/settings

# Multiple paths
path = "^/(admin|secure|private)"

# File extension
path = "\\.pdf$"   # All PDF files

# Complex pattern
path = "^/api/v[0-9]+/private"  # /api/v1/private, /api/v2/private
```

**Validation**: Application startup fails if regex is invalid.

### Security Considerations

**Password Storage**:
- Stored in plain text in config
- Use environment variables in production
- Rotate passwords regularly
- Consider using Fastly Secret Store

**Limitations**:
- HTTP Basic Auth (not OAuth/JWT)
- Single username/password per path
- No role-based access control
- No rate limiting (add at edge)

::: warning Production Use
For production, store credentials in environment variables:
```bash
TRUSTED_SERVER__HANDLERS__0__PASSWORD=$(cat /run/secrets/admin_password)
```
:::

## URL Rewrite Configuration

Control which domains are excluded from first-party rewriting.

### `[rewrite]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `exclude_domains` | Array[String] | No (default: []) | Domains to skip rewriting |

**Example**:
```toml
[rewrite]
exclude_domains = [
    "*.cdn.trusted-partner.com",  # Wildcard
    "first-party.publisher.com",  # Exact match
    "localhost",                  # Development
]
```

**Environment Override**:
```bash
# JSON array
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS='["*.cdn.example.com","localhost"]'

# Indexed
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS__0="*.cdn.example.com"
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS__1="localhost"

# Comma-separated
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS="*.cdn.example.com,localhost"
```

### Pattern Matching

**Wildcard Patterns** (`*`):
```toml
"*.cdn.example.com"
```
Matches:
- ✅ `assets.cdn.example.com`
- ✅ `images.cdn.example.com`
- ✅ `cdn.example.com` (base domain)
- ❌ `cdn.example.com.evil.com` (different domain)

**Exact Patterns** (no `*`):
```toml
"api.example.com"
```
Matches:
- ✅ `api.example.com`
- ❌ `www.api.example.com`
- ❌ `api.example.com.evil.com`

### Use Cases

**Trusted Partners**:
```toml
exclude_domains = ["*.approved-cdn.com"]
```

**First-Party Resources**:
```toml
exclude_domains = ["assets.publisher.com", "static.publisher.com"]
```

**Development**:
```toml
exclude_domains = ["localhost", "127.0.0.1", "*.local"]
```

**Performance** (already first-party):
```toml
exclude_domains = ["*.publisher.com"]  # Skip unnecessary proxying
```

See [Creative Processing](/guide/creative-processing#exclude-domains) for details.

## Integration Configurations

Settings for built-in integrations (Prebid, Next.js, Permutive, Testlight).

### Common Fields

All integrations support:

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | Boolean | Enable/disable integration (default: false) |

### Prebid Integration

**Section**: `[integrations.prebid]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | Boolean | `false` | Enable Prebid integration |
| `server_url` | String | Required | Prebid Server endpoint URL |
| `timeout_ms` | Integer | `1000` | Request timeout in milliseconds |
| `bidders` | Array[String] | `[]` | List of enabled bidders |
| `debug` | Boolean | `false` | Enable debug logging |
| `mode` | String | None | Default TSJS request mode when Prebid is enabled (`render` or `auction`); `auction` expects OpenRTB clients (for example, Prebid.js) calling `/ad/auction` |
| `script_patterns` | Array[String] | See below | Patterns for removing Prebid script tags and intercepting requests |

**Default `script_patterns`**:
```toml
["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"]
```

These patterns use suffix matching when stripping HTML, so `/static/prebid/v8/prebid.min.js` matches because it ends with `/prebid.min.js`. For request interception, exact paths are registered unless you use wildcard patterns (e.g., `/static/prebid/*`), which match paths under that prefix.

**Example**:
```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example/openrtb2/auction"
timeout_ms = 1200
bidders = ["kargo", "rubicon", "appnexus", "openx"]
debug = false
mode = "auction" # OpenRTB clients (for example, Prebid.js)
# script_patterns = ["/static/prebid/*"]  # Optional: restrict to specific path
```

**Environment Override**:
```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL=https://prebid.example/auction
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1200
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS=kargo,rubicon,appnexus
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=false
TRUSTED_SERVER__INTEGRATIONS__PREBID__MODE=auction
TRUSTED_SERVER__INTEGRATIONS__PREBID__SCRIPT_PATTERNS__0=/prebid.js
TRUSTED_SERVER__INTEGRATIONS__PREBID__SCRIPT_PATTERNS__1=/prebid.min.js
```

### Next.js Integration

**Section**: `[integrations.nextjs]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | Boolean | `false` | Enable Next.js integration |
| `rewrite_attributes` | Array[String] | `["href","link","url"]` | Attributes to rewrite |

**Example**:
```toml
[integrations.nextjs]
enabled = true
rewrite_attributes = ["href", "link", "url", "src"]
```

**Environment Override**:
```bash
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__REWRITE_ATTRIBUTES=href,link,url,src
```

### Permutive Integration

**Section**: `[integrations.permutive]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | Boolean | `false` | Enable Permutive integration |
| `organization_id` | String | Required | Permutive organization ID |
| `workspace_id` | String | Required | Permutive workspace ID |
| `project_id` | String | Required | Permutive project ID |
| `api_endpoint` | String | `https://api.permutive.com` | Permutive API URL |
| `secure_signals_endpoint` | String | `https://secure-signals.permutive.app` | Secure signals URL |
| `cache_ttl_seconds` | Integer | `3600` | Cache TTL in seconds |
| `rewrite_sdk` | Boolean | `true` | Rewrite Permutive SDK references |

**Example**:
```toml
[integrations.permutive]
enabled = true
organization_id = "org-12345"
workspace_id = "ws-67890"
project_id = "proj-abcde"
api_endpoint = "https://api.permutive.com"
secure_signals_endpoint = "https://secure-signals.permutive.app"
cache_ttl_seconds = 7200
rewrite_sdk = true
```

### Testlight Integration

**Section**: `[integrations.testlight]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | Boolean | `false` | Enable Testlight integration |
| `endpoint` | String | Required | Testlight auction endpoint |
| `timeout_ms` | Integer | `1000` | Request timeout in milliseconds |
| `rewrite_scripts` | Boolean | `true` | Rewrite Testlight script references |

**Example**:
```toml
[integrations.testlight]
enabled = true
endpoint = "https://testlight.example/openrtb2/auction"
timeout_ms = 1500
rewrite_scripts = true
```

## Validation

### Automatic Validation

Configuration is validated at startup:

**Publisher Validation**:
- All fields non-empty
- `origin_url` is valid URL

**Synthetic Validation**:
- `secret_key` ≥ 1 character
- `secret_key` ≠ `"secret-key"`
- `template` non-empty

**Handler Validation**:
- `path` is valid regex
- `username` non-empty
- `password` non-empty

**Integration Validation**:
- Each integration implements `Validate` trait
- Custom rules per integration

### Validation Errors

**Startup Failure** if:
- Required fields missing
- Invalid data types
- Regex compilation fails
- Secret key is default value
- Integration config fails validation

**Error Format**:
```
Configuration error: Integration 'prebid' configuration failed validation: 
server_url: must not be empty
```

## Best Practices

### Configuration Management

**Development**:
```toml
# trusted-server.dev.toml
[publisher]
domain = "localhost"
origin_url = "http://localhost:3000"
proxy_secret = "dev-secret"
```

**Staging**:
```bash
# .env.staging
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://staging.publisher.com
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=$(cat /run/secrets/proxy_secret_staging)
```

**Production**:
```bash
# All secrets from environment
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=$(cat /run/secrets/proxy_secret)
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY=$(cat /run/secrets/synthetic_secret)
TRUSTED_SERVER__HANDLERS__0__PASSWORD=$(cat /run/secrets/admin_password)
```

### Secret Management

**Do**:
✅ Use environment variables for secrets  
✅ Rotate secrets periodically  
✅ Generate cryptographically random values  
✅ Store in secure secret management (Fastly Secret Store, Vault)  
✅ Use different secrets per environment  

**Don't**:
❌ Commit secrets to version control  
❌ Use default/placeholder values  
❌ Share secrets across environments  
❌ Log secret values  
❌ Expose in error messages  

### File Organization

**Recommended Structure**:
```
trusted-server.toml          # Base config
trusted-server.dev.toml      # Development overrides
.env.development             # Dev environment vars
.env.staging                 # Staging environment vars
.env.production              # Production environment vars (not in git)
.env.example                 # Example template (in git)
```

**.gitignore**:
```
.env.production
.env.staging
.env.local
*.secret
```

## Troubleshooting

### Common Issues

**"Failed to build configuration"**:
- Check TOML syntax (trailing commas, quotes)
- Verify all required fields present
- Check environment variable format

**"Secret key is not valid"**:
- Cannot use `"secret-key"` (placeholder)
- Must be non-empty
- Change to secure random value

**"Invalid regex"**:
- Handler `path` must be valid regex
- Test pattern: `echo "^/admin" | grep -E "^/admin"`
- Escape special characters: `\.`, `\$`, etc.

**"Integration configuration could not be parsed"**:
- Check JSON syntax in env vars
- Verify indexed arrays (0, 1, 2...)
- Check field names match exactly

**Environment Variables Not Applied**:
- Verify prefix: `TRUSTED_SERVER__`
- Check separator: `__` (double underscore)
- Confirm variable is exported: `echo $VARIABLE_NAME`
- Try explicit string: `VARIABLE='value'` not `VARIABLE=value`

### Debug Configuration

**Print Loaded Config** (test only):
```rust
use trusted_server_common::settings::Settings;

let settings = Settings::new()?;
println!("{:#?}", settings);
```

**Check Environment**:
```bash
# List all TRUSTED_SERVER variables
env | grep TRUSTED_SERVER
```

**Validate TOML**:
```bash
# Use any TOML validator
cat trusted-server.toml | npx toml-cli validate
```

## Next Steps

- Learn about [First-Party Proxy](/guide/first-party-proxy) for URL proxying
- Set up [Request Signing](/guide/request-signing) for secure API calls
- Configure [Creative Processing](/guide/creative-processing) rewrites
- Explore [Integration Guide](/guide/integration-guide) for custom integrations

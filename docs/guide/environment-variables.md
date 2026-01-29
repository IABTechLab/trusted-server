# Environment Variables Reference

Complete guide to configuring Trusted Server using environment variables.

## Overview

Trusted Server can be configured using:
1. **TOML file** (`trusted-server.toml`) - Default configuration
2. **Environment variables** - Override TOML settings at runtime

Environment variables are useful for:
- Secrets management (never commit passwords/keys to git)
- Environment-specific settings (dev/staging/prod)
- CI/CD pipelines
- Container deployments
- Testing different configurations

---

## Variable Naming Pattern

All environment variables follow this pattern:

```
TRUSTED_SERVER__{SECTION}__{SUBSECTION}__{FIELD}
```

**Rules:**
- Double underscores (`__`) separate levels
- All uppercase
- Hyphens in TOML keys become underscores
- Array indices use double underscores with numbers

**Example:**
```toml
# TOML: trusted-server.toml
[publisher]
domain = "example.com"

# Environment variable:
TRUSTED_SERVER__PUBLISHER__DOMAIN="example.com"
```

---

## Publisher Configuration

### Required Settings

```bash
# Publisher domain (required)
TRUSTED_SERVER__PUBLISHER__DOMAIN="publisher.com"

# Origin server URL (required)
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="https://origin.publisher.com"

# Cookie domain (required)
TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=".publisher.com"

# Proxy secret for URL signing (required)
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="change-me-to-random-string-min-32-chars"
```

**TOML Equivalent:**
```toml
[publisher]
domain = "publisher.com"
origin_url = "https://origin.publisher.com"
cookie_domain = ".publisher.com"
proxy_secret = "change-me-to-random-string-min-32-chars"
```

---

## Synthetic ID Configuration

```bash
# KV store names
TRUSTED_SERVER__SYNTHETIC__COUNTER_STORE="counter_store"
TRUSTED_SERVER__SYNTHETIC__OPID_STORE="opid_store"

# Secret key for HMAC (required, min 16 chars)
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY="your-secret-key-min-16-chars"

# Template (comma-separated for array)
TRUSTED_SERVER__SYNTHETIC__TEMPLATE="{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}"
```

**Template Variables:**
- `client_ip` - Client IP address
- `user_agent` - User agent string
- `first_party_id` - Publisher's first-party cookie
- `auth_user_id` - Authenticated user ID
- `publisher_domain` - Publisher domain
- `accept_language` - Accept-Language header

**TOML Equivalent:**
```toml
[synthetic]
counter_store = "counter_store"
opid_store = "opid_store"
secret_key = "your-secret-key-min-16-chars"
template = "{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}"
```

---

## Request Signing Configuration

```bash
# Enable request signing
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true

# Fastly Config Store ID
TRUSTED_SERVER__REQUEST_SIGNING__CONFIG_STORE_ID="your-config-store-id"

# Fastly Secret Store ID
TRUSTED_SERVER__REQUEST_SIGNING__SECRET_STORE_ID="your-secret-store-id"
```

**TOML Equivalent:**
```toml
[request_signing]
enabled = true
config_store_id = "your-config-store-id"
secret_store_id = "your-secret-store-id"
```

---

## Response Headers

Custom headers to include in all responses:

```bash
# Single header
TRUSTED_SERVER__RESPONSE_HEADERS__X_CUSTOM_HEADER="custom value"

# Multiple headers (use separate variables)
TRUSTED_SERVER__RESPONSE_HEADERS__X_FRAME_OPTIONS="SAMEORIGIN"
TRUSTED_SERVER__RESPONSE_HEADERS__X_CONTENT_TYPE_OPTIONS="nosniff"
TRUSTED_SERVER__RESPONSE_HEADERS__STRICT_TRANSPORT_SECURITY="max-age=31536000"
```

**TOML Equivalent:**
```toml
[response_headers]
X-Custom-Header = "custom value"
X-Frame-Options = "SAMEORIGIN"
X-Content-Type-Options = "nosniff"
Strict-Transport-Security = "max-age=31536000"
```

---

## Handler Authentication

Basic auth for specific paths:

```bash
# Handler #1
TRUSTED_SERVER__HANDLERS__0__PATH="^/admin"
TRUSTED_SERVER__HANDLERS__0__USERNAME="admin"
TRUSTED_SERVER__HANDLERS__0__PASSWORD="secure-password"

# Handler #2
TRUSTED_SERVER__HANDLERS__1__PATH="^/secure"
TRUSTED_SERVER__HANDLERS__1__USERNAME="user"
TRUSTED_SERVER__HANDLERS__1__PASSWORD="another-password"
```

**Pattern:** Use `__0__`, `__1__`, etc. for array indices.

**TOML Equivalent:**
```toml
[[handlers]]
path = "^/admin"
username = "admin"
password = "secure-password"

[[handlers]]
path = "^/secure"
username = "user"
password = "another-password"
```

---

## Prebid Integration

```bash
# Enable integration
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true

# Prebid Server URL (required)
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL="https://prebid-server.example.com"

# Request timeout in milliseconds
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1000

# Bidders (comma-separated)
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS="appnexus,rubicon,openx"

# Enable debug logging
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=false

# Script patterns to remove Prebid tags and serve empty JS (indexed format)
# Default patterns match common Prebid filenames at exact paths
TRUSTED_SERVER__INTEGRATIONS__PREBID__SCRIPT_PATTERNS__0="/prebid.js"
TRUSTED_SERVER__INTEGRATIONS__PREBID__SCRIPT_PATTERNS__1="/prebid.min.js"
# For versioned paths, use wildcards:
# TRUSTED_SERVER__INTEGRATIONS__PREBID__SCRIPT_PATTERNS__0="/static/prebid/{*rest}"
```

**TOML Equivalent:**
```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000
bidders = ["appnexus", "rubicon", "openx"]
debug = false
script_patterns = ["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"]
```

---

## Next.js Integration

```bash
# Enable integration
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__ENABLED=true

# Attributes to rewrite (comma-separated)
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__REWRITE_ATTRIBUTES="href,link,url,src"
```

**TOML Equivalent:**
```toml
[integrations.nextjs]
enabled = true
rewrite_attributes = ["href", "link", "url", "src"]
```

---

## Permutive Integration

```bash
# Enable integration
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__ENABLED=true

# Permutive organization ID (required)
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__ORGANIZATION_ID="myorg"

# Permutive workspace ID (required)
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__WORKSPACE_ID="workspace-12345"

# Optional project ID
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__PROJECT_ID="project-789"

# API endpoints
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__API_ENDPOINT="https://api.permutive.com"
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__SECURE_SIGNALS_ENDPOINT="https://secure-signals.permutive.app"

# SDK cache TTL in seconds
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__CACHE_TTL_SECONDS=3600

# Auto-rewrite SDK URLs in HTML
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__REWRITE_SDK=true
```

**TOML Equivalent:**
```toml
[integrations.permutive]
enabled = true
organization_id = "myorg"
workspace_id = "workspace-12345"
project_id = "project-789"
api_endpoint = "https://api.permutive.com"
secure_signals_endpoint = "https://secure-signals.permutive.app"
cache_ttl_seconds = 3600
rewrite_sdk = true
```

---

## Testlight Integration

```bash
# Enable integration
TRUSTED_SERVER__INTEGRATIONS__TESTLIGHT__ENABLED=true

# Upstream endpoint (required)
TRUSTED_SERVER__INTEGRATIONS__TESTLIGHT__ENDPOINT="https://testlight-server.example.com"

# Request timeout in milliseconds
TRUSTED_SERVER__INTEGRATIONS__TESTLIGHT__TIMEOUT_MS=1000

# Script replacement URL
TRUSTED_SERVER__INTEGRATIONS__TESTLIGHT__SHIM_SRC="/static/tsjs-unified.js"

# Auto-rewrite testlight.js scripts
TRUSTED_SERVER__INTEGRATIONS__TESTLIGHT__REWRITE_SCRIPTS=false
```

**TOML Equivalent:**
```toml
[integrations.testlight]
enabled = true
endpoint = "https://testlight-server.example.com"
timeout_ms = 1000
shim_src = "/static/tsjs-unified.js"
rewrite_scripts = false
```

---

## Rewrite Configuration

Exclude domains from first-party rewriting:

```bash
# Comma-separated list of domains/patterns
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS="*.edgecompute.app,localhost:*,*.internal.com"
```

**TOML Equivalent:**
```toml
[rewrite]
exclude_domains = [
    "*.edgecompute.app",
    "localhost:*",
    "*.internal.com"
]
```

**Patterns:**
- Exact match: `example.com`
- Wildcard subdomain: `*.example.com`
- Wildcard port: `localhost:*`
- Full wildcard: `*`

---

## Data Type Formats

### Strings
```bash
# Simple string
TRUSTED_SERVER__PUBLISHER__DOMAIN="example.com"

# String with spaces (quote in shell)
TRUSTED_SERVER__RESPONSE_HEADERS__X_CUSTOM="value with spaces"
```

### Booleans
```bash
# Accepted values: true, false (lowercase)
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=false
```

### Numbers
```bash
# Integers (no quotes)
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1000
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__CACHE_TTL_SECONDS=3600
```

### Arrays (Comma-Separated)
```bash
# Comma-separated values (no spaces)
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS="appnexus,rubicon,openx"

# With spaces requires quotes
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__REWRITE_ATTRIBUTES="href,link,url"
```

### Arrays (Indexed)
```bash
# Use indices for complex arrays
TRUSTED_SERVER__HANDLERS__0__PATH="^/admin"
TRUSTED_SERVER__HANDLERS__0__USERNAME="admin"
TRUSTED_SERVER__HANDLERS__1__PATH="^/secure"
TRUSTED_SERVER__HANDLERS__1__USERNAME="user"
```

### Nested Objects
```bash
# Use double underscores for nesting
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL="https://server.com"
#                 ^section     ^subsection ^field
```

---

## Common Patterns

### Development Environment
```bash
# .env.development
TRUSTED_SERVER__PUBLISHER__DOMAIN="localhost:7676"
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://localhost:8080"
TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN="localhost"
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=true
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=false
```

### Production Environment
```bash
# .env.production
TRUSTED_SERVER__PUBLISHER__DOMAIN="publisher.com"
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="https://origin.publisher.com"
TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=".publisher.com"
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="${PROXY_SECRET}"  # From secrets manager
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY="${SYNTHETIC_KEY}"
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true
TRUSTED_SERVER__REQUEST_SIGNING__CONFIG_STORE_ID="${CONFIG_STORE_ID}"
TRUSTED_SERVER__REQUEST_SIGNING__SECRET_STORE_ID="${SECRET_STORE_ID}"
```

### CI/CD Pipeline
```bash
# GitHub Actions example
env:
  TRUSTED_SERVER__PUBLISHER__DOMAIN: ${{ vars.PUBLISHER_DOMAIN }}
  TRUSTED_SERVER__PUBLISHER__PROXY_SECRET: ${{ secrets.PROXY_SECRET }}
  TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL: ${{ vars.PREBID_URL }}
  TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS: "appnexus,rubicon"
```

---

## Secrets Management

### Never Commit Secrets
```bash
# ❌ WRONG - Don't commit to git
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="my-secret-key"

# ✅ CORRECT - Use environment-specific injection
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="${PROXY_SECRET}"
```

### Use Secrets Managers
```bash
# AWS Secrets Manager
export PROXY_SECRET=$(aws secretsmanager get-secret-value --secret-id trusted-server/proxy-secret --query SecretString --output text)
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="${PROXY_SECRET}"

# HashiCorp Vault
export PROXY_SECRET=$(vault kv get -field=value secret/trusted-server/proxy-secret)
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="${PROXY_SECRET}"

# Fastly Secret Store (for keys)
# Managed via Fastly dashboard - not environment variables
```

### Rotate Secrets Regularly
```bash
# Generate strong secrets
openssl rand -base64 32  # 32-byte random string

# Update environment and redeploy
```

---

## Loading Environment Variables

### Local Development

**Using .env file:**
```bash
# Create .env file
cat > .env <<EOF
TRUSTED_SERVER__PUBLISHER__DOMAIN="localhost:7676"
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://localhost:8080"
EOF

# Load and run
export $(grep -v '^#' .env | xargs)
fastly compute serve
```

**Using shell script:**
```bash
#!/bin/bash
export TRUSTED_SERVER__PUBLISHER__DOMAIN="localhost:7676"
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://localhost:8080"
fastly compute serve
```

### Fastly Deployment

Environment variables are embedded at build time:

```bash
# Set variables before build
export TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL="https://prod-prebid.com"

# Build with variables
cargo build --release --target wasm32-wasip1

# Deploy
fastly compute publish
```

**Note:** Changes require rebuild and redeploy.

---

## Validation

### Check Current Configuration

```bash
# Build and check logs for settings
cargo build 2>&1 | grep -i "settings"

# Or in Fastly logs
fastly log-tail | grep -i "settings"
```

### Verify Override

```toml
# trusted-server.toml
[publisher]
domain = "default.com"
```

```bash
# Override with environment variable
TRUSTED_SERVER__PUBLISHER__DOMAIN="override.com" cargo build

# Should log: "domain: override.com"
```

### Test Locally

```bash
# Start with environment variable
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=true fastly compute serve

# Verify in logs
# Should see debug output from Prebid integration
```

---

## Troubleshooting

### Variable Not Taking Effect

**Cause:** Environment variable set after build

**Solution:** Set before building:
```bash
export TRUSTED_SERVER__PUBLISHER__DOMAIN="example.com"
cargo build
```

### Parse Error

**Cause:** Wrong data type format

```bash
# ❌ Wrong
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS="1000"  # String

# ✅ Correct
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1000    # Number
```

### Array Not Working

**Cause:** Incorrect array format

```bash
# ❌ Wrong
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS='["appnexus", "rubicon"]'

# ✅ Correct
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS="appnexus,rubicon"
```

### Variable Name Typo

**Cause:** Incorrect casing or separator

```bash
# ❌ Wrong
TRUSTED_SERVER_PUBLISHER_DOMAIN="example.com"       # Single underscore
trusted_server__publisher__domain="example.com"     # Lowercase

# ✅ Correct
TRUSTED_SERVER__PUBLISHER__DOMAIN="example.com"     # Double underscore, uppercase
```

---

## Security Best Practices

1. **Never commit secrets** to version control
2. **Use environment-specific files** (.env.development, .env.production)
3. **Restrict access** to production environment variables
4. **Rotate secrets** regularly (every 90 days minimum)
5. **Use strong secrets** (minimum 32 characters, random)
6. **Audit access** to secrets management systems
7. **Encrypt at rest** (use secrets managers with encryption)
8. **Log carefully** (don't log secret values)

---

## Complete Example

```bash
#!/bin/bash
# Environment configuration for production

# Required: Publisher Settings
export TRUSTED_SERVER__PUBLISHER__DOMAIN="publisher.com"
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="https://origin.publisher.com"
export TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=".publisher.com"
export TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="${PROXY_SECRET}"  # From secrets manager

# Required: Synthetic ID
export TRUSTED_SERVER__SYNTHETIC__COUNTER_STORE="counter_store"
export TRUSTED_SERVER__SYNTHETIC__OPID_STORE="opid_store"
export TRUSTED_SERVER__SYNTHETIC__SECRET_KEY="${SYNTHETIC_KEY}"  # From secrets manager
export TRUSTED_SERVER__SYNTHETIC__TEMPLATE="{{ client_ip }}:{{ user_agent }}"

# Optional: Request Signing
export TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true
export TRUSTED_SERVER__REQUEST_SIGNING__CONFIG_STORE_ID="config-store-123"
export TRUSTED_SERVER__REQUEST_SIGNING__SECRET_STORE_ID="secret-store-456"

# Optional: Prebid Integration
export TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true
export TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL="https://prebid-server.com"
export TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=2000
export TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS="appnexus,rubicon,openx"

# Optional: Security Headers
export TRUSTED_SERVER__RESPONSE_HEADERS__STRICT_TRANSPORT_SECURITY="max-age=31536000"
export TRUSTED_SERVER__RESPONSE_HEADERS__X_CONTENT_TYPE_OPTIONS="nosniff"
export TRUSTED_SERVER__RESPONSE_HEADERS__X_FRAME_OPTIONS="SAMEORIGIN"

# Build and deploy
cargo build --release --target wasm32-wasip1
fastly compute publish
```

---

## Next Steps

- Review [Configuration Reference](./configuration-reference.md)
- Understand [Error Reference](./error-reference.md)
- Explore [API Reference](./api-reference.md)
- Learn about [Request Signing](./request-signing.md)

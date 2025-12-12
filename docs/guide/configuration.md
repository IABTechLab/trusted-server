# Configuration

Learn how to configure Trusted Server for your deployment.

## Configuration Files

### trusted-server.toml

Main application configuration file.

```toml
# Example configuration structure

[general]
environment = "production"
log_level = "info"

[synthetic_ids]
secret_key = "your-secret-key"
template = "{{domain}}-{{timestamp}}"
rotation_days = 90

[gdpr]
require_consent = true
tcf_version = "2.2"
default_action = "reject"

[ad_servers.equativ]
endpoint = "https://ad.example.com"
timeout_ms = 1000
enabled = true

[prebid]
timeout_ms = 1500
cache_ttl = 300
bidders = ["appnexus", "rubicon"]

[kv_stores]
counters = "counter_store"
domains = "domain_store"
```

### fastly.toml

Fastly service configuration for build and deployment settings.

```toml
manifest_version = 2
name = "trusted-server"
description = "Privacy-preserving ad serving"
authors = ["Your Name"]
language = "rust"

[local_server]
  [local_server.backends]
    [local_server.backends.ad_server]
      url = "https://ad-server.example.com"

[setup]
  [setup.dictionaries.config]
    format = "inline-toml"
    file = "trusted-server.toml"
```

### .env.dev

Local development environment variables.

```bash
FASTLY_SERVICE_ID=your-service-id
FASTLY_API_TOKEN=your-api-token
LOG_LEVEL=debug
```

## Configuration Sections

### General Settings

- `environment` - Deployment environment (dev/staging/production)
- `log_level` - Logging verbosity (trace/debug/info/warn/error)

### Synthetic IDs

- `secret_key` - HMAC secret key for ID generation
- `template` - Template string for ID construction
- `rotation_days` - Key rotation frequency

### GDPR Settings

- `require_consent` - Enforce consent checks
- `tcf_version` - TCF framework version
- `default_action` - Action when consent unclear (accept/reject)

### Ad Server Configuration

- `endpoint` - Ad server URL
- `timeout_ms` - Request timeout in milliseconds
- `enabled` - Enable/disable integration

### Prebid Settings

- `timeout_ms` - Auction timeout
- `cache_ttl` - Bid cache duration
- `bidders` - List of enabled bidders

### KV Stores

- `counters` - Counter storage name
- `domains` - Domain mapping storage name

## Environment-Specific Configuration

Override settings per environment:

```toml
[environments.production]
log_level = "warn"

[environments.development]
log_level = "debug"
```

## Secrets Management

Sensitive values should be:
1. Stored in Fastly Secret Store
2. Referenced in configuration
3. Never committed to version control

## Validation

Validate configuration before deployment:

```bash
fastly compute validate
```

## Hot Reloading

Some settings support hot reloading via Fastly dictionaries:
- Ad server endpoints
- Timeout values
- Feature flags

## Best Practices

1. Use environment-specific configurations
2. Rotate secrets regularly
3. Document all custom settings
4. Validate before deployment
5. Monitor configuration changes

## Next Steps

- Set up [Testing](/guide/testing)
- Review [Architecture](/guide/architecture)

# Key Rotation

Learn how to rotate signing keys to maintain security and manage the lifecycle of cryptographic keys in Trusted Server.

## Overview

Key rotation is the process of generating new signing keys and transitioning from old keys to new ones. Trusted Server provides automated key rotation with:

- **Zero-downtime rotation** - Old and new keys work simultaneously
- **Automatic key generation** - Date-based key identifiers
- **Grace period support** - Multiple active keys during transition
- **Safe deactivation** - Prevents removing the last active key

## Why Rotate Keys?

### Security Reasons

1. **Limit key exposure** - Reduce impact if a key is compromised
2. **Cryptographic hygiene** - Follow security best practices
3. **Compliance requirements** - Meet regulatory rotation schedules
4. **Incident response** - Quickly invalidate potentially compromised keys

### Recommended Schedule

- **Regular rotation**: Every 90 days minimum
- **Incident-based**: Immediately if compromise suspected
- **Before major releases**: Ensure fresh keys for new deployments

## Prerequisites

Before you can rotate keys, you need to set up the required Fastly stores and API credentials.

### Required Stores

Key rotation requires three Fastly stores:

1. **Config Store** (`jwks_store`) - Stores public JWKs and metadata
   - `current-kid` - The active key identifier
   - `active-kids` - Comma-separated list of valid key IDs
   - Individual JWKs keyed by their `kid`

2. **Secret Store** (`signing_keys`) - Stores private signing keys
   - Each key stored with its `kid` as the key name
   - Values are base64-encoded Ed25519 private keys

3. **Secret Store** (`api-keys`) - Stores Fastly API credentials
   - `api_key` - Fastly API token for managing stores

### Creating Stores

#### 1. Create Config Store

```bash
# Create the config store
fastly config-store create --name=jwks_store

# Get the store ID (you'll need this for configuration)
fastly config-store list
```

Note the Config Store ID from the output.

#### 2. Create Secret Stores

```bash
# Create secret store for signing keys
fastly secret-store create --name=signing_keys

# Create secret store for API credentials
fastly secret-store create --name=api-keys

# Get the store IDs
fastly secret-store list
```

Note both Secret Store IDs from the output.

::: tip Dashboard Alternative
You can also create stores via the Fastly dashboard, but CLI commands are recommended for automation and reproducibility.
:::

### Creating Fastly API Key

Key rotation uses the Fastly API to manage store contents. You need to create an API token:

#### Step 1: Generate API Token

1. Log in to the [Fastly Dashboard](https://manage.fastly.com)
2. Navigate to **Account → API Tokens → Personal Tokens**
3. Click **Create Token**
4. Configure the token:
   - **Name**: `trusted-server-key-rotation`
   - **Scope**: `global:read`, `global:write` (or scope to your specific service)
   - **Expiration**: Set according to your security policy

#### Step 2: Store API Token

Store the API token in the `api-keys` secret store:

```bash
# Store the API key
fastly secret-store-entry create \
  --store-id=<your-api-keys-store-id> \
  --name=api_key \
  --secret=<your-fastly-api-token>
```

::: warning Keep Your API Token Secure

- Never commit API tokens to version control
- Store them only in Fastly Secret Store
- Rotate API tokens according to your security policy
- Use minimal required permissions
  :::

### Linking Stores to Service

Stores must be linked to your Compute service to be accessible at runtime.

#### Production (CLI)

```bash
# Link config store
fastly service-version compute config-store create \
  --version=<version> \
  --config-store-id=<jwks-store-id> \
  --name=jwks_store

# Link signing keys secret store
fastly service-version compute secret-store create \
  --version=<version> \
  --secret-store-id=<signing-keys-store-id> \
  --name=signing_keys

# Link API keys secret store
fastly service-version compute secret-store create \
  --version=<version> \
  --secret-store-id=<api-keys-store-id> \
  --name=api-keys
```

::: tip Dashboard Linking
You can also link stores via the Fastly dashboard under your service's **Resources** section.
:::

#### Local Development

For local testing, configure stores in `fastly.toml`:

```toml
[local_server.config_stores]
  [local_server.config_stores.jwks_store]
    format = "inline-toml"
    [local_server.config_stores.jwks_store.contents]
      ts-2025-01-01 = "{\"kty\":\"OKP\",\"crv\":\"Ed25519\",\"kid\":\"ts-2025-01-01\",\"use\":\"sig\",\"x\":\"...\"}"
      current-kid = "ts-2025-01-01"
      active-kids = "ts-2025-01-01"

[local_server.secret_stores]
  [[local_server.secret_stores.signing_keys]]
    key = "ts-2025-01-01"
    data = "<signing-key>"

  [[local_server.secret_stores.api-keys]]
    key = "api_key"
    env = "FASTLY_KEY"  # Load from environment variable
```

### Configuration in trusted-server.toml

Update `trusted-server.toml` with your store IDs:

```toml
[request_signing]
enabled = true
config_store_id = "<config-store-id>"  # Your jwks_store ID
secret_store_id = "<secret-store-id"  # Your signing_keys ID
```

::: tip Getting Store IDs
Use `fastly config-store list` and `fastly secret-store list` to retrieve your store IDs.
:::

### Verification

Verify your setup is correct:

```bash
# Test local development
fastly compute serve

# Check that stores are accessible
curl http://localhost:7676/.well-known/trusted-server.json
```

You should see a JWKS response with your public keys.

## Key Rotation Process

### Architecture

```
┌──────────────────────────────────────┐
│  Key Rotation Flow                   │
├──────────────────────────────────────┤
│                                      │
│  1. Generate new Ed25519 keypair     │
│     ↓                                │
│  2. Store private key (Secret Store) │
│     ↓                                │
│  3. Store public JWK (Config Store)  │
│     ↓                                │
│  4. Update current-kid pointer       │
│     ↓                                │
│  5. Update active-kids list          │
│     ↓                                │
│  6. Both keys now active             │
│                                      │
└──────────────────────────────────────┘
```

### State During Rotation

**Before Rotation**:

- Current key: `ts-2024-01-15`
- Active keys: `["ts-2024-01-15"]`

**After Rotation**:

- Current key: `ts-2024-02-15` (new)
- Active keys: `["ts-2024-01-15", "ts-2024-02-15"]`

**After Grace Period**:

- Current key: `ts-2024-02-15`
- Active keys: `["ts-2024-02-15"]`

## Rotating Keys

### Using the Rotation Endpoint

**Endpoint**: `POST /admin/keys/rotate`

#### Automatic Key ID (Recommended)

Let Trusted Server generate a date-based key ID:

```bash
curl -X POST https://your-domain/admin/keys/rotate \
  -H "Content-Type: application/json" \
  -d '{}'
```

**Response**:

```json
{
  "success": true,
  "message": "Key rotated successfully",
  "new_kid": "ts-2024-02-15",
  "previous_kid": "ts-2024-01-15",
  "active_kids": ["ts-2024-01-15", "ts-2024-02-15"],
  "jwk": {
    "kty": "OKP",
    "crv": "Ed25519",
    "x": "new-public-key-base64url",
    "kid": "ts-2024-02-15",
    "alg": "EdDSA"
  }
}
```

#### Custom Key ID

Specify a custom key identifier:

```bash
curl -X POST https://your-domain/admin/keys/rotate \
  -H "Content-Type: application/json" \
  -d '{"kid": "production-2024-q1"}'
```

**Response**:

```json
{
  "success": true,
  "message": "Key rotated successfully",
  "new_kid": "production-2024-q1",
  "previous_kid": "ts-2024-01-15",
  "active_kids": ["ts-2024-01-15", "production-2024-q1"],
  "jwk": { ... }
}
```

### Using the Rust API

```rust
use trusted_server_common::request_signing::KeyRotationManager;

// Initialize rotation manager
let manager = KeyRotationManager::new("jwks_store", "signing_keys")?;

// Rotate with automatic kid
let result = manager.rotate_key(None)?;

println!("New key: {}", result.new_kid);
println!("Previous key: {:?}", result.previous_kid);
println!("Active keys: {:?}", result.active_kids);

// Or rotate with custom kid
let custom_result = manager.rotate_key(Some("my-custom-key".to_string()))?;
```

## Managing Active Keys

### Listing Active Keys

**Rust API**:

```rust
let manager = KeyRotationManager::new("jwks_store", "signing_keys")?;
let active_keys = manager.list_active_keys()?;

for kid in active_keys {
    println!("Active key: {}", kid);
}
```

**Config Store**:
Keys are stored as comma-separated values in the `active-kids` config item:

```
ts-2024-01-15,ts-2024-02-15,ts-2024-03-15
```

### Multiple Active Keys

You can have multiple active keys for:

- **Gradual rollout**: Different services adopt new key at different times
- **Geographic distribution**: Different regions rotate independently
- **A/B testing**: Test new keys with subset of traffic

## Deactivating Keys

### When to Deactivate

Deactivate old keys after:

1. All services have adopted the new key
2. Grace period has elapsed (recommended: 7-30 days)
3. No more requests using the old key
4. Old signatures no longer need verification

### Deactivation Endpoint

**Endpoint**: `POST /admin/keys/deactivate`

#### Deactivate (Keep in Storage)

Remove from active rotation but keep in storage:

```bash
curl -X POST https://your-domain/admin/keys/deactivate \
  -H "Content-Type: application/json" \
  -d '{
    "kid": "ts-2024-01-15",
    "delete": false
  }'
```

**Response**:

```json
{
  "success": true,
  "message": "Key deactivated successfully",
  "deactivated_kid": "ts-2024-01-15",
  "deleted": false,
  "remaining_active_kids": ["ts-2024-02-15"]
}
```

#### Delete Permanently

Remove from storage completely:

```bash
curl -X POST https://your-domain/admin/keys/deactivate \
  -H "Content-Type: application/json" \
  -d '{
    "kid": "ts-2024-01-15",
    "delete": true
  }'
```

**Response**:

```json
{
  "success": true,
  "message": "Key deleted successfully",
  "deactivated_kid": "ts-2024-01-15",
  "deleted": true,
  "remaining_active_kids": ["ts-2024-02-15"]
}
```

### Using the Rust API

```rust
let manager = KeyRotationManager::new("jwks_store", "signing_keys")?;

// Deactivate (keep in storage)
manager.deactivate_key("ts-2024-01-15")?;

// Delete completely
manager.delete_key("ts-2024-01-15")?;
```

### Safety Checks

The system prevents:

- **Deleting the last active key** - At least one key must remain active
- **Invalid key IDs** - Returns error for non-existent keys

## Key Naming Conventions

### Date-Based Keys (Default)

Format: `ts-YYYY-MM-DD`

Examples:

- `ts-2024-01-15`
- `ts-2024-02-15`
- `ts-2024-12-31`

**Advantages**:

- Easy to identify key age
- Automatic chronological sorting
- Clear rotation history

### Custom Key IDs

Use descriptive names for specific purposes:

- `production-2024-q1` - Quarterly rotation
- `staging-dev` - Development environment
- `emergency-2024-01` - Emergency rotation
- `service-a-v1` - Service-specific keys

**Advantages**:

- Meaningful identifiers
- Environment separation
- Service isolation

## Rotation Strategies

### Strategy 1: Scheduled Rotation

Regular rotation on a fixed schedule:

```bash
# Cron job: Rotate every 90 days
0 0 1 */3 * /usr/local/bin/rotate-keys.sh
```

**rotate-keys.sh**:

```bash
#!/bin/bash
# Rotate signing keys
curl -X POST https://your-domain/admin/keys/rotate

# Wait 30 days grace period
sleep $((30 * 24 * 60 * 60))

# Deactivate old key
OLD_KEY=$(date -d '90 days ago' +ts-%Y-%m-%d)
curl -X POST https://your-domain/admin/keys/deactivate \
  -d "{\"kid\": \"$OLD_KEY\", \"delete\": true}"
```

### Strategy 2: On-Demand Rotation

Manual rotation when needed:

1. Generate new key
2. Monitor adoption in logs
3. Deactivate when safe
4. Delete after retention period

### Strategy 3: Blue-Green Rotation

Immediate switchover with rollback capability:

1. **Rotate** to new key (both active)
2. **Monitor** for issues
3. **Rollback** if needed (keep old as current)
4. **Commit** if successful (deactivate old)

## Monitoring Key Usage

### Track Current Key

```rust
use trusted_server_common::request_signing::get_current_key_id;

let current_kid = get_current_key_id()?;
println!("Current signing key: {}", current_kid);
```

### Audit Key Usage

Log which keys are used for signing:

```rust
let signer = RequestSigner::from_config()?;
log::info!("Signing request with key: {}", signer.kid);
```

Log which keys are used for verification:

```rust
log::info!("Verifying signature with key: {}", kid);
let verified = verify_signature(payload, signature, kid)?;
```

### Metrics to Track

- **Keys per environment**: Active key count
- **Signature failures**: Failed verification attempts
- **Key age**: Time since last rotation
- **Verification latency**: Performance impact

## Best Practices

### 1. Grace Period

Always maintain a grace period:

- **Minimum**: 7 days
- **Recommended**: 30 days
- **Conservative**: 90 days

This allows:

- Partner systems to update cached keys
- In-flight requests to complete
- Troubleshooting signature issues

### 2. Communication

Before rotation, notify partners:

- Send advance notice (7-14 days)
- Publish new key in JWKS endpoint
- Document rotation schedule

### 3. Rollback Plan

Always have a rollback strategy:

- Keep previous key active initially
- Test new key before deactivating old key
- Document reactivation procedure

### 4. Documentation

Document your rotation:

- Record rotation dates
- Track key identifiers
- Note any issues or rollbacks
- Update runbooks

### 5. Testing

Test rotation in staging first:

- Verify new key generation
- Test signature verification
- Validate JWKS endpoint
- Check partner integrations

## Troubleshooting

### Rotation Failed

**Error**: `Failed to create KeyRotationManager`

**Solutions**:

- Verify all required stores are created (see [Prerequisites](#prerequisites))
- Check Fastly API token is stored in `api-keys` secret store as `api_key`
- Verify `config_store_id` and `secret_store_id` in `trusted-server.toml` match your actual store IDs
- Ensure stores are linked to your Compute service
- Confirm API token has `global:read` and `global:write` permissions

### Cannot Deactivate Key

**Error**: `Cannot deactivate the last active key`

**Solutions**:

- Rotate to generate a new key first
- Verify multiple keys are active
- Check active-kids list

### Signature Verification Fails After Rotation

**Symptoms**:

- Old signatures fail to verify
- `Key not found` errors

**Solutions**:

- Verify old key is still in active-kids
- Check JWKS endpoint includes old key
- Wait for partner caches to update

### Key Not in JWKS

**Symptoms**:

- New key missing from `.well-known/trusted-server.json`

**Solutions**:

- Check active-kids includes new key
- Verify JWK stored in Config Store
- Check Config Store cache expiration

## Security Considerations

### Key Compromise Response

If a key is compromised:

1. **Immediate**: Rotate to new key

```bash
curl -X POST /admin/keys/rotate
```

2. **Urgent**: Deactivate compromised key

```bash
curl -X POST /admin/keys/deactivate \
  -d '{"kid": "compromised-key", "delete": false}'
```

3. **Investigation**: Review logs for misuse

4. **Communication**: Notify partners of compromise

5. **Cleanup**: Delete compromised key after investigation

```bash
curl -X POST /admin/keys/deactivate \
  -d '{"kid": "compromised-key", "delete": true}'
```

### Access Control

Restrict rotation endpoints:

- Require authentication/authorization
- Use admin-only API keys
- Implement rate limiting
- Audit all rotation attempts

## Next Steps

- Complete the [Prerequisites](#prerequisites) setup if you haven't already
- Learn about [Request Signing](/guide/request-signing) for using keys
- Review [Configuration](/guide/configuration) for additional store setup
- Set up [Testing](/guide/testing) for rotation procedures
- Read about [GDPR Compliance](/guide/gdpr-compliance) for privacy considerations

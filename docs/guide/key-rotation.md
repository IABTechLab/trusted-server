# Key Rotation

Learn how to rotate signing keys to maintain security and manage the lifecycle of cryptographic keys in Trusted Server.

## Overview

Key rotation is the process of generating new signing keys and transitioning from old keys to new ones. The key rotation library in core, `KeyRotationManager` in `crates/trusted-server-core/src/request_signing/rotation.rs`, provides:

- **Zero-downtime rotation** - Old and new keys work simultaneously
- **Automatic key generation** - Date-based key identifiers
- **Grace period support** - Multiple active keys during transition
- **Safe deactivation** - Prevents removing the last active key

::: tip Run by an operator
Keys are rotated and retired with [`ts keys`](./cli.md#request-signing-keys), run by an operator holding a Fastly API token. Nothing in a running deployment rotates or retires keys, and there is no HTTP route for it.
:::

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

## Edge Cookie HMAC Passphrase

The Edge Cookie `ec.hmac.passphrase` is long-lived HMAC-SHA256 keying material used to derive visitor EC IDs. Use a high-entropy random value of at least 32 characters, because shorter values are rejected at settings validation. Rotating this passphrase changes derived EC IDs and requires rebuilding or allowing expiry of the existing EC identity graph.

## Prerequisites

Request signing needs two Fastly stores set up and linked to the service.

### Required Stores

Request signing reads two Fastly stores, and key rotation writes them:

1. **Config Store** (`jwks_store`) - Stores public JWKs and metadata
   - `current-kid` - The active key identifier
   - `active-kids` - Comma-separated list of valid key IDs
   - Individual JWKs keyed by their `kid`

2. **Secret Store** (`signing_keys`) - Stores private signing keys
   - Each key stored with its `kid` as the key name
   - Values are base64-encoded Ed25519 private keys

The running service only reads these stores, so it needs no Fastly API token of its own.

### Creating Stores

#### 1. Create Config Store

```bash
# Create the config store
fastly config-store create --name=jwks_store

# Get the store ID (you'll need this for `ts keys`)
fastly config-store list
```

Note the Config Store ID from the output.

#### 2. Create Secret Store

```bash
# Create secret store for signing keys
fastly secret-store create --name=signing_keys

# Get the store ID
fastly secret-store list
```

Note the Secret Store ID from the output.

::: tip Dashboard Alternative
You can also create stores via the Fastly dashboard, but CLI commands are recommended for automation and reproducibility.
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
```

### Configuration in trusted-server.toml

Switch request signing on in `trusted-server.toml`:

```toml
[request_signing]
enabled = true
```

The service finds the two stores by the names they are linked under, so the
configuration holds no store id.

::: tip Getting Store IDs
`ts keys` takes the two store IDs on its command line. Use `fastly config-store list` and `fastly secret-store list` to retrieve them.
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
│  4. Update active-kids list          │
│     ↓                                │
│  5. Update current-kid pointer       │
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

Rotate with `ts keys rotate`, with a Fastly API token in `FASTLY_API_TOKEN`:

```bash
ts keys rotate --config-store-id <jwks-store-id> --secret-store-id <signing-keys-store-id>
```

It writes the private key to the secret store, the public JWK to the config
store, then `active-kids`, then `current-kid` last, and prints the new and
previous key ids, the active keys and the public JWK as JSON. `--kid` names the
key, which otherwise is `ts-<date>`, with a random suffix when a key of that
date exists. A command that cannot read the config store writes nothing.

The command runs the rotation library in core, which another tool can call
directly.

### Using the Rust API

```rust
use trusted_server_core::request_signing::KeyRotationManager;

// The IDs of the two stores. The stores are written through the platform's
// `RuntimeServices`.
let manager = KeyRotationManager::new("<config-store-id>", "<secret-store-id>");

// Rotate with automatic kid
let result = manager.rotate_key(&services, None)?;

println!("New key: {}", result.new_kid);
println!("Previous key: {:?}", result.previous_kid);
println!("Active keys: {:?}", result.active_kids);

// Or rotate with custom kid. `rotate_key` does not check the kid, so check it
// with `kid_is_creatable` first.
let custom_result = manager.rotate_key(&services, Some("my-custom-key".to_string()))?;
```

## Managing Active Keys

### Listing Active Keys

**Rust API**:

```rust
let manager = KeyRotationManager::new("<config-store-id>", "<secret-store-id>");
let active_keys = manager.list_active_keys(&services)?;

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

Deactivate with `ts keys deactivate`, and add `--delete` to remove the key
from both stores as well:

```bash
ts keys deactivate --config-store-id <jwks-store-id> \
  --secret-store-id <signing-keys-store-id> --kid ts-2024-01-15 --delete
```

A delete that fails part way can be run again, because a key that is already
gone from a store counts as deleted there. The library calls the command makes
are below.

### Using the Rust API

```rust
let manager = KeyRotationManager::new("<config-store-id>", "<secret-store-id>");

// Deactivate (keep in storage)
manager.deactivate_key(&services, "ts-2024-01-15")?;

// Delete completely
manager.delete_key(&services, "ts-2024-01-15")?;
```

### Safety Checks

The library refuses to:

- **Deactivate the last active key** - At least one key must remain active
- **Deactivate or delete the current key** - Rotate first, then retire the old key

`ts keys deactivate` also refuses an ID that is not 1 to 128 letters, digits,
`-`, `_`, `.` and `:`, and the names `current-kid` and `active-kids`, before it
reads or writes anything. An ID that no store holds is not an error, because a
key that is gone counts as retired, so read `remaining_active_kids` in the
report to see what is left.

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

Regular rotation on a fixed schedule, for example every 90 days with a 30 day
grace period before the old key is deleted. A scheduler runs `ts keys rotate`,
and `ts keys deactivate --delete` once the grace period has passed.

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
use trusted_server_core::request_signing::get_current_key_id;

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
2. **Urgent**: Deactivate compromised key
3. **Investigation**: Review logs for misuse
4. **Communication**: Notify partners of compromise
5. **Cleanup**: Delete compromised key after investigation

Rotate with `ts keys rotate`, then retire the compromised key with
`ts keys deactivate --delete`, as [Rotating Keys](#rotating-keys) describes.

### Access Control

There is no rotation endpoint on the publisher's domain. Rotation writes the
signing stores, so only an operator holding platform credentials with write
access to those stores can rotate or retire a key.

## Next Steps

- Complete the [Prerequisites](#prerequisites) setup if you haven't already
- Learn about [Request Signing](/guide/request-signing) for using keys
- Review [Configuration](/guide/configuration) for additional store setup
- Set up [Testing](/guide/testing) for rotation procedures
- Read about [GDPR Compliance](/guide/gdpr-compliance) for consent signal handling

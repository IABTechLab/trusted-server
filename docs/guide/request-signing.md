# Request Signing

Learn how to implement cryptographic request signing using Ed25519 keys for secure communication with Trusted Server.

## Overview

Trusted Server provides Ed25519-based request signing capabilities to ensure authenticity and integrity of HTTP requests. This feature uses:

- **Ed25519 signatures** - Modern, fast, and secure elliptic curve cryptography
- **JWKS (JSON Web Key Set)** - Standard format for public key distribution
- **Fastly storage** - Private keys in Secret Store, public keys in Config Store
- **Automatic key rotation** - Date-based key identifiers and rotation support

## Architecture

### Key Storage

Keys are stored in two separate Fastly stores:

**Secret Store** (`signing_keys`):

- Private signing keys (Ed25519 private keys)
- Base64-encoded 32-byte keys
- Only accessible to the edge application

**Config Store** (`jwks_store`):

- Public verification keys (JWK format)
- Current key identifier (`current-kid`)
- Active key identifiers (`active-kids`)
- Public JWKS documents

### Key Components

```
┌─────────────────────┐
│  RequestSigner      │  ← Signs outgoing requests
│  - Loads private key│
│  - Signs payload    │
└─────────────────────┘

┌─────────────────────┐
│  verify_signature() │  ← Verifies incoming requests
│  - Loads public key │
│  - Validates sig    │
└─────────────────────┘

┌─────────────────────┐
│  JWKS Endpoint      │  ← Publishes public keys
│  - Discovery doc    │
│  - Active keys only │
└─────────────────────┘
```

## Signing Requests

### Basic Usage

```rust
use trusted_server_common::request_signing::RequestSigner;

// Initialize signer (loads current key from config)
let signer = RequestSigner::from_config()?;

// Sign a payload
let payload = "request data to sign";
let signature = signer.sign(payload)?;

// Signature is base64url-encoded
println!("Signature: {}", signature);
println!("Key ID: {}", signer.kid);
```

### Signing HTTP Requests

Include the signature and key ID in request headers:

```rust
// Sign the request body
let body = serde_json::to_string(&request_data)?;
let signature = signer.sign(body.as_bytes())?;

// Add headers
request.set_header("X-Signature", signature);
request.set_header("X-Signature-Kid", signer.kid);
```

### Recommended Headers

- `X-Signature` - Base64url-encoded signature
- `X-Signature-Kid` - Key identifier used for signing
- `X-Signature-Timestamp` - Unix timestamp (recommended for replay protection)

## Verifying Signatures

### Basic Verification

```rust
use trusted_server_common::request_signing::verify_signature;

// Extract signature and kid from request
let signature = request.get_header("X-Signature")?;
let kid = request.get_header("X-Signature-Kid")?;

// Verify the signature
let payload = request.get_body_bytes();
let is_valid = verify_signature(payload, signature, kid)?;

if !is_valid {
    return Err("Invalid signature");
}
```

### Verification Endpoint

Trusted Server provides a built-in endpoint for testing signatures:

**Endpoint**: `POST /admin/verify-signature`

**Request**:

```json
{
  "payload": "message to verify",
  "signature": "base64url-encoded-signature",
  "kid": "ts-2024-01-15"
}
```

**Response**:

```json
{
  "verified": true,
  "kid": "ts-2024-01-15",
  "message": "Signature verified successfully",
  "error": null
}
```

## Discovery Endpoint

Public keys are published via a standardized discovery endpoint following IAB patterns.

### Accessing the Discovery Document

**Endpoint**: `GET /.well-known/trusted-server.json`

**Response**:

```json
{
  "version": "1.0",
  "jwks": {
    "keys": [
      {
        "kty": "OKP",
        "crv": "Ed25519",
        "x": "base64url-encoded-public-key",
        "kid": "ts-2024-01-15",
        "alg": "EdDSA"
      }
    ]
  }
}
```

### Using Discovery for Verification

Partners can fetch and cache your public keys:

```javascript
// Fetch discovery document
const discovery = await fetch(
  'https://your-domain/.well-known/trusted-server.json'
).then((r) => r.json())

// Extract JWKS
const jwks = discovery.jwks

// Verify signatures using JWKS
const publicKey = jwks.keys.find((k) => k.kid === signatureKid)
```

## Key Format

### Ed25519 Keys

**Private Key**:

- 32 bytes
- Stored base64-encoded in Secret Store
- Never exposed via API

**Public Key**:

- 32 bytes
- Stored as JWK in Config Store
- Published in JWKS endpoint

### JWK Format

```json
{
  "kty": "OKP", // Key type: Octet Key Pair
  "crv": "Ed25519", // Curve: Ed25519
  "x": "public_key_b64", // Public key (base64url)
  "kid": "ts-2024-01-15", // Key identifier
  "alg": "EdDSA" // Algorithm: EdDSA
}
```

## Configuration

Configure request signing in `trusted-server.toml`:

```toml
[request_signing]
config_store_id = "jwks_store"
secret_store_id = "signing_keys"
```

### Fastly Config Store Setup

Create the Config Store in Fastly:

```bash
# Create config store
fastly config-store create --name=jwks_store

# Create secret store
fastly secret-store create --name=signing_keys
```

Link stores to your service in `fastly.toml`:

```toml
[local_server.config_stores]
  [local_server.config_stores.jwks_store]
    file = "test-data/jwks_store.json"

[local_server.secret_stores]
  [local_server.secret_stores.signing_keys]
    file = "test-data/signing_keys.json"
```

## Security Best Practices

### 1. Protect Private Keys

- Store private keys only in Fastly Secret Store
- Never log or expose private keys
- Rotate keys regularly (see [Key Rotation](/guide/key-rotation))

### 2. Validate Signatures

Always verify:

- Signature authenticity
- Key ID exists in active keys
- Timestamp freshness (if using timestamps)

### 3. Key Lifecycle

- Generate new keys periodically
- Keep previous key active during transition
- Deactivate old keys after grace period
- Delete deprecated keys

### 4. Transport Security

- Always use HTTPS
- Consider additional authentication (API keys, mutual TLS)
- Implement rate limiting on verification endpoints

## Error Handling

Common errors and solutions:

**Invalid key length**:

```
Error: Invalid key length (expected 32 bytes for Ed25519)
```

- Ensure key is exactly 32 bytes
- Check base64 encoding is correct

**Missing key**:

```
Error: Key not found: ts-2024-01-15
```

- Verify kid exists in Config Store
- Check active-kids list includes the kid

**Signature verification failed**:

```
verified: false
```

- Ensure payload matches exactly (no modifications)
- Verify correct kid is used
- Check signature encoding (base64url vs standard)

## Testing

### Unit Tests

Test signing and verification:

```rust
#[test]
fn test_sign_and_verify() {
    let payload = b"test message";
    let signer = RequestSigner::from_config().unwrap();
    let signature = signer.sign(payload).unwrap();

    // Verify the signature
    let verified = verify_signature(payload, &signature, &signer.kid).unwrap();
    assert!(verified);
}
```

### Integration Testing

Use the verification endpoint:

```bash
# Sign a payload
SIGNATURE=$(sign-payload "test message")

# Verify via API
curl -X POST https://your-domain/admin/verify-signature \
  -H "Content-Type: application/json" \
  -d '{
    "payload": "test message",
    "signature": "'$SIGNATURE'",
    "kid": "ts-2024-01-15"
  }'
```

## Next Steps

- Learn about [Key Rotation](/guide/key-rotation) for managing key lifecycle
- Review [Architecture](/guide/architecture) for system design
- Configure [Testing](/guide/testing) for your deployment

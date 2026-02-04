# Proxy Signing

This page covers the implementation details of how Trusted Server signs and validates
proxy and click URLs. For usage and endpoints, see [First-Party Proxy](/guide/first-party-proxy).

## Signature Generation

Signatures use HMAC-SHA256 with the publisher's `proxy_secret`:

```
1. Reconstruct full URL: tsurl + query params (sorted)
2. Encrypt with XChaCha20-Poly1305 (deterministic nonce)
3. Hash encrypted bytes with SHA-256
4. Base64 URL-safe encode (no padding)
5. Result = tstoken
```

**Configuration**:

```toml
[publisher]
proxy_secret = "your-secret-key-here"  # Must be secure random string
```

## Signature Validation

On incoming requests:

```
1. Extract tsurl and all query params (except tstoken, tsexp)
2. Reconstruct full URL in same order
3. Compute expected tstoken using proxy_secret
4. Compare with provided tstoken (constant-time)
5. Check tsexp has not passed (if present)
6. Reject if mismatch or expired
```

## Security Notes

- Keep `proxy_secret` confidential and secure
- Rotate secrets periodically
- Never expose the secret in client-side code
- Use strong random values (32+ bytes)

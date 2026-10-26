# Proxy Signing

This page covers the implementation details of how Trusted Server signs and validates
proxy and click URLs. For usage and endpoints, see [First-Party Proxy](/guide/first-party-proxy).

## Signature Generation

Signatures use HMAC-SHA256 with the publisher's implicit `publisher.proxy` credential:

```
1. Reconstruct full URL: tsurl + query params (sorted)
2. Encrypt with XChaCha20-Poly1305 (deterministic nonce)
3. Hash encrypted bytes with SHA-256
4. Base64 URL-safe encode (no padding)
5. Result = tstoken
```

**Provisioning**:

Provision the fixed `publisher.proxy` role from a [reviewed plan](./secrets).
No `proxy_secret` property belongs in public TOML. Use a confidential,
cryptographically random value, with at least 32 random bytes recommended.
Replacing it invalidates existing proxy URLs and requires state recreation.
This is separate from request-signing `signing_keys`/JWKS rotation.

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

- Keep the resolved signing value confidential
- Rotate the stored value periodically
- Never expose the resolved value in client-side code
- Use a strong random value of at least 32 characters

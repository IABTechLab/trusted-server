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
proxy_secret = "publisher_proxy_secret"
```

The config value is a key in the Trusted Server secret store. Provision the
secure random signing value under `publisher_proxy_secret`; the resolved value
must contain at least 32 characters.

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

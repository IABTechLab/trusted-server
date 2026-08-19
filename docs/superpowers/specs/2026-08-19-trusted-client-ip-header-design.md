# Trusted Client IP Header Design

## Summary

Trusted Server's Fastly adapter currently treats
`fastly::Request::get_client_ip_addr()` as the reader address. On a direct
request that is correct, but on a request chained through another Fastly
service it is the fronting edge node. Geo lookup, EC generation, cluster
classification, consent jurisdiction, auction device IPs, and downstream
integration forwarding then all consume the edge-node address.

Fastly preserves the original address in `Fastly-Client-IP`, but the header is
caller-controlled at the public edge unless the fronting service overwrites it.
Trusted Server must therefore not trust that header based on presence alone.

This change adds an optional authenticated client-IP header. Existing
deployments remain unchanged until an operator explicitly configures both the
forwarded-IP and authentication headers with a shared secret.

Related issues: #1040 and #1041.

## Goals

- Allow a CDN-fronted Fastly deployment to supply the reader's IP address.
- Establish an explicit trust boundary before using a forwarded address.
- Use one resolved address consistently for geo and all `ClientInfo` consumers.
- Preserve current peer-IP behavior when the feature is absent or authentication
  fails.
- Remove trust headers before routing so they cannot leak downstream or be
  interpreted by unrelated code.

## Non-goals

- Automatically infer whether a request came from another Fastly service.
- Trust `Fastly-Client-IP`, `Fastly-FF`, or `X-Forwarded-For` by presence.
- Change client-IP handling in Cloudflare, Spin, or Axum adapters.
- Add rotating or time-limited request signatures in this change.
- Make Fastly no-code request routing preserve a value it does not expose.

## Configuration Contract

Add an optional top-level section:

```toml
[trusted_client_ip]
ip_header = "fastly-client-ip"
auth_header = "x-ts-client-ip-auth"
shared_secret = "replace-with-a-random-shared-secret"
```

All three fields are required when the section is present. `shared_secret` uses
the existing `Redacted<String>` type so debug representations do not disclose
it. Configuration validation rejects invalid header names, identical header
names, and secrets shorter than the 32-character minimum already applied to
`ec.passphrase`. The shared `reject_placeholder_secrets` startup gate also
rejects the placeholder secret published in the example configuration and
guides, so a copied config cannot ship a publicly known secret.

To ensure request entry can remove the fields without deleting a required HTTP
field, `ip_header` must either be `fastly-client-ip` or begin with `x-`, and
`auth_header` must begin with `x-`. Comparison is case-insensitive after parsing
through `http::HeaderName`. The two Fastly-injected TLS bridge fields
(`x-ts-tls-protocol` and `x-ts-tls-cipher`) are forbidden for either setting
because the entry point owns and re-injects them after sanitization. These rules
exclude framing, routing, representation, cookie, and authorization fields such
as `host`, `content-length`, `accept`, `cookie`, and `authorization`. A fronting
CDN whose native client-IP field does not meet this contract must copy it into a
dedicated `x-` field before forwarding.

The section is absent by default. Because the runtime configuration uses strict
unknown-key validation, the example and configuration reference must document
the exact field names.

## Request Processing

The Fastly entry point resolves the client address after application state has
loaded but before spoofable headers are sanitized:

1. Capture `req.get_client_ip_addr()` as the fallback peer address.
2. If `trusted_client_ip` is absent, select the peer address.
3. If configured, require exactly one authentication-header field value. It
   must be valid UTF-8 and match the configured secret byte-for-byte, without
   trimming or other normalization. Compare fixed-size SHA-256 digests using a
   constant-time comparison. A missing, duplicated, non-UTF-8, empty, or
   mismatched authentication value fails authentication.
4. Only after authentication succeeds, require exactly one IP-header field
   value and parse it directly as `std::net::IpAddr`. Do not trim or normalize
   the value. This accepts canonical or otherwise Rust-supported IPv4 and IPv6
   spellings but rejects whitespace, ports, IPv6 zone identifiers,
   comma-separated lists, empty values, non-UTF-8 bytes, and duplicate fields.
5. Select the parsed forwarded address on success. Missing headers, a wrong
   secret, a malformed header value, or a non-IP value all select the peer
   address without rejecting the request.
6. Remove the configured IP and authentication headers, then run the existing
   forwarded-header sanitizer.
7. Pass the selected address into `client_info_from_request` and use the same
   value for entry-point geo response finalization.

`Fastly-Client-IP` is added to the static spoofable-header list. This ensures it
is stripped even when the feature is not configured. A configured header is
read before sanitization and removed explicitly, so configuring
`fastly-client-ip` remains valid.

`X-Forwarded-For` joins the same list. The shared proxy code forwards an inbound
value to publisher origins, so leaving it would let a client choose the address
the origin attributes the request to while Trusted Server itself used the
authenticated one. The Spin adapter already stripped it for this reason; moving
the rule into the shared list closes the equivalent gap on Fastly without
changing Spin behaviour. Integrations that need the address continue to send
their own `X-Forwarded-For` derived from the resolved client IP.

Authentication failures do not log supplied secrets or IP values. A debug-level
message may record only the reason category (missing authentication, mismatch,
or invalid IP) and that the peer fallback was used.

## Component Boundaries

### Core settings

`trusted-server-core/src/settings.rs` owns the serializable
`TrustedClientIpConfig`, validation, redaction, and constant-time secret
verification. Keeping the security rule with the configuration type prevents
adapter call sites from comparing variable-length secrets directly.

### Fastly client-IP resolution

`trusted-server-adapter-fastly/src/platform.rs` owns a small resolver that reads
Fastly request headers and returns either the authenticated forwarded IP or the
SDK peer IP. `client_info_from_request` accepts the already-resolved address so
it cannot accidentally re-read the immediate peer.

### Fastly entry point and sanitization

`trusted-server-adapter-fastly/src/main.rs` invokes the resolver once and shares
its result with request services and response geo finalization.
`trusted-server-adapter-fastly/src/compat.rs` removes the configured dynamic
headers and continues applying the static spoofable-header list.

No core EC, consent, auction, or integration logic changes: those consumers
already use `RuntimeServices::client_info().client_ip` correctly.

## Security Model

The fronting CDN must overwrite both configured headers on every request sent to
Trusted Server. It must never preserve caller-provided values. The shared secret
must be generated randomly, stored in both the fronting service and Trusted
Server configuration, and excluded from responses and origin requests.

This design protects against callers that can reach the Trusted Server hostname
and inject an arbitrary IP header, provided they do not know the shared secret.
It does not provide replay protection: any party that learns the static secret
can authenticate arbitrary IP values. A timestamped HMAC would address replay
and secret reuse but is outside #1041's requested scope.

Direct requests remain supported. Without valid trust headers they use the
direct peer address, which is the reader address on a one-hop request.

## Testing

Tests follow red-green-refactor and cover:

- absent configuration uses the peer IP;
- valid authentication plus an IPv4 header selects the forwarded IP;
- valid authentication plus an IPv6 header selects the forwarded IP;
- missing, empty, incorrect, non-UTF-8, or duplicate authentication falls back
  to the peer IP;
- whitespace-padded, port-bearing, zone-qualified, comma-separated, non-UTF-8,
  empty, or duplicate IP input falls back to the peer IP;
- configured headers are removed after resolution;
- `Fastly-Client-IP` is stripped when configuration is absent;
- settings parse, validation, secret redaction, and default behavior;
- `client_info_from_request` and entry-point geo finalization receive the same
  selected address.

Targeted Fastly and core tests run after each change. Final verification uses
the repository's Fastly/core test alias, formatting, and target-matched Clippy.
The existing local Viceroy certificate-keychain failure may require running the
Fastly integration suite in an environment with native certificates available;
native unit tests and Wasm compilation still provide local evidence.

## Documentation

- Add a commented `[trusted_client_ip]` example to
  `trusted-server.example.toml` using only `example.com`-safe material.
- Add the new section to `docs/guide/configuration.md`.
- Add Fastly front-door setup guidance to `docs/guide/fastly.md`, emphasizing
  that both headers must be overwritten and that enabling the reader without a
  correctly configured front door creates a spoofing vulnerability.
- Document the no-code request-routing limitation from #1041.

## Acceptance Criteria

- Configuration absent: runtime behavior remains peer-IP based and
  `Fastly-Client-IP` is stripped.
- Valid configured authentication: geo and every `ClientInfo` consumer use the
  forwarded reader IP.
- Missing, wrong, or malformed authentication: the request succeeds using the
  peer IP.
- Invalid forwarded IP: the request succeeds using the peer IP.
- Trust headers never reach routing or downstream origins.
- Existing direct Fastly deployments require no configuration migration.

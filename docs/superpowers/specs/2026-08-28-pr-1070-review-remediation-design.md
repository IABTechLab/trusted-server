# PR 1070 Review Remediation Design

## Goal

Address every review finding on PR 1070 while preserving its intended behavior: a request carrying exactly the Basic credential validated by Trusted Server may use the shared ESI template cache, while pass-through, repeated, or subsequently replaced authorization values must bypass it.

## Authorization marker

`EdgeTerminatedAuthorization` will store a SHA-256 digest of the exact raw `Authorization` header value accepted by `enforce_basic_auth`. The raw credential will not be duplicated in request extensions. The marker will expose a crate-private predicate that returns true only when the request still has exactly one authorization value and its digest matches the validated value.

The publisher cache gate will use that predicate. No authorization value is eligible, one matching marked value is eligible, and every other case disqualifies template sharing. This keeps the safety check next to the cache decision and remains correct if DataDome or another later request filter replaces or appends `Authorization`.

## Configuration invariant

`CreativeOpportunitiesConfig::validate_runtime` will reject `Authorization` in `template_cache_vary`, case-insensitively, alongside the existing `Cookie` rejection. This ensures an origin response declaring `Vary: Authorization` is refused instead of incorporating credential bytes into shared-template key material.

## Cleanup

The cache-gate boolean and helper parameters will be renamed from `request_had_authorization` to `authorization_disqualifies`. The marker documentation and publisher comment will be shortened, the unnecessary handler scoping block will be removed, and the repeated mutable-request comments will be removed from the Fastly, Axum, Cloudflare, and Spin middleware copies.

## Documentation

The configuration guide will distinguish pass-through and repeated authorization values from one unchanged, edge-validated Basic credential. It will state that Trusted Server forwards the credential, that an origin depending on it must return `Vary: Authorization` (which prevents template storage), and that whole-site staging gates should use an alias-proof raw-path expression such as `^/` rather than a decoded-path assumption.

## Tests

Tests will be added or updated for:

- digest insertion after successful Basic authentication;
- cache bypass after the validated authorization value is replaced;
- repeated and pass-through authorization values;
- case-insensitive rejection of `Authorization` in `template_cache_vary`;
- a Fastly adapter dispatch path that performs Basic auth, crosses the router boundary, stores a cold ESI template, and hits it on the warm request.

The adapter seam test will use test-local platform implementations and a router composed with the real Fastly `AuthMiddleware`, then invoke the real publisher handler. It will not add production dependency-injection fields solely for testing.

## Verification and review resolution

Focused tests will run after each behavior change. Final verification will include repository formatting, Fastly/Axum/Cloudflare/Spin adapter tests, and the corresponding target-specific clippy aliases. After the fixes are committed and pushed, each inline GitHub thread will receive a concise technical reply and be resolved.

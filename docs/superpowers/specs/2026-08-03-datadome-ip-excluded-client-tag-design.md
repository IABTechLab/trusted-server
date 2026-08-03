# DataDome IP-excluded client tag suppression

**Issue:** #994
**Date:** 2026-08-03
**Status:** Proposed

## Problem

Trusted Server has two DataDome protection layers:

1. Server-side Protection API validation, which can be skipped for configured
   client IP CIDRs.
2. Client-side tag auto-injection, which adds `window.ddjskey`,
   `window.ddoptions`, and the configured `tags.js` script to processed HTML.

When a request matches an IP-based server-side exclusion, the Protection API is
skipped, but the client-side tag is currently still injected. The browser can
therefore continue running client-side DataDome protection for a request that
was explicitly whitelisted at Trusted Server.

The desired behavior is that Fastly requests skipped by an IP-based DataDome
exclusion also omit Trusted Server's automatically injected client-side tag.

## Goals

- Suppress Trusted Server's automatically injected DataDome client-side tag for
  Fastly requests skipped by an IP-based protection exclusion.
- Reuse the existing authoritative protection-scope decision.
- Cover all supported IP-based exclusion mechanisms:
  - `protection_excluded_ip_cidrs`
  - `protection_excluded_ip_cidr_sources`
  - structured `ip_cidr` rules
  - structured `ip_cidr_source` rules
- Preserve current behavior for non-IP exclusions.
- Leave publisher-originated or manually configured DataDome tags untouched.
- Add an informational diagnostic indicating that the client tag is omitted.
- Keep the implementation independent of caller-supplied IP headers.
- Prevent a shared cache from replaying IP-specific, tag-suppressed HTML to
  non-excluded visitors.

## Non-goals

- Do not add a configuration flag or make this behavior opt-in.
- Do not change Axum, Cloudflare, or Spin request-filter wiring. This behavior
  is intentionally scoped to the Fastly adapter, where the DataDome server-side
  request filter is currently run.
- Do not suppress DataDome tags that originate in publisher HTML.
- Do not remove or disable the `/integrations/datadome/tags.js` route.
- Do not change DataDome signal-collection proxy behavior.
- Do not change ASN, path, query-parameter, method, static-asset, or internal
  route exclusions.
- Do not perform live production verification as part of implementation.

## Confirmed decisions

1. **Adapter scope:** Fastly only.
2. **IP scope:** all four IP-based exclusion mechanisms listed above.
3. **Tag scope:** Trusted Server's auto-injected tag only.
4. **HTML scope:** every HTML response that enters the existing HTML processing
   pipeline.
5. **Logging:** enrich the existing IP-exclusion skip log with
   `client_tag=omitted`, including the matched rule and reason.
6. **Live testing:** deferred until after implementation and deployment/testing
   workflow review.

## Current architecture

### Server-side protection

`DataDomeIntegration::is_request_protected()` in
`crates/trusted-server-core/src/integrations/datadome/protection.rs` evaluates
method, internal-route, ASN, IP, and structured exclusion conditions. It uses
the client IP from `RuntimeServices::client_info()`, which is populated from
trusted Fastly request metadata. It does not use a caller-supplied IP header.

The current function reduces the protection-scope result to a boolean. For an
IP exclusion it logs the skip and returns `false`, causing the request filter to
continue without calling the Protection API.

The Fastly EdgeZero fallback path runs this request filter before route
selection and publisher proxying. The request continues into
`handle_publisher_request()` after the filter returns a continue decision.

### Client-side injection

`DataDomeIntegration::head_inserts()` in
`crates/trusted-server-core/src/integrations/datadome.rs` emits the client-side
snippet when:

- `inject_client_side_tag` is true; and
- `client_side_key` is non-empty.

The injector currently receives `IntegrationHtmlContext`, which contains HTML
host/scheme and document state but no request IP or protection decision.

The publisher response path carries request-specific values through:

```text
Request
  -> OwnedProcessResponseParams
  -> HtmlStreamProcessorParams
  -> HtmlProcessorConfig
  -> IntegrationHtmlContext
  -> IntegrationHeadInjector
```

The existing DataDome attribute rewriter separately rewrites DataDome URLs
found in publisher HTML. That behavior must remain unchanged.

## Design

### 1. Capture an IP-exclusion marker at the request filter

The request filter must attach a typed, internal request-scoped marker when the
existing protection-scope evaluation returns a skip for one of these reasons:

- `client_ip`
- `client_ip_source`
- `ip_cidr`
- `ip_cidr_source`

The marker must be attached only after the existing scope decision confirms the
IP exclusion. It must not be inferred from request headers or recomputed later
in the HTML pipeline.

The request-filter API currently exposes an immutable request view. Add the
smallest internal mechanism needed for a filter to attach a typed request
extension without introducing a caller-visible header. Header mutations should
continue to use `RequestFilterEffects` as they do today.

The marker should be a zero-sized or otherwise minimal internal type. It only
needs to answer whether Trusted Server's DataDome client tag should be
suppressed; the existing skip log supplies the rule ID and reason.

The marker must not be attached for:

- `OPTIONS` or other excluded methods before scope evaluation;
- internal or integration routes;
- ASN exclusions;
- path, query, or other non-IP structured exclusions;
- unmatched IP rules;
- Protection API fail-open behavior; or
- requests where `enable_protection` is false and the request filter does not
  run.

### 2. Enrich the existing skip log

For IP-based skips, extend the existing informational log with
`client_tag=omitted`:

```text
[datadome] protection decision=skipped rule=excluded-ip-cidrs reason=client_ip client_tag=omitted method=GET host=example.com path=/page
```

The existing rule ID, reason, and request metadata remain part of the log.
Client IP values are not included. Non-IP skip logs retain their current
behavior and level.

This log represents the request policy decision. It may also apply to a
non-HTML response, for which no HTML tag would have been injected anyway.

### 3. Propagate the marker into HTML processing

Before the publisher request is moved into the platform HTTP client, snapshot
whether the request carries the marker. Carry that request-scoped boolean
through `OwnedProcessResponseParams`, `HtmlStreamProcessorParams`, and
`HtmlProcessorConfig`.

The value should default to `false` in all existing constructors and direct
unit-test fixtures. Non-Fastly adapters will naturally retain the default
because they do not currently produce the Fastly request-filter marker.

Expose the value to head injectors through the existing HTML processing context
or equivalent request-scoped integration context. The propagation must work for
both:

- the normal buffered HTML path; and
- the streaming HTML path, including the auction-hold path.

The value is irrelevant for non-HTML, RSC, pass-through, and unmodified
responses, which should retain their current processing.

### 4. Keep IP-specific HTML out of shared cache

A processed HTML response differs by client IP when the generated tag is
suppressed. In the `PublisherResponse::Stream` path, when suppression is active
and the response is HTML, set `Cache-Control: private, max-age=0` and remove
`Surrogate-Control` and `Fastly-Surrogate-Control` before the body is streamed.

This matches the existing per-user ad-stack cache policy. It prevents Fastly or
another shared cache from replaying a tag-suppressed response to a visitor whose
IP does not match an exclusion. Do not change cache headers for non-HTML,
pass-through, or unmodified responses because their output does not vary by this
feature.

### 5. Suppress only the generated DataDome snippet

At the start of `DataDomeIntegration::head_inserts()`:

1. Check the request-scoped suppression marker.
2. If present, return no DataDome head inserts.
3. Otherwise preserve the current `inject_client_side_tag` and
   `client_side_key` checks and emit the existing snippet unchanged.

When suppression is active, omit both:

```html
<script>
  window.ddjskey=...;window.ddoptions=...;
</script>
<script src="/integrations/datadome/tags.js" async></script>
```

Do not alter:

- publisher-originated DataDome script tags;
- `rewrite_sdk` behavior;
- the DataDome SDK proxy route;
- the signal collection API proxy;
- DataDome configuration serialization for non-suppressed requests; or
- injection behavior for requests without the marker.

## Testing plan

### Protection-filter tests

Add or extend tests in
`crates/trusted-server-core/src/integrations/datadome/protection.rs` to verify
that the marker is attached for:

- a matching inline IPv4 CIDR;
- a matching Config Store-backed CIDR source;
- a matching structured `ip_cidr` rule; and
- a matching structured `ip_cidr_source` rule.

Verify that the marker is absent for:

- a non-matching IP;
- an ASN exclusion;
- a path exclusion;
- a query-parameter exclusion;
- an excluded method; and
- an internal or integration route.

Verify the existing protection behavior remains unchanged: IP-matched requests
continue without a Protection API call.

### Head-injector tests

Add tests in
`crates/trusted-server-core/src/integrations/datadome.rs` verifying that:

- a configured client tag is omitted when suppression is active;
- a configured client tag is emitted when suppression is inactive;
- a blank client-side key remains a no-op; and
- `inject_client_side_tag = false` remains a no-op.

### HTML pipeline tests

Add coverage for the request-scoped value flowing through the HTML processor,
including the streaming path. Confirm that a suppressed processed HTML response
contains neither the injected `window.ddjskey` configuration nor the configured
DataDome `tags.js` script. For a suppressed HTML stream, assert the response is
private and has no surrogate cache headers. Confirm a non-suppressed HTML stream
retains its origin cache behavior.

Confirm that publisher-originated DataDome tags remain in the output and are
still rewritten according to the existing `rewrite_sdk` behavior.

### Fastly dispatch tests

Add a Fastly adapter dispatch test with:

- DataDome protection enabled;
- a client IP matching an inline exclusion;
- a configured client-side key; and
- an HTML publisher response.

The test should verify that the request continues without a Protection API
call, the response includes the `client_tag=omitted` decision log through the
existing test logging seam where available, and the generated tag is absent.

Also cover a non-excluded request to confirm the generated tag remains present.

## Documentation changes

Update `docs/guide/integrations/datadome.md` to state that IP-excluded Fastly
requests skip both:

- server-side Protection API validation; and
- Trusted Server's automatic client-side tag injection.

Document that this does not remove or disable publisher-originated DataDome
tags, and that non-IP exclusions do not automatically suppress the client-side
tag.

No configuration template changes are required because this behavior has no
new setting.

## Files expected to change

- `crates/trusted-server-core/src/integrations/registry.rs`
  - Support the internal request-scoped annotation mechanism.
- `crates/trusted-server-core/src/integrations/datadome.rs`
  - Define the marker and conditionally suppress head injection.
- `crates/trusted-server-core/src/integrations/datadome/protection.rs`
  - Attach the marker for IP-based scope skips and enrich the skip log.
- `crates/trusted-server-core/src/integrations/registry.rs` or the relevant
  HTML context definition
  - Carry the suppression decision into head injection.
- `crates/trusted-server-core/src/html_processor.rs`
  - Carry the request-scoped value into HTML integration context.
- `crates/trusted-server-core/src/publisher.rs`
  - Snapshot and propagate the request marker through response processing.
- `docs/guide/integrations/datadome.md`
  - Document the behavior.
- Relevant unit and Fastly adapter test modules.

The exact split between registry request annotations and HTML context plumbing
should remain minimal and should not introduce a new public configuration API.

## Verification

Implementation verification should use the repository's target-matched
commands:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
```

No live production validation is required for this implementation task. Live
browser verification will be performed later through the deployment/testing
workflow.

# DataDome Server-Side Protection API Integration

**Issue:** #317
**Date:** 2026-06-11
**Status:** In Progress

## Problem

Trusted Server already has a DataDome first-party proxy integration for the
client-side JavaScript tag and signal collection API. That layer improves
client-side signal delivery by routing DataDome browser traffic through the
publisher domain, but it does not perform server-side request validation before
requests reach Trusted Server routes or the publisher origin.

DataDome's Fastly Compute module adds that missing layer by calling the
DataDome Protection API before forwarding traffic. The Protection API returns a
request decision and header-mutation instructions. Trusted Server needs an
implementation of that behavior in Rust that is not tied to DataDome's Fastly
JavaScript SDK.

## Goals

- Add a pre-routing integration hook that can block/challenge requests before
  origin routing.
- Implement DataDome Protection API validation with fail-open behavior.
- Support DataDome pointer headers:
  - upstream request enrichment for allowed requests
  - downstream response headers/cookies for allowed and challenged requests
- Protect publisher-origin traffic and auction traffic by default.
- Exclude static assets and Trusted Server internal routes by default.
- Keep the Protection API client logic platform-neutral where possible by using
  `RuntimeServices`, `PlatformBackend`, and `PlatformHttpClient`.
- Auto-inject the DataDome client-side tag when a client-side key is configured.
- Preserve the existing DataDome first-party proxy and URL-rewrite behavior.

## Non-Goals

- No GraphQL body parsing in the initial implementation. The config can reserve
  a flag for it, but request-body inspection is deferred.
- No hard dependency on DataDome's JavaScript Fastly Compute package.
- No new edge-provider-specific behavior in `trusted-server-core` beyond the
  existing `fastly::Request` integration surfaces.
- No replay-protection or MCP-specific fields in v1.
- No automatic de-duplication when a publisher already manually loads the
  DataDome tag. The explicit `inject_client_side_tag = false` escape hatch is
  sufficient for v1.
- No literal DataDome server-side secret value in `trusted-server.toml`.
  Operators configure the runtime secret store and secret name, and the key is
  read from Secret Store at request time with process-local caching.

## Decisions from Design Discussion

1. **Protection scope:** protect publisher-origin and auction traffic by
   default. Default-exclude Trusted Server internal routes and static assets.
2. **Endpoint default:** default to DataDome's Fastly-specific Protection API
   endpoint from the official Fastly Compute docs, while allowing override.
3. **Header precedence:** apply DataDome downstream headers last so DataDome
   cookies/cache/challenge headers are not overwritten by generic finalization.
4. **GraphQL support:** defer.
5. **Client-side tag:** auto-inject when a client-side key is configured.
6. **Methods:** protect every non-`OPTIONS` method, including `HEAD`, when the
   URL is otherwise in scope.
7. **Secret handling:** read the DataDome server-side key from runtime Secret
   Store using configured store/name fields. Do not store the literal key in
   `trusted-server.toml`.
8. **Timeout:** use `1500ms` as the default Protection API timeout for v1.
9. **Duplicate tag handling:** do not attempt automatic duplicate-tag
   detection in v1; operators can disable injection with
   `inject_client_side_tag = false`.

## Current State

Implementation branch status as of 2026-06-15:

- Added the generic integration request-filter model in
  `crates/trusted-server-core/src/integrations/registry.rs`.
- Wired the Fastly adapter to run request filters after basic auth and before
  route matching in `crates/trusted-server-adapter-fastly/src/main.rs`.
- Added DataDome server-side configuration fields and validation in
  `crates/trusted-server-core/src/integrations/datadome.rs`.
- Added the DataDome Protection API helper module at
  `crates/trusted-server-core/src/integrations/datadome/protection.rs`.
- Added client-side tag auto-injection through `IntegrationHeadInjector`.
- Extended `ClientInfo` and Fastly runtime services with JA4, H2 fingerprint,
  edge hostname, and edge region fields.
- Added configurable protection-scope exclusions for methods, ASNs, inline IP
  CIDRs, Config Store-backed IP CIDR lists, and typed method-scoped rules for
  path/query/IP/ASN matching.
- Updated `trusted-server.toml` with the new DataDome configuration fields.
- Updated `docs/guide/integrations/datadome.md` with the first-party,
  server-side protection, fail-open, header-enrichment, auto-injection,
  configurable exclusion, Secret Store, and GraphQL-v1 limitation behavior.

Known remaining work before the PR is ready:

- Run JS checks if JS build output is touched.
- Perform staging validation against a DataDome test policy/rule.

Verification snapshot:

- `cargo fmt --all -- --check` passed on 2026-06-15.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passed
  on 2026-06-15.
- `cargo test --workspace -- --nocapture` passed on 2026-06-15.
- `cd docs && npx prettier --check guide/integrations/datadome.md superpowers/specs/2026-06-11-datadome-server-side-protection-design.md`
  passed on 2026-06-15.

Baseline DataDome integration before this work:

- File: `crates/trusted-server-core/src/integrations/datadome.rs`
- Provides:
  - `/integrations/datadome/tags.js` SDK proxy
  - `/integrations/datadome/js/*` signal collection proxy
  - HTML attribute rewriting for DataDome script URLs
- Registered:
  - `IntegrationProxy`
  - `IntegrationAttributeRewriter`

Baseline integration registry before this work supported proxies,
attribute/script rewriters, HTML post-processors, and head injectors. It did not
have a pre-routing request-filter hook.

Baseline Fastly routing flow before this work in
`crates/trusted-server-adapter-fastly/src/main.rs`:

```text
sanitize forwarded headers
→ extract request context
→ batch-sync special case
→ build EC context
→ enforce basic auth
→ route matching
→ publisher origin fallback
→ EC/final response headers
```

The new request filter should run after successful basic auth and before route
matching.

## Proposed Architecture

### 1. Request Filter Hook

Add a new integration hook in
`crates/trusted-server-core/src/integrations/registry.rs`.

The hook must be richer than `Option<Response>` because DataDome can allow a
request while still requiring request and response header mutations.

Suggested public model:

```rust
#[async_trait(?Send)]
pub trait IntegrationRequestFilter: Send + Sync {
    fn integration_id(&self) -> &'static str;

    async fn filter_request(
        &self,
        input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>>;
}

pub struct RequestFilterInput<'a> {
    pub settings: &'a Settings,
    pub services: &'a RuntimeServices,
    pub request: &'a Request,
}

pub enum RequestFilterDecision {
    Continue(RequestFilterEffects),
    Respond {
        response: Response,
        effects: RequestFilterEffects,
    },
}

#[derive(Default)]
pub struct RequestFilterEffects {
    pub request_headers: Vec<HeaderMutation>,
    pub response_headers: Vec<HeaderMutation>,
}

pub struct HeaderMutation {
    pub name: String,
    pub value: String,
    pub mode: HeaderMutationMode,
}

pub enum HeaderMutationMode {
    Set,
    Append,
}
```

Important behavior:

- Filters run in registration order.
- On `Continue`, request header mutations are applied immediately before the
  next filter and before route matching.
- Response header mutations are accumulated and applied to the final response.
- On `Respond`, routing short-circuits with that response while preserving any
  downstream response header effects that must be applied after finalization.
- DataDome transport/API failures should not bubble out as registry errors;
  DataDome should convert them to `Continue(Default::default())` to preserve
  fail-open behavior.

### 2. Registry Integration

Extend these types:

- `IntegrationRegistration`
- `IntegrationRegistrationBuilder`
- `IntegrationRegistryInner`
- `IntegrationRegistry`
- `IntegrationMetadata`

Add builder method:

```rust
.with_request_filter(integration.clone())
```

Add registry runner, for example:

```rust
pub async fn filter_request(
    &self,
    input: RequestFilterRegistryInput<'_>,
) -> Result<RequestFilterRegistryOutcome, Report<TrustedServerError>>
```

The registry outcome should contain either an immediate response plus response
header mutations, or a continue decision with accumulated response header
mutations.

### 3. Fastly Route Hook

In `route_request()`, run filters after normal basic auth succeeds and before
`path` / `method` are captured for route matching.

```text
basic auth ok
→ integration_registry.filter_request(...)
  → Respond { response, effects }: finalize response, apply DataDome headers last, return
  → Continue(effects): request is enriched; route normally; remember response effects
→ route matching
→ EC finalize
→ generic finalize_response
→ apply request-filter response headers last
```

Streaming publisher responses need the same treatment before headers are
committed via `stream_to_client()`.

### 4. Header Mutation Semantics

DataDome pointer headers are internal instructions and must not be forwarded.
Only headers named by the pointers should be copied.

| Pointer header               | Destination                                        |
| ---------------------------- | -------------------------------------------------- |
| `X-DataDome-request-headers` | Request forwarded to Trusted Server route / origin |
| `X-DataDome-headers`         | Response returned to browser                       |

Rules:

- `Set-Cookie` mutations use append mode.
- Other headers use set/replace mode.
- Pointer headers themselves are never forwarded.
- Header mutations must reject hop-by-hop, request-target, body framing, and
  Trusted Server internal headers such as `Connection`, `Transfer-Encoding`,
  `Content-Length`, `Host`, and `x-ts-*`.
- DataDome downstream headers are applied after `ec_finalize_response()` and
  `finalize_response()`.

## DataDome Protection Design

### Configuration

Extend `[integrations.datadome]` with server-side protection and client-side
injection fields.

```toml
[integrations.datadome]
enabled = false

# Existing first-party proxy layer
sdk_origin = "https://js.datadome.co"
api_origin = "https://api-js.datadome.co"
cache_ttl_seconds = 3600
rewrite_sdk = true

# New server-side protection layer
enable_protection = false
server_side_key_secret_store = "datadome"
server_side_key_secret_name = "server_side_key"
protection_api_origin = "https://api-fastly.datadome.co"
timeout_ms = 1500
protection_excluded_methods = ["OPTIONS"]
protection_excluded_asns = []
protection_excluded_ip_cidrs = []
protection_excluded_ip_cidr_sources = []
protection_ip_list_cache_ttl_seconds = 300
enable_graphql_support = false

# New client-side tag injection layer
client_side_key = ""
inject_client_side_tag = true
client_side_tag_url = "/integrations/datadome/tags.js"
client_side_configuration = { ajaxListenerPath = true }

[[integrations.datadome.protection_exclusion_rules]]
id = "default-static-assets"
type = "path_regex"
patterns = ["(?i)\\.(avi|flv|mka|mkv|mov|mp4|mpeg|mpg|mp3|flac|ogg|ogm|opus|wav|webm|webp|bmp|gif|ico|jpeg|jpg|png|svg|svgz|swf|eot|otf|ttf|woff|woff2|css|less|js|map)$"]
```

Notes:

- The literal server-side key is not stored in Rust config. Rust config stores
  only `server_side_key_secret_store` and `server_side_key_secret_name`.
- `server_side_key_secret_store` and `server_side_key_secret_name` are required
  only when `enable_protection = true`.
- The DataDome server-side key is read from Secret Store through
  `RuntimeServices::secret_store()` and cached per process by configured
  store/name.
- `client_side_key` is optional. Auto-injection emits a tag only when
  `inject_client_side_tag = true` and `client_side_key` is non-empty; an empty
  key is a valid no-op.
- `protection_api_origin` remains configurable for regional/static endpoint
  selection.
- Static-asset exclusion is represented as a default typed `path_regex` rule and
  should remain case-insensitive so uppercase file extensions such as `.PNG` are
  skipped.
- `protection_excluded_methods`, `protection_excluded_asns`, inline
  `protection_excluded_ip_cidrs`, Config Store-backed
  `protection_excluded_ip_cidr_sources`, and typed
  `protection_exclusion_rules` provide migration parity for legacy VCL bypass
  policies without hardcoding publisher-specific rules in Rust.
- `enable_graphql_support` is reserved but should remain unsupported or ignored
  with a warning until the deferred body-handling work is implemented.

### Protection Scope

A request is protected when:

1. DataDome integration is enabled.
2. `enable_protection = true`.
3. The method is not listed in `protection_excluded_methods`; by default this
   skips `OPTIONS`.
4. The URL does not match the default Trusted Server internal exclusions.
5. The client IP does not match inline or Config Store-backed excluded CIDR
   lists.
6. The client ASN is not listed in `protection_excluded_asns`.
7. No typed `protection_exclusion_rules` match.

Default internal exclusions should include:

- `/static/tsjs=`
- `/integrations/`
- `/first-party/`
- `/.well-known/trusted-server.json`
- `/verify-signature`
- `/admin/`
- `/_ts/admin/`
- `/_ts/api/v1/identify`
- `/_ts/api/v1/batch-sync`
- CORS preflight `OPTIONS` requests

Auction traffic at `/auction` is intentionally protected by default.

Typed exclusion rules use a small rule-engine pattern so new matcher types can
be added without growing `is_request_protected()` into a large conditional. A
rule has an operator-provided `id`, optional `methods`, and one matcher selected
by `type`:

```toml
[[integrations.datadome.protection_exclusion_rules]]
id = "legacy-static-get-head"
methods = ["GET", "HEAD"]
type = "path_regex"
patterns = ["(?i)\\.(css|css\\.map|js|js\\.map|json|png|jpg|webp|woff2)$"]

[[integrations.datadome.protection_exclusion_rules]]
id = "next-rsc"
methods = ["GET", "HEAD"]
type = "query_param_non_empty"
names = ["_rsc"]
```

Supported v1 rule types:

- `path_exact`
- `path_prefix`
- `path_regex`
- `query_param_non_empty`
- `asn`
- `ip_cidr`
- `ip_cidr_source`

Config Store-backed CIDR lists are non-secret operational data and may be
encoded as JSON arrays, comma-separated strings, or newline/whitespace-separated
strings. Load failures log a warning and do not match the bypass list, so a bad
list does not accidentally disable DataDome for all traffic.

### Protection API Request

Add a DataDome protection helper module, either as a nested module in
`datadome.rs` or as:

`crates/trusted-server-core/src/integrations/datadome/protection.rs`

Responsibilities:

1. Decide whether a request should be protected.
2. Build the form-encoded Protection API payload.
3. Send `POST /validate-request` through platform services.
4. Classify the API response.
5. Extract pointer-header mutations.
6. Return a request-filter decision.

Use platform abstractions for the outbound call:

- Parse `protection_api_origin` with `url`.
- Build a `PlatformBackendSpec` with `first_byte_timeout = timeout_ms`.
- Resolve/register backend with `RuntimeServices::backend().ensure(...)`.
- Send an `edgezero_core::http::Request` through
  `RuntimeServices::http_client().send(...)`.

Request headers:

```text
Content-Type: application/x-www-form-urlencoded
Content-Length: <encoded body length>
X-DataDome-X-Set-Cookie: true  # only when X-DataDome-ClientID is used
```

Payload fields should include the core fields from DataDome's official module:

- `Key`
- `IP`
- `Method`
- `Protocol`
- `Host`
- `ServerHostname`
- `Request` as path plus query
- `RequestModuleName`
- `ModuleVersion`
- `TimeRequest`
- `ClientID`
- `CookiesLen`
- `HeadersList`
- common request headers:
  - `Accept`
  - `Accept-Charset`
  - `Accept-Encoding`
  - `Accept-Language`
  - `AuthorizationLen`
  - `Cache-Control`
  - `Connection`
  - `Content-Type`
  - `From`
  - `Origin`
  - `PostParamLen`
  - `Pragma`
  - `Referer`
  - `User-Agent`
  - `Via`
  - `X-Forwarded-For`
  - `X-Real-IP`
  - `X-Requested-With`
  - Sec-CH and Sec-Fetch headers supported by the official module
- TLS/client metadata when available from `RuntimeServices::client_info()`

`ClientID` source priority:

1. `X-DataDome-ClientID` request header
2. `datadome` cookie

When `X-DataDome-ClientID` is used, send
`X-DataDome-X-Set-Cookie: true` to the Protection API.

Encoding and size rules:

- URL-encode all values.
- Omit empty fields.
- Apply per-field truncation before encoding.
- Keep the global payload under DataDome's documented limit.

### Client Metadata

Current `RuntimeServices::client_info()` exposes:

- client IP
- TLS protocol
- TLS cipher

For better DataDome signal quality, extend `ClientInfo` with optional fields
that adapters can populate when available:

```rust
pub struct ClientInfo {
    pub client_ip: Option<IpAddr>,
    pub tls_protocol: Option<String>,
    pub tls_cipher: Option<String>,
    pub tls_ja4: Option<String>,
    pub h2_fingerprint: Option<String>,
    pub server_hostname: Option<String>,
    pub server_region: Option<String>,
}
```

Fastly can populate `tls_ja4` and `h2_fingerprint` from the request APIs already
used by the JA4/debug device-signal code. Other adapters may leave these fields
empty.

### Protection API Response

Before acting on a response, validate that the HTTP status code matches the
`X-DataDomeResponse` header.

| Status | Meaning   | Behavior                                       |
| ------ | --------- | ---------------------------------------------- |
| `200`  | Allow     | Continue routing with request/response effects |
| `301`  | Challenge | Return DataDome response directly              |
| `302`  | Challenge | Return DataDome response directly              |
| `401`  | Challenge | Return DataDome response directly              |
| `403`  | Challenge | Return DataDome response directly              |
| `429`  | Challenge | Return DataDome response directly              |
| other  | Fail-open | Continue without effects                       |

If status/header mismatch, missing `X-DataDomeResponse`, timeout, network error,
backend error, malformed headers, or any unexpected Protection API behavior:
fail open and continue without effects.

### Challenge Responses

For challenge statuses:

1. Build a response using DataDome's API response status and body.
2. Copy only headers listed in `X-DataDome-headers`.
3. Append `Set-Cookie` values.
4. Do not contact the publisher origin.
5. Still run Trusted Server response finalization, then apply DataDome headers
   last.

### Allowed Requests

For allow status `200`:

1. Copy headers listed in `X-DataDome-request-headers` into the request before
   Trusted Server route matching.
2. Accumulate headers listed in `X-DataDome-headers` for the final browser
   response.
3. Continue normal route matching.
4. Apply accumulated DataDome downstream headers after EC and generic response
   finalization.

## Client-Side Auto-Injection

Implement `IntegrationHeadInjector` for DataDome when `client_side_key` is
configured and `inject_client_side_tag = true`.

Injected snippet should run before the TSJS bundle and configure DataDome's
client-side tag:

```html
<script>
  window.ddjskey = '...'
  window.ddoptions = { ajaxListenerPath: true }
</script>
<script src="/integrations/datadome/tags.js" async></script>
```

Rust implementation requirements:

- Serialize `client_side_key`, `client_side_configuration`, and
  `client_side_tag_url` with `serde_json`.
- Escape `</` as `<\/` before inserting into a script tag.
- Use the first-party DataDome tag URL by default.
- Provide `inject_client_side_tag = false` for publishers that already manage
  the tag themselves.
- Do not attempt duplicate-tag detection in v1; the configuration escape hatch
  is the supported duplicate-avoidance mechanism.

The existing DataDome script guard remains useful for dynamically inserted
DataDome scripts.

## File-by-File Design

### `crates/trusted-server-core/src/integrations/registry.rs`

Add:

- `IntegrationRequestFilter`
- `RequestFilterInput`
- `RequestFilterDecision`
- `RequestFilterEffects`
- `HeaderMutation`
- `HeaderMutationMode`
- request-filter storage in `IntegrationRegistryInner`
- builder method `with_request_filter`
- registry method to run filters
- unit-test helpers for filters

### `crates/trusted-server-core/src/integrations/mod.rs`

Re-export the new request-filter types.

### `crates/trusted-server-core/src/integrations/datadome.rs`

Extend `DataDomeConfig`:

- protection fields
- client-side injection fields
- validation for required keys and regexes

Extend `DataDomeIntegration`:

- implement `IntegrationRequestFilter`
- implement `IntegrationHeadInjector`
- register request filter only when `enable_protection = true`
- register head injector when auto-injection is enabled and a key exists

### `crates/trusted-server-core/src/integrations/datadome/protection.rs`

New module for:

- URL protection matching
- payload construction
- form encoding
- field truncation
- ClientID/cookie parsing
- platform HTTP call
- response classification
- pointer-header extraction

### `crates/trusted-server-core/src/platform/types.rs`

Optionally extend `ClientInfo` with DataDome-relevant client metadata.

### `crates/trusted-server-adapter-fastly/src/platform.rs`

Populate new `ClientInfo` fields from Fastly request/environment when available.

### `crates/trusted-server-adapter-fastly/src/main.rs`

- Run integration request filters after basic auth and before route matching.
- Apply request header mutations before route matching.
- Carry response header mutations through all non-streaming and streaming
  response paths.
- Apply DataDome/filter response headers last.

### `trusted-server.toml`

Document default DataDome protection and injection fields in the sample config.
Use blank keys in sample config.

### `docs/guide/integrations/datadome.md`

Update after implementation to describe:

- layer 1: first-party JS/proxy
- layer 2: server-side Protection API validation
- fail-open behavior
- default exclusions
- header enrichment
- auto-injection behavior
- GraphQL deferred limitation

## Testing Strategy

### Registry Tests

- filter runs in registration order
- `Continue` applies request headers before next filter
- response header effects accumulate
- `Respond` short-circuits later filters
- append/set header modes behave correctly

### DataDome Config Tests

- existing first-party proxy config still parses
- protection disabled does not require server-side key secret store/name fields
- protection enabled requires non-empty server-side key secret store/name fields
- protection fails open when the configured server-side key secret cannot be read
- invalid regex fails startup
- injection disabled allows empty `client_side_key`
- injection enabled with empty `client_side_key` emits no head insert and does
  not fail config validation
- injection enabled with key emits head insert

### Protection Matching Tests

- static extensions are excluded case-insensitively
- Trusted Server internal routes are excluded
- `/auction` is protected
- publisher-origin page path is protected
- inclusion regex narrows scope
- exclusion regex skips matching URLs
- query string is ignored for matching

### Payload Tests

- form encoding is correct
- empty fields are omitted
- `ClientID` comes from `X-DataDome-ClientID` before cookie
- `X-DataDome-X-Set-Cookie` is sent when header-based ClientID is used
- `datadome` cookie is parsed safely
- long fields are truncated according to configured limits
- request headers list is generated deterministically enough for tests

### Response Classification Tests

- `200` + matching `X-DataDomeResponse` allows request
- `301`, `302`, `401`, `403`, `429` challenge
- mismatched status/header fails open
- missing `X-DataDomeResponse` fails open
- `5xx` fails open
- pointer headers are not forwarded
- request enriched headers are applied to allowed requests
- downstream headers are applied to final responses
- `Set-Cookie` appends instead of replacing

### Route Tests

- filter runs after basic auth
- auth challenge short-circuits before DataDome
- DataDome challenge bypasses publisher origin
- allowed DataDome response enriches request before publisher origin
- DataDome downstream headers apply to buffered responses
- DataDome downstream headers apply before streaming response headers commit

## Acceptance Criteria

Checkboxes should be marked complete only when the behavior is implemented,
covered by targeted tests where practical, and the relevant verification command
passes.

- [x] Trusted Server can validate configured traffic through DataDome before
      route matching. Covered by adapter route tests for challenged and allowed
      DataDome-protected requests.
- [x] DataDome API timeouts/errors fail open. Covered by an adapter route test
      that lets malformed auction JSON reach the route after a platform-client
      failure.
- [x] DataDome challenge responses return without contacting the origin. Covered
      by an adapter route test that returns the DataDome challenge response even
      with no publisher-origin fallback.
- [x] Allowed requests receive DataDome request-enrichment headers. Covered by a
      registry test that applies DataDome-style request mutations before routing.
- [x] Final responses receive DataDome downstream headers/cookies. Covered by
      adapter route tests for allowed and challenged responses.
- [x] `Set-Cookie` is appended, not coalesced or overwritten. Covered by pointer
      header route tests for DataDome downstream cookies.
- [x] Static assets and internal Trusted Server routes are excluded by default.
      Covered by adapter route tests for discovery and default static-extension
      exclusions.
- [x] `/auction` is protected by default. Covered by the DataDome-allowed auction
      route test.
- [x] Client-side DataDome tag is auto-injected when configured. Covered by
      DataDome head-injector tests.
- [x] GraphQL body parsing is not implemented in v1 and is clearly documented.
- [x] Existing DataDome first-party proxy behavior remains unchanged. Existing
      DataDome proxy/rewrite tests pass as part of full workspace verification.
- [x] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace` pass after implementation. Verified on 2026-06-15.

## Resolved Questions

1. DataDome protection excludes methods listed in
   `protection_excluded_methods`, which defaults to `OPTIONS`. All other
   methods, including `HEAD`, are eligible when the URL is otherwise in scope.
2. The DataDome server-side key is loaded from runtime Secret Store in v1. The
   config contains only the secret store and secret name.
3. The default Protection API timeout is `1500ms` for v1.
4. Auto-injection does not attempt duplicate-tag detection in v1. The explicit
   `inject_client_side_tag = false` escape hatch is sufficient.

## Implementation Clarifications

1. **Timeout semantics:** `timeout_ms = 1500` is the v1 default and maps to the
   dynamic backend first-byte timeout. It is not a full end-to-end response-body
   deadline in v1.
2. **Client metadata scope:** JA4 and H2 fingerprint values are sent only in the
   form-encoded Protection API payload to DataDome. They are not forwarded to
   the publisher origin or returned to the browser unless DataDome independently
   returns mapped enriched headers. Include them in v1 when the platform exposes
   them because DataDome recommends TLS fingerprints and these signals are
   useful for distinguishing browser and automation network stacks. Omit the
   fields when unavailable.
3. **Challenge status source of truth:** follow the Protection API docs in v1:
   `301`, `302`, `401`, `403`, and `429` are challenge statuses when
   `X-DataDomeResponse` matches the HTTP status.
4. **Payload truncation limits:** use DataDome's documented per-field limits
   unless DataDome confirms different limits.

## References

- Issue #317 — Add server-side bot protection via DataDome Protection API
- DataDome Fastly Compute module documentation
- DataDome Protection API documentation
- DataDome API server / regional endpoint documentation
- `@datadome/module-fastly-compute` package behavior, version 1.3.1

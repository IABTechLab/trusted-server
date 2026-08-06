# DataDome Server-Side Protection API Integration

> **Supersession note (PR #986):** the response-effects portions of this
> document — in particular "DataDome headers/cookies apply last and win"
> and any post-finalization ordering — are **superseded** by the
> response-header hook spec's §4a security-channel contract
> (`2026-07-30-integration-response-header-hook-design.md`): one global
> order applies (core finalization → ordinary mutators → security
> effects → final cache/privacy invariant pass, unconditionally last),
> with typed cookie/header operations, the enumerated field contract in
> §4a.2 of that spec, and owner-only identifier
> boundaries. Where this document conflicts, the hook spec governs. Additionally, this document's sessionByHeader requirement ("always
> send `X-DataDome-X-Set-Cookie` when the header ID is used") is
> **superseded for v1**: header-session mode is startup-rejected (hook
> spec §4a); TS never requests it and does not forward incoming header
> ClientIDs to the vendor. This document's generic "TLS/client metadata"
> instruction is also narrowed by §4a: DataDome may receive only explicitly
> enumerated, request-scoped security fields under `SecurityUse`; the device
> provider remains deferred, no fingerprint-derived classification is stored,
> and unlisted host evidence is omitted.

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
3. **Header precedence (updated by PR #986):** apply DataDome through the
   core-owned security channel after ordinary mutators, then run the final
   cache/privacy invariant pass unconditionally last. Security effects do not
   override framing, cache safety, or privacy invariants.
4. **GraphQL support:** defer.
5. **Client-side tag:** auto-inject when a client-side key is configured.
6. **Methods:** protect every non-`OPTIONS` method, including `HEAD`, when the
   URL is otherwise in scope.
7. **Secret handling:** read the DataDome server-side key from runtime Secret
   Store using configured store/name fields. Do not store the literal key in
   `trusted-server.toml`.
8. **Timeout:** use `1500ms` as the default Protection API timeout for v1.
   _(Superseded: `1500 ms` is the **first-byte** bound only; the
   complete-response deadline is 3000 ms with defined measurement
   points — hook spec §4a.)_
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
    /// The only request surface generic integrations receive.
    pub request: &'a RedactedRequestView<'a>,
}

pub enum RequestFilterDecision {
    Continue(OrdinaryRequestFilterEffects),
    Respond {
        response: Response,
        effects: OrdinaryRequestFilterEffects,
    },
}

// Separate, core-only registration path. It is not a supertrait or optional
// field on IntegrationRequestFilter, so another integration cannot receive
// the DataDome capability through the generic runner.
#[async_trait(?Send)]
pub(crate) trait DataDomeSecurityRequestFilter: sealed::Sealed + Send + Sync {
    async fn filter_datadome(
        &self,
        input: DataDomeSecurityFilterInput<'_>,
    ) -> Result<DataDomeSecurityDecision, Report<TrustedServerError>>;
}

pub(crate) struct DataDomeSecurityFilterInput<'a> {
    pub settings: &'a Settings,
    pub services: &'a RuntimeServices,
    pub request: &'a RedactedRequestView<'a>,
    /// Constructed by core from the normative field allowlist. No raw
    /// Request/header map or AuthorizedIdentity is exposed.
    pub security: &'a DataDomeSecurityRequestView<'a>,
}

pub(crate) enum DataDomeSecurityDecision {
    Continue(DataDomeSecurityEffects),
    Respond {
        response: Response,
        effects: DataDomeSecurityEffects,
    },
}

#[derive(Default)]
pub(crate) struct DataDomeSecurityEffects {
    pub upstream_overlay: Vec<DataDomeUpstreamOperation>,
    pub browser_effects: Vec<DataDomeBrowserOperation>,
}
```

`RedactedRequestView`, `DataDomeSecurityRequestView`, the security trait, and
both security operation enums are core-owned sealed surfaces. Generic
integrations cannot construct or read them or recover the underlying raw
request. Core strips `ts-*`, EID/identity material,
`X-DataDome-ClientID`, and the `datadome` cookie before building the shared
view; the security view restores only the one typed cookie value and exact
request evidence admitted by the hook spec §4a.2.1. Another filter
receives only the shared redacted view and cannot inherit this owner
capability. This paragraph and the hook spec §4a replace every earlier generic
`&Request`/generic security-header-mutation sketch in this document. The
ordinary effects type remains subject to the hook's ordinary attributed-batch
registry and cannot express `SecurityUse`, owner overlay, cookies, or reserved
security names.

Important behavior:

- Ordinary filters run in registration order over the redacted view. DataDome's
  security owner view is evaluated in its dedicated security position and is
  never passed to the next filter.
- On `Continue`, allowlist-validated upstream operations enter only DataDome's
  owner-scoped publisher overlay; they never mutate the shared request.
- Typed browser effects are accumulated and applied through the hook spec's
  single pointer matrix and security budget.
- On `Respond`, routing short-circuits with that response while preserving any
  downstream response header effects that must be applied after finalization.
  _(Superseded: one global order applies — core finalization → ordinary
  mutators → security effects → invariant pass unconditionally last;
  nothing applies after the invariant pass — hook spec §4a.)_
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

The registry outcome should contain either an immediate response plus typed
security operations, or a continue decision with accumulated typed security
operations and an owner-scoped publisher overlay. Generic header name/value
mutations are not part of this API.
mutations.

### 3. Fastly Route Hook

In `route_request()`, run filters after normal basic auth succeeds and before
`path` / `method` are captured for route matching.

```text
basic auth ok
→ integration_registry.filter_request(...)
  → Respond { response, security_effects }: validate the complete security batch
  → Continue(security_effects): apply only the owner-scoped upstream overlay; route normally
→ route matching
→ EC finalize
→ ordinary response mutators
→ validated security effects
→ final cache/privacy invariant pass (always last)
```

Streaming publisher responses need the same treatment before headers are
committed via `stream_to_client()`.

### 4. Header Mutation Semantics

DataDome pointer headers are internal instructions and are never forwarded.
The one normative field/pointer contract is the hook spec §4a.2, with the
publisher-upstream overlay in §4a.2.2 and the browser-response matrix in
§4a.2.3; a pointer does not authorize an unlisted name.

| Pointer header               | Destination                                        |
| ---------------------------- | -------------------------------------------------- |
| `X-DataDome-request-headers` | Request forwarded to Trusted Server route / origin |
| `X-DataDome-headers`         | Response returned to browser                       |

Rules:

- `datadome` cookie effects use the hook spec's typed cookie operation; raw
  `Set-Cookie` is not a generic mutation.
- Every other admitted field follows its exact decision-matrix cell; there is
  no generic set/replace default.
- Pointer headers themselves are never forwarded.
- Hop-by-hop, request-target, body-framing, credential, Trusted Server
  internal, and unlisted headers invalidate the applicable batch.
- Security effects run before the final invariant pass, never after it.

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
server_side_key_secret_store = "ts_secrets"
server_side_key_secret_name = "datadome_server_side_key"
timeout_ms = 1500
complete_response_timeout_ms = 3000
challenge_body_max_bytes = 65536
protection_excluded_methods = ["OPTIONS"]
protection_excluded_asns = []
protection_excluded_ip_cidrs = []
protection_excluded_ip_cidr_sources = []
protection_ip_list_cache_ttl_seconds = 300
enable_graphql_support = false

# Security identity/lifecycle. No default exists for max age: enabling
# protection without an explicit value is a startup error.
security_cookie_max_age = 2592000 # example: 30 days; allowed 7d..=365d
security_cookie_domain = "host-only" # or one exact normalized ASCII domain
security_cookie_same_site = "Lax" # Lax | Strict | None
expose_client_id_to_origin = false
expose_host_fingerprints_to_vendor = false

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

This block is the canonical v1 DataDome configuration inventory; the hook and
allowlist specs reference it rather than defining another schema. Unknown
legacy security/session fields are errors, not ignored compatibility toggles.
In particular, `sessionByHeader`, `session_by_header`, and any equivalent are
startup-rejected in v1.

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
- The v1 Protection API URL is the core-owned constant
  `https://api-fastly.datadome.co/validate-request`. It is not operator
  configurable. Supporting another DataDome region or a publisher proxy is a
  new reviewed endpoint-registry entry and product/security decision, not a
  free-form URL setting. Unknown legacy `protection_api_origin` fields are
  startup errors so an old override cannot silently exfiltrate the server key.
- `complete_response_timeout_ms` defaults to and may not exceed 3000;
  `challenge_body_max_bytes` defaults to and may not exceed 65,536. The hook
  spec §4a owns the measurement/abort semantics.
- `security_cookie_max_age` is required when protection is enabled and must be
  604,800..=31,536,000 seconds. `security_cookie_domain` defaults to
  `host-only`; an explicit domain must pass the hook spec's exact-domain,
  domain-match, PSL, and active-scope-change checks.
- `security_cookie_same_site` accepts exactly `Lax`, `Strict`, or `None`;
  `None` is valid only with the unconditionally emitted `Secure` attribute.
  DataDome cookies never carry `HttpOnly`.
- Both exposure booleans default to `false`. ClientID-to-origin requires the
  exact owner-overlay capability; host fingerprints require qualified JA4
  availability and sign-offs 23/28. A selected adapter that cannot preserve
  admitted request-header field-line order or enforce the request/body limits
  fails startup for protection rather than synthesizing different evidence.
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
3. Send `POST https://api-fastly.datadome.co/validate-request` through platform
   services.
4. Classify the API response.
5. Extract pointer-header mutations.
6. Return a request-filter decision.

Use platform abstractions for the outbound call:

- Construct the URL only from the core constant and assert at startup that its
  scheme is `https`, host is exactly `api-fastly.datadome.co`, port is absent
  (therefore 443), path is exactly `/validate-request`, and it has no userinfo,
  query, or fragment. No request/config value participates in this URL.
- Build a `PlatformBackendSpec` with `first_byte_timeout = timeout_ms` and
  automatic redirect following disabled. A 3xx response is returned to the
  DataDome decision parser; it is never followed to a second authority.
- Resolve/register backend with `RuntimeServices::backend().ensure(...)`.
- Send an `edgezero_core::http::Request` through
  `RuntimeServices::http_client().send(...)`.

Request headers:

```text
Content-Type: application/x-www-form-urlencoded
Content-Length: <encoded body length>
X-DataDome-X-Set-Cookie: true  # only when X-DataDome-ClientID is used — SUPERSEDED for v1: never sent (hook spec §4a)
```

The exhaustive payload field set is the hook spec §4a.2.1. The list below is
informative and may not expand that normative allowlist:

- `Key`
- `IP`
- `Method`
- `Protocol`
- `Host`
- `ServerHostname`
- `Request` as the normalized path only; query and fragment are never disclosed
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
  - `Origin` as parsed origin only
  - `PostParamLen`
  - `Pragma`
  - `Referer` as parsed origin only
  - `User-Agent`
  - `Via`
  - `X-Requested-With`
  - only the individually enumerated Sec-CH and Sec-Fetch fields in the
    normative allowlist
- only the TLS/client metadata fields explicitly admitted by the normative
  allowlist and sign-offs 23/28; JA4 egress additionally requires
  `expose_host_fingerprints_to_vendor = true`, and availability alone is not
  authorization. `TlsCipher` and `H2Fingerprint` are omitted in v1 for the
  semantic reasons recorded in that allowlist

In cookie-mode v1, `ClientID` comes only from a single unambiguous
`datadome` cookie. `X-DataDome-ClientID` is stripped from every shared
surface and is not forwarded to the vendor, so TS never sends
`X-DataDome-X-Set-Cookie: true`.

Encoding and size rules:

- URL-encode all values.
- Omit empty source-header fields; keep mandatory `ClientID` present with an
  empty value when there is no unambiguous cookie.
- Apply the exact per-field decoded-byte limits in the normative allowlist
  before encoding.
- Measure the complete form-encoded body and enforce the allowlist's 24,576-byte
  ceiling before issuing the call; overflow takes metered fail-open and never
  triggers ad hoc field dropping.

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
    pub client_port: Option<u16>,
    pub tls_protocol: Option<String>,
    pub tls_cipher: Option<String>,
    pub tls_ja4: Option<String>,
    pub h2_fingerprint: Option<String>,
    pub server_hostname: Option<String>,
    pub server_region: Option<String>,
}
```

Fastly can populate `tls_ja4` and `h2_fingerprint` from the request APIs already
used by the JA4/debug device-signal code. Other adapters may leave those
optional fingerprint fields empty. `client_ip` and `client_port` are required
for a release-qualified Protection API call and come from the adapter's trusted
connection metadata, never a request header. If either is unavailable, the
adapter skips the vendor call through the metered fail-open path and remains
unqualified until vendor sign-off explicitly accepts a different profile; it
never invents port `0` or substitutes a forwarded header.

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
2. Validate the complete decision-scoped pointer batch against the hook spec
   §4a.2.3 and the typed-cookie contract.
3. Apply the accepted security batch atomically.
4. Do not contact the publisher origin.
5. Run the final cache/privacy invariant pass after the security batch.

### Allowed Requests

For allow status `200`:

1. Apply only the owner-scoped publisher-upstream fields admitted by the hook
   spec §4a.2.2 before route matching; the default is no
   ClientID exposure.
2. Validate and retain the decision-scoped browser security batch.
3. Continue normal route matching.
4. Apply ordinary response mutators, then the security batch, then the final
   invariant pass.

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

- Serialize `client_side_key` and `client_side_configuration` with
  `serde_json`.
- Escape `</` as `<\/` before inserting values into a script tag.
- Validate `client_side_tag_url` as root-relative or HTTPS, then HTML-escape it
  before inserting it into the script `src` attribute.
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
- `OrdinaryRequestFilterEffects`
- sealed `DataDomeSecurityRequestFilter`, `DataDomeSecurityFilterInput`, and
  `DataDomeSecurityRequestView`
- typed `DataDomeSecurityDecision`, `DataDomeSecurityEffects`,
  `DataDomeUpstreamOperation`, and `DataDomeBrowserOperation`
- separate ordinary-filter storage and one core-owned DataDome security slot in
  `IntegrationRegistryInner`
- public builder method `with_request_filter` for ordinary filters; a
  crate-private `with_datadome_security_filter` callable only by the built-in
  DataDome registration path
- separate registry runners; the ordinary runner's input type cannot carry the
  security view
- unit-test helpers for filters

### `crates/trusted-server-core/src/integrations/mod.rs`

Re-export only the ordinary request-filter types. The sealed DataDome security
trait, input/view, and operations remain crate-private.

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
- Apply DataDome/filter response effects through the hook spec's security
  channel, followed by the invariant pass.

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

- ordinary filters run in registration order over `RedactedRequestView`
- DataDome alone receives the sealed typed security view
- `Continue` applies only validated owner-overlay operations before publisher
  origin; another filter never observes them
- `Respond` short-circuits later filters and discards ordinary batches under the
  hook's security ordering
- generic operations cannot express reserved security names or cookies

### DataDome Config Tests

- existing first-party proxy config still parses
- protection disabled does not require server-side key secret store/name fields
- protection enabled requires non-empty server-side key secret store/name fields
- protection fails open when the configured server-side key secret cannot be read
- legacy/free-form `protection_api_origin`, session-header, and unknown security
  fields fail startup
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
- empty source-header fields are omitted while mandatory `ClientID` remains
  present as an empty value
- the outbound authority/path is exactly the core-owned HTTPS endpoint and 3xx
  responses are never followed
- `Request` contains normalized path only, with query/fragment absent, and
  `Referer` contains origin only
- `IP`/`Port` come only from trusted connection metadata; their absence skips
  the call, and raw `true-client-ip`, `x-forwarded-for`, and `x-real-ip` values
  never enter the payload or `HeadersList`
- `ClientID` comes only from a single unambiguous `datadome` cookie
- incoming `X-DataDome-ClientID` is stripped and
  `X-DataDome-X-Set-Cookie` is never sent in cookie-mode v1
- `datadome` cookie is parsed safely
- repeated list-valued source headers normalize in received field-line order
  with literal `, ` separators, while repeated singleton,
  `authorization`, or `content-length` headers skip the call without choosing
  first/last; empty and comma-containing values match the normative allowlist
- multiple cookie field lines use the normative `; ` join for `CookiesLen` and
  parsing; duplicate or malformed `datadome` pairs leave mandatory `ClientID`
  empty and expose no other cookie value
- long fields are truncated according to the one normative allowlist
- the cross-adapter repeated-field corpus produces byte-identical normalized
  form fields, lengths, `HeadersList`, and reject/omit outcomes; an adapter
  without that capability cannot enable protection

### Response Classification Tests

- `200` + matching `X-DataDomeResponse` allows request
- `301`, `302`, `401`, `403`, `429` challenge
- mismatched status/header fails open
- missing `X-DataDomeResponse` fails open
- `5xx` fails open
- pointer headers are not forwarded
- request enriched headers are applied to allowed requests
- admitted security fields are applied atomically before final invariants
- the typed `datadome` cookie never overwrites another cookie name

### Route Tests

- filter runs after basic auth
- auth challenge short-circuits before DataDome
- DataDome challenge bypasses publisher origin
- allowed DataDome response enriches request before publisher origin
- DataDome security batches apply to buffered responses before final invariants
- DataDome security batches and final invariants both complete before streaming
  response headers commit

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
- [ ] Allowed-request enrichment conforms to the owner-scoped allowlist and
      default-disabled ClientID exposure in the hook spec.
- [ ] Final responses use the hook spec's atomic security batch and final
      invariant ordering on every adapter/path.
- [ ] The typed `datadome` cookie contract and complete response-pointer matrix
      replace generic `Set-Cookie`/header mutation.
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
- [ ] `cargo fmt --all -- --check` and the repository's target-matched test and
      lint aliases pass after implementation: `cargo test-fastly`,
      `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`,
      `cargo clippy-fastly`, `cargo clippy-axum`,
      `cargo clippy-cloudflare`, `cargo clippy-cloudflare-wasm`,
      `cargo clippy-spin-native`, and `cargo clippy-spin-wasm`. Do not use bare
      `cargo test --workspace` or workspace-wide all-feature clippy for this
      multi-WASM-target repository.

## Resolved Questions

1. DataDome protection excludes methods listed in
   `protection_excluded_methods`, which defaults to `OPTIONS`. All other
   methods, including `HEAD`, are eligible when the URL is otherwise in scope.
2. The DataDome server-side key is loaded from runtime Secret Store in v1. The
   config contains only the secret store and secret name.
3. The default Protection API timeout is `1500ms` for v1. _(Superseded:
   first-byte bound only; 3000 ms complete-response deadline — hook
   spec §4a.)_
4. Auto-injection does not attempt duplicate-tag detection in v1. The explicit
   `inject_client_side_tag = false` escape hatch is sufficient.

## Implementation Clarifications

1. **Timeout semantics:** `timeout_ms = 1500` is the v1 default and maps to the
   dynamic backend first-byte timeout. It is not a full end-to-end response-body
   deadline in v1. _(The hook spec §4a now adds the 3000 ms
   complete-response deadline on the monotonic clock with defined
   measurement points; both bounds apply.)_
2. **Client metadata scope:** only JA4 may be optionally admitted to the
   form-encoded Protection API payload, never publisher origin, browser, graph,
   another integration, or raw logs. Availability is not authorization: omit
   it unless `expose_host_fingerprints_to_vendor = true`. `TlsCipher` is omitted
   because the platform exposes a negotiated cipher while the vendor field
   means ordered offered ciphers; `H2Fingerprint` is not a documented
   Protection API field. Admit no host evidence outside the hook spec §4a.2.1.
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

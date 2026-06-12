# Asset S3 Auth and Fastly IO Design

## Problem

Issue #695 requires authenticated S3 bucket support for path-based asset
proxying. The asset proxy work in #668 routes selected first-party asset paths
to alternate origins, but private S3 buckets return `403` unless upstream
requests are signed.

Issue #696 requires Fastly Image Optimizer (IO) support for those image asset
routes. These concerns must compose without tightly coupling the asset proxy to
S3, Fastly IO, or any customer-specific URL/profile convention.

The migration target includes production-style image URLs such as:

```text
/.image/<public-id>/<basename>.jpg?profile=w828&ar=1-1
```

where query parameters describe image transformation intent, not the S3 object
identity. For private S3 + IO, the origin request usually needs to be signed for
the final object path with origin query parameters stripped.

## Goals

- Support private S3 buckets as asset route origins using AWS Signature Version
  4 header authentication.
- Keep asset routes generic: routes may use no auth, S3 auth, public S3, a CDN,
  Fastly IO, or no IO in any combination.
- Keep S3 signing platform-neutral and independent from Fastly APIs.
- Keep Fastly IO platform-neutral at the core boundary; only the Fastly adapter
  should translate to `fastly::image_optimizer::ImageOptimizerOptions`.
- Support configurable profile-table image transformations equivalent to common
  VCL table-based IO setups.
- Avoid committing customer-specific names, profile sets, or URL semantics.
- Reuse existing path-prefix and path rewrite behavior from asset routes; S3
  signing signs the final rewritten origin URL.
- Fail fast on invalid configuration where possible.

## Non-Goals

- Full AWS SDK integration or AWS default credential-provider support.
- Uploads or signed request bodies. Initial support is read-only `GET` and
  `HEAD` asset access.
- Presigned S3 URLs in query parameters.
- Arbitrary IO query DSL support. Initial profile-table values support a strict
  subset of IO params.
- Legacy path-parameter normalization such as
  `/.image/w_828,ar_1:1/<id>/<file>`. Query-profile URLs are handled first;
  legacy path normalization can be added later if traffic requires it.
- Local end-to-end IO transformation verification. Viceroy currently does not
  perform real image optimization.

## Proposed Configuration Model

### Asset route capabilities

Asset route auth and image optimization are independent optional capabilities:

```toml
[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://bucket.s3.us-east-1.amazonaws.com"
# Existing #668 path rewrite behavior remains available and unchanged.
# path_pattern = "^/\\.image/(.*)$"
# target_path = "/images/$1"

[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
origin_query = "strip"
# Optional secret lookup overrides; these default if omitted.
# secret_store = "s3-auth"
# access_key_id = "access_key_id"
# secret_access_key = "secret_access_key"
# session_token = "session_token"

[proxy.asset_routes.image_optimizer]
enabled = true
region = "us_east"
profile_set = "default_images"
```

Other supported combinations:

```toml
# Public non-IO asset origin.
[[proxy.asset_routes]]
prefix = "/static/"
origin_url = "https://cdn.example.com"

# Private S3, no IO.
[[proxy.asset_routes]]
prefix = "/private-assets/"
origin_url = "https://bucket.s3.us-east-1.amazonaws.com"
auth = { type = "s3_sigv4", region = "us-east-1" }

# Public image origin with IO.
[[proxy.asset_routes]]
prefix = "/images/"
origin_url = "https://images.example.com"
image_optimizer = { enabled = true, region = "us_east", profile_set = "default_images" }
```

### S3 auth defaults

`auth.type = "s3_sigv4"` uses route-scoped config with default secret names:

| Field               | Default                                | Meaning                                          |
| ------------------- | -------------------------------------- | ------------------------------------------------ |
| `secret_store`      | `s3-auth`                              | Runtime secret store name                        |
| `access_key_id`     | `access_key_id`                        | Secret key containing AWS access key ID          |
| `secret_access_key` | `secret_access_key`                    | Secret key containing AWS secret access key      |
| `session_token`     | unset                                  | Optional secret key containing AWS session token |
| `origin_query`      | `preserve` without IO, `strip` with IO | Query behavior for the origin/S3 request         |

Credential fields name secret-store entries, not literal credential values.

### Image optimizer profile sets

Profile tables are reusable and defined globally in `trusted-server.toml`:

```toml
[image_optimizer.profile_sets.default_images]
base_params = "quality=70&resize-filter=bicubic"
default_profile = "default"
unknown_profile = "use_default" # "use_default" | "reject"
profile_param = "profile"
aspect_ratio_param = "ar"
debug_param = "_io_debug"

[image_optimizer.profile_sets.default_images.profiles]
default = "width=1920"
thumbnail = "width=150&crop=1:1,smart"
medium = "format=auto&width=828"
large = "format=auto&width=1536"

[image_optimizer.profile_sets.default_images.aspect_ratios]
allowed = ["1-1", "16-9", "4-3"]
profiles = ["medium", "large"]

[image_optimizer.profile_sets.default_images.crop_offsets]
enabled = true
x_param = "x"
y_param = "y"
buckets = [10, 30, 50, 70, 90]
default = 50
when_missing = "smart"
```

Only a small generic commented example should be committed to
`trusted-server.toml`. Customer-specific profile set names and tables belong in
private deployment configuration or environment overrides.

## Runtime Semantics

### Route handling order

Asset routes are evaluated after explicit built-in and integration routes and
before publisher fallback, as designed in #668. Only `GET` and `HEAD` requests
participate.

### Asset request pipeline

For a matched asset route:

```text
incoming request
  -> build final origin URL using existing path_pattern/target_path behavior
  -> evaluate image_optimizer profile table from the original client query
  -> determine origin query policy
  -> build platform-neutral upstream request
  -> apply origin auth, if configured
  -> attach platform-neutral IO metadata, if enabled and not debug-bypassed
  -> PlatformHttpClient::send()
```

S3 signing happens after path rewrite and after origin query policy is applied,
so the signature always covers the exact URL that the origin should see.

### Origin query policy

`origin_query` controls whether the origin request includes the inbound query:

- `strip`: origin URL query is empty.
- `preserve`: origin URL query preserves the inbound query string.

For S3-authenticated routes with IO enabled, default to `strip`. This matches
the common shape where `profile`, `ar`, `x`, and `y` are transformation inputs
rather than S3 object identity.

Initial implementation rejects `origin_query = "preserve"` when IO is enabled.
Fastly treats request query parameters as possible IO transformation inputs, so
preserving arbitrary client query parameters would bypass the closed profile set
and can also change the URL Fastly ultimately sends to the origin. A future
explicit allowlist can relax this if origin query preservation is needed.

### Debug bypass

If the configured `debug_param` is present with value `1`, image optimization is
disabled for that request. The asset route still applies, and origin auth still
applies if configured. The response is the original source object.

This provides a simple way to compare optimized vs. origin images.

## S3 SigV4 Auth

### Signing method

Use header-based AWS Signature Version 4. Do not generate presigned query URLs.

For `GET` and `HEAD`, add or overwrite:

```http
Host: <origin-host>
x-amz-date: <YYYYMMDDTHHMMSSZ>
x-amz-content-sha256: UNSIGNED-PAYLOAD
x-amz-security-token: <token> # only when configured and present
Authorization: AWS4-HMAC-SHA256 Credential=..., SignedHeaders=..., Signature=...
```

Use:

- service: `s3`
- payload hash: `UNSIGNED-PAYLOAD`
- method: `GET` or `HEAD`
- canonical URI/query from the final origin URL
- canonical headers including at least `host`, `x-amz-content-sha256`,
  `x-amz-date`, and `x-amz-security-token` when present

### Credential loading

Read credentials from `RuntimeServices::secret_store()` using the configured
runtime `StoreName`. The core signer receives strings/bytes and does not depend
on Fastly Secret Store directly.

Missing access key or secret key is a request-time proxy error. Missing session
token is allowed when `session_token` is absent. When `session_token` is
configured, the named secret must be present. Do not log secret values.

### Endpoint assumptions

For `s3_sigv4`, `origin_url` must be the real S3 or S3-compatible endpoint host
that the origin will validate in the signature, for example:

```toml
origin_url = "https://my-bucket.s3.us-east-1.amazonaws.com"
```

or path-style/S3-compatible usage:

```toml
origin_url = "https://s3.us-east-1.amazonaws.com"
path_pattern = "^/\\.image/(.*)$"
target_path = "/my-bucket/$1"
```

Do not sign for one host while sending to a different vanity host.

## Platform-Neutral Image Optimizer Metadata

Extend the platform HTTP abstraction so core can request image optimization
without importing Fastly SDK types.

Suggested core shape:

```rust
pub struct PlatformHttpRequest {
    pub request: EdgeRequest,
    pub backend_name: String,
    pub image_optimizer: Option<PlatformImageOptimizerOptions>,
    pub stream_response: bool,
}

pub struct PlatformImageOptimizerOptions {
    pub region: PlatformImageOptimizerRegion,
    pub preserve_query_string_on_origin_request: bool,
    pub params: PlatformImageOptimizerParams,
}
```

`PlatformImageOptimizerParams` initially needs only the strict subset produced
by profile tables:

- `quality`
- `resize_filter`
- `format`
- `width`
- `height`
- `crop`

The Fastly adapter maps these neutral options to
`fastly::image_optimizer::ImageOptimizerOptions` and calls
`Request::set_image_optimizer()` in `send()`.

`FastlyPlatformHttpClient::send_async()` must reject requests that carry image
optimizer metadata because the Fastly Rust SDK does not support IO with
`send_async` or `send_async_streaming`.

Test platform clients should record the metadata so core tests can assert that
IO would be requested without needing real Fastly IO locally. Asset routes should
set `stream_response` for final origin responses so large images/static assets are
not materialized into WASM memory as a single buffer.

## Profile Table Conversion

### Supported profile parameters

The profile parser is intentionally strict in v1. Profile strings may contain:

- `quality=<0..100>`
- `resize-filter=bicubic` initially, plus any Fastly SDK resize filters we map
- `format=auto|avif|gif|jpeg|jxl|mp4|png|webp` as supported by the SDK/version
- `width=<pixels>`
- `height=<pixels>`
- `crop=<w>:<h>`
- `crop=<w>:<h>,smart`
- `crop=<w>:<h>,offset-x<N>,offset-y<N>` as generated by offset handling

Unknown keys or invalid values fail config preparation.

### Profile lookup

For a request query:

```text
?profile=medium&ar=1-1&x=70&y=30
```

1. Read `profile_param` from the query.
2. Look up the profile in the configured profile set.
3. If missing/unknown and `unknown_profile = "use_default"`, use
   `default_profile`.
4. If missing/unknown and `unknown_profile = "reject"`, return a bad request or
   proxy error before sending upstream.
5. Merge `base_params` and the selected profile params, with profile params
   overriding base params if the same key appears.

### Aspect ratio override

If `aspect_ratio_param` is present and valid:

- the value must be in `aspect_ratios.allowed`
- the selected profile must be in `aspect_ratios.profiles`

Then set/replace `crop` to the requested aspect ratio. Convert `1-1` to `1:1`.
Invalid or unsupported aspect ratio values are ignored to match tolerant VCL
behavior and avoid breaking images.

### Crop offset bucketing

When crop offsets are enabled and the final crop is a bare aspect ratio:

- if either `x_param` or `y_param` is present, normalize each value to the
  configured bucket list and append `offset-xN,offset-yN`
- invalid, missing, non-numeric, or out-of-range values normalize to the
  configured default
- if neither offset is present and `when_missing = "smart"`, append `smart`

Default bucket behavior should match the common VCL pattern:

```text
<20 => 10
<40 => 30
<60 => 50
<80 => 70
else => 90
```

The configured bucket list must be sorted and bounded to a safe range to avoid
variant explosion.

### Closed output set

The profile mapper consumes `profile`, `ar`, `x`, and `y`. It does not pass the
client query through as raw IO params. The generated IO metadata is the only
transformation request, unless debug bypass disables IO.

## Local Development and Verification

Local development can validate:

- config parsing and validation
- asset route matching and path rewrite behavior
- profile table conversion
- S3 SigV4 canonical request/signature generation
- platform-neutral IO metadata attachment
- Fastly adapter rejection of IO metadata on async sends

Local Viceroy cannot prove real image resizing/format conversion. Current
Viceroy source returns unsupported for the Image Optimizer hostcall. End-to-end
acceptance requires a Fastly Compute service with IO enabled for the account and
service.

## Testing Strategy

### Settings tests

- parse `auth = { type = "s3_sigv4", ... }`
- default S3 secret store/key names
- reject invalid S3 region/empty credential key names
- parse top-level `image_optimizer.profile_sets`
- reject route `image_optimizer.profile_set` references that do not exist
- reject unsupported profile-table params
- reject invalid crop/quality/width/height values
- accept generic commented-example-equivalent config

### Profile conversion tests

- missing profile uses default profile
- unknown profile uses default when configured
- unknown profile rejects when configured
- `profile=medium` produces base + profile IO params
- valid aspect ratio override appends/replaces crop only for configured profiles
- invalid aspect ratio is ignored
- no offsets appends `smart` for bare crops
- `x`/`y` offsets normalize to configured buckets
- debug param disables IO metadata
- arbitrary query params are not passed into generated IO metadata

### S3 signer tests

- canonical request uses final rewritten path
- origin query is stripped when configured
- origin query is preserved when configured
- generated `Authorization` matches a stable fixture
- session token is added and signed when present
- unsigned payload header is present
- inbound `Authorization` and `x-amz-*` headers are overwritten/not trusted
- secret values are not logged or surfaced in error text

### Asset proxy tests

- public asset route still works without auth or IO
- S3 auth route signs before sending
- IO route attaches platform-neutral metadata
- S3 + IO route strips origin query by default
- S3 + IO rejects preserve-query configuration while profile-table IO is enabled
- path rewrite occurs before signing
- `GET` and `HEAD` are supported
- non-`GET`/`HEAD` skip asset route and fall through as existing behavior
- upstream `Set-Cookie` and HSTS stripping remains unchanged from #668

### Fastly adapter tests

- `send()` maps neutral IO options to Fastly SDK options
- `send_async()` fails when IO metadata is present
- request headers from S3 signing survive Fastly request conversion
- Host/backend behavior remains consistent with signed host expectations

## Implementation Plan

1. Add config structs for:
   - `AssetOriginAuth`
   - `S3SigV4AuthConfig`
   - top-level `ImageOptimizerSettings`
   - `ImageOptimizerProfileSet`
   - route-level `AssetImageOptimizerConfig`
2. Add profile-set normalization and validation during settings preparation.
3. Extend `PlatformHttpRequest` with optional `PlatformImageOptimizerOptions`.
4. Add Fastly adapter mapping for neutral IO metadata in `send()` and explicit
   rejection in `send_async()`.
5. Implement strict profile-table parser/converter.
6. Implement internal lightweight S3 SigV4 signer using existing crypto crates.
7. Wire asset route handling:
   - build final origin URL using existing rewrite behavior
   - compute IO metadata or debug bypass
   - apply origin query policy
   - build outbound request
   - apply S3 signing if configured
   - send synchronously
8. Add generic commented config examples to `trusted-server.toml`.
9. Run:
   - `cargo test --workspace`
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   - wasm build for `trusted-server-adapter-fastly`

## Open Questions

- What exact S3 object key mapping will production deployments need? Keep this
  configurable through the existing path rewrite fields.
- `session_token` is optional by default; if a route config names a session-token
  secret, that secret must exist at request time.
- Which Fastly IO region should production configs use? It should be close to
  the S3/image origin.
- Should a later phase add legacy path-parameter normalization? Defer until
  active traffic requires it.

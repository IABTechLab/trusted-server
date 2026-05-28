# EdgeZero Image Optimizer Primitive Design

> Proposed extraction shape for moving Trusted Server's Fastly Image Optimizer
> request metadata into EdgeZero.
> Date: 2026-05-28.

---

## Goal

Make the Image Optimizer support introduced for asset routes easy to extract
into `~/Projects/stackpop/edgezero` without moving Trusted Server-specific asset
routing, S3 authentication, or profile-table behavior.

The extraction should let a Trusted Server asset route produce a closed image
transformation request, attach it to an EdgeZero proxy request, and rely on the
Fastly adapter to translate that request into `fastly::image_optimizer` calls.

---

## Keep in Trusted Server

These pieces are product/application policy and should not move to EdgeZero:

- `proxy.asset_routes` configuration and route matching.
- Path rewrite behavior for asset origins.
- Top-level image profile tables and query controls such as `profile`, `ar`,
  `x`, `y`, and debug bypass.
- Origin query strip/preserve policy.
- S3 SigV4 credential loading and request signing.
- S3 preflight behavior used to avoid Image Optimizer masking origin errors.
- Response-header stripping for publisher-domain safety.

Trusted Server should continue converting route config plus request query into a
small platform-neutral options object.

---

## Move to EdgeZero

Move only the generic proxy capability shape:

```text
edgezero-core
  src/proxy/image_optimizer.rs
    ImageOptimizerOptions
    ImageOptimizerParams
    ImageOptimizerCrop
    ImageOptimizerCropMode
```

Then expose it through `edgezero_core::proxy`:

```rust
use edgezero_core::proxy::{ImageOptimizerOptions, ProxyRequest};

let request = ProxyRequest::new(method, uri).with_image_optimizer(options);
```

Recommended API shape:

```rust
pub struct ProxyRequest {
    // existing fields
    image_optimizer: Option<ImageOptimizerOptions>,
}

impl ProxyRequest {
    pub fn with_image_optimizer(mut self, options: ImageOptimizerOptions) -> Self;
    pub fn image_optimizer(&self) -> Option<&ImageOptimizerOptions>;
}
```

A first-class field/method is preferred over raw `Extensions` because adapters
must not silently ignore requested transformations.

---

## Adapter Contract

Adapters that support the capability should translate it at send time.
Adapters that do not support it should return an unsupported/internal proxy error
before sending the origin request.

Silent fallback is not acceptable: returning the unoptimized origin image would
make a successful HTTP response hide a platform capability failure.

### Fastly adapter

`edgezero-adapter-fastly` should map `ImageOptimizerOptions` to
`fastly::image_optimizer::ImageOptimizerOptions` and call
`FastlyRequest::set_image_optimizer()` before sending.

The current Fastly proxy path uses `send_async_streaming()`. Fastly IO requires a
separate branch because the request must be decorated before send:

```text
ProxyRequest without ImageOptimizerOptions
  -> existing send_async_streaming path

ProxyRequest with ImageOptimizerOptions
  -> build FastlyRequest
  -> set_image_optimizer(...)
  -> send(...)
  -> return streaming response body when supported
```

The initial Fastly IO branch may reject streaming request bodies if the Fastly SDK
path cannot combine request-body streaming with Image Optimizer. Asset image
routes currently send empty `GET`/`HEAD` bodies, so that limitation is acceptable
for the extraction.

### Cloudflare adapter

Cloudflare is compatible with the same EdgeZero proxy-capability direction, but
it should not be treated as identical to Fastly IO.

Cloudflare Workers expose image transformations by passing options on a fetch
subrequest:

```javascript
fetch(imageURL, { cf: { image: { fit: "scale-down", width: 800, height: 600 } } });
```

Relevant documented capabilities:

- custom Worker URL schemes and hidden origins are supported;
- origin access can be controlled in Worker code;
- width, height, fit, quality, and format are supported;
- `fit=cover`/`fit=crop` plus `gravity` provide crop behavior;
- `gravity=auto` provides saliency-based smart cropping;
- Worker coordinate gravity uses `{ x, y }` values from `0.0` to `1.0`;
- authenticated origins can be fetched with signed headers and
  `origin-auth = "share-publicly"` when caching privately-authenticated images
  as public variants is acceptable.

Sources:

- <https://developers.cloudflare.com/images/optimization/transformations/transform-via-workers/>
- <https://developers.cloudflare.com/images/optimization/features/>
- <https://developers.cloudflare.com/images/optimization/transformations/control-origin-access/>

Mapping notes for a future `edgezero-adapter-cloudflare` implementation:

| EdgeZero option | Cloudflare mapping |
| --- | --- |
| `quality = Some(n)` | `cf.image.quality = n` |
| `format = "avif"`, `"webp"`, `"jpeg"`, `"png"` | `cf.image.format = ...` |
| `format = "auto"` | inspect the request `Accept` header and choose AVIF/WebP, matching Cloudflare's Worker guidance |
| `width` / `height` | `cf.image.width` / `cf.image.height` |
| bare aspect-ratio crop plus width | derive target height and use `fit = "cover"` or `fit = "crop"` |
| `Smart` crop mode | `gravity = "auto"` |
| offset `x` / `y` in `0..=100` | `gravity = { x: x / 100.0, y: y / 100.0 }` |

The current Trusted Server DTO also contains Fastly-shaped fields:

- `region` has no Cloudflare equivalent.
- `preserve_query_string_on_origin_request` has no direct Cloudflare equivalent;
  Cloudflare fetches the exact source URL the adapter/request builder provides,
  so origin query policy should stay in Trusted Server's URL construction.
- `resize_filter` has no Cloudflare equivalent in the documented Worker image
  options.

For a Fastly-first extraction, keeping these fields is still acceptable because
unsupported adapters can reject metadata. If Cloudflare support is added soon,
prefer splitting common transform parameters from provider-specific options
before making the EdgeZero API public/stable.

### Other non-Fastly adapters

Spin and Axum adapters should initially reject `ImageOptimizerOptions` with a
clear unsupported-capability error. They can add provider-specific
implementations later without changing Trusted Server route logic.

---

## Type Shape

The current Trusted Server DTOs are intentionally close to the future EdgeZero
shape:

```rust
pub struct ImageOptimizerOptions {
    pub region: String,
    pub preserve_query_string_on_origin_request: bool,
    pub params: ImageOptimizerParams,
}

pub struct ImageOptimizerParams {
    pub format: Option<String>,
    pub quality: Option<u32>,
    pub resize_filter: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub crop: Option<ImageOptimizerCrop>,
}

pub struct ImageOptimizerCrop {
    pub width: u32,
    pub height: u32,
    pub mode: Option<ImageOptimizerCropMode>,
    pub offset_x: Option<u32>,
    pub offset_y: Option<u32>,
}

pub enum ImageOptimizerCropMode {
    Smart,
}
```

For the first EdgeZero extraction, copying this shape is acceptable if the
primitive is explicitly Fastly-first and unsupported adapters reject it. Before
shipping Cloudflare support or treating the EdgeZero API as stable, split common
fields from provider-specific fields:

```rust
pub struct ImageOptimizerOptions {
    pub params: ImageOptimizerParams,
    pub provider: ImageOptimizerProviderOptions,
}

pub enum ImageOptimizerProviderOptions {
    Fastly {
        region: String,
        preserve_query_string_on_origin_request: bool,
    },
    Cloudflare {
        origin_auth: Option<CloudflareOriginAuth>,
    },
    None,
}
```

A later public API cleanup can also replace stringly fields with enums/builders
once the primitive's cross-provider story is clearer.

---

## Trusted Server Migration Steps

1. Move the DTO file from `trusted-server-core::platform::image_optimizer` to
   `edgezero_core::proxy::image_optimizer`.
2. Replace `PlatformHttpRequest::image_optimizer` with
   `edgezero_core::proxy::ProxyRequest::image_optimizer` or the equivalent
   EdgeZero proxy request field.
3. Move the Fastly translation helpers from
   `trusted-server-adapter-fastly/src/platform.rs` into
   `edgezero-adapter-fastly/src/proxy.rs`.
4. Keep `asset_image_optimizer.rs`, `s3_sigv4.rs`, and asset-route handling in
   Trusted Server.
5. Make non-Fastly EdgeZero proxy clients reject requests carrying image
   optimizer metadata.

---

## Current PR Seam

This PR keeps runtime behavior in Trusted Server but isolates the future move by
placing the neutral DTOs in:

```text
crates/trusted-server-core/src/platform/image_optimizer.rs
```

That file is the primary extraction candidate for EdgeZero. The Fastly-specific
mapping remains isolated in:

```text
crates/trusted-server-adapter-fastly/src/platform.rs
```

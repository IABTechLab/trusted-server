# Asset Routes

Asset routes proxy selected first-party paths to an alternate asset origin. They are useful when a publisher-facing URL should stay stable while the bytes come from a CDN, a static origin, or a private S3 bucket.

Asset routes are separate from signed `/first-party/proxy` URLs. They match configured path prefixes directly, do not require `tstoken`, and are intended for publisher-owned asset paths such as images and static files.

## Request flow

For a matching `GET` or `HEAD` request, Trusted Server runs this sequence:

1. Match the longest configured `proxy.asset_routes` prefix.
2. Optionally rewrite the request path with `path_pattern` and `target_path`.
3. Build Image Optimizer metadata from the request query when the route enables it.
4. Apply the origin query policy.
5. Add origin authentication headers when the route configures auth.
6. Send the request to the resolved backend origin.
7. Return the origin status, body, and safe headers.

The origin query policy runs before S3 signing, so the signature covers the exact URL sent to S3. Image Optimizer metadata stays separate from the origin URL and is translated by the Fastly adapter.

## Basic route

```toml
[proxy]

[[proxy.asset_routes]]
prefix = "/assets/"
origin_url = "https://assets.example.com"
```

A request for `/assets/logo.png?v=1` is sent to:

```text
https://assets.example.com/assets/logo.png?v=1
```

Plain asset routes preserve the incoming query string by default.
`origin_url` must be only an absolute `http` or `https` origin (scheme, host, and optional port), without userinfo, path, query, or fragment components.

## Path rewrite

Use `path_pattern` and `target_path` when the public path shape differs from the origin object path.

```toml
[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://assets-cdn.example.com"
path_pattern = "^/\\.image/(.*)/[^/]+\\.([^/.]+)$"
target_path = "/image/upload/$1.$2"
```

Both fields must be configured together. The rewritten path must start with `/`.

## Private S3 origins

Add an auth block when the asset origin is a private S3 bucket.

```toml
[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
origin_query = "strip"
secret_store = "s3-auth"
access_key_id = "access_key_id"
secret_access_key = "secret_access_key"
# session_token = "session_token"
```

### S3 requirements

- `origin_url` must be the real S3 host that AWS validates in the SigV4 canonical request.
- Use `https` origins for authenticated routes. `http` origins are accepted by the generic asset route validator, but they would send SigV4 headers in clear text.
- S3 support is for `GET` and `HEAD` asset reads.
- Signing uses header-based AWS SigV4, not presigned URLs.
- The signer uses `x-amz-content-sha256: UNSIGNED-PAYLOAD`.
- Credentials are loaded from the configured runtime secret store and cached per process by configured secret names.
- Successful authenticated S3 responses preserve the origin `Cache-Control`; configure object cache headers intentionally.
- Existing client `Authorization` and `x-amz-*` signing headers are replaced before signing.

### Secret store values

The default secret store and key names are:

| Config field        | Default value       | Secret value                         |
| ------------------- | ------------------- | ------------------------------------ |
| `secret_store`      | `s3-auth`           | Secret store name                    |
| `access_key_id`     | `access_key_id`     | AWS access key ID                    |
| `secret_access_key` | `secret_access_key` | AWS secret access key                |
| `session_token`     | unset               | Optional AWS temporary session token |

Use private deployment configuration for environment-specific store names or profile tables.

## Origin query policy

`origin_query` controls whether the upstream origin receives the browser query string.

| Value      | Behavior                                            |
| ---------- | --------------------------------------------------- |
| `preserve` | Keep the incoming query string on the origin URL    |
| `strip`    | Remove the incoming query string before origin send |

Precedence:

1. `proxy.asset_routes.auth.origin_query`, when auth is configured.
2. `proxy.asset_routes.image_optimizer.origin_query`, when Image Optimizer is enabled.
3. The route default.

Defaults:

- Plain asset routes default to `preserve`.
- Image-optimized asset routes default to `strip`.
- Enabled Image Optimizer routes reject effective `preserve` to avoid arbitrary client query parameters becoming transformation inputs.

## Fastly Image Optimizer profiles

Image Optimizer support is configured in two places:

1. A top-level reusable profile set under `[image_optimizer.profile_sets.<name>]`.
2. A route-level `[proxy.asset_routes.image_optimizer]` block that selects the profile set and processing region.

```toml
[image_optimizer.profile_sets.default_images]
base_params = "quality=70&resize-filter=bicubic"
default_profile = "default"
unknown_profile = "use_default"
profile_param = "profile"
aspect_ratio_param = "ar"
debug_param = "_io_debug"

[image_optimizer.profile_sets.default_images.profiles]
default = "width=1920"
medium = "format=auto&width=828"
thumbnail = "width=150&crop=1:1,smart"

[image_optimizer.profile_sets.default_images.aspect_ratios]
allowed = ["1-1", "16-9", "4-3"]
profiles = ["medium"]

[image_optimizer.profile_sets.default_images.crop_offsets]
enabled = true
x_param = "x"
y_param = "y"
buckets = [10, 30, 50, 70, 90]
default = 50
when_missing = "smart"

[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
origin_query = "strip"

[proxy.asset_routes.image_optimizer]
enabled = true
region = "us_east"
profile_set = "default_images"
```

A request such as:

```text
/.image/id/example.jpg?profile=medium&ar=1-1&x=44&y=63
```

uses the `medium` profile, applies the allowed `1-1` crop override, buckets offsets to the configured values, strips the origin query, signs the S3 request if auth is enabled, and sends Fastly IO metadata through the Fastly adapter.

## Supported profile parameters

Profile strings intentionally accept a strict subset of Image Optimizer parameters.

| Parameter       | Example value | Notes                                                                            |
| --------------- | ------------- | -------------------------------------------------------------------------------- |
| `quality`       | `70`          | Integer in `0..=100`                                                             |
| `resize-filter` | `bicubic`     | `nearest`, `bilinear`, `bicubic`, `lanczos2`, `lanczos3`                         |
| `format`        | `auto`        | Also accepts `avif`, `gif`, `jpeg`, `jpg`, `jxl`, `jpegxl`, `mp4`, `png`, `webp` |
| `width`         | `828`         | Positive integer pixels                                                          |
| `height`        | `466`         | Positive integer pixels                                                          |
| `crop`          | `1:1,smart`   | Aspect ratio, optional `smart`, or paired `offset-xN,offset-yN` suffixes         |

Unknown profile parameters are configuration errors. Arbitrary client query parameters are not forwarded as Image Optimizer options.

## Debug bypass

Set the configured debug parameter to `1` to disable Image Optimizer for one request:

```text
/.image/id/example.jpg?profile=medium&_io_debug=1
```

The asset route still matches. Path rewrite, origin query policy, and S3 signing still run. The response comes from the unoptimized origin object.

## Local testing notes

Viceroy does not perform real Fastly Image Optimizer transformations. Local tests can verify routing, query stripping, SigV4 headers, and metadata attachment. End-to-end image transformation verification requires a deployed Fastly Compute service with Image Optimizer enabled.

## Security checklist

- Use a private S3 bucket policy that permits only the configured AWS principal.
- Store AWS credentials in Fastly Secret Store or an equivalent runtime secret store.
- Use HTTPS origins for authenticated routes so SigV4 headers are not transmitted in clear text.
- Set origin `Cache-Control` for authenticated S3 objects based on the intended audience: use public cache headers for public assets in private buckets, and `private` or `no-store` for assets that must not be shared from cache.
- Keep customer-specific profile names and profile tables in private deployment config when needed.
- Use `origin_query = "strip"` for image transformation routes unless the query string is part of the S3 object identity.
- Configure narrow path prefixes so asset routes do not capture unrelated application paths.

# Asset Routes

Asset routes proxy selected first-party paths to an alternate asset origin. They are useful when a publisher-facing URL should stay stable while the bytes come from a CDN, a static origin, or a private S3 bucket.

Asset routes are separate from signed `/first-party/proxy` URLs. They match configured path prefixes directly, do not require `tstoken`, and are intended for publisher-owned asset paths such as images and static files.

## Request flow

For a matching `GET` or `HEAD` request, Trusted Server runs this sequence:

1. Match the longest configured `proxy.asset_routes` prefix.
2. Optionally rewrite the request path with `path_pattern` and `target_path`.
3. Build Image Optimizer metadata from the request query when the route enables it.
4. Apply the origin query policy.
5. Add origin authentication headers when the route's source configures auth.
6. Send the request to the resolved backend origin.
7. Return the origin status, body, and safe headers.

The origin query policy runs before S3 signing, so the signature covers the exact URL sent to S3. Image Optimizer metadata stays separate from the origin URL and is translated by the Fastly adapter.

## Basic route

Each route names where its content comes from in a `source` table, in the same shape as the publisher's [`[source]`](/guide/configuration#source). `module = "origin"` selects core's HTTP origin, and `[proxy.asset_routes.source.origin]` holds its settings. Write the route's own keys, `prefix`, `path_pattern` and `target_path`, before the source tables, because a table header ends the route's own keys.

```toml
[proxy]

[[proxy.asset_routes]]
prefix = "/assets/"

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://assets.example.com"
```

A request for `/assets/logo.png?v=1` is sent to:

```text
https://assets.example.com/assets/logo.png?v=1
```

Plain asset routes preserve the incoming query string by default.
`url` must be an absolute `http` or `https` address with a host, without a user name or password, a query, a fragment, or a trailing `/`. A path in `url` names a folder, and the route's target path is added under it. With `url = "https://storage.example.com/bucket/site"`, a request for `/assets/logo.png` is sent to `https://storage.example.com/bucket/site/assets/logo.png`.

`host_header` sets the outbound `Host` header where it is not the host of `url`. Trusted Server still connects, uses SNI, and checks certificates against the host in `url`.

## Path rewrite

Use `path_pattern` and `target_path` when the public path shape differs from the origin object path.

```toml
[[proxy.asset_routes]]
prefix = "/.image/"
path_pattern = "^/\\.image/(.*)/[^/]+\\.([^/.]+)$"
target_path = "/image/upload/$1.$2"

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://assets-cdn.example.com"
```

Both fields must be configured together. The rewritten path must start with `/`. When `url` has a path, the rewritten path is added under that folder.

## Signed origins

Add an auth block to the route's source when the asset origin answers only signed reads. `type` names an origin signer a crate in the build offers, and the rest of the table is that signer's. A stock build offers `s3_sigv4`, from `crates/shared/aws`, for a private S3 bucket.

```toml
[[proxy.asset_routes]]
prefix = "/.image/"

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://bucket.s3.us-east-1.amazonaws.com"

[proxy.asset_routes.source.origin.auth]
type = "s3_sigv4"
region = "us-east-1"
origin_query = "strip"
access_key_id = "s3_access_key_id"
secret_access_key = "s3_secret_access_key"
# session_token = "s3_session_token"
```

### S3 requirements

- `url` must be the real S3 host that AWS validates in the SigV4 canonical request.
- `url` must use `https` when the route signs. A `type` no crate in the build offers is refused at startup naming the signers on offer, and a signing table the signer cannot use, such as an empty or malformed `region`, is refused at startup and by `ts config validate` naming the route, as in `proxy.asset_routes /.image/ source.origin.auth`.
- S3 support is for `GET` and `HEAD` asset reads.
- Signing uses header-based AWS SigV4, not presigned URLs.
- The signer uses `x-amz-content-sha256: UNSIGNED-PAYLOAD`.
- Credential references resolve from the logical `trusted_server_secrets` store while runtime settings are built. Signing performs no request-time secret-store reads.
- Successful authenticated S3 responses preserve the origin `Cache-Control`; configure object cache headers intentionally.
- Existing client `Authorization` and `x-amz-*` signing headers are replaced before signing.

### Secret store values

Credential fields contain secret key references:

| Config field        | Default key         | Resolved value                       |
| ------------------- | ------------------- | ------------------------------------ |
| `access_key_id`     | `access_key_id`     | AWS access key ID                    |
| `secret_access_key` | `secret_access_key` | AWS secret access key                |
| `session_token`     | unset               | Optional AWS temporary session token |

Place those values in the logical `trusted_server_secrets` store. Adapter configuration maps that logical ID to an environment-specific physical store such as Fastly `ts_secrets`. The legacy `secret_store` field is accepted by the signer and ignored.

## Origin query policy

`origin_query` controls whether the upstream origin receives the browser query string.

| Value      | Behavior                                            |
| ---------- | --------------------------------------------------- |
| `preserve` | Keep the incoming query string on the origin URL    |
| `strip`    | Remove the incoming query string before origin send |

Precedence:

1. `proxy.asset_routes.source.origin.auth.origin_query`, when auth is configured.
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

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://bucket.s3.us-east-1.amazonaws.com"

[proxy.asset_routes.source.origin.auth]
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

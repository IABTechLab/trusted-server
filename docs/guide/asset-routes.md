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

Every adapter serves asset routes. The Axum, Cloudflare and Spin adapters hold the asset body whole, which Cloudflare limits to 10 MiB and Spin to 8 MiB, answering a larger one with `502`. They cannot optimize images, so each logs a warning naming every route that enables the Image Optimizer, once for each instance of the adapter, which on Axum is the process (the [Cloudflare](./cloudflare) and [Spin](./spin) guides say where the lines appear), and answers each request such a route would optimize with `502 Bad Gateway` and `Cache-Control: no-store, private`. A signed route first asks its origin for the object with a `HEAD`, and where that fails it answers with the origin's own error. SVG and SVGZ paths, and a request that sets the profile set's debug parameter to `1`, carry no Image Optimizer metadata, so they are served unoptimized.

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

`host_header` sets the outbound `Host` header where it is not the host of `url`. Trusted Server still connects, uses SNI, and checks certificates against the host in `url`. Spin sends the host in `url` as the `Host`, so there every request under a route whose `host_header` names another host answers `502` before its origin is asked. Cloudflare documents that a Worker cannot set a `Host` naming a host outside its zone unless the request goes to that host (see [Cloudflare](./cloudflare)).

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

## Optimized images and C2PA provenance

Where a media signer is installed, such as the Encypher provenance module, an optimized request also asks Fastly Image Optimizer to keep its input's C2PA manifest (`metadata=c2pa`). Image Optimizer then writes a manifest of its own that records the transformations it performed and binds the input it read by hash. The answer is signed only when its body carries a C2PA manifest store, and the signature records no operation of Trusted Server's own: Image Optimizer's manifest is the parent of what is signed, and the signed answer carries `Vary: Accept`.

Image Optimizer writes that manifest only when its input already carries one. An optimized answer of an image without C2PA provenance records nothing about what was done to it, so it is delivered as Image Optimizer made it, unsigned. Only a whole `200` answer to a `GET` is signed; ranges and `HEAD` requests on an optimized route are answered as Image Optimizer answers them. The SDK's metadata option offers only `copyright`, so the adapter sends the filters as the `metadata` query parameter, which Image Optimizer removes from its origin request, so an S3 signature over the origin address is unaffected. Where the request already asks for the `copyright` filter, the adapter sends `metadata=copyright,c2pa`, which applies both.

Fastly enables the `c2pa` filter for every image format Image Optimizer supports, and notes that some C2PA verification tooling may not handle JPEG XL. Whether Trusted Server signs an optimized answer depends on the media signer accepting the format it reads from the answer's bytes. None of this has been observed on a deployed Image Optimizer service yet: Viceroy does not run Image Optimizer.

Two ways give Image Optimizer a signed input:

- The origin serves C2PA-signed originals.
- The route sets `signed_source = true`. Image Optimizer is then sent to this host, at the reader's path with the profile set's debug parameter set to `1` and no other query, instead of to the origin. The route answers that request without Image Optimizer, with the exact origin bytes it captured, signed with a manifest that opens them. Image Optimizer keeps that manifest and adds its own, so the chain reads: the signed original, Image Optimizer's transformations, and the signature over Image Optimizer's answer. Every optimized image costs this service a second request, and the request Image Optimizer sends reaches this host as a reader's would. Where Image Optimizer answers with a `4xx` or `5xx` or cannot be reached, for example while signing is refused and the original goes out `private`, which Image Optimizer will not transform, the route answers once from the origin without Image Optimizer, so an origin's own error still reaches the reader as the origin made it.

An optimized answer whose origin headers mark it as one reader's (`private`, `no-store`, a cookie or a `Vary` on the reader) is never signed, and keeps the origin's own `Cache-Control` in place of the route's `[[cache.asset_rules]]` policy.

The manifests embedded in an optimized image are history. A policy revocation stops the signature Trusted Server adds to each answer, but an image Image Optimizer already holds keeps the earlier manifests until its cache expires it, bounded by the origin's own `Cache-Control`. Revoking the signing certificate is what invalidates those manifests for verifiers. The debug parameter is public, so any reader can request the signed, unoptimized original the same way. On Axum, Cloudflare and Spin, whose HTTP clients refuse Image Optimizer metadata, a `signed_source` route therefore answers from the origin, signed and unoptimized, where a route without it answers `502`.

## Rehosting third-party files

A route that sets `rehost_page_references = true` serves a third party's public files from the publisher's own host, so a media signer can sign them and a stylesheet `[[serve]]` entry can run on them. Pages served through the publisher pipeline then name the route's address in place of the third party's.

This covers eligible bytes that pass through the configured routes, not every
media item a browser may load. Direct third-party frame or script requests
and SVG created later by scripts remain outside these routes. See the
[provenance coverage limits](./configuration#placing-page-changes) before
claiming page-wide coverage.

```toml
[[proxy.asset_routes]]
prefix = "/_rehost/assets.example.com/library/"
rehost_page_references = true

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://assets.example.com/library"

[[serve]]
media_type = "text/css"
path = "/_rehost/assets.example.com/library/carousel/"
middleware = ["provenance.encypher.stylesheet"]
```

The route's request path below its prefix is appended to the origin's path, byte for byte, and the reader's query is sent as written. The prefix must end in `/` and must not sit under `/_ts`. Settings are refused at startup where the route also sets `path_pattern`, `target_path` or `image_optimizer`, where its source is not the `origin` source over `https`, signs its origin requests, overrides `Host` with an authority other than the URL's own (including a non-default port), sets a `directory_index` or strips the query, or where two rehosting routes overlap.

### What is rewritten in a page

An address is rewritten only where it names a file inside a rehosting route's origin folder, the route that request dispatch would pick for the rewritten path is that same route, and that route fetches exactly the address the page wrote. The rewritten address keeps the page's query and fragment, and carries any path prefix request addressing removed.

When any rehosting route is enabled, the publisher buffers the complete decoded original HTML under `publisher.max_buffered_body_bytes` before rewriting. The first effective HTML `base[href]` resolves against the full fetched document URL, including its path and query. It applies even to references preceding that element; later bases, comments, raw text, template contents and foreign-namespace base elements do not replace it. Relative references resolve against that base, and protocol-relative references take its scheme, never an automatic HTTPS upgrade. An oversized or unreadable original is refused before any rewritten output is emitted. Deployments with no rehosting route keep the normal streaming path.

HTML attribute references are decoded before URL resolution and serialized once afterward. Percent escapes, query ordering and fragments keep their decoded-attribute bytes. In both `srcset` and `imagesrcset`, the publisher-origin fallback changes only a leading URL authority or supported bare host with a host boundary. A publisher name inside another host, path, query or fragment stays unchanged.

These references are read:

- media: `img`, `source`, `video`, `audio` and `track` `src`, `img` and `source` `srcset` candidates, `link` `imagesrcset` candidates, `video` `poster`, `input type="image"` `src`, and `link rel="preload"` or `rel="prefetch"` with `as` set to `image`, `font`, `audio`, `video` or `track`;
- stylesheets: `link rel="stylesheet"` `href`, and preloads with `as="style"`. A stylesheet is rewritten only where a `[[serve]]` entry for `text/css` covers its route path, because a stylesheet served from the route unmoved would name its relative files at the wrong address.

An element with an `integrity` attribute keeps its address, since the browser would refuse any body other than the one hashed. An address already on a rehosting route is left as it is. Any other address is left to the page's other rewriters.

### What the route answers

A rehosting route sends no `Referer` to the third party, and asks for `Accept-Encoding: identity`, so a body it moves or signs is never compressed.

- A redirect is answered with its `Location` resolved against the upstream address and written as a rehosting route's address where one serves it, otherwise as the absolute upstream address. Nothing follows a redirect at the edge. Its discarded origin body contributes no encoding, length, digest, validator or range metadata to the empty response; cache and privacy headers remain applicable.
- A stylesheet whose route path a `[[serve]]` entry covers is moved: each `url()`, `src()`, `image-set()` candidate and `@import` it names is resolved against its upstream address and written as a rehosting route's address where one serves the file (an `@import` only where its own path is covered), or as the absolute upstream address. Fragment-only references and `data:` addresses stay as written. The entry's middleware then run on the moved stylesheet, and the answer carries no validator, `Content-Encoding` or range support of the origin's, since its bytes are this service's.
- A stylesheet that cannot be moved is answered with a `307` to its upstream address, held by no cache (`Cache-Control: private, no-store`). That covers a stylesheet no `[[serve]]` entry covers, a status other than `200`, a body still content-coded, a `private` or `no-transform` origin answer, a charset other than UTF-8 (in `Content-Type` or a leading `@charset`), a body over 10 MiB, or a refusal by the entry's middleware.
- A `HEAD` request for a moved stylesheet is answered with the origin's metadata only, without its length or validators.
- Anything else is media, answered as any route's is, and signed where a media signer accepts it.

On a route path a stylesheet entry covers, the origin is asked without the reader's conditional or range headers, and `If-Match`, `If-None-Match`, `If-Modified-Since` and `If-Unmodified-Since` are answered against what is delivered, by its own `ETag` and `Last-Modified`. A moved stylesheet carries neither, so `If-Match` on it gives `412`. A range is not applied on those paths: the whole answer is sent.

CSS-chain candidate paths bypass the route's raw-copy cache before the response MIME type is known, including images or fonts under the same prefix. A moved stylesheet keeps browser freshness (`max-age`) separate from shared freshness (`s-maxage`, falling back to `max-age`). Each subtracts the greater of received `Age` and apparent age from `Date`, plus elapsed processing time, then takes its corresponding browser or shared route cap. These bounds run after middleware headers and survive adapter finalization. Missing, malformed, expired or non-reusable origin freshness authorizes no fresh copy. Only after calculating the remaining lifetimes does rebasing remove the origin's `Age`, `Date` and `Expires`; stale allowances and `immutable` do not transfer to the new representation. Streamed stylesheet bodies use the existing bounded collector, which checks remaining capacity before appending a chunk. Buffered bodies reuse their owned bytes without copying.

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

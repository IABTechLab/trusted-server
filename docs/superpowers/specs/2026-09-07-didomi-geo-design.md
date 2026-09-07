# Didomi loader geo forwarding — issue 85

Status: Revised design for publisher review. No runtime implementation is included.

## Outcome

When Trusted Server serves a Didomi notice loader, it resolves trusted visitor geo
and redirects the browser to the same first-party loader URL with `country` and
`region` query parameters. It then proxies the canonical request to Didomi while
preserving the SDK origin's cache headers.

For example, a browser request to:

```text
https://publisher.example.com/integrations/didomi/consent/example-key/loader.js?target_type=notice&target=example-notice
```

with platform geo `US` / `CA` receives a temporary redirect to:

```text
https://publisher.example.com/integrations/didomi/consent/example-key/loader.js?target_type=notice&target=example-notice&country=US&region=CA
```

Trusted Server proxies that canonical request to the configured SDK origin with
the same path and query. The browser URL, downstream cache key, and upstream cache
key therefore identify the same geographic variant.

## Requirements and evidence

Issue 85 asks Trusted Server to produce the country and region query parameters
that Didomi's browser configuration normally adds to the notice-loader URL.

Didomi's reverse-proxy contract requires:

- ISO 3166-1 alpha-2 country and ISO 3166-2 region codes on loader requests;
- SDK cache separation by the full path, query, country, and region;
- preservation of the SDK origin's cache and freshness headers downstream;
- no caching for requests to the API origin;
- no cookies forwarded upstream; and
- the visitor IP forwarded in `X-Forwarded-For`.

Sources: [issue 85](https://github.com/IABTechLab/trusted-server/issues/85) and
[Didomi reverse-proxy guidance](https://developers.didomi.io/api-and-platform/domains/reverse-proxy).
The live vendor contract must be reconfirmed during staging because it can change.

Current code in `crates/trusted-server-core/src/integrations/didomi.rs` preserves
the incoming query but does not add geo. It copies selected Fastly-style geo
headers to the SDK origin and forwards the trusted client IP from runtime
services. The JavaScript integration only sets `window.didomiConfig.sdkPath`.

`PlatformGeo` already supplies request-scoped `GeoInfo`. Fastly provides country
and optional region. Cloudflare currently provides country only. Axum and Spin
provide no geo.

## Design choice

Use a canonical same-origin redirect for the notice loader.

| Approach                                    | Benefit                                                                    | Reason not selected                                                                                                      |
| ------------------------------------------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Populate `window.didomiConfig.user` in HTML | Avoids a redirect                                                          | Per-reader geo cannot enter shared HTML templates without a separate assembly design                                     |
| Enrich only the outbound URL                | No extra browser request                                                   | The browser caches the geo-specific response under a geo-less URL and can reuse the wrong notice after a location change |
| Canonical same-origin redirect — selected   | Qualifies browser, CDN, and origin cache keys while keeping HTML shareable | Adds one non-cacheable redirect request when loading the notice                                                          |

Every page continues using the stable, geo-less embed URL. Trusted Server resolves
geo and returns `307 Temporary Redirect` to the same path with canonical geo
parameters. A later location change produces a different target instead of reusing
a loader cached under a geo-less URL.

## Configuration and compatibility

Add `geo_query_parameters: bool` to `DidomiIntegrationConfig`, defaulting to
`false`. Existing deployments retain their current behavior. Publishers opt in
after verifying complete platform geo and cache configuration.

```toml
[integrations.didomi]
enabled = true
geo_query_parameters = true
```

This is application configuration, not adapter process configuration. Operators
set it in their private `trusted-server.toml` and publish it with
`ts config push --adapter <adapter>`. The CLI deserializes and validates the file
as `TrustedServerAppConfig`, serializes the resolved `Settings` into an EdgeZero
blob envelope, and writes that envelope to the adapter's configured app-config
store. The Fastly adapter loads that blob through
`get_settings_from_config_store`, verifies the envelope, reconstructs `Settings`,
and constructs the `IntegrationRegistry`.
`settings.integration_config::<DidomiIntegrationConfig>()` then deserializes and
validates the Didomi object when the registry registers the integration.

Document the TOML field as the normal way to configure this feature. The deployed
runtime does not read a `TRUSTED_SERVER__INTEGRATIONS__...` variable. EdgeZero's
CLI can optionally apply the mechanically derived
`TRUSTED_SERVER__INTEGRATIONS__DIDOMI__GEO_QUERY_PARAMETERS` overlay while
validating or pushing config, but that is only a push-time convenience. It works
only when the scalar leaf already exists in the input TOML and does not change a
running deployment by itself.

Add `geo_query_parameters = false` to the active disabled Didomi stub in
`trusted-server.example.toml`. This gives `ts config init` users a discoverable
field while keeping `ts audit` behavior stable: audit may flip `enabled` when it
detects Didomi, but geo canonicalization remains an explicit publisher choice.

When disabled, loader handling remains unchanged by this feature. API cache bypass
and downstream API no-store behavior apply regardless because API responses must
never be cached. When enabled, eligible loaders use the redirect contract and fail
closed if complete trusted geo is unavailable.

The first implementation supports Fastly only. Cloudflare, Axum, and Spin retain
current loader behavior with the option disabled. Configuration documentation must
say that enabling it on those adapters is unsupported.

## Trust and precedence

Platform geo is authoritative. Query parameters, request headers, cookies, and
`window.didomiConfig` are not trusted geo inputs.

For an eligible request with `geo_query_parameters = true`:

1. Resolve geo with
   `services.geo().lookup(services.client_info().client_ip)`.
2. Accept only a complete, structurally valid country/region pair supplied by the
   adapter's documented ISO-code contract.
3. Remove every incoming country/region field and append the trusted pair to a
   canonical relative URL.
4. Redirect without contacting Didomi when the incoming URL is not canonical.
5. Proxy to Didomi when the incoming URL already equals the canonical form.

Parameter-name comparison is ASCII case-insensitive after one application of URL
query decoding. Variants such as `Country` and `%63ountry`, including duplicates,
cannot remain beside the authoritative values.

Publisher URL overrides are intentionally unsupported. Letting browser values
select a shared upstream entry would require a maintained ISO 3166-1/3166-2
registry and verified Didomi behavior for unsupported pairs. Issue 85 does not
require that configuration surface.

Normalize the trusted platform pair as follows:

- trim ASCII whitespace and uppercase both fields;
- country must be exactly two ASCII letters and not `XX` or `ZZ`;
- region must be one to three ASCII alphanumeric characters;
- for a country-prefixed region such as `US-CA`, require the prefix to match and
  send only the subdivision portion (`CA`);
- never infer geo from language, city, coordinates, arbitrary headers, or the
  forwarded client IP inside core.

Structural checks guard adapter bugs. ISO membership derives from the adapter's
provider contract, rather than a syntactically plausible browser value.

## Incomplete geo

| State                          | Loader behavior                           | Cache behavior                       |
| ------------------------------ | ----------------------------------------- | ------------------------------------ |
| Complete trusted pair          | Canonicalize and proxy                    | Cache final URL using Didomi headers |
| Geo unavailable                | Return `503`; do not contact Didomi       | `private, no-store`                  |
| Lookup failure                 | Log a value-free warning and return `503` | `private, no-store`                  |
| Invalid or country-only result | Log a value-free warning and return `503` | `private, no-store`                  |

Failing closed prevents an incorrect notice and avoids querying Didomi once per
visitor without a safe geo cache key. The response is a small generic integration
error with no location data and no fallback to caller-supplied geo.

Fastly can legitimately omit region for territories without an ISO subdivision.
Those locations are explicitly unsupported in the first release and receive the
same `503` when the option is enabled. Publishers must accept this boundary before
global enablement. Supporting those territories requires separate Didomi guidance
for a canonical cache-safe representation; this design does not invent one.

## Loader matching and canonical URL

Apply the feature only to SDK `GET` requests whose path relative to the configured
proxy prefix is exactly `/<public-key>/loader.js`, where `<public-key>` is one
nonempty segment. Do not match `/loader.js`, API paths, POST, other SDK assets,
suffix lookalikes, trailing slashes, or deeper paths. Match case-sensitively and do
not perform a second path-decoding pass.

Do not append `country` or `region` to API-origin requests. Didomi's current
contract uses these query parameters to select the notice loader; API paths retain
their incoming path, query, method, headers, and body while following the separate
no-cache rules below.

Parse and serialize the query with the existing `url` crate's WHATWG form-URL
encoding behavior. Preserve every unrelated decoded name/value pair, including
order, duplicates, and empty values. Remove every pair whose decoded name is a geo
field, then append exactly one `country` and `region`, in that order. Canonical
serialization may normalize equivalent raw spellings—for example, percent escapes,
spaces, plus signs, and an unescaped apostrophe—but must not change the decoded
unrelated names or values. The request is canonical only when its path and query
equal this serialized form. Canonicalization is idempotent.

For a non-canonical request, put the constructed path and query in a relative
same-origin `Location`. Never derive an authority from `Host` or forwarded-host
headers. Return `307 Temporary Redirect` with `Cache-Control: private, no-store`;
remove validators and independent edge-cache headers with the shared response
privacy utility. Do not contact either Didomi origin.

For a canonical request, build the Didomi target from the same serialized path and
query. Passing it through the real Fastly request conversion must produce the same
canonical query; an adapter-level test covers characters such as an apostrophe that
`http::Uri` and `url::Url` represent differently. Generate `X-Geo-Country`,
`X-Geo-Region`, and `CloudFront-Viewer-Country` from the same normalized pair. Do
not copy caller-supplied Fastly geo headers on this path. Continue deriving
`X-Forwarded-For` from `RuntimeServices.client_info`. Do not forward cookies or
publisher `Authorization`.

Other SDK assets retain current query and header behavior. This issue does not
claim that every existing Didomi SDK path satisfies the vendor's full hosting
contract; that audit remains separate.

## Cache behavior

The geo-less entry URL is private and non-storable. The final browser URL contains
geo, so browser and shared-cache entries naturally separate locations by the full
query. Operators must ensure all shared caches retain the full path and query in
their key and honor redirect no-store. A cache rule that drops or normalizes these
parameters is incompatible.

Send canonical loader requests through the platform's normal outbound cache. The
full enriched URL is the variant key. Preserve Didomi's `Cache-Control`,
`Expires`, `ETag`, `Last-Modified`, `Age`, and CDN cache headers on the response.

All API-origin requests use `PlatformHttpRequest::with_cache_bypass()`. Their
responses use `Cache-Control: private, no-store`; independent edge-cache headers
and freshness validators are removed with shared response-privacy utilities. SDK
status and body forwarding otherwise remain unchanged, including upstream errors.

Adapter-specific cache behavior cannot be proven by core unit tests. Staging must
demonstrate distinct variants, reuse within a variant, vendor TTL handling,
conditional validation, and correct response-header propagation.

## Cloudflare follow-up

Cloudflare documents `request.cf.regionCode` as an ISO 3166-2 first-level region
code, but pinned EdgeZero v0.0.7 discards `request.cf` during conversion. Its typed
Cloudflare context retains only `Env` and `Context`.

Cloudflare parity is outside the first implementation and its acceptance gate. A
follow-up must:

1. extend `edgezero-adapter-cloudflare` to capture `request.cf.country()` and
   `request.cf.region_code()` in a private typed request extension before consuming
   the Worker request;
2. release and pin the new EdgeZero version;
3. consume that extension in `trusted-server-adapter-cloudflare`; and
4. verify valid, missing-`request.cf`, and missing-region behavior on wasm32 and
   native stubs.

A caller-writable `cf-region-code` header must not become an authority. Until this
follow-up lands, documentation and release notes list Cloudflare as unsupported for
`geo_query_parameters = true`.

## Errors and observability

Do not log geo values, client IPs, full URLs, or query strings. Lookup failure and
incomplete geo log one warning containing the integration and a bounded reason
category. Invalid incoming geo is silently replaced because it is not an input.
Existing URL, backend, body-size, and transport failures retain current
`error-stack` handling.

Do not add location-valued metrics. Bounded failure reasons may use an existing
integration metric if one exists; otherwise logging is sufficient. No adapter name
is required because `RuntimeServices` does not expose one.

## Implementation boundaries

- `crates/trusted-server-core/src/integrations/didomi.rs`: configuration flag,
  loader matcher, geo resolution and normalization, `url`-canonical redirect,
  coherent geo headers, failure response, SDK cache preservation, and API cache
  privacy.
- `trusted-server.example.toml`: add the disabled-by-default field to the existing
  active Didomi stub. Operator-owned `trusted-server.toml` files remain untracked.
- `crates/trusted-server-core/src/config.rs` and
  `crates/trusted-server-core/src/config_payload.rs`: no new configuration
  transport; existing typed deploy validation and blob deserialization must
  exercise the added field.
- `crates/trusted-server-adapter-fastly/src/platform.rs`: test that canonical query
  serialization survives conversion to a Fastly request; no production behavior
  change is expected.
- `docs/guide/integrations/didomi.md`: option, redirect flow, precedence, loader
  path, Fastly support, excluded geographies, failure behavior, full-query cache
  requirement, API no-cache behavior, and rollout checks. Correct the existing
  claim that publisher `Authorization` is forwarded.
- No production JavaScript, shared HTML-template, Cloudflare, Axum, Spin, or
  EdgeZero changes in the first implementation.

Keep helpers private. Do not add an ISO registry dependency, publisher override,
general geo endpoint, or shared integration interface.

## Acceptance criteria

1. The new option defaults to disabled. Existing loader behavior remains unchanged
   in that state. The example template exposes it as `false`; typed TOML and pushed
   blob round-trip tests preserve it. Default and custom prefixes work when
   enabled.
2. A geo-less `/<public-key>/loader.js` redirects once to its relative same-origin
   canonical URL with normalized country and region. The redirect does not call
   upstream and is private and non-storable without validators or edge-cache
   directives.
3. Incoming lowercase, mixed-case, duplicated, and percent-encoded geo field names
   are replaced. Unrelated decoded pairs retain their order, duplicates, empty
   values, names, and values after documented WHATWG serialization.
4. A canonical request calls the SDK origin once with the same canonical path and
   query. Core and Fastly conversion tests cover percent escapes, spaces, plus
   signs, apostrophes, duplicates, and empty values. Its URL and three geo
   compatibility headers contain the same trusted pair. Caller geo headers cannot
   affect them.
5. Matching rejects `/loader.js`, API paths, other assets, POST, filename
   lookalikes, extra segments, and trailing slashes.
6. Missing, failed, invalid, and country-only geo—including a no-subdivision
   territory—return a private, non-storable `503` without an upstream call when
   enabled. Disabled mode retains current behavior.
7. Canonical SDK responses preserve all upstream cache and validator headers. API
   requests bypass outbound cache and return without shared-cache or validator
   headers.
8. IP forwarding still uses client info; cookies and publisher Authorization stay
   excluded. Existing SDK CORS behavior and unrelated POST body limits remain.
9. A browser-level staging test follows the redirect and executes the Didomi
   script under the canonical URL. Changing the same browser's simulated location
   yields a different final URL and correct notice instead of prior cached content.
10. Repeated requests for one pair demonstrate cache reuse and vendor TTL/validator
    behavior. Same-pair requests with different IPs confirm the loader is
    interchangeable.
11. Publisher validation confirms expected notices for at least two locations and
    accepts the visible failure boundary for unsupported geo.

## Verification and rollout

Implementation verification follows `CLAUDE.md`: target-matched adapter tests and
clippy aliases, Rust formatting, integration parity, JavaScript build/tests/format,
and docs formatting. Do not use bare `cargo test --workspace`.

Configuration verification must cover the real operator path: initialize or use a
fixture `trusted-server.toml`, validate it through `TrustedServerAppConfig`, verify
the serialized blob retains the Didomi field, and load that blob back into
`Settings`. A process environment variable is not a runtime acceptance path.

Before production, verify the live Didomi contract, same-origin redirect and CSP
compatibility, complete Fastly geo coverage, the accepted no-subdivision exclusion,
full-query cache keys, cache reuse, conditional requests, and response-header
preservation. Purge previously cached loader and API responses before enabling the
option. Do not enable it on unsupported adapters.

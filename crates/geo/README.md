# Geo providers

Geo and IP-intelligence provider crates live here, one per implementation, each
implementing the `PlatformGeo` trait from `trusted-server-core`:

- `crates/geo/fastly` (`trusted-server-geo-fastly`) is the host platform geo
  provider for Fastly Compute, wrapping Fastly's `geo_lookup`. The Fastly adapter
  injects it via `build_geo_provider`. It depends on the Fastly SDK, so it builds
  only for `wasm32-wasip1`.
- Vendor geo providers (for example `crates/geo/<vendor>`) will live alongside
  it, one per vendor, selected by the `[geo] provider` setting.

Whatever the source, a provider returns the same `GeoInfo` coding. The country
is an ISO 3166-1 alpha-2 code (`US`) and the region is the ISO 3166-2 subdivision
code with no country prefix (`CA`). The permission model keys its country and
region rules on these codes, matched case-insensitively, so the Fastly and
other providers feed the same rules without translation.

The platform-neutral `PlatformGeo` trait and the `DisabledGeo` default (no
location) both live in `trusted-server-core`, so the default deployment resolves
no location until a provider is selected.

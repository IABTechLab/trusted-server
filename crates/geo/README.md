# Geo modules

Geo and IP-intelligence module crates live here, one per implementation, each
implementing the `PlatformGeo` trait from `trusted-server-core`:

- `crates/geo/fastly` (`trusted-server-geo-fastly`) is the host platform geo
  module for Fastly Compute, wrapping Fastly's `geo_lookup`. The Fastly adapter
  injects it via `build_geo_module`. It depends on the Fastly SDK, so it builds
  only for `wasm32-wasip1`.
- Vendor geo modules (for example `crates/geo/<vendor>`) will live alongside
  it, one per vendor, selected by the `[geo] module` setting.

Whatever the source, a module returns the same `GeoInfo` coding. The country
is an ISO 3166-1 alpha-2 code (`US`) and the region is the ISO 3166-2 subdivision
code with no country prefix (`CA`), so the Fastly and other modules feed the
same downstream rules without translation.

The platform-neutral `PlatformGeo` trait and `DisabledGeo` both live in
`trusted-server-core`. With no `[geo] module`, or `module = "platform"`, the
adapter uses its host platform's own lookup. `module = "none"` selects
`DisabledGeo`, which resolves no location and makes no host call.

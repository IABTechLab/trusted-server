# Synthetic GeoIP fixtures

These original Apache-2.0 test fixtures come from the pinned EdgeZero revision
`fda87556f5aab160711eb5244c1d1065a6bb48d6`, under
`crates/edgezero-adapter-axum/tests/fixtures/geoip/`. They contain no production
GeoLite data. EdgeZero's `scripts/generate_geoip_fixtures.mjs` regenerates them.

- `city.mmdb` maps loopback and 192.0.2.1 to US/CA, and 192.0.2.2 to country-only GB.
- `country.mmdb` exercises country-only projection.
- `ipv6.mmdb` maps 2001:db8::/32 to GB/ENG.

Loopback's synthetic location makes accidental proxy-IP lookup observable.
Real operator datasets stay outside this repository.

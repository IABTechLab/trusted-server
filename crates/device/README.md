# Device providers

Device-detection provider crates live here, one per vendor. The Fastly provider
(`trusted-server-device-fastly`) classifies a request with the host's TLS and
HTTP/2 fingerprints; future vendor providers (for example
`crates/device/<vendor>`) slot in alongside it.

The built-in default provider (User-Agent only) ships in `trusted-server-core`
(`ec::device`). Adapters select and inject the vendor provider via
`build_device_provider`.

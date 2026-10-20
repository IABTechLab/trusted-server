# Device modules

Device-detection module crates live here, one per vendor. The Fastly module
(`trusted-server-device-fastly`) classifies a request with the host's TLS and
HTTP/2 signals. Future vendor modules (for example
`crates/device/<vendor>`) slot in alongside it.

The built-in default module (User-Agent only) ships in `trusted-server-core`
(`ec::device`). Adapters select and inject the vendor module via
`build_device_module`.

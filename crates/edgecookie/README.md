# Edge Cookie providers

Vendor Edge Cookie provider crates live here, one per vendor, for example
`crates/edgecookie/<vendor>`. Each implements the `EdgeCookieProvider` trait
from `trusted-server-core` and is wired in by an adapter.

The built-in default provider (HMAC over the client IP) ships in
`trusted-server-core` (`ec::provider`), so no crate is needed for it. This
directory is a placeholder until a vendor provider is added.

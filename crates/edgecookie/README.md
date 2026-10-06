# Edge Cookie modules

Vendor Edge Cookie module crates live here, one per vendor, for example
`crates/edgecookie/<vendor>`. Each implements the `EdgeCookieModule` trait
from `trusted-server-core` and is wired in by an adapter.

The built-in HMAC module (HMAC over the client IP) ships in
`trusted-server-core` (`ec::module`), so no crate is needed for it. There is
no default module, and a deployment selects one explicitly with
`[ec] module`.

A module's own settings live in the `[ec.<name>]` table the selector names.
The name is the module's implementation id, the same string its
`EdgeCookieModule::id` returns, unless the table names one with
`implementation = "<id>"`, which lets an operator configure a module under a
name of their own choosing. A module with no settings needs no table.

This directory is a placeholder until a vendor module is added.

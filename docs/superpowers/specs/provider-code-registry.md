# Provider-code registry (normative, append-only, never reused)

Four-character codes (`[a-z0-9]`, zero-padded) that namespace Edge Cookie
identifiers. Every EC provider MUST allocate a code here before it can
exist, because the `EdgeCookieProvider::code()` trait method is mandatory, and core
applies the code as the `{code}~` prefix of every identifier the provider
creates, checks it at read-back, and keys the identity graph with it. A
provider only ever sees its own value part, so identifiers from different
providers can never collide in the cookie, the graph, or a withdrawal, and
every identifier records which provider created it.

Allocation is a reviewed commit to this file. Codes are immutable and never
recycled, including for retired providers. A leading digit is valid. The
tilde separator keeps parsing exact while pre-envelope identifiers remain
deployed, where a legacy bare identifier contains no tilde and dual-reads under
the built-in HMAC provider only.

The class of provider expected to grow this table is one that consumes a
web-browser-supplied unique identifier, arriving either as a new web
platform feature or from a user-installed extension, delivered to the edge
through the client-cycle resolve path and verified by the provider before
creating.

| Code   | Provider                                                                                                                                                                          | Allocated  | Status                     |
| ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | -------------------------- |
| `hmac` | Built-in HMAC EC provider. Creates `hmac~<64 hex>.<6 alnum>`; dual-reads its pre-envelope bare form so deployed cookies keep working (retirement condition in `provider_owns_id`) | 2026-08-02 | active                     |
| `hs00` | Built-in host-signal EC provider (opt-in; TLS JA4 plus HTTP/2 signals plus client IP)                                                                                             | 2026-08-25 | active                     |
| `cfix` | Client-fixed demonstration provider (compiled only behind the `client-fixed-demo` cargo feature)                                                                                  | 2026-08-25 | active, test and demo only |
| `t0..` | Prefix family reserved for in-tree test providers (`t0cc`, `t0op`, and similar); never valid in configuration                                                                     | 2026-08-25 | reserved                   |

# Production Readiness Report

**Date:** 2026-03-04
**Branch:** `split-prebid-deferred-bundle`
**Scope:** Correctness, security foot-guns, optimization
**Out of scope:** Test coverage gaps (tracked separately)

## Verdict

**Not production-ready for an internet-exposed deployment.** There are several
high-risk correctness/security foot-guns plus clear performance wins left on the
table. The codebase is well-structured and uses Rust's type system effectively,
but the issues below need resolution before a public-facing deployment.

---

## Summary

| Severity | Count | Areas                                                                                                                                                                                        |
| -------- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| CRITICAL | 5     | Request signing coherence, admin auth, creative XSS, prototype stacking                                                                                                                      |
| HIGH     | 5     | Secret logging, auction timeouts, weak secret validation, regex HTML rewriting, config error swallowing                                                                                      |
| MEDIUM   | 16    | Host spoofing, non-constant-time comparison, cookie injection, SSRF, memory buffering, per-request CPU waste, prototype perf, SDK polling, observer leaks, prebid host trust, bid truncation |
| LOW      | 14    | Cache headers, error details, MIME types, regex compilation, status codes, allocations                                                                                                       |

---

## CRITICAL

### C-1: Request-signing store selection is inconsistent (hardcoded vs config-driven)

Store names are hardcoded as `"jwks_store"` / `"signing_keys"` in standalone
functions but read from `settings.request_signing` in the admin endpoints. If
config and hardcoded values diverge, signing produces keys the verifier cannot
find, and key rotation writes to the wrong store.

**Refs:**

- `signing.rs:20` -- `FastlyConfigStore::new("jwks_store")` hardcoded
- `signing.rs:122` -- `FastlyConfigStore::new("jwks_store")` hardcoded
- `signing.rs:130` -- `FastlySecretStore::new("signing_keys")` hardcoded
- `jwks.rs:63` -- `FastlyConfigStore::new("jwks_store")` hardcoded
- `rotation.rs:44` -- `FastlyConfigStore::new("jwks_store")` hardcoded, ignores `config_store_id` constructor arg
- `endpoints.rs:151` -- reads `config_store_id`/`secret_store_id` from settings

**Recommendation:** Single source of truth -- either always read store IDs from
`Settings` and thread them through, or document + assert the hardcoded names
match config.

---

### C-2: Admin endpoints unprotected unless handler regex covers them

`/admin/keys/rotate` and `/admin/keys/deactivate` are always routed. The
`enforce_basic_auth` gate only triggers for paths that match a configured
`handlers[].path` regex. The default config (`^/secure`) does not cover
`/admin/*`. An operator who doesn't add an explicit admin handler has
**publicly-accessible key rotation/deletion endpoints**.

**Refs:**

- `main.rs:97-98` -- admin route matching
- `auth.rs:10` -- `enforce_basic_auth` checks `handlers` list
- `settings.rs:381` -- `handlers` parsing
- `trusted-server.toml:1` -- default handler only covers `^/secure`

**Recommendation:** Either hard-require auth for `/admin/*` paths regardless of
handler config, or validate at startup that an admin handler exists.

---

### C-3: Unsanitized creative HTML injected into iframe with weakened sandbox (JS)

Creative HTML (`bid.adm`) from the upstream bidder response is injected into an
iframe via `srcdoc` with no sanitization. The iframe sandbox includes both
`allow-scripts` and `allow-same-origin`, which together allow script inside the
iframe to remove its own sandbox attribute and gain full access to the
publisher's page context, cookies, and localStorage.

**Refs:**

- `request.ts:77-102` -- `iframe.srcdoc = buildCreativeDocument(creativeHtml)`
- `render.ts:104-111` -- sandbox with `allow-scripts` + `allow-same-origin`
- `render.ts:133-137` -- `buildCreativeDocument` does raw string replace
- `auction.ts:141-172` -- `parseAuctionResponse` passes `adm` straight through

**Recommendation:** Either (a) remove `allow-same-origin` if creatives don't
need it, (b) serve creatives from a separate origin, or (c) sanitize `adm` with
DOMPurify before injection.

---

### C-4: Multiple global prototype patches stack without coordination (JS)

Five integrations (Lockr, Permutive, DataDome, GTM, GPT) independently
monkey-patch `Element.prototype.appendChild` and `Element.prototype.insertBefore`.
Each captures the current prototype method at install time, creating a chain of
4+ wrappers. Every single `appendChild` call on the publisher page (including
text nodes, divs, analytics pixels) now executes 4+ function calls with string
checks.

The shared guard's `reset()` only flips a boolean -- it does not restore the
original prototype methods, so SPA contexts and test runs accumulate wrappers.

**Refs:**

- `shared/script_guard.ts:155-191` -- shared factory patches
- `gpt/script_guard.ts:432-450` -- independent GPT patch
- `shared/script_guard.ts:197-199` -- `reset()` doesn't restore originals

**Recommendation:** Use a centralized dispatcher -- single prototype patch,
register per-integration handlers. Implement proper `reset()` that restores
originals.

---

### C-5: `.expect()` on regex compilation from user configuration

Configuration-derived regex patterns use `.expect()` which will panic at runtime
if the pattern is invalid. If `handler.path` or integration config contains
invalid regex metacharacters and validation is bypassed (env override, manual
TOML edit), the service crashes on first matching request.

**Refs:**

- `settings.rs:255` -- `Regex::new(&self.path).expect(...)`
- `script_rewriter.rs:125` -- `Regex::new(&pattern).expect(...)`
- `script_rewriter.rs:141` -- `Regex::new(&pattern).expect(...)`
- `shared.rs:83` -- `Regex::new(...).expect(...)`

**Recommendation:** Return `Result` from these constructors; catch at startup
with a descriptive error message.

---

## HIGH

### H-1: Secrets and PII logged at INFO/DEBUG level

The full `Settings` struct (including `proxy_secret`, `synthetic.secret_key`,
handler passwords) is logged via `Debug` format at `INFO` level on every
request. Synthetic ID generation logs client IP, user agent, and other PII.
Integration responses (full bid payloads) are logged at debug. Logger is
globally set to debug level.

**Refs:**

- `main.rs:42` -- `log::info!("Settings {settings:?}")`
- `main.rs:177` -- logger level set to debug
- `synthetic.rs:99` -- logs HMAC input (IP, UA)
- `synthetic.rs:112` -- logs synthetic ID details
- `prebid.rs:832` -- logs full bid response
- `aps.rs:444` -- logs APS response
- `adserver_mock.rs:284` -- logs mock response

**Recommendation:** Implement a `Redacted<T>` wrapper for secret fields that
prints `[REDACTED]` in `Debug`/`Display`. Set production log level to `INFO` or
`WARN`. Move payload logging to `TRACE`.

---

### H-2: Auction timeout config not enforced by orchestrator wait logic

`settings.auction.timeout_ms` is passed to `AuctionContext` but the orchestrator
uses `select()` which blocks until each pending request completes or hits the
backend's `first_byte_timeout` (15s). There is no mechanism to abort remaining
requests when the auction timeout is reached.

**Refs:**

- `endpoints.rs:51` -- `timeout_ms: settings.auction.timeout_ms`
- `provider.rs:54` -- `fn timeout_ms(&self) -> u32`
- `orchestrator.rs:287` -- `while !remaining.is_empty() { select(remaining) }`
- `backend.rs:118-119` -- hardcoded 15s first_byte_timeout

**Recommendation:** Implement a deadline-based loop that drops remaining pending
requests when `timeout_ms` elapses, returning partial results.

---

### H-3: Weak/inconsistent secret default validation

Only `synthetic.secret_key` is checked against the literal `"secret-key"`.
`publisher.proxy_secret` defaults to `"change-me-proxy-secret"` and
`synthetic.secret_key` defaults to `"trusted-server"` -- neither is caught by
the single check. A deployment using defaults has predictable encryption keys.

**Refs:**

- `trusted-server.toml:10` -- `proxy_secret = "change-me-proxy-secret"`
- `trusted-server.toml:15` -- `secret_key = "trusted-server"`
- `settings.rs:197` -- `Settings::validate()` -- no proxy_secret check
- `settings_data.rs:37` -- only checks `== "secret-key"`

**Recommendation:** Reject all known placeholder values for both secrets.
Consider minimum entropy requirements.

---

### H-4: Regex-based HTML rewriting in `document.write` interception can fail-open (JS)

The GPT guard uses a regex to match and rewrite GPT domain script URLs in
`document.write` calls. If the regex fails to match (escaped quotes, unusual
spacing, mixed quote styles), the original unproxied URL is passed through,
causing the browser to load the GPT script directly from Google's CDN instead of
through the first-party proxy.

**Refs:**

- `gpt/script_guard.ts:206-230` -- `rewriteHtmlString` regex

**Recommendation:** Use DOM-based parsing (`DOMParser`) instead of regex, or
fail-closed (block unmatched URLs rather than passing through).

---

### H-5: Configuration errors silently disable integrations

When `integration_config()` returns an error (typo in TOML, wrong type), `.ok()`
converts it to `None`, making the integration appear "not configured" rather than
"misconfigured". Operators get no feedback that their config is broken.

**Refs:**

- `prebid.rs:211-212` -- `.ok().flatten()?`
- `nextjs/mod.rs:97-99` -- `.ok().flatten()?`
- `adserver_mock.rs:373` -- `BackendConfig::from_url(...).ok()`
- `aps.rs:521` -- `BackendConfig::from_url(...).ok()`
- `prebid.rs:950` -- `BackendConfig::from_url(...).ok()`

**Recommendation:** Log a warning with the error before converting to `None`, or
fail the integration registration with a clear message.

---

## MEDIUM

### M-1: X-Forwarded-Host / Forwarded header spoofing

`RequestInfo` trusts `Forwarded`, `X-Forwarded-Host`, and `X-Forwarded-Proto`
headers without validation against trusted proxies. An attacker setting
`X-Forwarded-Host: evil.com` causes the HTML rewriter to replace all origin URLs
with `evil.com`.

**Refs:**

- `http_util.rs:55-75` -- `extract_request_host` trusts forwarded headers

**Recommendation:** Strip or validate forwarded headers at the Fastly VCL layer,
or validate against a configured allowlist.

---

### M-2: Non-constant-time token/password comparison

Signature verification (`tstoken`, clear URL signatures) and basic auth use
standard `==` comparison, enabling timing side-channel attacks.

**Refs:**

- `proxy.rs:1054-1058` -- `expected != sig`
- `http_util.rs:289-291` -- `sign_clear_url(...) == token`
- `auth.rs:17-18` -- `password == handler.password`

**Recommendation:** Use `subtle::ConstantTimeEq` (already in dependency tree via
crypto crates).

---

### M-3: Synthetic ID cookie missing HttpOnly flag

The `synthetic_id` cookie is set with `Secure; SameSite=Lax` but no `HttpOnly`.
Any XSS on the publisher's page can exfiltrate this tracking identifier via
`document.cookie`.

**Refs:**

- `cookies.rs:67-72` -- `create_synthetic_cookie` format string

**Recommendation:** Add `HttpOnly` if client-side JS doesn't need to read this
cookie directly (it already gets the value via the `x-synthetic-id` header).

---

### M-4: No synthetic ID format validation on inbound values

The synthetic ID from cookies or headers is accepted without format validation.
An attacker can inject arbitrary strings (very long, special characters,
newlines) which are then set as response headers, cookies, and forwarded to
third-party APIs.

**Refs:**

- `synthetic.rs:129-153` -- `get_synthetic_id` accepts any string
- `publisher.rs:336` -- set as response header
- `proxy.rs:442` -- forwarded as query parameter

**Recommendation:** Validate against the expected format (64 hex + dot + 6
alphanumeric) in production code, not just tests.

---

### M-5: Cookie value not sanitized in Set-Cookie construction

`synthetic_id` is interpolated directly into the `Set-Cookie` header string
without escaping. A controlled synthetic ID containing semicolons could alter
cookie attributes (e.g., `evil; Domain=.attacker.com`).

**Refs:**

- `cookies.rs:67-72` -- `format!("...={}; Domain=...", synthetic_id, ...)`

**Recommendation:** Validate/sanitize the value before interpolation, or use a
cookie builder library.

---

### M-6: SSRF via first-party proxy -- no target domain allowlist

The `/first-party/proxy` endpoint proxies to arbitrary URLs (protected only by
`tstoken` signature). `proxy_with_redirects` follows up to 4 redirects with no
domain or IP range restriction, allowing SSRF to internal services if a signed
URL redirects.

**Refs:**

- `proxy.rs:600-621` -- `handle_first_party_proxy`
- `proxy.rs:463-582` -- `proxy_with_redirects` follows redirects

**Recommendation:** Validate redirect targets against an allowlist or block
private IP ranges.

---

### M-7: "Streaming" processing buffers whole bodies in key paths

The gzip+HTML path reads the entire decompressed body into memory, then the
`HtmlRewriterAdapter` accumulates it again. Processing a 1MB HTML page creates
3+ full copies in memory simultaneously.

**Refs:**

- `streaming_processor.rs:196` -- `decoder.read_to_end(&mut decompressed)`
- `streaming_processor.rs:398` -- `HtmlRewriterAdapter::accumulated_input`
- `publisher.rs:129` -- `process_response_streaming` collects into `Vec<u8>`

**Recommendation:** Feed chunks incrementally to `lol_html::HtmlRewriter`
instead of accumulating. Use the streaming `process_through_compression` path
for gzip.

---

### M-8: Per-request CPU waste -- settings parse/validate and registry rebuild

`get_settings()`, `build_orchestrator()`, and `IntegrationRegistry::new()` all
run on every request. TOML parsing, regex compilation, and router construction
are repeated for every incoming request.

**Refs:**

- `main.rs:35` -- `get_settings()` per request
- `main.rs:45` -- `build_orchestrator()` per request
- `main.rs:47` -- `IntegrationRegistry::new()` per request
- `settings_data.rs:28-32` -- TOML parsing + validation

**Recommendation:** Cache parsed settings and registry in `OnceLock` or
equivalent per-instance state (Fastly Compute instances can reuse across
requests within the same isolate).

---

### M-9: Prebid response rewriting trusts host/scheme from upstream response body

`request_host` and `request_scheme` for URL rewriting are read from the Prebid
server's response JSON (`ext.trusted_server.request_host`), not from the local
request context. A compromised or misconfigured bidder can inject arbitrary
host/scheme values.

**Refs:**

- `prebid.rs:904` -- reads `request_host` from response JSON
- `prebid.rs:911` -- reads `request_scheme` from response JSON
- `prebid.rs:922` -- passes them to `transform_prebid_response`

**Recommendation:** Use the local request's host/scheme from `RequestInfo`
instead of the bidder's response body.

---

### M-10: `concatenated_hash()` allocates full bundle just to hash it, every HTML response

`concatenate_modules()` builds a full `String` of all JS modules (potentially
hundreds of KB) solely to hash it, then drops it. This runs on every HTML
response. Since module content is `&'static str`, the hash is constant and
should be computed once.

**Refs:**

- `bundle.rs:50-55` -- `concatenated_hash` allocates full bundle
- `tsjs.rs:7` -- called on every HTML response

**Recommendation:** Hash modules incrementally without concatenation, and cache
the result in a `OnceLock`.

---

### M-11: `UrlPatterns` allocates 5+ Strings per HTML attribute

`rewrite_url_value()` calls 5 `format!()` methods for origin/replacement URLs on
every `href`, `src`, `action`, `srcset` attribute in the HTML. A typical page has
dozens to hundreds of these.

**Refs:**

- `html_processor.rs:128-177` -- 5 methods each allocating a String

**Recommendation:** Pre-compute these strings once and store as fields in
`UrlPatterns`.

---

### M-12: Integer truncation on bid dimensions from external input

Bid width/height from external bidder responses are cast from `u64` to `u32` via
`as u32`, silently wrapping on values > `u32::MAX`.

**Refs:**

- `prebid.rs:751-755` -- `as u32` truncation
- `adserver_mock.rs:235-236` -- `as u32` truncation

**Recommendation:** Use `u32::try_from()` or `.min(u32::MAX as u64) as u32`.

---

### M-13: MutationObservers on full document subtree never disconnected (JS)

Three separate modules install `MutationObserver` on `document` with
`subtree: true`. None expose cleanup APIs. In SPA contexts, observers accumulate
on each bundle re-evaluation, creating memory leaks and callback overhead.

**Refs:**

- `creative/click.ts:355-359`
- `creative/dynamic_src_guard.ts:160-165`
- `gpt/script_guard.ts:499-504`

**Recommendation:** Expose `disconnect()` APIs. Consider a shared observer with
multiple handlers. Only install when needed.

---

### M-14: Fixed-timeout SDK polling with no retry or event-based fallback (JS)

Lockr and Permutive integrations poll for SDK availability with a fixed 2.5s
window (`50 attempts * 50ms`). On slow networks, the SDK loads after the window
closes, and the shim is silently skipped.

**Refs:**

- `lockr/index.ts:82-101`
- `permutive/index.ts:81-100`

**Recommendation:** Increase timeout, use exponential backoff, or add
event-based detection (MutationObserver on script insertion).

---

### M-15: `requestAds` callback fires synchronously before bids arrive (JS)

The `requestAds` callback is invoked immediately after initiating the auction
fetch, not after bids return. Publishers expecting Prebid-style "bids ready"
semantics will find an empty bid state.

**Refs:**

- `core/request.ts:15-60` -- callback at line 55

**Recommendation:** Fire callback inside `.then()` after rendering, or document
the difference from Prebid's contract.

---

### M-16: `with_body_text_plain` may override `APPLICATION_JSON` Content-Type

All JSON API responses chain `.with_content_type(APPLICATION_JSON)` then
`.with_body_text_plain(...)`. The latter likely clobbers the Content-Type to
`text/plain`. Automated clients checking Content-Type before parsing will fail.

**Refs:**

- `endpoints.rs:49-51` -- repeated across 6 endpoints

**Recommendation:** Use `.with_body()` instead of `.with_body_text_plain()`.

---

## LOW

### L-1: Lockr regex compiled per request

**Ref:** `lockr.rs:123-126`

Should use `Lazy<Regex>` like other integrations (datadome, nextjs).

### L-2: `serve_static_with_etag` rehashes body already hashed for URL

**Ref:** `http_util.rs:182-186`

The SHA-256 hash is computed twice for the same static content.

### L-3: Handlebars engine created per request in synthetic ID generation

**Ref:** `synthetic.rs:84`

Template engine should be initialized once.

### L-4: ETag comparison doesn't handle multi-value `If-None-Match`

**Ref:** `http_util.rs:188-192`

Per RFC 7232, `If-None-Match` can contain comma-separated ETags.

### L-5: `image/*` is not a valid Content-Type

**Ref:** `proxy.rs:247-248`

Wildcard MIME types are only valid in Accept headers. Use
`application/octet-stream`.

### L-6: Proxy errors return 502 for client-caused failures

**Ref:** `error.rs:107`

"Missing tsurl", "invalid tstoken", "expired tsexp" should be 400/403.

### L-7: Body re-sent on 301/302 redirects

**Ref:** `proxy.rs:503-506`

Per HTTP spec, only 307/308 should preserve body.

### L-8: `rewrite_attribute` allocates String even when no rewriters match

**Ref:** `registry.rs:696`

Use `Cow<str>` to avoid allocation on the common no-match path.

### L-9: `StreamingReplacer` clones overlap buffer + N+1 String allocs per chunk

**Ref:** `streaming_replacer.rs:63`

Clone on empty buffer and chained `String::replace()` per replacement pattern.

### L-10: `Compression::from_content_encoding` allocates String for case comparison

**Ref:** `streaming_processor.rs:46-53`

Use `eq_ignore_ascii_case` instead of `.to_lowercase()`.

### L-11: `all_module_ids()` allocates Vec on every call

**Ref:** `bundle.rs:17-19`

Return from a `OnceLock` or return an iterator.

### L-12: No `X-Content-Type-Options: nosniff` on server-generated responses

**Refs:** `http_util.rs:204`, `error.rs:18`

### L-13: Error responses expose internal error context to clients

**Ref:** `error.rs:17-18`

`user_message()` includes configuration/proxy error strings.

### L-14: Static JS bundles cached for only 300 seconds despite cache-busting hash

**Ref:** `http_util.rs:207-208`

With `?v={hash}` query strings, `max-age` could be much longer (1 year).

---

## Assumptions / Open Questions

1. Assumes `handlers` are the only auth gate for admin endpoints (C-2).
2. If Fastly reuses instances, repeated logger init via `.apply()` could become
   a runtime foot-gun; worth validating in the runtime model
   (`main.rs:197`).
3. The creative HTML injection (C-3) severity depends on whether upstream bidder
   responses are already considered trusted. In an open RTB context they are not.
4. Per-request settings parsing (M-8) may be inherent to Fastly Compute's
   per-request isolate model -- verify whether instance reuse is possible.

---

## Positive Findings

- No `unsafe` code anywhere in the codebase
- No `unwrap()` in production code (all use `expect("should ...")`)
- Proper scheme validation in proxy (`http`/`https` only)
- Internal header filtering prevents leaking `x-synthetic-id`, `x-geo-*` to third parties
- `tstoken` signature validation on first-party proxy URLs
- Insecure default secret key detection (partial -- see H-3)
- Max redirect limit (4) prevents infinite redirect loops
- Ed25519 request signing with canonical JSON payloads
- Module filename allowlisting prevents path traversal in JS serving
- Well-structured error handling with `error-stack` throughout

# Creative asset host allowlist

**Issue:** [IABTechLab/trusted-server#1231](https://github.com/IABTechLab/trusted-server/issues/1231)

**Date:** 2026-10-07

**Status:** Draft (design validated by an exploratory implementation; see the plan, `docs/superpowers/plans/2026-10-07-1231-creative-asset-host-allowlist.md`)

**Related:** [#1234](https://github.com/IABTechLab/trusted-server/issues/1234) (separate click-rewriting switch). Both issues replace `to_abs`; see [Coordination with #1234](#coordination-with-1234).

Line numbers below are against `main` at `7a0ecb4c`.

## Problem

The creative rewrite pass proxies every absolute HTTP(S) asset URL in a creative through `/first-party/proxy` unless its host matches `[rewrite] exclude_domains`. An operator cannot say "proxy only these hosts".

That matters for more than egress control. A proxied fetch appends the visitor's EC ID to the upstream URL (`append_ec_id`, `crates/trusted-server-core/src/proxy.rs:1204`, called at `proxy.rs:793`), so today every third-party asset host that appears in a winning creative receives `ts-ec`. A deny list cannot bound that set, because creatives name new hosts all the time.

`proxy.allowed_domains` does not solve it. It is the fetch-time SSRF control, shared with the Prebid external bundle fetch. With it set, the rewriter still turns off-list URLs into `/first-party/proxy` links, and the fetch then fails with `403`. The asset breaks instead of loading directly.

A second, smaller defect sits in the same code: `Rewrite::is_excluded` compares raw config entries against a host the `url` crate has already lowercased, so an entry written with an uppercase letter never matches.

## Goals

- Add an asset host allowlist, `[rewrite] include_domains`. Empty (the default) keeps today's behavior.
- When non-empty, rewrite only asset URLs whose host matches the list. Every other asset URL keeps its original bytes.
- `exclude_domains` wins when a host matches both lists.
- Apply the allowlist to every asset handler in the pass, on all three entry points, including `srcset` candidates and CSS `url()`, `image-set()` strings and `@import`.
- Never apply the allowlist to anchors (`<a href>`, `<area href>`).
- Apply the same policy at `/first-party/sign` for absolute and protocol-relative input, returning the same non-`403` error excluded hosts already get.
- Use one case-insensitive host matcher for both rewrite lists, which also fixes `exclude_domains` case sensitivity.
- Keep older binaries able to read config blobs written with the default.
- Fix the inline path's double rewrite of `<link rel="preload" imagesrcset>`, so each candidate is proxied exactly once on every path.

## Non-goals

- Click rewriting policy or a click switch. That is #1234.
- Domain checks at `/first-party/proxy`, `/first-party/click` or `/first-party/proxy-rebuild`. Fetch-time SSRF stays with `proxy.allowed_domains`, and the click endpoints redirect any validly signed target by design (`proxy.rs:1898-1907`).
- Narrowing EC forwarding on click redirects (`handle_first_party_click`, `proxy.rs:1545`) or on proxy redirect hops. The allowlist only decides which asset URLs become proxy URLs.
- Cross-validating `include_domains` against `proxy.allowed_domains`. The docs recommend keeping the allowlist inside the proxy allowlist; a load-time warning can be a follow-up.
- Client-side changes. The render guard asks `/first-party/sign` and follows its answer.
- Environment-overlay support for the list. EdgeZero overlays only replace scalar leaves that already exist in the TOML, so the list is set in `trusted-server.toml` and published with `ts config push`, like `exclude_domains`.

## Current behavior

### The rewrite pass

`rewrite_creative_html_impl` (`crates/trusted-server-core/src/creative.rs:1125`) is one lol_html pass. Its asset handlers live at `creative.rs:1180-1339`:

| Handler                                                            | Lines     | Per-URL helper                         |
| ------------------------------------------------------------------ | --------- | -------------------------------------- |
| `img` `src`, `data-src`                                            | 1180-1192 | `proxy_if_abs`                         |
| `script[src]`                                                      | 1194-1201 | `proxied_attr_value` → `proxy_if_abs`  |
| `link[href]` (stylesheet, preload, prefetch) and its `imagesrcset` | 1203-1225 | `proxied_attr_value`, `rewrite_srcset` |
| `video`, `audio`, `source` `src`                                   | 1227-1234 | `proxied_attr_value`                   |
| `object[data]`, `embed[src]`                                       | 1236-1251 | `proxied_attr_value`                   |
| `input[type=image]`                                                | 1253-1267 | `proxied_attr_value`                   |
| SVG `image`/`use` `href`, `xlink:href`                             | 1269-1281 | `proxied_attr_value`                   |
| `[style]` attribute                                                | 1294-1302 | `CssUrlRewriter::rewrite`              |
| `<style>` text, including `@import`                                | 1304-1311 | `CssUrlRewriter::rewrite`              |
| `iframe[src]`                                                      | 1313-1320 | `proxy_if_abs`                         |
| `[srcset]`, `[imagesrcset]`                                        | 1322-1339 | `rewrite_srcset`                       |

The anchor handler, `a[href], area[href]` at `creative.rs:1283-1292`, calls `to_abs` then `build_click_url` and sets both `href` and `data-tsclick`.

Every per-URL decision goes through `to_abs` (`creative.rs:54-75`):

```rust
pub(super) fn to_abs(settings: &Settings, u: &str) -> Option<String>
```

It trims, turns `//host/…` into `https://host/…`, accepts only `http://` and `https://` prefixes (ASCII case-insensitive), and returns `None` when `settings.rewrite.is_excluded(&absolute)` matches (`creative.rs:70`). `None` leaves the URL untouched. The call sites are `CssUrlRewriter::rewrite` (`creative.rs:402`), `proxy_if_abs` (`creative.rs:639-641`), `rewrite_srcset` (`creative.rs:726`), the anchor handler (`creative.rs:1285`) and `/first-party/sign` (`proxy.rs:1676`). `proxied_attr_value` (`creative.rs:741-750`) delegates to `proxy_if_abs`.

`to_abs` returns the trimmed input string, not a parsed URL. Its callers hand that string to `build_signed_url_for` (`creative.rs:554-596`), which parses it with `url::Url::parse` and signs the parsed serialization. The rewritten output already depends only on the parsed URL.

### Entry points

All three share the pass:

- `POST /auction` adm, via `process_auction_creative` (`auction/formats.rs:398`), gated by `[auction] rewrite_creatives` (`creative.rs:1031`).
- SSAT and page-bids inline creatives, via `process_inline_auction_creative` (`publisher.rs:5835`), same gate, absolute output against `base_origin`.
- HTML and CSS fetched through `/first-party/proxy`: `finalize_proxied_response` (`proxy.rs:643-678`) runs `CreativeHtmlProcessor` (`creative.rs:1409`) and `CreativeCssProcessor` (`creative.rs:1454`). This path ignores `rewrite_creatives`.

### `/first-party/sign`

`handle_first_party_proxy_sign` (`proxy.rs:1625`) normalizes the target in two branches (`proxy.rs:1672-1681`): `//` input gets the request's scheme; anything else goes through `to_abs`, which already applies `exclude_domains`. Because the `//` branch skips `to_abs`, the handler checks `is_excluded` a second time (`proxy.rs:1683-1687`). Both rejections return `TrustedServerError::Proxy { message: "unsupported url" }`, which maps to `502 Bad Gateway` (`error.rs:128`). It then parses, checks scheme and host, and enforces `proxy.allowed_domains` with `403` (`proxy.rs:1689-1711`).

The render guard maps `403` to `blocked` and every other failure to `fallback`, which loads the raw URL directly (`crates/trusted-server-js/lib/src/integrations/creative/proxy_sign.ts:62-64`).

### Host matching

`Rewrite::is_excluded` (`settings.rs:647-669`) parses the URL, takes `host_str()` (already lowercase), and compares it against raw entries with `==` and `ends_with`. An entry such as `CDN.example.com` never matches. It still carries a stale `#[allow(dead_code)]` (`settings.rs:647`).

`proxy::is_host_allowed` (`proxy.rs:1242-1255`) lowercases both sides and enforces a dot boundary: `example.com` matches only itself; `*.example.com` matches `example.com` and any subdomain, not `evil-example.com`. Prebid (`integrations/prebid.rs:918`) and GTM (`integrations/google_tag_manager.rs:1983`) reuse it. `Proxy::normalize` (`settings.rs:1766-1789`) trims and lowercases `allowed_domains`, drops empty entries, and drops a bare `*` with a warning.

## Design

### Configuration

```toml
[rewrite]
# Never rewrite these hosts (assets and click-through links).
exclude_domains = ["static.publisher.example.com"]
# When non-empty, proxy only asset URLs on these hosts. Click-through links
# are not affected. Empty (the default) proxies every eligible asset URL.
include_domains = ["*.cdn.example.com", "img.example.net"]
```

```rust
#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Rewrite {
    /// Hosts never rewritten, for assets and click-through links. ...
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    /// When non-empty, the only hosts whose asset URLs are proxied. ...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(custom(function = validate_include_domains))]
    pub include_domains: Vec<String>,
}
```

`skip_serializing_if` keeps the default out of serialized blobs, the same pattern as `rewrite_creatives` (`auction_config_types.rs:46-50`, tested at `config_payload.rs:715-736`). `exclude_domains` keeps its current serialization so existing blob hashes do not change.

Pattern syntax is the same for both lists: an exact host, or `*.example.com`, which matches the apex and any subdomain at any depth. Matching is on `Url::host_str()` only; port, user info, path, query and fragment never participate.

### Load-time normalization

Add `Rewrite::normalize(&mut self)` and call it from `Settings::normalize_deserialized` (`settings.rs:2988`), next to `self.proxy.normalize()`. That function runs on every load path: TOML, config blob, and `TrustedServerAppConfig` deserialization used by `ts config push` validation (`config.rs:111-112`).

- Both lists: trim each entry and lowercase it with `to_ascii_lowercase`.
- `exclude_domains`: drop empty entries and a bare `*` with a `log::warn!`, as `Proxy::normalize` does. Neither can match a host today, so dropping them changes no behavior, and rejecting them would make an existing blob fail to load after a binary upgrade.
- `include_domains`: keep entries as normalized. `validate_include_domains` rejects an empty entry, a bare `*`, `*.` with an empty suffix, and any `*` other than a leading `*.`. The key is new, so no existing config can trip this, and a malformed allowlist is better caught at `ts config push` than silently widened to "proxy everything".

Settings validation runs after normalization (`settings.rs:2997-3013`), so the validator sees trimmed, lowercased entries.

### Policy methods on `Rewrite`

`is_excluded(&self, url: &str)` is removed. Its two production callers (`creative.rs:70`, `proxy.rs:1683`) go away with `to_abs`, and nothing else uses it. Its replacement takes a host, not a URL, so callers parse once:

```rust
use crate::proxy::{is_host_allowed, is_host_permitted};

impl Rewrite {
    /// Returns `true` when an asset URL on `host` should be rewritten to
    /// `/first-party/proxy`. ...
    #[must_use]
    pub fn should_proxy_asset(&self, host: &str) -> bool {
        !self.is_excluded_host(host) && is_host_permitted(&self.include_domains, host)
    }

    /// Returns `true` when a click-through URL on `host` should be wrapped in
    /// `/first-party/click`. Only `exclude_domains` applies. ...
    #[must_use]
    pub fn should_wrap_click(&self, host: &str) -> bool {
        !self.is_excluded_host(host)
    }

    fn is_excluded_host(&self, host: &str) -> bool {
        self.exclude_domains
            .iter()
            .any(|pattern| is_host_allowed(host, pattern))
    }
}
```

Both lists reuse the existing matchers in `proxy.rs`. `is_host_allowed` (`proxy.rs:1242`) is case-insensitive, has the dot-boundary rule, and is what `proxy.allowed_domains`, Prebid and GTM use, so every host list in the crate matches the same way. `is_host_permitted` (`proxy.rs:1228`) adds open mode, so an empty list permits every host; that is exactly the "include list empty" half of the rule. It becomes `pub(crate)` for this; no new matcher helper is added. Neither function moves; a module move would touch unrelated callers for no behavior gain. Their per-call lowercase allocations are negligible next to the signing cost each proxied URL already pays.

The stale `#[allow(dead_code)]` disappears with `is_excluded`.

### Splitting `to_abs` into normalization and policy

`to_abs` is replaced by a pure normalizer that makes no policy decision:

```rust
/// Normalizes a creative URL to an absolute HTTP(S) [`url::Url`].
///
/// Trims surrounding whitespace, resolves a protocol-relative `//host/...`
/// against `https:`, and accepts only `http://` and `https://` input (ASCII
/// case-insensitive). Returns `None` for empty, relative, non-network-scheme or
/// unparseable input. Applies no host policy.
pub(super) fn normalize_creative_url(url: &str) -> Option<url::Url>
```

Returning `Url` rather than `String` gives callers the host without a second parse, and `Url::parse` of an `http(s)` URL always yields a host, so `host_str()` is `Some` on every value it returns. Callers keep passing `url.as_str()` to the existing `build_proxy_url`, `build_click_url` and `build_proxy_url_with_extras`, whose signatures do not change.

Two private helpers pair the normalizer with policy, one for assets and one for click-through links:

```rust
/// Normalizes `raw` and returns it when asset policy allows proxying its host.
fn asset_target(settings: &Settings, raw: &str) -> Option<url::Url> {
    normalize_creative_url(raw).filter(|url| {
        url.host_str()
            .is_some_and(|host| settings.rewrite.should_proxy_asset(host))
    })
}

/// Normalizes `raw` and returns it when click policy allows wrapping its host.
fn click_target(settings: &Settings, raw: &str) -> Option<url::Url> {
    normalize_creative_url(raw).filter(|url| {
        url.host_str()
            .is_some_and(|host| settings.rewrite.should_wrap_click(host))
    })
}
```

Resulting call sites (signatures unchanged unless shown):

| Call site                                                               | Change                                                                                                                             |
| ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `proxy_if_abs(settings, val, base_origin) -> Option<String>`            | `asset_target(settings, val).map(\|url\| build_proxy_url(settings, url.as_str(), base_origin))`                                    |
| `proxied_attr_value(...)`                                               | Unchanged; delegates to `proxy_if_abs`.                                                                                            |
| `rewrite_srcset(settings, srcset, base_origin) -> String`               | Per candidate, `asset_target(settings, url)` replaces `to_abs(settings, url)`.                                                     |
| `CssUrlRewriter::rewrite(&mut self, value, token_start, parser, shape)` | `let Some(target) = asset_target(self.settings, value) else { return };`                                                           |
| Anchor handler, `creative.rs:1283-1292`                                 | `click_target(settings, &href)`, then `build_click_url(settings, target.as_str(), base_origin)`. Never consults `include_domains`. |
| `handle_first_party_proxy_sign`, `proxy.rs:1672-1726`                   | See below.                                                                                                                         |

Because no caller reads the original string, every handler in the table gets the allowlist with no per-handler code. Nested HTML and CSS fetched through `/first-party/proxy` run the same pass and pick it up too.

**Output parity with an empty allowlist.** `build_signed_url_for` already parses the clear URL and signs its `Url` serialization, and `Url::parse(url.as_str())` round-trips, so for every input that parses, the rewritten bytes are identical to today's. The only difference is an absolute-looking value that `Url::parse` rejects (for example `https://exa mple.example/x`). Today `to_abs` accepts it and `build_signed_url_for` hands back the raw string, so the handler rewrites the attribute to its own value and `CssUrlRewriter` re-serializes it as a quoted string. After the change the value is left untouched. No existing fixture contains such a URL; `unparseable_absolute_url_is_left_byte_identical` pins the new behavior.

### `/first-party/sign`

The two normalization branches collapse into one, and the duplicate exclusion check goes away:

```rust
let trimmed = payload.url.trim();
let protocol_relative;
let candidate = if trimmed.starts_with("//") {
    protocol_relative = format!("{request_scheme}:{trimmed}");
    protocol_relative.as_str()
} else {
    trimmed
};
let target = crate::creative::normalize_creative_url(candidate).ok_or_else(unsupported_url)?;
let host = target.host_str().ok_or_else(missing_host)?;
if !settings.rewrite.should_proxy_asset(host) {
    log::debug!("sign request for `{host}` declined: host excluded or not in rewrite.include_domains");
    return Err(unsupported_url());
}
if !is_host_permitted(&settings.proxy.allowed_domains, host) {
    // Unchanged: warn and return AllowlistViolation (403).
}
// Unchanged: tsexp, build_proxy_url_with_extras(settings, target.as_str(), &extras), { href, base }.
```

`unsupported_url()` is the existing `TrustedServerError::Proxy { message: "unsupported url" }`, mapped to `502`. Protocol-relative input still inherits the request scheme before normalization, so `//` and absolute input reach the same check. The order keeps the model spec's rule (`2026-08-26-first-party-sign-allowlist-enforcement-design.md`, "Validation order"): rewrite policy first, then `proxy.allowed_domains`, so `403` stays reserved for a valid host rejected by the proxy allowlist.

The parse and scheme checks at `proxy.rs:1689-1698` become unreachable once `normalize_creative_url` has accepted the input. Remove them; keep the `missing host` guard because `host_str()` returns `Option`.

### Single rewrite of `<link imagesrcset>`

Today `<link rel="preload" imagesrcset>` is rewritten twice: once by the `link[href]` handler (`creative.rs:1217`) and again by the standalone `[imagesrcset]` handler (`creative.rs:1331`), which runs on the same element. On the root-relative paths (`/auction`, proxied HTML) the second pass sees `/first-party/proxy?...`, which is relative, and leaves it alone. On the inline path the first pass emits absolute `https://<base_origin>/first-party/proxy?...` candidates, so the second pass proxies them again. The result carries duplicated `tsurl` and `tstoken` parameters.

The fix removes the `imagesrcset` block from the `link[href]` handler and keeps its `href` handling. `[imagesrcset]` already rewrites the attribute on every element, so each candidate is proxied exactly once on every path. The fix is independent of `include_domains`: it happens with an empty list too. It is in scope here because the allowlist matrix exercises `link imagesrcset` on the inline path and should use a realistic `base_origin`.

### Behavior summary

| Asset host                 | `include_domains` | `exclude_domains` | Asset rewrite | Anchor wrap | `/first-party/sign` |
| -------------------------- | ----------------- | ----------------- | ------------- | ----------- | ------------------- |
| any                        | empty             | no match          | proxied       | wrapped     | `200`¹              |
| any                        | empty             | match             | raw           | raw         | `502`               |
| `img.example.com`          | `*.example.com`   | no match          | proxied       | wrapped     | `200`¹              |
| `img.example.net`          | `*.example.com`   | no match          | raw           | wrapped     | `502`               |
| `img.example.com`          | `*.example.com`   | `img.example.com` | raw           | raw         | `502`               |
| `IMG.Example.com` in input | `*.EXAMPLE.com`   | no match          | proxied       | wrapped     | `200`¹              |

¹ Subject to `proxy.allowed_domains`, which still returns `403` for an off-list host.

## Open decisions resolved

### 1. Setting name and table: `[rewrite] include_domains`

`[rewrite]` is the table for the creative URL rewrite policy, and the only one that already covers all three entry points; `[auction]` does not, because proxied HTML ignores `rewrite_creatives`. Pairing `include_domains` with `exclude_domains` uses the same syntax and reads as its counterpart. `allowed_domains` is ruled out to avoid confusion with `proxy.allowed_domains`.

The cost of the name is that it does not say "assets only", while `exclude_domains` covers anchors too. The struct doc comment, the example TOML comment, `configuration.md` and `creative-processing.md` all state that `include_domains` never applies to click-through links. `asset_domains` was considered and rejected: it breaks the include/exclude pairing, and #1234's work is described in terms of an "include list".

### 2. Splitting `to_abs`

`to_abs(&Settings, &str) -> Option<String>` becomes `normalize_creative_url(&str) -> Option<url::Url>`, a pure normalizer. Policy moves to `Rewrite::should_proxy_asset(&self, host: &str) -> bool` and `Rewrite::should_wrap_click(&self, host: &str) -> bool`, built on the existing `proxy::is_host_allowed` and `proxy::is_host_permitted` (made `pub(crate)`). Asset call sites go through the private `asset_target(&Settings, &str) -> Option<url::Url>`, the anchor handler through `click_target(&Settings, &str) -> Option<url::Url>`, and `/first-party/sign` calls the normalizer plus `should_proxy_asset`. `proxy_if_abs`, `proxied_attr_value`, `rewrite_srcset`, `CssUrlRewriter::rewrite` and the `build_*_url` helpers keep their signatures. See [Splitting `to_abs`](#splitting-to_abs-into-normalization-and-policy).

### 3. `/first-party/sign` response for an off-list host

`502 Bad Gateway` via `TrustedServerError::Proxy { message: "unsupported url" }`, identical to an excluded host today, for both absolute and `//` input, through GET and POST. The render guard treats non-`403` as `fallback` and loads the raw URL, which is the allowlist's intent: off-list assets load directly. `403` keeps meaning "blocked by `proxy.allowed_domains`". The handler logs the declined host at `debug`, not the full URL, because declines are expected in normal operation once an allowlist is set.

### 4. Lowercasing `exclude_domains` is a behavior change

Yes. An entry with uppercase letters or surrounding whitespace never matched before and will match after. Those hosts stop being proxied and click-wrapped and stop receiving `ts-ec`. It is filed under `Fixed` in `CHANGELOG.md`, not `Breaking`, because it makes the setting do what operators wrote, and the entry tells operators to audit `exclude_domains` for mixed-case entries before upgrading. Dropping empty and bare `*` entries is not a behavior change, since neither could match a host.

### 5. Landing alongside #1234

See [Coordination with #1234](#coordination-with-1234).

## Coordination with #1234

Both specs describe the same split, so either implementation can land first:

- `to_abs` is replaced by `normalize_creative_url(url: &str) -> Option<url::Url>` in `creative.rs`: trimming, protocol-relative, `http(s)` only, no policy.
- Policy lives on `Rewrite` in `settings.rs` as `should_proxy_asset(&self, host: &str) -> bool` (not excluded and (include list empty or host included)) and `should_wrap_click(&self, host: &str) -> bool` (not excluded), with a private `is_excluded_host`. Both use the existing case-insensitive matchers `proxy::is_host_allowed` and `proxy::is_host_permitted` (made `pub(crate)`): an exact host, or `*.example.com` matching the apex and subdomains.
- `creative.rs` gains two private helpers, `asset_target` and `click_target`, each `normalize_creative_url` filtered by the matching policy method. The anchor handler calls `click_target`.
- The exact shared code is reproduced in the plan's "Shared step" section so the two PRs can be diffed against it.
- Whichever implementation PR lands first introduces the normalizer, both policy methods, the `Rewrite::normalize` case fix, and the removal of `is_excluded`. The second rebases and adds only its own piece: #1231 adds `include_domains` to `should_proxy_asset`; #1234 adds the click switch that gates the anchor handler.

If #1234 lands first, `should_proxy_asset` is `!self.is_excluded_host(host)`, and this PR appends `&& is_host_permitted(&self.include_domains, host)` and adds the field, its validator, the sign-handler test cases and the docs. If this PR lands first, #1234 finds `should_wrap_click` already in place and gates the anchor handler on its switch.

## Test plan

All tests are unit tests in `trusted-server-core` and run under `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare` and `cargo test-spin`. Hosts are under `example.com`, `example.net` and `example.org`.

### `settings.rs`

- `should_proxy_asset` table: empty include list proxies any host; exact match; wildcard matches apex and nested subdomain; wildcard does not match `evil-example.com`; off-list host is rejected; a host in both lists is rejected; uppercase host and uppercase entry both match.
- `should_wrap_click` ignores `include_domains` and honors `exclude_domains`.
- `Rewrite::normalize`: trims and lowercases both lists; drops `""` and `*` from `exclude_domains`.
- Regression for the case bug: `exclude_domains = ["CDN.Example.com"]` loaded from TOML excludes `https://cdn.example.com/x`.
- `include_domains` validation rejects `""`, `"   "`, `*`, `*.` and `cdn.*.example.com` through `Settings::from_toml`, and accepts exact and `*.` entries.
- Port the `test_rewrite_is_excluded` cases (`settings.rs:6523-6546`) to the new methods. Tests build `Rewrite::default()` and extend its lists; assigning a whole field on a `Default` value trips `clippy::field_reassign_with_default`.

### `creative.rs`

- `normalize_creative_url`: port `to_abs_conversions`, `to_abs_preserves_port_in_protocol_relative` and `to_abs_additional_cases` (`creative.rs:1700-1736`, `3420-3430`), asserting on `Url::as_str()`. `HTTPS://cdn.example/x` now yields `https://cdn.example/x`, the form already signed today. Add an unparseable absolute value returning `None`, and a rewrite test asserting that such a value survives byte-identical (by `contains`, since the TSJS tag is injected even without `<body>`).
- Port `to_abs_respects_exclude_domains` and `to_abs_respects_wildcard_domains` (`creative.rs:3445-3504`) to `proxy_if_abs`, leaving the other exclusion tests' rewrite-output assertions untouched.
- Allowlist matrix: one creative containing a listed host and an off-list host in each of `img src`, `img data-src`, `srcset`, `link rel=stylesheet href`, `link imagesrcset`, `script src`, `video`/`audio`/`source src`, `object data`, `embed src`, `input type=image`, SVG `image href`, `use xlink:href`, `iframe src`, inline `style`, and `<style>` with `url()`, an `image-set()` string and `@import`. Listed URLs become `/first-party/proxy`; off-list URLs are byte-identical to the input. Run through `rewrite_creative_html`, `rewrite_inline_creative_html` (absolute output, with a realistic `base_origin`, `https://www.example.com`, that matches the include list) and `rewrite_proxied_html`.
- `rewrite_css_body` with a listed and an off-list `@import` and `url()`.
- A host in both lists stays raw in every handler.
- Anchors and `area` on an off-list host are wrapped with `data-tsclick` while the allowlist is set; an excluded anchor stays raw.
- Empty-allowlist parity: no existing rewrite-output assertion in `creative.rs`, `proxy.rs`, `auction/formats.rs` or `publisher.rs` changes. Reviewers check this in the diff.

- `inline_link_imagesrcset_is_proxied_once`: on the inline path, a `<link rel="preload" imagesrcset>` candidate carries exactly one `tsurl=`. It fails before the fix, with duplicated `tsurl` parameters.
- `root_relative_link_imagesrcset_is_proxied_once`: the same markup through `rewrite_creative_html` (`/auction`) and `rewrite_proxied_html` stays proxied exactly once. It guards the paths that were never affected.

### `proxy.rs`

Table-driven over GET and POST, extending `proxy_sign_rejects_excluded_urls` (`proxy.rs:2692-2715`):

| Case                        | `include_domains` | `exclude_domains` | `proxy.allowed_domains` | Target                          | Expected |
| --------------------------- | ----------------- | ----------------- | ----------------------- | ------------------------------- | -------- |
| Listed, absolute            | `*.example.com`   | empty             | empty                   | `https://img.example.com/a.png` | `200`    |
| Listed, protocol-relative   | `*.example.com`   | empty             | empty                   | `//img.example.com/a.png`       | `200`    |
| Off-list, absolute          | `*.example.com`   | empty             | empty                   | `https://img.example.net/a.png` | `502`    |
| Off-list, protocol-relative | `*.example.com`   | empty             | empty                   | `//img.example.net/a.png`       | `502`    |
| Both lists                  | `*.example.com`   | `img.example.com` | empty                   | `https://img.example.com/a.png` | `502`    |
| Off include and off proxy   | `*.example.com`   | empty             | `*.example.org`         | `https://img.example.net/a.png` | `502`    |
| Listed but off proxy        | `*.example.com`   | empty             | `*.example.org`         | `https://img.example.com/a.png` | `403`    |
| Mixed case                  | `*.example.com`   | empty             | empty                   | `https://IMG.Example.COM/a.png` | `200`    |

Existing scheme-inheritance and allowlist tests stay as they are.

### `config_payload.rs`

- Next to `legacy_blob_without_rewrite_creatives_preserves_rewriting` (`config_payload.rs:715`): serializing `test_settings()` omits `rewrite.include_domains`, and a blob without the key loads with an empty list.
- A non-empty `include_domains` survives the blob round trip.

### JS

No changes. `proxy_sign.test.ts` already covers non-`403` fallback.

## Rollout and rollback

1. Deploy the binary. With no `include_domains`, asset behavior is unchanged except for the `exclude_domains` case fix.
2. Add `include_domains` to `trusted-server.toml` and `ts config push`. `ts config push` validates entries before publishing.

Rollback is the reverse: remove `include_domains` and push, then roll back the binary. `Rewrite` uses `deny_unknown_fields` (`settings.rs:637`), so an older binary rejects a blob that carries the key. A blob written by the new binary with the list empty omits the key and stays readable. Rolling the binary back also restores the case-sensitive `exclude_domains` match.

Recommend that operators keep `include_domains` inside `proxy.allowed_domains` when both are set. A host in the include list but not the proxy list is rewritten to a proxy URL that then fails with `403`.

This PR follows the coordination rule above: if #1234 has merged, rebase onto its normalizer and policy split and add only the include list.

## Documentation updates

- `docs/guide/configuration.md`: add `include_domains` to the `[rewrite]` table (`configuration.md:993-995`) and the section intro, state that it applies to assets only, that `exclude_domains` wins, that both lists are case-insensitive, and that neither can be set through environment overrides. Recommend the subset relationship with `proxy.allowed_domains`.
- `docs/guide/creative-processing.md`: add an "Include Domains" section beside "Exclude Domains" (`creative-processing.md:637`). Fix the wildcard example at `creative-processing.md:656-663`, which says `*.cdn.example.com` does not match `cdn.example.com`; the code matches the apex, as `configuration.md:1022-1025` already says. Note that click-through links ignore the allowlist.
- `docs/guide/first-party-proxy.md`: extend "URL Rewrite Exclusions" (`first-party-proxy.md:509-521`) with the allowlist, and state in the `/first-party/sign` section (`first-party-proxy.md:139-175`) that excluded and off-list hosts return a non-`403` error that falls back to a direct load.
- `docs/guide/api-reference.md`: in `/first-party/sign` error responses (`api-reference.md:565-570`), list the excluded or off-list `502`.
- `trusted-server.example.toml`: add a commented `include_domains` line under `# [rewrite]` (`trusted-server.example.toml:127-129`) and note that `exclude_domains` also covers click-through links.
- `crates/trusted-server-core/src/creative.rs`: update the module doc (`creative.rs:24-27`) to describe `normalize_creative_url` and the policy methods.
- `CHANGELOG.md` under `[Unreleased]`:
  - Added: `[rewrite] include_domains` asset host allowlist, its effect on EC forwarding, and on `/first-party/sign`.
  - Fixed: `exclude_domains` matching is now case-insensitive and ignores surrounding whitespace. Entries that never matched before now take effect; audit mixed-case entries.
  - Fixed: inline SSAT/page-bids creatives no longer proxy `<link rel="preload" imagesrcset>` candidates twice.

## Expected files

| File                                               | Change                                                                                                                                        |
| -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/settings.rs`       | `include_domains`, validator, `Rewrite::normalize`, policy methods, remove `is_excluded`, tests.                                              |
| `crates/trusted-server-core/src/creative.rs`       | `normalize_creative_url`, `asset_target`, `click_target`, call-site updates, single `imagesrcset` rewrite in `link[href]`, module doc, tests. |
| `crates/trusted-server-core/src/proxy.rs`          | `pub(crate)` `is_host_permitted`, sign handler normalization and policy, sign test matrix.                                                    |
| `crates/trusted-server-core/src/config_payload.rs` | Default-omission and round-trip tests.                                                                                                        |
| `docs/guide/configuration.md`                      | `[rewrite]` table and semantics.                                                                                                              |
| `docs/guide/creative-processing.md`                | Include Domains section, wildcard fix.                                                                                                        |
| `docs/guide/first-party-proxy.md`                  | Rewrite allowlist and sign behavior.                                                                                                          |
| `docs/guide/api-reference.md`                      | Sign error responses.                                                                                                                         |
| `trusted-server.example.toml`                      | Commented `include_domains`.                                                                                                                  |
| `CHANGELOG.md`                                     | Added and Fixed entries.                                                                                                                      |

No adapter, routing, dependency or JS changes.

## Completion criteria

- With `include_domains` empty, every existing creative rewrite assertion passes unchanged.
- With it set, listed hosts are proxied and other hosts keep their raw URL in every asset handler, on all three entry points.
- A host in both lists is left alone.
- Matching is case-insensitive in both lists, and `*.example.com` matches `example.com`.
- `/first-party/sign` returns `502` for off-list hosts with absolute and `//` input, through GET and POST, and `403` only for `proxy.allowed_domains`.
- Anchors are wrapped regardless of the allowlist.
- `<link rel="preload" imagesrcset>` is proxied exactly once on the inline, `/auction` and proxied-HTML paths.
- The default is omitted from serialized config blobs.
- Docs, example TOML and CHANGELOG describe the behavior, and all CI gates pass.

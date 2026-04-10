# JS Asset Auditor — Engineering Spec

**Date:** 2026-04-01  
**Status:** Approved for engineering breakdown  
**Related:** [JS Asset Proxy spec](2026-04-01-js-asset-proxy-design.md) _(on `js-asset-proxy-spec` branch until merged)_

---

## Context

The JS Asset Proxy requires a `js-assets.toml` file declaring which third-party JS assets to proxy. Without tooling, populating this file requires manually inspecting network requests in browser DevTools, extracting URLs, generating opaque slugs, and writing TOML — a tedious error-prone process that is a barrier to publisher onboarding.

The Auditor eliminates this friction. It sweeps a publisher's page using the Chrome DevTools MCP, detects third-party JS assets, auto-generates `js-assets.toml` entries, and auto-detects `inject_in_head` from the page DOM. The operator's only remaining decision is reviewing the output before committing.

It also runs as a monitoring tool — `--diff` mode compares a new sweep against the existing config and surfaces new or removed assets, giving publishers ongoing visibility into their third-party JS footprint.

**Implementation:** Pure Claude Code skill — no Rust, no compiled code, no additional dependencies. Uses the Chrome DevTools MCP already configured in `.claude/settings.json`.

---

## Command Interface

```bash
/audit-js-assets https://www.publisher.com                # init — generate js-assets.toml
/audit-js-assets https://www.publisher.com --diff         # diff — compare against existing file
/audit-js-assets https://www.publisher.com --settle 15000 # longer settle for ad-tech-heavy pages
```

---

## Sweep Protocol

1. Read `trusted-server.toml` → extract `publisher.domain` (defines first-party boundary)
2. Open Chrome via `mcp__plugin_chrome-devtools-mcp_chrome-devtools__new_page`, navigate to target URL via `mcp__plugin_chrome-devtools-mcp_chrome-devtools__navigate_page`
3. Wait for page load settle: `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` with `await new Promise(r => setTimeout(r, SETTLE_MS))` where `SETTLE_MS` defaults to 6000 (configurable via `--settle <ms>`)
4. In parallel:
   - `mcp__plugin_chrome-devtools-mcp_chrome-devtools__list_network_requests` with `resourceTypes: ["script"]` → post-filter to exclude first-party hosts (see URL Processing below)
   - `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` → `Array.from(document.head.querySelectorAll('script[src]')).map(s => s.src)` → collect head-loaded script URLs
5. Apply URL normalization (see below), then heuristic filter (see below)
6. For each surviving asset, generate a `[[js_assets]]` entry (see below)
7. Write output (init or diff mode)
8. Print terminal summary
9. Close page via `mcp__plugin_chrome-devtools-mcp_chrome-devtools__close_page`

**`inject_in_head` semantics:** The DOM snapshot in step 4 captures the final state of `<head>` after the settle window. Scripts that were briefly inserted and then removed by a loader will not appear. This is intentional — `inject_in_head = true` means "the script is present in `<head>` at page-stable state." If a loader removes it before the snapshot, the proxy should not re-inject it.

---

## URL Processing

### First-party boundary

A network request is **first-party** if the request URL's host, after stripping a leading `www.`, matches `publisher.domain` (from `trusted-server.toml`) after the same stripping. Matching is exact on the resulting strings.

Publisher-owned CDN subdomains (e.g., `cdn.publisher.com`, `static.publisher.com`) are treated as third-party by default. If the publisher wants to exclude them, they can be added to a `first_party_hosts` list in the command invocation (e.g., `--first-party cdn.publisher.com`).

### URL normalization

Applied to every captured script URL before slug generation and before persisting `origin_url`:

1. Strip fragment (`#...`)
2. Strip all query parameters — cache-busters (`?v=123`, `?cb=timestamp`), consent params, and session tokens all live in query strings. JS asset versioning uses path segments, not query params.
3. Strip trailing slash from the path

The normalized URL is what gets stored in `origin_url` and fed into the slug hash.

---

## Heuristic Filter

The following origin categories are excluded silently. The terminal summary reports what was filtered and why so operators can manually add entries if needed.

**Matching:** Filter entries match if the request URL's host ends with the filter entry, with a dot-boundary check. For example, `googletagmanager.com` in the filter matches `www.googletagmanager.com` but not `evil-googletagmanager.com`.

| Category       | Excluded origins                                                               |
| -------------- | ------------------------------------------------------------------------------ |
| Framework CDNs | `cdnjs.cloudflare.com`, `ajax.googleapis.com`, `cdn.jsdelivr.net`, `unpkg.com` |
| Error tracking | `sentry.io`, `bugsnag.com`, `rollbar.com`                                      |
| Font services  | `fonts.googleapis.com`, `fonts.gstatic.com`                                    |
| Social embeds  | `platform.twitter.com`, `platform.x.com`, `connect.facebook.net`               |

**`googletagmanager.com` is not filtered** — GTM is ad tech and should be proxied.

Everything else surfaces for operator review.

---

## Asset Entry Generation

| Field            | Derivation                                                                                          |
| ---------------- | --------------------------------------------------------------------------------------------------- |
| `slug`           | `{publisher_prefix}:{asset_stem}` — see slug algorithm below                                        |
| `path`           | Fixed: `/js-assets/{publisher_prefix}/{asset_stem}.js`. Wildcard: `/js-assets/{publisher_prefix}/*` |
| `origin_url`     | Normalized URL (see URL Processing), with wildcard substitution applied if versioned                |
| `ttl_sec`        | Omitted — proxy defaults to 1800 (wildcard) or 3600 (fixed)                                         |
| `stale_ttl_sec`  | Omitted — proxy defaults to 86400 (24h)                                                             |
| `inject_in_head` | `true` if URL appeared in head script list from DOM evaluation, else `false`                        |

### Slug algorithm

```
publisher_prefix = first_8_chars(base62(sha256(publisher.domain + "|" + origin_url)))
asset_stem       = filename_without_extension(origin_url)
slug             = "{publisher_prefix}:{asset_stem}"
```

The pipe (`|`) separator is required — it cannot appear in domain names or at the start of a URL, so the hash input is unambiguous. The `origin_url` fed into the hash must be the normalized URL (see URL Processing).

**base62 charset:** `0-9A-Za-z` (digits first, then uppercase, then lowercase). This matches the `base62` crate convention.

**Rationale:** Fully opaque and hash-derived — no human naming required, no ambiguity for cryptic vendor filenames. The KV metadata (`origin_url`, `content_type`, `asset_slug`) serves as the lookup table. Operators can query `js-asset:{slug}` in the KV store to retrieve full provenance. The terminal summary also prints slug → origin_url at generation time.

**Important:** This algorithm must produce identical output to the Proxy's KV key derivation. Engineering should implement this as a shared utility (e.g., a small JS/TS helper in the skill, or a standalone `scripts/` utility) rather than duplicating the logic.

### Wildcard detection

Path segments matching any of these patterns are replaced with `*`:

- Semver: `\d+\.\d+[\.\d-]*` (e.g., `1.19.8-hcskhn`)
- Hex hash: `[a-f0-9]{8,}` between path separators (lowercase hex, minimum 8 characters)
- Mixed alphanumeric hash: `[A-Za-z0-9]{8,}` between path separators, **must contain at least one digit and at least one letter** — this excludes pure-alpha dictionary words like `analytics` or `bootstrap`

The original URL is preserved as a comment above the generated entry so operators can verify the wildcard substitution is correct.

---

## Init Mode Output

### `js-assets.toml` (written to repo root)

```toml
# Generated by /audit-js-assets on 2026-04-01
# Publisher: publisher.com
# Source URL: https://www.publisher.com

[[js_assets]]
# https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js
slug = "aB3kR7mN:prebid-load"
path = "/js-assets/aB3kR7mN/prebid-load.js"
origin_url = "https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js"
inject_in_head = true

[[js_assets]]
# https://raven-static.vendor.io/prod/1.19.8-hcskhn/raven.js (wildcard detected)
slug = "xQ9pL2wY:raven"
path = "/js-assets/xQ9pL2wY/*"
origin_url = "https://raven-static.vendor.io/prod/*/raven.js"
inject_in_head = false
```

### Terminal summary

```
JS Asset Audit — publisher.com
────────────────────────────────
Detected:  8 third-party JS requests
Filtered:  3 (cdnjs.cloudflare.com ×2, sentry.io ×1)
Surfaced:  5 assets → js-assets.toml

  aB3kR7mN  inject_in_head=true   web.prebidwrapper.com/.../prebid-load.js
  xQ9pL2wY  inject_in_head=false  raven-static.vendor.io/prod/*/raven.js  [wildcard]
  zM4nK8vP  inject_in_head=true   googletagmanager.com/gtm.js
  ...

Review inject_in_head values and commit js-assets.toml when ready.
Diff mode: /audit-js-assets <url> --diff
```

---

## Diff Mode Output

Compares sweep results against the existing `js-assets.toml`.

| Condition                   | Behavior                                                                |
| --------------------------- | ----------------------------------------------------------------------- |
| Asset in sweep, not in file | **New** — appended to `js-assets.toml` as a commented-out block         |
| Asset in file, not in sweep | **Missing** — flagged in terminal summary with `⚠`. Never auto-removed. |
| Asset in both               | **Confirmed** — listed as present                                       |

New entries are appended as TOML comments so the file stays valid and nothing is activated without the operator explicitly uncommenting.

### `js-assets.toml` (new entry appended as comment)

```toml
# --- NEW (detected by /audit-js-assets --diff on 2026-04-01, uncomment to activate) ---
# [[js_assets]]
# # https://googletagmanager.com/gtm.js
# slug = "zM4nK8vP:gtm"
# path = "/js-assets/zM4nK8vP/gtm.js"
# origin_url = "https://googletagmanager.com/gtm.js"
# inject_in_head = true
```

### Terminal summary (diff mode)

```
JS Asset Audit (diff) — publisher.com
────────────────────────────────
Confirmed:  4 assets still present on page
New:        1 asset detected (appended as comment to js-assets.toml)
Missing:    1 asset no longer seen on page ⚠

  NEW      zM4nK8vP  googletagmanager.com/gtm.js  → review in js-assets.toml
  MISSING  xQ9pL2wY  raven-static.vendor.io/...   → may have been removed or renamed
```

---

## Implementation

The Auditor is a Claude Code skill file. No compiled code.

**Skill location:** `.claude/commands/audit-js-assets.md`

**MCP tools used:**

- `mcp__plugin_chrome-devtools-mcp_chrome-devtools__new_page` — open browser tab
- `mcp__plugin_chrome-devtools-mcp_chrome-devtools__navigate_page` — load publisher URL
- `mcp__plugin_chrome-devtools-mcp_chrome-devtools__list_network_requests` — capture JS requests
- `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` — settle window + detect head-loaded scripts via DOM query
- `mcp__plugin_chrome-devtools-mcp_chrome-devtools__close_page` — clean up tab

**Permission grants required:** `navigate_page`, `list_network_requests`, and `close_page` are not currently approved in `.claude/settings.json`. Add them to `permissions.allow` before running the skill, or expect interactive permission prompts on first run.

**File tools used:**

- `Read` — read `trusted-server.toml` (publisher domain) and existing `js-assets.toml` (diff mode)
- `Write` — write generated/updated `js-assets.toml`

---

## Delivery Order

The Auditor should be delivered **after Proxy Phase 1** (so `js-assets.toml` schema is defined) and **before Proxy Phase 2** (so engineering has real populated entries to test the cache pipeline against actual vendor origins).

See [delivery order in the Proxy spec](2026-04-01-js-asset-proxy-design.md) _(on `js-asset-proxy-spec` branch until merged)_.

---

## Verification

- Run `/audit-js-assets https://www.publisher.com` against a known test publisher page with identified third-party JS
- Verify generated entries match actual third-party JS observed on the page (cross-check in browser DevTools)
- Verify `inject_in_head = true` only for scripts that appear in `<head>` (not `<body>`)
- Verify wildcard detection fires for versioned path segments and not for stable paths
- Verify GTM (`googletagmanager.com`) is captured and not filtered
- Verify framework CDNs (`cdnjs.cloudflare.com` etc.) are filtered with reason in summary
- Run `--diff` against an unchanged page → all entries confirmed, no new/missing
- Run `--diff` after adding a new vendor script to the page → appears as `NEW` in summary
- Run `--diff` after removing a script → appears as `MISSING ⚠` in summary, file unchanged

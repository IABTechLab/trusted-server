# JS Asset Auditor — Engineering Spec

**Date:** 2026-04-01  
**Status:** Approved for engineering breakdown  
**Related:** [JS Asset Proxy spec](2026-04-01-js-asset-proxy-design.md) _(on `js-asset-proxy-spec` branch until merged)_

---

## Context

The JS Asset Proxy requires a `js-assets.toml` file declaring which third-party JS assets to proxy. Without tooling, populating this file requires manually inspecting network requests in browser DevTools, extracting URLs, generating opaque slugs, and writing TOML — a tedious error-prone process that is a barrier to publisher onboarding.

The Auditor eliminates this friction. It sweeps a publisher's page using Playwright (headless Chromium), detects third-party JS assets, auto-generates `js-assets.toml` entries, and auto-detects `inject_in_head` from the page DOM. The operator's only remaining decision is reviewing the output before committing.

It also runs as a monitoring tool — `--diff` mode compares a new sweep against the existing config and surfaces new or removed assets, giving publishers ongoing visibility into their third-party JS footprint.

**Implementation:** Claude Code plugin at `packages/js-asset-auditor/` containing a standalone Playwright CLI, a processing library, and a skill definition. No Rust, no compiled code. Can also be run directly without Claude Code.

---

## Command Interface

```bash
# Via Claude Code plugin skill
/js-asset-auditor:audit-js-assets https://www.publisher.com                # init — generate js-assets.toml
/js-asset-auditor:audit-js-assets https://www.publisher.com --diff         # diff — compare against existing file
/js-asset-auditor:audit-js-assets https://www.publisher.com --settle 15000 # longer settle for ad-tech-heavy pages
/js-asset-auditor:audit-js-assets https://www.publisher.com --no-filter    # bypass heuristic filtering
/js-asset-auditor:audit-js-assets https://www.publisher.com --headed       # visible browser for debugging
/js-asset-auditor:audit-js-assets https://www.publisher.com --config       # also generate trusted-server.toml

# Direct CLI invocation (no Claude Code required)
node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com
node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com --domain publisher.com
node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com --diff --output js-assets.toml
node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com --config my-config.toml
```

---

## Sweep Protocol

The CLI (`packages/js-asset-auditor/lib/audit.mjs`) performs the full sweep:

1. Resolve publisher domain: `--domain` flag → `trusted-server.toml` → infer from target URL
2. Launch headless Chromium via Playwright (visible with `--headed`)
3. Register a response listener for `resourceType() === 'script'` to capture all script network requests
4. Navigate to target URL (`page.goto`, 30s timeout, follows redirects transparently)
5. Wait for page load settle: `page.waitForTimeout(SETTLE_MS)` where `SETTLE_MS` defaults to 6000 (configurable via `--settle <ms>`)
6. Evaluate `document.head.querySelectorAll('script[src]')` to collect head-loaded script URLs
7. Close browser
8. Pass collected URLs to `processAssets()` from `lib/process.mjs` — applies URL normalization, first-party filtering, heuristic filtering, wildcard detection, slug generation
9. Write `js-assets.toml` output (init or diff mode)
10. Print JSON summary to stdout (progress lines go to stderr)

**`inject_in_head` semantics:** The DOM snapshot in step 6 captures the final state of `<head>` after the settle window. Scripts that were briefly inserted and then removed by a loader will not appear. This is intentional — `inject_in_head = true` means "the script is present in `<head>` at page-stable state." If a loader removes it before the snapshot, the proxy should not re-inject it.

---

## URL Processing

### First-party boundary

A network request is **first-party** if the request URL's host, after stripping a leading `www.`, matches the publisher domain after the same stripping. Matching is exact on the resulting strings.

**Domain resolution order:** `--domain <host>` flag → `publisher.domain` from `trusted-server.toml` → inferred from the target URL's hostname. This makes the tool usable in any project — `trusted-server.toml` is not required.

**Auto-detection:** The target URL's hostname is automatically included as first-party, in addition to the resolved publisher domain. This ensures that auditing `https://golf.com` when `publisher.domain = "test-publisher.com"` correctly excludes `golf.com` scripts without requiring `--first-party golf.com`.

Publisher-owned CDN subdomains (e.g., `cdn.publisher.com`, `static.publisher.com`) are treated as third-party by default. If the publisher wants to exclude them, they can be added via `--first-party cdn.publisher.com`.

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

| Category            | Excluded origins                                                               |
| ------------------- | ------------------------------------------------------------------------------ |
| Framework CDNs      | `cdnjs.cloudflare.com`, `ajax.googleapis.com`, `cdn.jsdelivr.net`, `unpkg.com` |
| Error tracking      | `sentry.io`, `bugsnag.com`, `rollbar.com`                                      |
| Font services       | `fonts.googleapis.com`, `fonts.gstatic.com`                                    |
| Social embeds       | `platform.twitter.com`, `platform.x.com`, `connect.facebook.net`               |
| Google ad rendering | `pagead2.googlesyndication.com`, `tpc.googlesyndication.com`, `s0.2mdn.net`,   |
|                     | `googleads.g.doubleclick.net`, `www.googleadservices.com`                      |
| Ad fraud detection  | `adtrafficquality.google`                                                      |
| Ad verification     | `adsafeprotected.com`, `moatads.com`, `doubleverify.com`                       |
| reCAPTCHA           | `recaptcha.net`, `www.google.com/recaptcha/*`, `www.gstatic.com/recaptcha/*`   |

**Path-prefix matching:** Some hosts (e.g., `www.google.com`) serve both filterable and non-filterable resources. Entries with a path suffix (e.g., `www.google.com/recaptcha/*`) match only when the URL's path begins with the specified prefix. Plain host entries use dot-boundary suffix matching as before.

**`googletagmanager.com` is not filtered** — GTM is ad tech and should be proxied.

**`securepubads.g.doubleclick.net` is not filtered** — this is the GPT ad server SDK. Publishers deliberately place this tag. Its sub-resources (e.g., `pubads_impl.js`) are also intentional. The filter targets ad-rendering infrastructure (iframes, creatives, verification), not ad-serving SDKs.

**`--no-filter`** bypasses heuristic filtering entirely, surfacing all non-first-party scripts. First-party filtering always applies.

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

**Important:** This algorithm must produce identical output to the Proxy's KV key derivation. The reference implementation lives in `packages/js-asset-auditor/lib/slug.mjs` (standalone CLI) and `packages/js-asset-auditor/lib/process.mjs` (processing library), with a copy in `scripts/js-asset-slug.mjs`. Any changes must be synchronized across all files and the Rust proxy.

### Wildcard detection

Path segments matching any of these patterns are replaced with `*`:

- Semver: `\d+\.\d+[\.\d\w-]*` (e.g., `1.19.8-hcskhn`)
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

## Integration Detection & Config Generation

When invoked with `--config [path]`, the CLI also detects known integrations from the swept URLs and generates a `trusted-server.toml` with appropriate `[integrations.*]` sections.

### Detection patterns

Integration detection runs on raw URLs (before normalization) to preserve query parameters needed for field extraction.

| URL Pattern                                      | Integration          | Extracted Fields                           |
| ------------------------------------------------ | -------------------- | ------------------------------------------ |
| `securepubads.g.doubleclick.net/tag/js/gpt*`     | `gpt`                | `script_url`                               |
| `www.googletagmanager.com/gtm.js?id=GTM-XXX`     | `google_tag_manager` | `container_id` from `?id=`                 |
| `sdk.privacy-center.org`                         | `didomi`             | (defaults)                                 |
| `js.datadome.co`                                 | `datadome`           | (defaults)                                 |
| `aim.loc.kr/*identity-lockr*.js`                 | `lockr`              | `sdk_url`                                  |
| `*.edge.permutive.app/*-web.js`                  | `permutive`          | `organization_id`, `workspace_id` from URL |
| `*/prebid.js`, `*/prebidjs.js` (+ .min variants) | `prebid`             | (detect only)                              |
| `c.amazon-adsystem.com/aax2/apstag*`             | `aps`                | (detect only)                              |

### Field categories

- **Full** — all config fields have defaults or are auto-extracted. Config section is ready to use.
- **Partial** — some fields auto-extracted, others need manual input (marked with `# TODO:`).
- **Detect only** — integration detected but key fields (e.g., `server_url`, `pub_id`) require manual input.

### Config output

```toml
# Generated by js-asset-auditor on 2026-04-13
# Source URL: https://www.publisher.com

[publisher]
domain = "publisher.com"
# cookie_domain = ".publisher.com"
# origin_url = "https://origin.publisher.com"
# proxy_secret = "change-me"

[integrations.gpt]
enabled = true
script_url = "https://securepubads.g.doubleclick.net/tag/js/gpt.js"  # auto-detected
# cache_ttl_seconds = 3600
# rewrite_script = true

[integrations.google_tag_manager]
enabled = true
container_id = "GTM-TRCJMD6"  # auto-detected

[integrations.lockr]
enabled = true
sdk_url = "https://aim.loc.kr/identity-lockr-trust-server.js"  # auto-detected
app_id = ""  # TODO: set your Lockr Identity app_id
# api_endpoint = "https://identity.loc.kr"
```

If the target file already exists, the CLI errors unless `--force` is passed.

---

## Implementation

The Auditor is packaged as a Claude Code plugin at `packages/js-asset-auditor/` with three components:

```
packages/js-asset-auditor/
├── .claude-plugin/plugin.json       # Plugin manifest
├── skills/audit-js-assets/SKILL.md  # Skill definition
├── bin/audit-js-assets              # Executable (added to PATH by Claude Code)
├── lib/
│   ├── audit.mjs                    # Playwright CLI — browser automation + orchestration
│   ├── detect.mjs                   # Integration detection engine + config generation
│   ├── process.mjs                  # Processing library — normalization, filtering, slugs, TOML
│   └── slug.mjs                     # Standalone slug generator
├── package.json                     # playwright dependency
└── settings.json                    # Auto-grants Bash(audit-js-assets:*) permission
```

1. **Playwright CLI** (`lib/audit.mjs`) — Launches headless Chromium, navigates to the target URL, collects script network requests and head script DOM state, then calls `processAssets()`. Outputs TOML file + JSON summary. Can be run directly without Claude Code.
2. **Processing library** (`lib/process.mjs`) — Pure Node.js module (no external dependencies) that exports `processAssets()` and individual utility functions. Handles URL normalization, first-party filtering, heuristic filtering, wildcard detection, slug generation, and TOML formatting.
3. **Claude Code skill** (`skills/audit-js-assets/SKILL.md`) — Thin wrapper that invokes the CLI via the `bin/audit-js-assets` executable and formats the JSON summary.

**Plugin installation:**

```bash
# Local testing (loads for one session)
claude --plugin-dir packages/js-asset-auditor

# Via marketplace (permanent installation)
/plugin marketplace add <org>/<repo>
/plugin install js-asset-auditor
```

**Setup (one-time after install):**

```bash
cd packages/js-asset-auditor && npm install && npx playwright install chromium
```

**Standalone utilities:**

- `scripts/js-asset-slug.mjs` — Standalone slug generator for individual URLs (kept outside the plugin for backward compatibility)

---

## Delivery Order

The Auditor should be delivered **after Proxy Phase 1** (so `js-assets.toml` schema is defined) and **before Proxy Phase 2** (so engineering has real populated entries to test the cache pipeline against actual vendor origins).

See [delivery order in the Proxy spec](2026-04-01-js-asset-proxy-design.md) _(on `js-asset-proxy-spec` branch until merged)_.

---

## Verification

- Run `node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com` against a known test publisher page
- Verify generated entries match actual third-party JS observed on the page (cross-check in browser DevTools)
- Verify `inject_in_head = true` only for scripts that appear in `<head>` (not `<body>`)
- Verify wildcard detection fires for versioned path segments (e.g., `1.19.13-0fnlww`) and not for stable paths
- Verify GTM (`googletagmanager.com`) is captured and not filtered
- Verify Google ad rendering infra (`pagead2.googlesyndication.com`, `s0.2mdn.net` etc.) is filtered with reason in summary
- Verify `securepubads.g.doubleclick.net` (GPT) is **not** filtered
- Verify first-party auto-detection: auditing `golf.com` with `publisher.domain = "test-publisher.com"` excludes `golf.com` scripts
- Run `--diff` against an unchanged page → all entries confirmed, no new/missing
- Run `--diff` after adding a new vendor script to the page → appears as `NEW` in summary
- Run `--diff` after removing a script → appears as `MISSING ⚠` in summary, file unchanged
- Run `/js-asset-auditor:audit-js-assets <url>` via Claude Code plugin → identical results to direct CLI invocation
- Run CLI without `trusted-server.toml` (using `--domain` or domain inference) → works in any project
- Run with `--config` → generates `trusted-server.toml` with detected integrations
- Verify GTM `container_id` is auto-extracted from `?id=GTM-XXXXX` query param
- Verify integrations with TODO fields are marked with `# TODO:` comments
- Verify `--config` without `--force` errors when target file exists
- Verify JSON summary includes `integrations` array when `--config` is used

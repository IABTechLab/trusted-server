# JS Asset Auditor

Use the JS Asset Auditor to sweep a publisher page, capture third-party script requests, and generate candidate `js-assets.toml` and integration config entries for review.

The auditor is a Claude Code plugin and a standalone Node.js CLI. Use it during onboarding to create the first asset list, then run it in diff mode when the publisher page changes.

## What it generates

The auditor can produce two outputs:

- `js-assets.toml`, candidate `[[js_assets]]` entries for the page sweep
- `trusted-server.generated.toml`, a generated integration config skeleton when `--config` is used

Review both files before committing or deploying them.

## Setup

Install the plugin dependencies and browser once:

```bash
cd packages/js-asset-auditor
npm install
npx playwright install chromium
```

## Basic usage

Run the auditor against a publisher URL from the repository root:

```bash
audit-js-assets https://www.publisher.com
```

This writes `js-assets.toml` in the current directory and prints a JSON summary to stdout. Progress messages print to stderr.

You can also run the package directly:

```bash
node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com
```

## How it works

The auditor launches Chromium through Playwright, loads the target page, and records script network responses. It also reads the final set of `<script src>` elements present in `<head>` after the settle window.

Each captured script URL is normalized before the auditor creates an asset entry. Normalization removes fragments, removes query parameters, and strips a trailing slash from the path. The normalized URL is stored as `origin_url` and is used for slug generation.

The generated `inject_in_head` value is based on the final DOM snapshot. If a loader briefly inserts a script into `<head>` and removes it before the page settles, the generated entry uses `inject_in_head = false`.

## Sequence

A sweep follows this sequence:

1. Resolve the publisher domain from `--domain`, then `[publisher].domain` in `trusted-server.toml`, then the target URL hostname.
2. Launch Chromium. The browser is headed by default. Use `--headless` for CI or automation.
3. Register a response listener for script resources.
4. Navigate to the target URL and wait for the configured settle window.
5. Read `<head>` script URLs from the page DOM.
6. Normalize URLs and remove first-party scripts.
7. Apply heuristic filtering unless `--no-filter` is set.
8. Detect wildcardable path segments, generate slugs, and format TOML.
9. Write `js-assets.toml` or append commented suggestions in diff mode.
10. Print the JSON summary.

## Domain precedence

The publisher domain used for slug generation is resolved in this order:

1. `--domain <domain>`
2. `[publisher].domain` from `trusted-server.toml`
3. The target URL hostname when `trusted-server.toml` is missing

That means the auditor may show a configured domain such as `test.publisher.com` even when you sweep a different URL. Pass `--domain` when you want the slugs to reflect a different publisher domain than the one in `trusted-server.toml`.

Example:

```bash
audit-js-assets https://preview.publisher.com --domain publisher.com
```

The target URL hostname is also treated as first party. This prevents a sweep of `https://golf.com` from surfacing `golf.com` scripts when `trusted-server.toml` contains a different publisher domain. Publisher-owned CDN subdomains are not excluded by default. Add them with `--first-party` when they should be treated as first party.

```bash
audit-js-assets https://www.publisher.com --first-party cdn.publisher.com
```

## Asset entry format

Each surfaced script produces a `[[js_assets]]` entry:

```toml
[[js_assets]]
# https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js
slug = "aB3kR7mN:prebid-load"
path = "/js-assets/aB3kR7mN/prebid-load.js"
origin_url = "https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js"
inject_in_head = true
```

| Field            | Description                                                            |
| ---------------- | ---------------------------------------------------------------------- |
| `slug`           | Hash-derived identifier in the form `{publisher_prefix}:{asset_stem}`. |
| `path`           | First-party path where the asset proxy serves the script.              |
| `origin_url`     | Normalized vendor URL.                                                 |
| `inject_in_head` | `true` when the script appears in `<head>` after the page settles.     |

The auditor omits `ttl_sec` and `stale_ttl_sec`. The asset proxy applies its defaults when those fields are absent.

## Slugs and wildcard paths

The auditor generates slugs from the publisher domain and normalized origin URL:

```text
publisher_prefix = first_8_chars(base62(sha256(publisher.domain + "|" + origin_url)))
asset_stem = filename_without_extension(origin_url)
slug = "{publisher_prefix}:{asset_stem}"
```

The canonical implementation is in `packages/js-asset-auditor/lib/process.mjs`. The standalone slug utilities import that implementation so the generated values stay aligned.

When a path contains a version or hash segment, the auditor replaces that segment with `*` and writes a wildcard path:

```toml
[[js_assets]]
# https://raven-static.vendor.io/prod/1.19.8-hcskhn/raven.js (wildcard detected)
slug = "xQ9pL2wY:raven"
path = "/js-assets/xQ9pL2wY/*"
origin_url = "https://raven-static.vendor.io/prod/*/raven.js"
inject_in_head = false
```

Review wildcard entries before committing them. Confirm that the wildcard segment is a version or hash, not a stable path component.

## Filtering

The auditor always removes first-party scripts. It also filters common framework, font, social, ad rendering, ad verification, fraud detection, and reCAPTCHA origins so the output focuses on scripts a publisher is likely to proxy.

`googletagmanager.com` is not filtered. `securepubads.g.doubleclick.net` is not filtered because it serves the GPT ad server SDK.

Use `--no-filter` to surface all non-first-party scripts:

```bash
audit-js-assets https://www.publisher.com --no-filter
```

## Diff mode

Use `--diff` to compare a fresh sweep with an existing `js-assets.toml`:

```bash
audit-js-assets https://www.publisher.com --diff
```

Diff mode:

- confirms assets still present on the page
- appends newly detected assets as commented suggestions
- reports missing assets that were not seen in the latest sweep

New entries are appended as comments so the file stays valid and nothing is activated until you uncomment the entry.

```toml
# --- NEW (detected by /audit-js-assets --diff on 2026-04-01, uncomment to activate) ---
# [[js_assets]]
# # https://googletagmanager.com/gtm.js
# slug = "zM4nK8vP:gtm"
# path = "/js-assets/zM4nK8vP/gtm.js"
# origin_url = "https://googletagmanager.com/gtm.js"
# inject_in_head = true
```

Repeated diff runs against unchanged input are designed to stay idempotent and not keep re-appending the same commented suggestions.

## Generating integration config

Use `--config` to generate a separate Trusted Server config skeleton:

```bash
audit-js-assets https://www.publisher.com --config
```

By default this writes:

```text
trusted-server.generated.toml
```

You can also provide an explicit path:

```bash
audit-js-assets https://www.publisher.com --config ./tmp/publisher-audit.toml
```

If the target config file already exists, the command fails unless you pass `--force`.

The auditor detects known integrations from the raw swept URLs, before URL normalization. It can generate sections for GPT, Google Tag Manager, Didomi, Datadome, Lockr, Permutive, Prebid, and APS.

Generated sections are enabled only when all required fields are present or have defaults. Sections that need manual values include `# TODO:` comments and are generated with `enabled = false`.

```toml
[publisher]
domain = "publisher.com"
# cookie_domain = ".publisher.com"
# origin_url = "https://origin.publisher.com"
# proxy_secret = "change-me"

[integrations.gpt]
enabled = true
script_url = "https://securepubads.g.doubleclick.net/tag/js/gpt.js"  # auto-detected
# cache_ttl_seconds = 3600

[integrations.google_tag_manager]
enabled = true
container_id = "GTM-TRCJMD6"  # auto-detected
```

## Recommended review workflow

1. Run the sweep against the target page.
2. Review `js-assets.toml` for false positives, wildcarded assets, and `inject_in_head` values.
3. If using `--config`, review generated integration blocks and fill in all TODO fields before enabling incomplete integrations.
4. Re-run with `--diff` after site changes to confirm additions and removals.
5. Commit only the reviewed files.

## Common options

| Option                 | Description                                                                  |
| ---------------------- | ---------------------------------------------------------------------------- |
| `--diff`               | Compare against existing `js-assets.toml`.                                   |
| `--domain <domain>`    | Override publisher domain used for slug generation.                          |
| `--settle <ms>`        | Settle window after page load. The default is 6000 ms.                       |
| `--first-party <host>` | Additional first-party host. Pass a comma-separated list for multiple hosts. |
| `--no-filter`          | Bypass heuristic filtering.                                                  |
| `--headless`           | Run browser headlessly.                                                      |
| `--output <path>`      | Output `js-assets.toml` path.                                                |
| `--config [path]`      | Generate Trusted Server config skeleton.                                     |
| `--force`              | Overwrite an existing output or `--config` target.                           |

## Related docs

- [Configuration](/guide/configuration)
- [Integration Guide](/guide/integration-guide)

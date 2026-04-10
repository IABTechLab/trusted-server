Audit a publisher page for third-party JS assets and generate `js-assets.toml` entries.

Usage: /audit-js-assets $ARGUMENTS

`$ARGUMENTS`: `<url> [--diff] [--settle <ms>] [--first-party <host>,...]`

- `<url>` — publisher page URL (required)
- `--diff` — compare sweep against existing `js-assets.toml` instead of generating from scratch
- `--settle <ms>` — settle window in milliseconds after page load (default: 6000)
- `--first-party <host>,...` — additional hosts to treat as first-party (comma-separated)

---

Follow these steps exactly. Stop and report if any step fails.

## 1. Parse arguments

Extract the URL from `$ARGUMENTS` (required — error if missing). Parse optional flags: `--diff` (boolean), `--settle <ms>` (integer, default 6000), `--first-party <host>,...` (comma-separated list).

## 2. Read publisher config

Use the `Read` tool on `trusted-server.toml` in the repo root. Extract the `domain` value from the `[publisher]` section. Error if the file is missing or `[publisher].domain` is not found.

## 3. Open browser and navigate

1. Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__new_page` to open a new browser tab
2. Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__navigate_page` with the target URL
3. If navigation fails, close the page and report the error

## 4. Wait for page settle

Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` with:

```js
await new Promise(r => setTimeout(r, SETTLE_MS))
```

Replace `SETTLE_MS` with the `--settle` value (default 6000).

## 5. Collect data

Make these two calls in parallel:

**Network requests:**
Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__list_network_requests` with `resourceTypes: ["script"]`. Save the full list of script URLs.

**Head scripts:**
Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` with:

```js
Array.from(document.head.querySelectorAll('script[src]')).map(s => s.src)
```

Save the resulting array — this determines `inject_in_head` later.

## 6. URL normalization

For each captured script URL, normalize it:

1. Strip the fragment (`#` and everything after)
2. Strip all query parameters (`?` and everything after)
3. Strip trailing slash from the path

Use the **normalized** URL for all subsequent steps (filtering, slug generation, `origin_url` output).

## 7. First-party filtering

For each normalized URL, parse the hostname. Strip a leading `www.` from both the URL's host and `publisher.domain`. If they match exactly, exclude the URL. Also exclude URLs whose host (after `www.` stripping) matches any `--first-party` host.

Count and track excluded URLs — they don't appear in output but don't appear in the filtered summary either.

## 8. Heuristic filtering

Exclude URLs whose host matches any entry below using **dot-boundary suffix matching**: the URL's host must either equal the filter entry or end with `.` + the filter entry. For example, `sentry.io` matches `sentry.io` and `o123.ingest.sentry.io` but not `notsentry.io`.

| Category | Excluded hosts |
|---|---|
| Framework CDNs | `cdnjs.cloudflare.com`, `ajax.googleapis.com`, `cdn.jsdelivr.net`, `unpkg.com` |
| Error tracking | `sentry.io`, `bugsnag.com`, `rollbar.com` |
| Font services | `fonts.googleapis.com`, `fonts.gstatic.com` |
| Social embeds | `platform.twitter.com`, `platform.x.com`, `connect.facebook.net` |

**`googletagmanager.com` is NOT filtered** — GTM is ad tech and should be proxied.

Track each filtered URL with its category and host for the terminal summary.

## 9. Wildcard detection

For each surviving URL, check each path segment (split by `/`) against these patterns. Replace matching segments with `*`:

- **Semver:** `/^\d+\.\d+[\.\d-]*$/` (e.g., `1.19.8-hcskhn`)
- **Hex hash:** `/^[a-f0-9]{8,}$/` (lowercase hex, 8+ chars)
- **Mixed alphanumeric hash:** `/^[A-Za-z0-9]{8,}$/` AND the segment must contain at least one digit AND at least one letter (excludes dictionary words like `analytics`)

If any segment was wildcarded, save the **original** URL (before substitution) as a comment for the TOML entry.

## 10. Slug generation

For each surviving asset, generate a slug by running:

```bash
node scripts/js-asset-slug.mjs "<publisher.domain>" "<normalized_origin_url>"
```

The output is the full slug (e.g., `ZSZksDbq:prebid-load`). Extract the part before `:` as the `publisher_prefix` for the path field.

## 11. Determine `inject_in_head`

For each asset, check if its normalized URL appears in the head scripts list from step 5. If yes, set `inject_in_head = true`. Otherwise, `inject_in_head = false`.

Note: compare normalized URLs — the head scripts list may contain URLs with query params that were stripped during normalization.

## 12. Build path

For each asset:
- **Fixed (no wildcards):** `path = "/js-assets/{publisher_prefix}/{asset_stem}.js"`
- **Wildcard:** `path = "/js-assets/{publisher_prefix}/*"`

Where `publisher_prefix` is the 8-char prefix from the slug, and `asset_stem` is the filename without extension from the URL.

## 13. Generate output

### Init mode (no `--diff`)

Write `js-assets.toml` to the repo root using the `Write` tool:

```toml
# Generated by /audit-js-assets on YYYY-MM-DD
# Publisher: {publisher.domain}
# Source URL: {target_url}

[[js_assets]]
# {original_url}
slug = "{slug}"
path = "{path}"
origin_url = "{normalized_origin_url_with_wildcards}"
inject_in_head = {true|false}
```

Add the comment `# {original_url} (wildcard detected)` above entries with wildcard substitution.

### Diff mode (`--diff`)

1. Read the existing `js-assets.toml` with the `Read` tool
2. Parse existing entries by `origin_url` (after normalizing both)
3. Classify each asset:
   - **Confirmed:** in both sweep and file
   - **New:** in sweep but not in file → append as commented-out TOML block
   - **Missing:** in file but not in sweep → flag in terminal only, do NOT modify the file
4. Append new entries to `js-assets.toml` as comments:

```toml
# --- NEW (detected by /audit-js-assets --diff on YYYY-MM-DD, uncomment to activate) ---
# [[js_assets]]
# # {original_url}
# slug = "{slug}"
# path = "{path}"
# origin_url = "{normalized_origin_url_with_wildcards}"
# inject_in_head = {true|false}
```

## 14. Terminal summary

Print a formatted summary to the user.

### Init mode

```
JS Asset Audit — {publisher.domain}
────────────────────────────────
Detected:  {total} third-party JS requests
Filtered:  {filtered_count} ({host} ×{count}, ...)
Surfaced:  {surfaced_count} assets → js-assets.toml

  {prefix}  inject_in_head={true|false}  {host}/.../{filename}
  {prefix}  inject_in_head={true|false}  {host}/.../{filename}  [wildcard]
  ...

Review inject_in_head values and commit js-assets.toml when ready.
Diff mode: /audit-js-assets <url> --diff
```

### Diff mode

```
JS Asset Audit (diff) — {publisher.domain}
────────────────────────────────
Confirmed:  {count} assets still present on page
New:        {count} asset(s) detected (appended as comment to js-assets.toml)
Missing:    {count} asset(s) no longer seen on page ⚠

  NEW      {prefix}  {host}/.../{filename}  → review in js-assets.toml
  MISSING  {prefix}  {host}/.../{filename}  → may have been removed or renamed
```

## 15. Cleanup

Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__close_page` to close the browser tab.

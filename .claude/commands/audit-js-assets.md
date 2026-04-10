Audit a publisher page for third-party JS assets and generate `js-assets.toml` entries.

Usage: /audit-js-assets $ARGUMENTS

`$ARGUMENTS`: `<url> [--diff] [--settle <ms>] [--first-party <host>,...] [--no-filter]`

- `<url>` — publisher page URL (required)
- `--diff` — compare sweep against existing `js-assets.toml` instead of generating from scratch
- `--settle <ms>` — settle window in milliseconds after page load (default: 6000)
- `--first-party <host>,...` — additional hosts to treat as first-party (comma-separated)
- `--no-filter` — bypass heuristic filtering for full visibility

---

Follow these steps exactly. Stop and report if any step fails.

## 1. Parse arguments

Extract the URL from `$ARGUMENTS` (required — error if missing). Parse optional flags: `--diff` (boolean), `--settle <ms>` (integer, default 6000), `--first-party <host>,...` (comma-separated list), `--no-filter` (boolean).

## 2. Read publisher config

Use the `Read` tool on `trusted-server.toml` in the repo root. Extract the `domain` value from the `[publisher]` section. Error if the file is missing or `[publisher].domain` is not found.

## 3. Open browser and navigate

1. Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__new_page` to open a new browser tab
2. Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__navigate_page` with the target URL
3. If navigation fails, close the page and report the error

## 4. Wait for page settle

Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` with:

```js
async () => { await new Promise(r => setTimeout(r, SETTLE_MS)); return "settled"; }
```

Replace `SETTLE_MS` with the `--settle` value (default 6000).

## 5. Collect data

Make these two calls in parallel:

**Network requests:**
Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__list_network_requests` with `resourceTypes: ["script"]`. Save the full list of script URLs.

**Head scripts:**
Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__evaluate_script` with:

```js
() => { return Array.from(document.head.querySelectorAll('script[src]')).map(s => s.src); }
```

Save the resulting array.

## 6. Process assets

Write a JSON file containing the collected data:

```json
{"networkUrls": [<all script URLs from network requests>], "headUrls": [<all script src URLs from head DOM query>]}
```

Use the `Write` tool to create `/tmp/audit-input.json`, then run:

```bash
cat /tmp/audit-input.json | node scripts/audit-js-assets.mjs \
  --domain "<publisher.domain>" \
  --target "<target_url>" \
  --output js-assets.toml \
  [--diff] \
  [--first-party <hosts>] \
  [--no-filter]
```

The script writes TOML to the output file and prints a JSON summary to stdout.

## 7. Terminal summary

Parse the JSON summary from step 6 and print a formatted report.

### Init mode

```
JS Asset Audit — {publisherDomain}
────────────────────────────────
Detected:  {totalDetected} third-party JS requests
Filtered:  {heuristicFilteredTotal} ({host} x{count}, ...)
Surfaced:  {surfaced} assets → js-assets.toml

  {prefix}  inject_in_head={true|false}  {shortUrl}
  {prefix}  inject_in_head={true|false}  {shortUrl}  [wildcard]
  ...

Review inject_in_head values and commit js-assets.toml when ready.
Diff mode: /audit-js-assets <url> --diff
```

### Diff mode

```
JS Asset Audit (diff) — {publisherDomain}
────────────────────────────────
Confirmed:  {confirmed.length} assets still present on page
New:        {new.length} asset(s) detected (appended as comment to js-assets.toml)
Missing:    {missing.length} asset(s) no longer seen on page ⚠

  NEW      {prefix}  {shortUrl}  → review in js-assets.toml
  MISSING  {slug}    {originUrl} → may have been removed or renamed
```

## 8. Cleanup

Call `mcp__plugin_chrome-devtools-mcp_chrome-devtools__close_page` to close the browser tab.

---
name: audit-js-assets
description: Audit a publisher page for third-party JavaScript assets. Use when analyzing external scripts, generating js-assets.toml entries, or monitoring changes to a publisher's JS footprint.
---

Audit a publisher page for third-party JS assets and generate `js-assets.toml` entries.

Usage: /js-asset-auditor:audit-js-assets $ARGUMENTS

`$ARGUMENTS`: `<url> [--diff] [--domain <domain>] [--settle <ms>] [--first-party <host>,...] [--no-filter] [--headless] [--config [path]] [--force]`

---

Follow these steps exactly. Stop and report if any step fails.

## 1. Run the auditor

Run the Playwright CLI via Bash, forwarding all arguments from `$ARGUMENTS`:

```bash
audit-js-assets $ARGUMENTS
```

The CLI resolves the publisher domain from `--domain`, then `trusted-server.toml`, then the target URL if no config file exists. It opens a headed browser by default (use `--headless` to disable the UI), collects script URLs, processes them, and writes `js-assets.toml`. Progress lines appear on stderr; a JSON summary prints to stdout.

If the command fails with "Playwright not installed" or "Chromium not installed", tell the user to run:

```bash
cd packages/js-asset-auditor && npm install && npx playwright install chromium
```

## 2. Show results

Parse the JSON summary from stdout and print a formatted report.

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
Diff mode: /js-asset-auditor:audit-js-assets <url> --diff
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

### Integration detection (when --config is used)

If the JSON summary includes an `integrations` array, append:

```
Detected Integrations:
  {id}  ✓ fully configured
  {id}  ✓ {field}={value} (auto-detected)
  {id}  ⚠ {field} needs manual input

→ {config path} generated with {count} integrations
```

Use ✓ for `full` category and integrations with no TODOs. Use ⚠ for integrations with TODO fields.

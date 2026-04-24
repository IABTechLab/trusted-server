# JS Asset Auditor

Use the JS Asset Auditor to sweep a publisher page, capture third-party script requests, and generate candidate `js-assets.toml` and integration config entries for review.

## What it generates

The auditor can produce two outputs:

- `js-assets.toml` — candidate `[[js_assets]]` entries for the page sweep
- `trusted-server.generated.toml` — a generated integration config skeleton when `--config` is used

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

This writes `js-assets.toml` in the current directory and prints a JSON summary to stdout.

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

## Diff mode

Use `--diff` to compare a fresh sweep with an existing `js-assets.toml`:

```bash
audit-js-assets https://www.publisher.com --diff
```

Diff mode:

- confirms assets still present on the page
- appends newly detected assets as commented suggestions
- reports missing assets that were not seen in the latest sweep

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

If the target file already exists, the command fails unless you pass `--force`.

## Recommended review workflow

1. Run the sweep against the target page.
2. Review `js-assets.toml` for false positives, wildcarded assets, and `inject_in_head` values.
3. If using `--config`, review generated integration blocks and fill in all TODO fields before enabling incomplete integrations.
4. Re-run with `--diff` after site changes to confirm additions and removals.
5. Commit only the reviewed files.

## Common options

```text
--diff              Compare against existing js-assets.toml
--domain <domain>   Override publisher domain used for slug generation
--settle <ms>       Settle window after page load
--first-party <h>   Additional first-party hosts
--no-filter         Bypass heuristic filtering
--headless          Run browser headlessly
--output <path>     Output js-assets.toml path
--config [path]     Generate trusted-server config skeleton
--force             Overwrite an existing --config target
```

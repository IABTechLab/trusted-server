# Next.js RSC Test App

Minimal Next.js 15 App Router application for testing Trusted Server's RSC
(React Server Components) URL rewriting integration.

## Purpose

This app generates realistic RSC Flight payloads containing
`origin.example.com` URLs. These payloads exercise every rewriting path in the
Trusted Server HTML processor:

| Route | RSC Pattern | Rewriting Path |
|-------|------------|----------------|
| `/` | Simple JSON URLs in `__next_f.push` | Placeholder substitution |
| `/about` | HTML content with URLs (T-chunks) | T-chunk length recalculation |
| `/blog/hello-world` | Large payload spanning multiple scripts | Cross-script T-chunk handling |

## Quick Start

```bash
npm install
npm run dev
# Visit http://localhost:3099
```

## Testing RSC Streaming

### Quick Test with Live HTML

Test with HTML from your **currently running** server:

```bash
# Terminal 1: Start dev server
npm run dev

# Terminal 2: Test live HTML
./test-live-html.sh                                    # Test home page
./test-live-html.sh http://localhost:3099/about        # Test specific route
```

This fetches fresh HTML from your server and processes it through the trusted-server pipeline. Perfect for rapid iteration during development.

### Full E2E Test

Run a complete end-to-end test (builds production server):

```bash
./test-streaming.sh
```

This script:
1. Builds and starts the Next.js production server
2. Fetches HTML from all routes
3. Verifies RSC content is present
4. Runs Rust integration tests
5. Shows streaming metrics for each route

**Expected Results:**
- ✅ RSC payloads contain `origin.example.com` URLs before processing
- ✅ After processing through trusted-server pipeline: **0 origin URLs remain in RSC payloads**
- ✅ Streaming ratios: 20-40% for RSC pages (vs 0% before the fix)
- ✅ Non-RSC pages stream at 96%+

📖 See [TESTING.md](./TESTING.md) for detailed testing documentation.

## Capturing Fixtures

To regenerate the HTML fixtures used by Rust integration tests:

```bash
npm ci
npm run capture-fixtures
```

This installs dependencies with `npm ci`, builds the app, starts `next start`,
captures HTML from each route, validates that RSC payloads are present, and
saves the output to `crates/common/src/integrations/nextjs/fixtures/`.

## How It Works

Each page component includes URLs with the `origin.example.com` hostname. When
Next.js renders these as RSC Flight data (inlined `<script>` tags with
`self.__next_f.push`), the Trusted Server's streaming HTML processor detects and
rewrites them to the proxy hostname.

The key RSC patterns exercised:

- **Simple payloads**: `self.__next_f.push([1,"...URL..."])` — single script,
  unfragmented
- **T-chunks**: `self.__next_f.push([1,"id:Tlen,<html content>"])` — HTML
  content with hex-encoded byte length that must be recalculated after rewriting
- **Cross-script T-chunks**: T-chunk header in one script, content continuing
  in subsequent scripts — requires combined payload processing

# Testing RSC Streaming

This directory contains tools for testing RSC (React Server Components) streaming behavior.

## Quick Test with Live Server

**Test HTML from your running Next.js server:**

```bash
# 1. Start your Next.js server
npm run dev
# Server starts on http://localhost:3099

# 2. In another terminal, test the live HTML
./test-live-html.sh

# Or test a specific route
./test-live-html.sh http://localhost:3099/about
./test-live-html.sh http://localhost:3099/blog/test-post
```

### What `test-live-html.sh` does:

1. ✅ Checks if server is running
2. 📥 Fetches fresh HTML from your live server
3. 📊 Analyzes raw HTML (RSC scripts, origin URLs)
4. 🔄 Processes through trusted-server pipeline
5. 📈 Shows streaming ratios for different chunk sizes
6. ✅ Verifies RSC payload URL rewriting

**Output example:**
```
📊 Raw HTML Analysis:
   RSC scripts: 5
   origin.example.com occurrences: 28

🧪 Running test with live HTML...
Chunk size    32B:  27.5% streamed (2014 intermediate, 5301 final)
Chunk size    64B:  27.6% streamed (2051 intermediate, 5264 final)
Chunk size   256B:  27.5% streamed (2014 intermediate, 5301 final)
Chunk size  8192B:   0.0% streamed (0 intermediate, 7315 final)

🔍 RSC Payload Verification (64B chunks):
   Origin URLs in RSC payloads: 0
   ✅ All RSC payload URLs successfully rewritten!
```

## Full Integration Test Suite

**Test with pre-captured fixtures (no server needed):**

```bash
# From project root
cargo test --test nextjs_integration -- --nocapture
```

This runs 20 tests across 7 fixtures:
- Hand-crafted fixtures (simple, tchunk, large, non-RSC)
- Real Next.js fixtures (home, about, blog)
- Multiple chunk sizes (32B, 64B, 256B, 8KB)

## Full E2E Test (Server + Tests)

**Build production server, fetch HTML, and run all tests:**

```bash
./test-streaming.sh
```

This script:
1. Installs dependencies (if needed)
2. Builds Next.js production bundle
3. Starts production server on port 3099
4. Fetches HTML from all routes
5. Verifies RSC content is present
6. Runs full integration test suite
7. Automatically stops server on exit

⚠️ **Note**: This uses production build (`npm run start`), not dev mode.

## Comparison

| Script | Server | HTML Source | Use Case |
|--------|--------|-------------|----------|
| `test-live-html.sh` | Uses existing | Live fetch | Quick iteration during development |
| `test-streaming.sh` | Starts own (prod) | Live fetch | Full E2E verification |
| `cargo test` | Not needed | Pre-captured fixtures | CI/CD, unit testing |

## Capturing New Fixtures

If you update the Next.js app and want to capture new fixtures:

```bash
npm ci
npm run capture-fixtures
```

This:
1. Builds production version
2. Starts server
3. Fetches HTML from all routes
4. Validates RSC content
5. Saves to `crates/common/src/integrations/nextjs/fixtures/`

## Interpreting Results

### Streaming Ratios

- **27-38% for RSC pages** ✅ Optimal (RSC scripts are 60-72% of HTML)
- **96%+ for non-RSC pages** ✅ Excellent
- **0% (all buffered)** ❌ RSC detected but not streaming

### RSC Payload Rewriting

- **0 origin URLs remaining** ✅ Perfect rewriting
- **> 0 origin URLs** ⚠️ Indicates fragmentation or issue

### Why 27-38% is optimal:

Next.js places RSC scripts at the END of the HTML:
```
<!DOCTYPE html>...         ← 27-38% streamed immediately
<html><body>...</body>
<script>__next_f.push(...) ← RSC detected, start buffering
<script>__next_f.push(...) ← 60-72% buffered for post-processing
</html>                    ← ~5 bytes
```

The RSC scripts themselves make up 60-72% of the document, so streaming more is not possible without streaming unbuffered RSC scripts (which would break URL rewriting).

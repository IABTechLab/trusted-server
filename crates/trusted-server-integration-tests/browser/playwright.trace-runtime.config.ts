import { defineConfig } from '@playwright/test'

const origin = process.env.TRACE_BROWSER_ORIGIN
const runtime = process.env.TRACE_BROWSER_RUNTIME
if (
  !origin ||
  !runtime ||
  !['fastly', 'axum', 'cloudflare', 'spin'].includes(runtime)
)
  throw new Error(
    'The Rust trace browser harness must supply its runtime and origin'
  )
const url = new URL(origin)
if (
  url.protocol !== 'http:' ||
  url.hostname !== 'localhost' ||
  !url.port ||
  url.username ||
  url.password ||
  url.pathname !== '/' ||
  url.search ||
  url.hash
)
  throw new Error(
    'Trace browser tests require an explicit localhost HTTP origin'
  )

// Rust owns the runtime and origin container. This config never starts or stops
// another fixture, and Playwright creates a fresh browser context for each test.
export default defineConfig({
  testDir: './tests',
  testMatch: 'shared/mobile-trace-runtime.spec.ts',
  timeout: 60_000,
  retries: 0,
  workers: 1,
  use: {
    baseURL: url.origin,
    headless: true,
    viewport: { width: 390, height: 844 },
    acceptDownloads: true,
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
  },
  projects: [{ name: runtime, use: { browserName: 'chromium' } }],
  reporter: [['list']],
  outputDir: `./test-results/trace-runtime-${runtime}`,
})

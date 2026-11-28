import { defineConfig } from '@playwright/test'

const origin = process.env.AUCTIONEER_BROWSER_ORIGIN
if (
  origin !== 'http://localhost:18081' ||
  process.env.AUCTIONEER_EVIDENCE_MANIFEST !== '/work/listeners.json'
)
  throw new Error(
    'Run the isolated Rust production worker; it owns the runtime and listener manifest'
  )

export default defineConfig({
  testDir: './tests',
  testMatch: 'shared/auction-runtime.spec.ts',
  timeout: 60_000,
  retries: 0,
  workers: 1,
  use: {
    baseURL: origin,
    headless: true,
    serviceWorkers: 'block',
    ignoreHTTPSErrors: false,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    { name: 'production-pbs-baseline', use: { browserName: 'chromium' } },
  ],
  reporter: [['list'], ['json', { outputFile: '/results/playwright.json' }]],
  outputDir: '/results/browser',
})

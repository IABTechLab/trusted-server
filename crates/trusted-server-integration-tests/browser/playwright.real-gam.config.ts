import { defineConfig } from "@playwright/test";

const pageUrl = process.env.TS_REAL_GAM_PAGE_URL;
const authorization = process.env.TS_REAL_GAM_AUTH_HEADER;
const releaseId = process.env.TS_REAL_GAM_EXPECTED_RELEASE_ID;

if (!pageUrl || !authorization || !releaseId) {
  throw new Error(
    "TS_REAL_GAM_PAGE_URL, TS_REAL_GAM_AUTH_HEADER, and TS_REAL_GAM_EXPECTED_RELEASE_ID are required",
  );
}

export default defineConfig({
  testDir: "./tests/shared",
  testMatch: "aps-real-gam.spec.ts",
  timeout: 75_000,
  expect: { timeout: 15_000 },
  retries: 0,
  workers: 3,
  fullyParallel: false,
  forbidOnly: true,
  use: {
    headless: true,
    extraHTTPHeaders: { Authorization: authorization },
    ignoreHTTPSErrors: false,
    screenshot: "off",
    trace: "off",
    video: "off",
  },
  projects: (["chromium", "firefox", "webkit"] as const).map((browserName) => ({
    name: browserName,
    use: { browserName },
  })),
  reporter: [
    ["list"],
    ["html", { open: "never", outputFolder: "playwright-report" }],
  ],
  outputDir: "./test-results",
});

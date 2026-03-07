import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  timeout: 30_000,
  retries: 0,
  // Sequential execution: all tests share a single origin port (8888)
  workers: 1,
  use: {
    baseURL: process.env.VICEROY_BASE_URL || "http://127.0.0.1:7878",
    headless: true,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { browserName: "chromium" },
    },
  ],
  reporter: [["list"], ["html", { open: "never" }]],
  outputDir: "./test-results",
});

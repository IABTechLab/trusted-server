import { defineConfig } from "@playwright/test";

// Firefox and WebKit are opt-in for targeted local validation.
const additionalBrowserProjects =
  process.env.PLAYWRIGHT_CROSS_BROWSER === "1"
    ? [
        {
          name: "firefox",
          use: { browserName: "firefox" as const },
        },
        {
          name: "webkit",
          use: { browserName: "webkit" as const },
        },
      ]
    : [];

export default defineConfig({
  testDir: "./tests",
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  timeout: 30_000,
  retries: 1,
  // Sequential execution: all tests share a single origin port (8888)
  workers: 1,
  use: {
    headless: true,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { browserName: "chromium" },
    },
    ...additionalBrowserProjects,
  ],
  reporter: [["list"], ["html", { open: "never" }]],
  outputDir: "./test-results",
});

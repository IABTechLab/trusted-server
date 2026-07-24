import { defineConfig } from "@playwright/test";

const framework = process.env.TEST_FRAMEWORK || "nextjs";

export default defineConfig({
  testDir: "./tests",
  testMatch:
    framework === "ad-trace"
      ? ["ad-trace/**/*.spec.ts"]
      : ["nextjs/**/*.spec.ts", "shared/**/*.spec.ts", "wordpress/**/*.spec.ts"],
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
  ],
  reporter: [["list"], ["html", { open: "never" }]],
  outputDir: `./test-results-${framework}`,
});

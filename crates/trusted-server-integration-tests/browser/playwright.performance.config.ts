import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  timeout: 30_000,
  retries: 0,
  workers: 1,
  use: { headless: true },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
  reporter: [["list"]],
  outputDir: "./test-results",
});

import { defineConfig } from "@playwright/test";

if (process.env.PUBLISHER_REGISTRATION_RUN !== "1") {
  throw new Error(
    "Use scripts/test-publisher-registration.sh for the isolated fixture settings",
  );
}

export default defineConfig({
  testDir: "./tests/nextjs",
  testMatch: "publisher-registration.spec.ts",
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  workers: 1,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  use: {
    headless: true,
    viewport: { width: 900, height: 800 },
    screenshot: "on",
    trace: "on",
  },
  projects: [
    {
      name: "publisher-registration-chromium",
      use: { browserName: "chromium" },
    },
  ],
  outputDir: process.env.PUBLISHER_REGISTRATION_RESULTS,
  reporter: [
    ["list"],
    [
      "json",
      {
        outputFile: `${process.env.PUBLISHER_REGISTRATION_RESULTS}/results.json`,
      },
    ],
  ],
});

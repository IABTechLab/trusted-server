import { defineConfig } from "@playwright/test";

const { loadInventory } = require("./publisher-registration/real-gpt.cjs");
loadInventory(process.env.PUBLISHER_REGISTRATION_REAL_GPT_INVENTORY_FILE);

export default defineConfig({
  testDir: "./tests/nextjs",
  testMatch: "publisher-registration.real-gpt.spec.ts",
  workers: 1,
  retries: 0,
  timeout: 90_000,
  expect: { timeout: 20_000 },
  use: {
    browserName: "chromium",
    headless: true,
    viewport: { width: 900, height: 800 },
    trace: "on",
    screenshot: "on",
    serviceWorkers: "block",
  },
  outputDir: process.env.PUBLISHER_REGISTRATION_REAL_GPT_RESULTS,
  reporter: [
    ["list"],
    [
      "json",
      {
        outputFile: `${process.env.PUBLISHER_REGISTRATION_REAL_GPT_RESULTS || "test-results/real-gpt"}/results.json`,
      },
    ],
  ],
});

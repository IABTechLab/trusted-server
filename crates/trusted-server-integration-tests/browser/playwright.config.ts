import { defineConfig } from "@playwright/test";

const SUPPORTED_PROJECTS = ["chromium", "firefox", "webkit"] as const;
const requestedProjects: string[] = (
  process.env.TS_BROWSER_PROJECTS ?? "chromium"
)
  .split(/[\s,]+/u)
  .filter(Boolean);

for (const project of requestedProjects) {
  if (
    !SUPPORTED_PROJECTS.includes(project as (typeof SUPPORTED_PROJECTS)[number])
  ) {
    throw new Error(
      `Unsupported TS_BROWSER_PROJECTS entry: ${project}. Expected chromium, firefox, or webkit.`,
    );
  }
}

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
  projects: requestedProjects.map((name) => ({
    name,
    use: { browserName: name as "chromium" | "firefox" | "webkit" },
  })),
  reporter: [["list"], ["html", { open: "never" }]],
  outputDir: "./test-results",
});

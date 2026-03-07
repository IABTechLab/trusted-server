import { test, expect } from "@playwright/test";

test.describe("Script injection", () => {
  test("injected script tag is present in the live DOM", async ({ page }) => {
    await page.goto("/", { waitUntil: "domcontentloaded" });
    const scriptTag = page.locator("script#trustedserver-js");
    await expect(scriptTag).toHaveCount(1);

    const src = await scriptTag.getAttribute("src");
    expect(src).toContain("/static/tsjs=");
  });

  test("no unexpected console errors on page load", async ({ page }) => {
    const errors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") errors.push(msg.text());
    });

    await page.goto("/", { waitUntil: "domcontentloaded" });

    // Give scripts a moment to execute and log errors
    await page.waitForTimeout(2000);

    // Suppress benign errors:
    // - favicon: not served by test containers
    // - "Failed to load resource": test fixture pages reference images/assets
    //   at origin URLs purely for attribute-rewriting tests; the resources
    //   don't need to exist on the server
    const unexpected = errors.filter(
      (e) =>
        !e.includes("favicon") &&
        !e.includes("Failed to load resource"),
    );
    expect(unexpected).toEqual([]);
  });
});

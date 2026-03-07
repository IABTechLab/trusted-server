import { test, expect } from "@playwright/test";

test.describe("Script bundle", () => {
  test("JS bundle loads with 200 and executes without parse errors", async ({
    page,
  }) => {
    const bundleResponses: { url: string; status: number }[] = [];
    page.on("response", (response) => {
      if (response.url().includes("/static/tsjs=")) {
        bundleResponses.push({
          url: response.url(),
          status: response.status(),
        });
      }
    });

    const jsErrors: string[] = [];
    page.on("pageerror", (error) => jsErrors.push(error.message));

    await page.goto("/", { waitUntil: "domcontentloaded" });

    // Wait for the bundle response specifically (up to 10s)
    if (bundleResponses.length === 0) {
      await page.waitForResponse(
        (resp) => resp.url().includes("/static/tsjs="),
        { timeout: 10_000 },
      );
    }

    // Bundle was requested and returned 200
    expect(bundleResponses.length).toBeGreaterThan(0);
    expect(bundleResponses[0].status).toBe(200);

    // No JS parse/runtime errors from the bundle
    expect(jsErrors).toEqual([]);
  });

  test("bundle response has correct content type", async ({ page }) => {
    let bundleContentType = "";

    const bundlePromise = page.waitForResponse(
      (resp) => resp.url().includes("/static/tsjs="),
      { timeout: 10_000 },
    );

    await page.goto("/", { waitUntil: "domcontentloaded" });

    const bundleResp = await bundlePromise;
    bundleContentType = bundleResp.headers()["content-type"] || "";

    expect(bundleContentType).toContain("javascript");
  });
});

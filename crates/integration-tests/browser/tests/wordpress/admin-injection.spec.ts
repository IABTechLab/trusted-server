import { test, expect } from "@playwright/test";
import { readState } from "../../helpers/state.js";

test.beforeEach(async ({}, testInfo) => {
  const state = readState();
  if (state.framework !== "wordpress") {
    testInfo.skip();
  }
});

test.describe("WordPress admin injection", () => {
  test("admin page has script tag in live DOM", async ({ page }) => {
    // Documents current behavior: the trusted-server injects the script
    // on ALL pages, including /wp-admin/. This test captures that behavior
    // so any future change (e.g. excluding admin pages) is intentional.
    await page.goto("/wp-admin/", { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);

    const src = await page.locator("script#trustedserver-js").getAttribute("src");
    expect(src).toContain("/static/tsjs=");
  });
});

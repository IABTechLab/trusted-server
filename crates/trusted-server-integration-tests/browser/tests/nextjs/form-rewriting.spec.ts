import { test, expect } from "@playwright/test";
import { readState, runtimeUrl } from "../../helpers/state.js";

test.beforeEach(async ({}, testInfo) => {
  const state = readState();
  if (state.framework !== "nextjs") {
    testInfo.skip();
  }
});

test.describe("Next.js form action rewriting", () => {
  test("form action URL is rewritten from origin to proxy", async ({
    page,
  }) => {
    await page.goto(runtimeUrl("/contact"), { waitUntil: "domcontentloaded" });

    const form = page.locator("form#contact-form");
    await expect(form).toHaveCount(1);

    const action = await form.getAttribute("action");
    expect(action).toBeTruthy();

    // Origin host should be rewritten to proxy host
    const originPort = process.env.INTEGRATION_ORIGIN_PORT || "8888";
    expect(action).not.toContain(`127.0.0.1:${originPort}`);
    // The path should be preserved
    expect(action).toContain("/api/contact");
  });

  test("contact page has script injection", async ({ page }) => {
    await page.goto(runtimeUrl("/contact"), { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);
  });
});

import { test, expect, type Page } from "@playwright/test";
import { readState, runtimeUrl } from "../../helpers/state.js";

const KERNEL_API_NAMES = [
  "_internal",
  "_registerIntegration",
  "addAdUnits",
  "boot",
  "diagnostics",
  "log",
  "que",
  "releaseId",
  "requestAds",
  "version",
] as const;

test.beforeEach(async ({}, testInfo) => {
  const state = readState();
  if (state.framework !== "nextjs") {
    testInfo.skip();
  }
});

/**
 * Wait for the Next.js client router to hydrate.
 *
 * The Navigation component sets `data-hydrated="true"` on `<html>` inside a
 * `useEffect`, which only fires after React hydration completes. Waiting for
 * this attribute prevents clicking `<Link>` elements before the client router
 * is ready (which would cause a full-page navigation instead of SPA).
 */
async function waitForHydration(page: Page): Promise<void> {
  await page.waitForSelector("html[data-hydrated='true']", {
    timeout: 10_000,
  });
}

async function captureKernel(page: Page): Promise<void> {
  await page.waitForFunction(
    () => (window as any).tsjs?._internal?.state === "kernel",
  );
  expect(
    await page.evaluate(() => {
      const browserWindow = window as any;
      browserWindow.__initialTsjsReference = browserWindow.tsjs;
      return {
        names: Object.getOwnPropertyNames(browserWindow.tsjs).sort(),
        legacy: Object.prototype.hasOwnProperty.call(
          browserWindow.tsjs,
          "gptDiagnostics",
        ),
      };
    }),
  ).toEqual({ names: KERNEL_API_NAMES, legacy: false });
}

async function expectSameKernel(page: Page): Promise<void> {
  expect(
    await page.evaluate(() => {
      const browserWindow = window as any;
      return {
        sameOwner: browserWindow.tsjs === browserWindow.__initialTsjsReference,
        state: browserWindow.tsjs?._internal?.state,
        names: Object.getOwnPropertyNames(browserWindow.tsjs).sort(),
      };
    }),
  ).toEqual({ sameOwner: true, state: "kernel", names: KERNEL_API_NAMES });
}

test.describe("Next.js client-side navigation", () => {
  test("4-page SPA navigation chain preserves script injection without full reload", async ({
    page,
  }) => {
    const jsErrors: string[] = [];
    page.on("pageerror", (error) => jsErrors.push(error.message));

    await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });

    // Script present on initial load
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);
    await captureKernel(page);

    // Wait for the client router to hydrate before clicking any links.
    await waitForHydration(page);

    // Track document-level navigations after initial load.
    // A full page reload triggers a new "document" request; SPA navigation does not.
    const documentRequests: string[] = [];
    page.on("request", (req) => {
      if (req.resourceType() === "document") {
        documentRequests.push(req.url());
      }
    });

    // Navigate: Home → About
    await page.click('#site-nav a[href="/about"]');
    await page.waitForURL("**/about", { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);
    await expectSameKernel(page);

    // Navigate: About → Dashboard
    await page.click('#site-nav a[href="/dashboard"]');
    await page.waitForURL("**/dashboard", { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);
    await expectSameKernel(page);

    // Navigate: Dashboard → Contact
    await page.click('#site-nav a[href="/contact"]');
    await page.waitForURL("**/contact", { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);
    await expectSameKernel(page);

    // Prove all navigations were true SPA — no document-level requests fired
    expect(documentRequests).toEqual([]);

    // No JS errors during the entire navigation chain
    expect(jsErrors).toEqual([]);
  });

  test("navigating back preserves script injection", async ({ page }) => {
    await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);

    // Wait for hydration before navigating
    await waitForHydration(page);

    // Navigate forward
    await page.click('#site-nav a[href="/about"]');
    await page.waitForURL("**/about", { waitUntil: "domcontentloaded" });

    // Navigate back
    await page.goBack({ waitUntil: "domcontentloaded" });

    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);
  });

  test("deferred route script executes after SPA transition to dashboard", async ({
    page,
  }) => {
    await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });
    await waitForHydration(page);

    // No route script marker on home page
    const before = await page.evaluate(
      () => (window as any).__routeScriptExecuted,
    );
    expect(before).toBeUndefined();

    // Track document requests to prove this is a true SPA transition
    const documentRequests: string[] = [];
    page.on("request", (req) => {
      if (req.resourceType() === "document") {
        documentRequests.push(req.url());
      }
    });

    // SPA navigate to dashboard
    await page.click('#site-nav a[href="/dashboard"]');
    await page.waitForURL("**/dashboard", { waitUntil: "domcontentloaded" });

    // Wait for the deferred script to execute
    await page.waitForFunction(
      () => (window as any).__routeScriptExecuted === "dashboard",
      { timeout: 5_000 },
    );

    // Script executed exactly once
    const count = await page.evaluate(
      () => (window as any).__routeScriptExecutionCount,
    );
    expect(count).toBe(1);

    // Prove this was an SPA transition, not a full reload
    expect(documentRequests).toEqual([]);
  });

  test("about page has script injection via direct navigation", async ({
    page,
  }) => {
    await page.goto(runtimeUrl("/about"), { waitUntil: "domcontentloaded" });
    await expect(page.locator("script#trustedserver-js")).toHaveCount(1);

    const src = await page
      .locator("script#trustedserver-js")
      .getAttribute("src");
    expect(src).toContain("/static/tsjs=");
  });
});

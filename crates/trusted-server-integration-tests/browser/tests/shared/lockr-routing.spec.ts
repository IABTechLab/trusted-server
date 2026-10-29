import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

const PUBLISHER_ORIGIN = "https://publisher.example";
const EXTERNAL_ORIGIN = "https://external-base.example";
const SDK_PATH = "/integrations/lockr/sdk";
const API_PREFIX = "/integrations/lockr/api/publisher/app/v2/identityLockr";
const REQUEST_PATHS = [
  `${API_PREFIX}/settings`,
  `${API_PREFIX}/page-view`,
  "/unrelated/api",
];
const LOCKR_BUNDLE = readFileSync(
  resolve(__dirname, "../../../../trusted-server-js/dist/tsjs-lockr.js"),
  "utf8",
);

const STUB_SDK = `
window.lockrRequests = Promise.all(${JSON.stringify(REQUEST_PATHS)}.map(function(path) {
  return new Promise(function(resolve, reject) {
    var xhr = new XMLHttpRequest();
    xhr.onload = function() { resolve({ url: xhr.responseURL, status: xhr.status }); };
    xhr.onerror = function() { reject(new Error("Stub SDK request failed: " + path)); };
    xhr.open("POST", path);
    xhr.send("{}");
  });
}));
`;

for (const scenario of [
  { name: "base before the bundle", guarded: true, baseFirst: true },
  { name: "base after the bundle", guarded: true, baseFirst: false },
  { name: "unguarded control", guarded: false, baseFirst: true },
]) {
  test(`Lockr XHR destinations with ${scenario.name}`, async ({ page }) => {
    const base = `<base href="${EXTERNAL_ORIGIN}/assets/">`;
    const bundle = scenario.guarded
      ? `<script src="${PUBLISHER_ORIGIN}/tsjs-lockr.js"></script>`
      : "";
    const html = `<!doctype html><html><head>
      ${scenario.baseFirst ? base + bundle : bundle + base}
      <script src="${PUBLISHER_ORIGIN}${SDK_PATH}"></script>
      </head><body></body></html>`;
    const apiRequests: string[] = [];
    const sdkRequests: string[] = [];
    const unexpectedRequests: string[] = [];
    const errors: string[] = [];

    page.on("pageerror", (error) => errors.push(error.message));

    // Intercept every destination so neither the SDK nor identity data reaches a vendor.
    await page.route("**/*", async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (
        url.origin === PUBLISHER_ORIGIN &&
        url.pathname === "/lockr-fixture"
      ) {
        await route.fulfill({ contentType: "text/html", body: html });
      } else if (
        url.origin === PUBLISHER_ORIGIN &&
        url.pathname === "/tsjs-lockr.js"
      ) {
        await route.fulfill({
          contentType: "application/javascript",
          body: LOCKR_BUNDLE,
        });
      } else if (url.origin === PUBLISHER_ORIGIN && url.pathname === SDK_PATH) {
        sdkRequests.push(request.url());
        await route.fulfill({
          contentType: "application/javascript",
          body: STUB_SDK,
        });
      } else if (
        [PUBLISHER_ORIGIN, EXTERNAL_ORIGIN].includes(url.origin) &&
        REQUEST_PATHS.includes(url.pathname) &&
        request.method() === "POST"
      ) {
        apiRequests.push(request.url());
        await route.fulfill({
          contentType: "application/json",
          headers: { "Access-Control-Allow-Origin": "*" },
          body: "{}",
        });
      } else {
        unexpectedRequests.push(request.url());
        await route.abort();
      }
    });

    const expected = REQUEST_PATHS.map(
      (path) =>
        `${scenario.guarded && path.startsWith(API_PREFIX) ? PUBLISHER_ORIGIN : EXTERNAL_ORIGIN}${path}`,
    );

    // Each navigation gets a fresh document and must install the guard before SDK execution.
    for (const reload of [false, true]) {
      apiRequests.length = 0;
      sdkRequests.length = 0;
      if (reload) {
        await page.reload();
      } else {
        await page.goto(`${PUBLISHER_ORIGIN}/lockr-fixture`);
      }

      const responses = await page.evaluate(
        () =>
          (
            window as unknown as {
              lockrRequests: Promise<{ url: string; status: number }[]>;
            }
          ).lockrRequests,
      );

      expect(responses).toEqual(expected.map((url) => ({ url, status: 200 })));
      expect([...apiRequests].sort()).toEqual([...expected].sort());
      expect(sdkRequests).toEqual([`${PUBLISHER_ORIGIN}${SDK_PATH}`]);
      expect(unexpectedRequests).toEqual([]);
      expect(errors).toEqual([]);
    }
  });
}

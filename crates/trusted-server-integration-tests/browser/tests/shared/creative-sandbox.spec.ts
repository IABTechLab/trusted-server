import { test, expect } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

// Creative iframes are sandboxed WITHOUT `allow-same-origin`, so the creative
// runtime executes in an opaque origin whose `location.href` is `about:srcdoc`.
// jsdom cannot reproduce either condition, so the recovery path these tests
// cover — resolve against the stamped first-party origin, skip the CORS-doomed
// POST, navigate the GET rebuild fallback — is only observable in a real
// browser.
const CREATIVE_SANDBOX_TOKENS = [
  "allow-forms",
  "allow-popups",
  "allow-popups-to-escape-sandbox",
  "allow-scripts",
  "allow-top-navigation-by-user-activation",
].join(" ");

// Mirrors the srcdoc document the client builds: the first-party parent stamps
// its own origin ahead of any creative markup, then the runtime, then the
// creative. The anchor carries a root-relative signed click exactly as the
// server-side rewriter emits it.
function creativeDocument(origin: string, bundleUrl: string): string {
  const signedClick =
    "/first-party/click?tsurl=https%3A%2F%2Fadvertiser.example%2Flanding&foo=1&tstoken=browser-test-token";
  return `<!DOCTYPE html>
<html>
  <head>
    <script>window.__tsCreativeOrigin = '${origin}';</script>
    <script src="${bundleUrl}"></script>
  </head>
  <body>
    <a id="creative-link" href="${signedClick}" data-tsclick="${signedClick}">ad</a>
  </body>
</html>`;
}

test.describe("Sandboxed creative iframe", () => {
  test("recovers a mutated click through the GET rebuild fallback", async ({
    page,
  }) => {
    await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });

    // Prefer whichever hashed bundle URL the server injected into the page so
    // this test never has to know the current content hash; fall back to the
    // stable unified path if the fixture page carries no injected script.
    const injectedBundle = await page.evaluate(() => {
      const script = Array.from(document.querySelectorAll("script[src]")).find(
        (element) => (element as HTMLScriptElement).src.includes("/static/tsjs="),
      );
      return script ? (script as HTMLScriptElement).src : null;
    });
    const bundleUrl =
      injectedBundle ?? runtimeUrl("/static/tsjs=tsjs-unified.min.js");

    const rebuildRequest = page.waitForRequest(
      (request) => request.url().includes("/first-party/proxy-rebuild"),
      { timeout: 15_000 },
    );

    await page.evaluate(
      ({ sandbox, html }) => {
        const iframe = document.createElement("iframe");
        iframe.setAttribute("sandbox", sandbox);
        iframe.srcdoc = html;
        iframe.style.width = "300px";
        iframe.style.height = "250px";
        document.body.appendChild(iframe);
      },
      {
        sandbox: CREATIVE_SANDBOX_TOKENS,
        html: creativeDocument(new URL(runtimeUrl("/")).origin, bundleUrl),
      },
    );

    const frame = page.frameLocator("iframe");
    const link = frame.locator("#creative-link");
    await link.waitFor({ state: "attached", timeout: 10_000 });

    // The creative mutates its own click target, the shape the click guard
    // exists to repair.
    await link.evaluate((element) => {
      element.setAttribute(
        "href",
        "https://advertiser.example/landing?foo=1&bar=2",
      );
    });

    await link.click({ force: true });

    const request = await rebuildRequest;
    expect(request.url()).toContain("tsclick=");
    expect(decodeURIComponent(request.url())).toContain("bar");
  });
});

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
function creativeDocument(
  origin: string,
  bundleUrl: string,
  releaseId: string,
  signedClick: string,
): string {
  const boot = JSON.stringify({
    abi: 1,
    releaseId,
    manifest: {
      version: 1,
      releaseId,
      integrations: [{ id: "creative", required: true }],
    },
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: "creative-sandbox", results: [] },
      slots: [],
      bids: [],
    },
    creative: {
      version: 1,
      enabled: true,
      clickGuard: true,
      renderGuard: false,
    },
    diagnostics: {
      version: 1,
      renderTraceOverlay: false,
      gpt: { active: false },
    },
  });
  return `<!DOCTYPE html>
<html>
  <head>
    <script>
      Object.defineProperty(window, '__tsCreativeOrigin', {
        value: ${JSON.stringify(origin)},
        writable: false,
        configurable: false,
        enumerable: false,
      });
      window.tsjs = { boot: ${boot}, que: [], _integrationConfig: {} };
    </script>
    <script>
      try { window.__tsCreativeOrigin = 'https://attacker.invalid'; } catch (_error) {}
    </script>
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
    const origin = new URL(runtimeUrl("/")).origin;
    const landing = runtimeUrl("/health");
    const signResponse = await page.request.post(
      runtimeUrl("/first-party/sign"),
      { data: { url: landing } },
    );
    expect(signResponse.ok()).toBe(true);
    const signedProxyHref = ((await signResponse.json()) as { href: string })
      .href;
    const signedClick = signedProxyHref.replace(
      "/first-party/proxy?",
      "/first-party/click?",
    );
    expect(signedClick.startsWith("/first-party/click?")).toBe(true);

    await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });

    // Prefer whichever hashed bundle URL the server injected into the page so
    // this test never has to know the current content hash; fall back to the
    // stable unified path if the fixture page carries no injected script.
    const runtime = await page.evaluate(() => {
      const script = Array.from(document.querySelectorAll("script[src]")).find(
        (element) =>
          (element as HTMLScriptElement).src.includes("/static/tsjs="),
      );
      return {
        bundleUrl: script ? (script as HTMLScriptElement).src : null,
        releaseId: (window as any).tsjs?.releaseId as string | undefined,
      };
    });
    const bundleUrl =
      runtime.bundleUrl ?? runtimeUrl("/static/tsjs=tsjs-unified.min.js");
    expect(runtime.releaseId).toMatch(/^[a-f0-9]{64}$/);

    const rebuildResponse = page.waitForResponse(
      (response) => response.url().includes("/first-party/proxy-rebuild"),
      { timeout: 15_000 },
    );
    const clickResponse = page.waitForResponse(
      (response) =>
        response.url().includes("/first-party/click") &&
        response.request().resourceType() === "document",
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
        html: creativeDocument(
          origin,
          bundleUrl,
          runtime.releaseId!,
          signedClick,
        ),
      },
    );

    const frame = page.frameLocator("iframe");
    const link = frame.locator("#creative-link");
    await link.waitFor({ state: "attached", timeout: 10_000 });
    await expect
      .poll(() =>
        frame.locator("html").evaluate(() => {
          const api = (window as any).tsjs;
          return {
            state: api?._internal?.state,
            names: Object.getOwnPropertyNames(api ?? {}).sort(),
            legacyCreativeGlobal: Object.prototype.hasOwnProperty.call(
              window,
              "tscreative",
            ),
          };
        }),
      )
      .toEqual({
        state: "kernel",
        names: [
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
        ],
        legacyCreativeGlobal: false,
      });

    // The creative mutates its own click target, the shape the click guard
    // exists to repair.
    await link.evaluate((element, click) => {
      element.setAttribute("href", `${click}&bar=2`);
    }, signedClick);

    await link.click({ force: true });

    const rebuild = await rebuildResponse;
    expect(rebuild.url().startsWith(origin)).toBe(true);
    expect(rebuild.status()).toBe(302);
    const click = await clickResponse;
    expect(click.url().startsWith(origin)).toBe(true);
    expect(click.status()).toBe(302);
    expect(decodeURIComponent(click.url())).toContain("bar=2");
  });
});

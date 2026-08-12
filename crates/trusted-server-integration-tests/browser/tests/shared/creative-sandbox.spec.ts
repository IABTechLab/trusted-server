import { test, expect } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

// Creative iframes are sandboxed WITHOUT `allow-same-origin`, so the creative
// runtime executes in an opaque origin whose `location.href` is `about:srcdoc`.
// jsdom cannot reproduce either condition, so the recovery path these tests
// cover — resolve against the stamped first-party origin, skip the CORS-doomed
// POST, navigate the GET rebuild fallback, follow the re-signed click — is only
// observable in a real browser.
const CREATIVE_SANDBOX_TOKENS = [
  "allow-forms",
  "allow-popups",
  "allow-popups-to-escape-sandbox",
  "allow-scripts",
  "allow-top-navigation-by-user-activation",
].join(" ");

// A creative <head> script that tries to redirect click resolution to an
// attacker origin. It executes before the runtime injected at the top of
// <body>, so the stamp must be non-writable for the recovery below to stay on
// the first-party origin.
const HOSTILE_STAMP_OVERWRITE = `<script>
  try { window.__tsCreativeOrigin = 'https://attacker.invalid'; } catch (e) {}
</script>`;

test.describe("Sandboxed creative iframe", () => {
  test("recovers a mutated click through the signed GET rebuild chain", async ({
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
    const signedClick = signedProxyHref.replace(
      "/first-party/proxy?",
      "/first-party/click?",
    );
    expect(signedClick.startsWith("/first-party/click?")).toBe(true);

    await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });

    // Reuse whichever hashed bundle URL the server injected into the page so
    // this test never has to know the current content hash; fall back to the
    // stable unified path if the fixture page carries no injected script.
    const injectedBundle = await page.evaluate(() => {
      const script = Array.from(document.querySelectorAll("script[src]")).find(
        (element) =>
          (element as HTMLScriptElement).src.includes("/static/tsjs="),
      );
      return script ? (script as HTMLScriptElement).src : null;
    });
    const bundleUrl =
      injectedBundle ?? runtimeUrl("/static/tsjs=tsjs-unified.min.js");

    // Mirrors the srcdoc document the client builds: the first-party parent
    // stamps its own origin ahead of any creative markup, then the runtime,
    // then the creative — here preceded by hostile markup attempting to move
    // the stamp.
    const creativeDocument = `<!DOCTYPE html>
<html>
  <head>
    <script>
      Object.defineProperty(window, '__tsCreativeOrigin', {
        value: '${origin}', writable: false, configurable: false, enumerable: false,
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
    // The hostile overwrite must not have moved resolution off the first-party
    // origin, and the rebuild must actually succeed rather than error.
    expect(rebuild.url().startsWith(origin)).toBe(true);
    expect(rebuild.status(), "GET rebuild should redirect").toBe(302);

    const click = await clickResponse;
    expect(click.url().startsWith(origin)).toBe(true);
    expect(click.status(), "re-signed click should redirect").toBe(302);
    // The rebuilt click carries the mutation the creative added.
    expect(decodeURIComponent(click.url())).toContain("bar=2");
  });
});

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

const IFRAME_CREATIVE_URL = "https://creative.example/iframe";
const APS_TEST_ORIGIN = "https://aps-renderer.test";
const APS_TEST_RENDERER_URL = `${APS_TEST_ORIGIN}/integrations/aps/renderer/v2`;
const APS_TEST_RUNNER_URL = `${APS_TEST_ORIGIN}/integrations/aps/runner.js`;
const SANDBOX =
  "allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation";
const PERMANENT_SANDBOX = `${SANDBOX} allow-same-origin`;
const FICTIONAL_APS_RUNNER = readFileSync(
  resolve(__dirname, "../../fixtures/fictional-aps-runner.js"),
  "utf8",
);

function descriptor(
  bidId = "fictional-iframe-bid",
  overrides: Record<string, unknown> = {},
) {
  const tagType = overrides.tagType === "script" ? "script" : "iframe";
  const creativeUrl =
    typeof overrides.creativeUrl === "string"
      ? overrides.creativeUrl
      : IFRAME_CREATIVE_URL;
  const width = typeof overrides.width === "number" ? overrides.width : 300;
  const height = typeof overrides.height === "number" ? overrides.height : 250;
  const envelope = {
    seatbid: [
      {
        bid: [
          {
            id: bidId,
            price: 1.23,
            w: width,
            h: height,
            ext: { creativeurl: creativeUrl, tagtype: tagType },
          },
        ],
      },
    ],
  };
  return {
    type: "aps",
    version: 1,
    accountId: "example-account-id",
    bidId,
    creativeId: `fictional-${tagType}-creative`,
    tagType,
    creativeUrl,
    aaxResponse: Buffer.from(JSON.stringify(envelope), "utf8").toString(
      "base64",
    ),
    width,
    height,
    ...overrides,
  };
}

function containerCsp(
  creativeOrigin: string,
  outer: boolean,
  scriptCreative = false,
): string {
  const scriptSources = scriptCreative
    ? `'unsafe-inline' ${APS_TEST_ORIGIN} ${creativeOrigin}`
    : `'unsafe-inline' ${APS_TEST_ORIGIN}`;
  return (
    "default-src 'none'; base-uri 'none'; object-src 'none'; script-src " +
    `${scriptSources}; connect-src https: ${APS_TEST_ORIGIN}; frame-src ` +
    `${outer ? "data: " : ""}${creativeOrigin}; img-src https: data: blob:; ` +
    "media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; " +
    "worker-src https: blob:; form-action https:;"
  );
}

test.describe("APS renderer v2 protocol", () => {
  test("leaves every removed or unknown APS route unserved", async ({
    page,
  }) => {
    for (const path of [
      "/integrations/aps/renderer",
      "/integrations/aps/renderer/v1",
      "/integrations/aps/runner/v1.js",
    ]) {
      const response = await page.request.get(runtimeUrl(path));
      expect(response.status(), path).toBe(404);
    }
  });

  test("uses one port, reports ordered progress, and fails closed", async ({
    page,
  }) => {
    test.setTimeout(60_000);
    const rendererResponse = await page.request.get(
      runtimeUrl("/integrations/aps/renderer/v2"),
    );
    expect(rendererResponse.status()).toBe(200);
    expect(rendererResponse.headers()["content-type"]).toBe(
      "text/html; charset=utf-8",
    );
    expect(rendererResponse.headers()["cache-control"]).toBe(
      "public, max-age=31536000, immutable",
    );
    expect(rendererResponse.headers()["x-content-type-options"]).toBe(
      "nosniff",
    );
    expect(rendererResponse.headers()["referrer-policy"]).toBe("no-referrer");
    expect(rendererResponse.headers()["x-frame-options"]).toBeUndefined();
    const csp = rendererResponse.headers()["content-security-policy"];
    expect(csp).toBe(
      "default-src 'none'; sandbox allow-scripts; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline'; frame-ancestors 'self'; form-action 'none';",
    );
    const rendererDocument = await rendererResponse.text();

    // Fulfil the exact served document and its parent at one HTTPS test origin so
    // frame-ancestors 'self', parent-origin authentication, and the live runner
    // proxy URL all exercise the production shape over the local test transport.
    await page.route(APS_TEST_RENDERER_URL, (route) =>
      route.fulfill({
        status: 200,
        headers: {
          "content-type": "text/html; charset=utf-8",
          "content-security-policy": csp!,
          "x-content-type-options": "nosniff",
          "referrer-policy": "no-referrer",
        },
        body: rendererDocument,
      }),
    );

    let runnerRequests = 0;
    await page.route(APS_TEST_RUNNER_URL, async (route) => {
      runnerRequests += 1;
      await route.fulfill({
        status: 200,
        headers: {
          "content-type": "application/javascript; charset=utf-8",
          "access-control-allow-origin": "*",
          "cross-origin-resource-policy": "cross-origin",
          "x-content-type-options": "nosniff",
          "referrer-policy": "no-referrer",
        },
        body: FICTIONAL_APS_RUNNER,
      });
    });
    await page.route("https://external.example/blocked.js", (route) => {
      return route.fulfill({
        status: 200,
        contentType: "application/javascript",
        body: "document.documentElement.dataset.externalScript='executed'",
      });
    });
    await page.route("https://other.example/other.js", (route) => {
      return route.fulfill({
        status: 200,
        contentType: "application/javascript",
        body: "document.documentElement.dataset.otherScript='executed'",
      });
    });
    await page.route("https://redirect.example/final.js", (route) => {
      return route.fulfill({
        status: 200,
        contentType: "application/javascript",
        body: "document.documentElement.dataset.redirectedScript='executed'",
      });
    });
    await page.route("https://creative.example/**", (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === "/iframe-policy") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          headers: {
            "content-security-policy":
              "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'",
          },
          body: `<!doctype html><style>html,body{border:0;height:100%;margin:0;overflow:hidden;padding:0;width:100%}</style><script src="https://external.example/blocked.js"></script>`,
        });
      }
      if (url.pathname === "/script-allowed.js") {
        return route.fulfill({
          status: 200,
          contentType: "application/javascript",
          body: `document.documentElement.dataset.scriptCreative='executed';var script=document.createElement('script');script.src='https://other.example/other.js';document.head.appendChild(script);`,
        });
      }
      if (url.pathname === "/script-redirect.js") {
        return route.fulfill({
          status: 302,
          headers: { location: "https://redirect.example/final.js" },
        });
      }
      return route.abort();
    });
    await page.route(`${APS_TEST_ORIGIN}/aps-v2-protocol-test`, (route) =>
      route.fulfill({
        status: 200,
        contentType: "text/html",
        headers: {
          "content-security-policy":
            "default-src 'none'; script-src 'unsafe-inline'; frame-src 'self' data: https://creative.example",
        },
        body: `<!doctype html><meta charset="utf-8"><div id="slots"></div><script>
window.apsV2Records = Object.create(null);
window.startApsV2 = function(options) {
  var slot = document.createElement('div');
  slot.id = options.slotId;
  slot.style.height = options.renderer.height + 'px';
  slot.style.overflow = 'hidden';
  slot.style.position = 'relative';
  slot.style.width = options.renderer.width + 'px';
  slot.innerHTML = '<span class="existing">existing publisher content</span>';
  document.getElementById('slots').appendChild(slot);
  var frame = document.createElement('iframe');
  frame.setAttribute('sandbox', ${JSON.stringify(SANDBOX)});
  frame.setAttribute('width', String(options.renderer.width));
  frame.setAttribute('height', String(options.renderer.height));
  frame.src = ${JSON.stringify(APS_TEST_RENDERER_URL)} + '#' + options.bootstrapNonce;
  frame.style.border = '0';
  frame.style.display = 'none';
  frame.style.height = options.renderer.height + 'px';
  frame.style.margin = '0';
  frame.style.overflow = 'hidden';
  frame.style.width = options.renderer.width + 'px';
  var record = {messages: [], snapshots: [], bootstrapConfiguration: null, frame: frame, port: null};
  window.apsV2Records[options.slotId] = record;
  function receive(event) {
    if (event.source !== frame.contentWindow || event.origin !== 'null' ||
        typeof event.data !== 'string') return;
    var value;
    try { value = JSON.parse(event.data); } catch (_error) { return; }
    if (value.message === 'TS APS Bootstrap Ready' && value.version === 1 &&
        value.bootstrapNonce === options.bootstrapNonce && event.ports.length === 0) {
      frame.setAttribute('sandbox', ${JSON.stringify(PERMANENT_SANDBOX)});
      var creativeOrigin = new URL(options.renderer.creativeUrl).origin;
      if (options.adversarialBootstrap) {
        var invalidChannel = new MessageChannel();
        frame.contentWindow.postMessage('{"message":', '*', []);
        frame.contentWindow.postMessage('x'.repeat(16385), '*', []);
        frame.contentWindow.postMessage(JSON.stringify({
          message: 'TS APS Bootstrap Configure',
          version: 2,
          bootstrapNonce: 'b1_0000000000000000000000',
          rendererNonce: options.rendererNonce,
          creativeOrigin: creativeOrigin,
          tagType: options.renderer.tagType
        }), '*', []);
        frame.contentWindow.postMessage(JSON.stringify({
          message: 'TS APS Bootstrap Configure',
          version: 2,
          bootstrapNonce: options.bootstrapNonce,
          rendererNonce: options.rendererNonce,
          creativeOrigin: creativeOrigin,
          tagType: options.renderer.tagType
        }), '*', [invalidChannel.port1]);
        invalidChannel.port2.close();
      }
      record.bootstrapConfiguration = {
        message: 'TS APS Bootstrap Configure',
        version: 2,
        bootstrapNonce: options.bootstrapNonce,
        rendererNonce: options.rendererNonce,
        creativeOrigin: creativeOrigin,
        tagType: options.renderer.tagType
      };
      frame.contentWindow.postMessage(JSON.stringify(record.bootstrapConfiguration), '*', []);
      if (options.adversarialBootstrap) {
        frame.contentWindow.postMessage(JSON.stringify(record.bootstrapConfiguration), '*', []);
      }
      return;
    }
    if (value.message !== 'TS APS Container Ready' || value.version !== 1 ||
        value.bootstrapNonce !== options.bootstrapNonce ||
        value.rendererNonce !== options.rendererNonce || event.ports.length !== 1) return;
    window.removeEventListener('message', receive, true);
    record.port = event.ports[0];
    record.port.onmessage = function(portEvent) {
      record.messages.push(portEvent.data);
      record.snapshots.push({
        message: portEvent.data && portEvent.data.message,
        display: frame.style.display,
        existing: slot.querySelectorAll('.existing').length
      });
      if (portEvent.data && portEvent.data.message === 'TS APS Render Completed') {
        var existing = slot.querySelector('.existing');
        if (existing) existing.remove();
        frame.style.display = '';
      }
    };
    record.port.start();
    record.port.postMessage({
      version: 1,
      nonce: options.rendererNonce,
      publisherOrigin: location.origin,
      renderer: options.renderer
    });
  }
  window.addEventListener('message', receive, true);
  slot.appendChild(frame);
};
</script>`,
      }),
    );
    await page.goto(`${APS_TEST_ORIGIN}/aps-v2-protocol-test`);

    const start = async (
      slotId: string,
      bidId: string,
      rendererOverrides: Record<string, unknown> = {},
      adversarialBootstrap = false,
    ) => {
      const bootstrapNonce = `b1_${slotId.padEnd(22, "b").slice(0, 22)}`;
      const rendererNonce = `n1_${slotId.padEnd(22, "n").slice(0, 22)}`;
      await page.evaluate(
        ({ slotId, bootstrapNonce, rendererNonce, renderer }) => {
          (
            window as unknown as {
              startApsV2(options: Record<string, unknown>): void;
            }
          ).startApsV2({ slotId, bootstrapNonce, rendererNonce, renderer });
        },
        {
          slotId,
          bootstrapNonce,
          rendererNonce,
          renderer: descriptor(bidId, rendererOverrides),
          adversarialBootstrap,
        },
      );
      return rendererNonce;
    };
    const messages = (slotId: string) =>
      page.evaluate(
        (id) =>
          (
            window as unknown as {
              apsV2Records: Record<
                string,
                { messages: Array<Record<string, unknown>> }
              >;
            }
          ).apsV2Records[id]?.messages ?? [],
        slotId,
      );
    const snapshots = (slotId: string) =>
      page.evaluate(
        (id) =>
          (
            window as unknown as {
              apsV2Records: Record<
                string,
                {
                  snapshots: Array<{
                    message: string;
                    display: string;
                    existing: number;
                  }>;
                }
              >;
            }
          ).apsV2Records[id]?.snapshots ?? [],
        slotId,
      );
    const bootstrapConfiguration = (slotId: string) =>
      page.evaluate(
        (id) =>
          (
            window as unknown as {
              apsV2Records: Record<
                string,
                { bootstrapConfiguration: Record<string, unknown> | null }
              >;
            }
          ).apsV2Records[id]?.bootstrapConfiguration ?? null,
        slotId,
      );

    await start("duplicate-success", "duplicate-success-bid");
    await expect
      .poll(async () =>
        (await messages("duplicate-success")).map((message) => message.message),
      )
      .toEqual([
        "TS APS Document Accepted",
        "TS APS Runner Loaded",
        "TS APS Render Completed",
      ]);
    await expect(page.locator("#duplicate-success .existing")).toHaveCount(0);
    expect(
      (await messages("duplicate-success")).filter((message) =>
        String(message.message).includes("Render "),
      ),
    ).toHaveLength(1);

    await start("reject-case", "reject-case-bid");
    await expect
      .poll(async () => await messages("reject-case"))
      .toContainEqual(
        expect.objectContaining({
          message: "TS APS Render Failed",
          reason: "runner_failed",
        }),
      );
    await expect(page.locator("#reject-case .existing")).toHaveCount(1);

    await start("silent-case", "silent-case-bid");
    await expect
      .poll(async () =>
        (await messages("silent-case")).map((message) => message.message),
      )
      .toEqual(["TS APS Document Accepted", "TS APS Runner Loaded"]);
    await page.waitForTimeout(150);
    expect(await messages("silent-case")).toHaveLength(2);

    await start("deferred-visibility", "deferred-visibility-bid");
    await expect
      .poll(async () =>
        (await messages("deferred-visibility")).map(
          (message) => message.message,
        ),
      )
      .toEqual(["TS APS Document Accepted", "TS APS Runner Loaded"]);
    expect(await snapshots("deferred-visibility")).toEqual([
      {
        message: "TS APS Document Accepted",
        display: "none",
        existing: 1,
      },
      {
        message: "TS APS Runner Loaded",
        display: "none",
        existing: 1,
      },
    ]);
    await expect(page.locator("#deferred-visibility > iframe")).toHaveCSS(
      "display",
      "none",
    );
    await page
      .frameLocator("#deferred-visibility > iframe")
      .frameLocator('iframe[title="Ad content"]')
      .locator("body")
      .evaluate(() =>
        (
          window as unknown as {
            __fictionalApsResolve?: () => void;
          }
        ).__fictionalApsResolve?.(),
      );
    await expect
      .poll(async () =>
        (await messages("deferred-visibility")).map(
          (message) => message.message,
        ),
      )
      .toEqual([
        "TS APS Document Accepted",
        "TS APS Runner Loaded",
        "TS APS Render Completed",
      ]);
    await expect(page.locator("#deferred-visibility > iframe")).toBeVisible();
    await expect(page.locator("#deferred-visibility .existing")).toHaveCount(0);

    const nestedRendererNonce = await start(
      "nested-case",
      "nested-case-bid",
      {},
      true,
    );
    await expect
      .poll(async () => await messages("nested-case"))
      .toContainEqual(
        expect.objectContaining({
          message: "TS APS Render Completed",
        }),
      );
    const nestedOuterFrame = page.locator("#nested-case > iframe");
    const nestedOuter = page.frameLocator("#nested-case > iframe");
    const nestedInnerFrame = nestedOuter.locator(
      'body > iframe[title="Ad content"]',
    );
    const nestedInner = nestedOuter.frameLocator(
      'body > iframe[title="Ad content"]',
    );
    const nestedCreativeFrame = nestedInner.locator(
      'body > iframe[data-fictional-creative="nested"]',
    );
    const nestedCreative = nestedInner.frameLocator(
      'body > iframe[data-fictional-creative="nested"]',
    );
    await expect(nestedOuterFrame).toHaveCount(1);
    await expect(nestedInnerFrame).toHaveCount(1);
    await expect(nestedCreativeFrame).toHaveCount(1);
    await expect(nestedOuterFrame).toHaveAttribute(
      "sandbox",
      PERMANENT_SANDBOX,
    );
    await expect(nestedInnerFrame).toHaveAttribute(
      "sandbox",
      PERMANENT_SANDBOX,
    );
    await expect(nestedCreativeFrame).toHaveAttribute(
      "sandbox",
      "allow-scripts",
    );
    expect(
      await nestedOuter.locator("html").evaluate(() => location.origin),
    ).toBe("null");
    expect(
      await nestedInner.locator("html").evaluate(() => location.origin),
    ).toBe("null");
    expect(
      await nestedCreative.locator("html").evaluate(() => location.origin),
    ).toBe("null");
    for (const frame of [nestedOuter, nestedInner, nestedCreative]) {
      expect(
        await frame.locator("html").evaluate(() => {
          try {
            return window.top?.document === document;
          } catch {
            return false;
          }
        }),
      ).toBe(false);
    }
    await expect(
      nestedOuter.locator('meta[http-equiv="Content-Security-Policy"]'),
    ).toHaveAttribute(
      "content",
      containerCsp("https://creative.example", true),
    );
    await expect(
      nestedInner.locator('meta[http-equiv="Content-Security-Policy"]'),
    ).toHaveAttribute(
      "content",
      containerCsp("https://creative.example", false),
    );
    const nestedBootstrapNonce = `b1_${"nested-case".padEnd(22, "b").slice(0, 22)}`;
    expect(nestedRendererNonce).not.toBe(nestedBootstrapNonce);
    await expect(nestedOuterFrame).toHaveAttribute(
      "src",
      `${APS_TEST_RENDERER_URL}#${nestedBootstrapNonce}`,
    );
    expect(await nestedInnerFrame.getAttribute("src")).toContain(
      `#${nestedRendererNonce}`,
    );
    for (const html of [
      await nestedOuter
        .locator("html")
        .evaluate((element) => element.outerHTML),
      await nestedInner
        .locator("html")
        .evaluate((element) => element.outerHTML),
    ]) {
      expect(html).not.toContain("nested-case-bid");
      expect(html).not.toContain("example-account-id");
      expect(html).not.toContain("fictional-nested-case-creative");
    }
    expect(
      await page.locator("#nested-case").evaluate((slot) => ({
        iframeAncestor: slot.parentElement?.closest("iframe") !== null,
        gamAncestor:
          slot.parentElement?.closest(
            "[data-google-query-id],[data-google-container-id]",
          ) !== null,
        safeFrameAncestor:
          slot.parentElement?.closest('[id*="safeframe" i]') !== null,
      })),
    ).toEqual({
      iframeAncestor: false,
      gamAncestor: false,
      safeFrameAncestor: false,
    });
    expect(await bootstrapConfiguration("nested-case")).toEqual({
      message: "TS APS Bootstrap Configure",
      version: 2,
      bootstrapNonce: nestedBootstrapNonce,
      rendererNonce: nestedRendererNonce,
      creativeOrigin: "https://creative.example",
      tagType: "iframe",
    });

    await start("creative-script", "creative-script-bid", {
      creativeUrl: "https://creative.example/script-allowed.js",
      tagType: "script",
    });
    await expect
      .poll(async () => await messages("creative-script"))
      .toContainEqual(
        expect.objectContaining({ message: "TS APS Render Completed" }),
      );
    const scriptInner = page
      .frameLocator("#creative-script > iframe")
      .frameLocator('iframe[title="Ad content"]');
    await expect(
      page
        .frameLocator("#creative-script > iframe")
        .locator('meta[http-equiv="Content-Security-Policy"]'),
    ).toHaveAttribute(
      "content",
      containerCsp("https://creative.example", true, true),
    );
    await expect(
      scriptInner.locator('meta[http-equiv="Content-Security-Policy"]'),
    ).toHaveAttribute(
      "content",
      containerCsp("https://creative.example", false, true),
    );
    expect(
      await scriptInner.locator("html").evaluate(() => ({
        allowed: document.documentElement.dataset.scriptCreative,
        unrelated: document.documentElement.dataset.otherScript,
      })),
    ).toEqual({ allowed: "executed", unrelated: undefined });

    await start("creative-redirect", "creative-redirect-bid", {
      creativeUrl: "https://creative.example/script-redirect.js",
      tagType: "script",
    });
    await expect
      .poll(async () => await messages("creative-redirect"))
      .toContainEqual(
        expect.objectContaining({
          message: "TS APS Render Failed",
          reason: "runner_failed",
        }),
      );
    const redirectInner = page
      .frameLocator("#creative-redirect > iframe")
      .frameLocator('iframe[title="Ad content"]');
    expect(
      await redirectInner
        .locator("html")
        .evaluate(() => document.documentElement.dataset.redirectedScript),
    ).toBeUndefined();

    for (const { slotId, bidId, width, height } of [
      {
        slotId: "creative-boundary-min",
        bidId: "creative-min",
        width: 1,
        height: 1,
      },
      {
        slotId: "creative-boundary-max",
        bidId: "creative-max",
        width: 4096,
        height: 4096,
      },
    ]) {
      await start(slotId, bidId, {
        creativeUrl: "https://creative.example/iframe-policy",
        tagType: "iframe",
        width,
        height,
      });
      await expect
        .poll(async () => await messages(slotId))
        .toContainEqual(
          expect.objectContaining({ message: "TS APS Render Completed" }),
        );
      const outerFrame = page.locator(`#${slotId} > iframe`);
      const outer = page.frameLocator(`#${slotId} > iframe`);
      const innerFrame = outer.locator('iframe[title="Ad content"]');
      const inner = outer.frameLocator('iframe[title="Ad content"]');
      const creativeFrame = inner.locator(
        'iframe[data-fictional-creative="iframe"]',
      );
      const creative = inner.frameLocator(
        'iframe[data-fictional-creative="iframe"]',
      );
      for (const frame of [outerFrame, creativeFrame]) {
        await expect(frame).toHaveAttribute("width", String(width));
        await expect(frame).toHaveAttribute("height", String(height));
      }
      for (const frame of [outerFrame, innerFrame, creativeFrame]) {
        expect(
          await frame.evaluate((element) => {
            const value = element as HTMLElement;
            const box = value.getBoundingClientRect();
            const computed = getComputedStyle(value);
            return {
              width: box.width,
              height: box.height,
              clientWidth: value.clientWidth,
              clientHeight: value.clientHeight,
              margin: computed.margin,
              overflow: computed.overflow,
            };
          }),
        ).toEqual({
          width,
          height,
          clientWidth: width,
          clientHeight: height,
          margin: "0px",
          overflow: "hidden",
        });
      }
      for (const frame of [outer, inner, creative]) {
        expect(
          await frame.locator("body").evaluate((body) => {
            const computed = getComputedStyle(body);
            return {
              clientWidth: body.clientWidth,
              clientHeight: body.clientHeight,
              scrollWidth: body.scrollWidth,
              scrollHeight: body.scrollHeight,
              margin: computed.margin,
              overflow: computed.overflow,
            };
          }),
        ).toEqual({
          clientWidth: width,
          clientHeight: height,
          scrollWidth: width,
          scrollHeight: height,
          margin: "0px",
          overflow: "hidden",
        });
      }
      expect(
        await creative
          .locator("html")
          .evaluate(() => document.documentElement.dataset.externalScript),
      ).toBeUndefined();
    }
    const requestsBeforeInvalid = runnerRequests;
    await start("invalid-case", "invalid-case-bid", {
      unexpected: true,
    });
    await expect
      .poll(async () => await messages("invalid-case"))
      .toContainEqual(
        expect.objectContaining({
          message: "TS APS Render Failed",
          reason: "descriptor_invalid",
        }),
      );
    expect(runnerRequests).toBe(requestsBeforeInvalid);

    await page.unroute(APS_TEST_RUNNER_URL);
    await page.route(APS_TEST_RUNNER_URL, (route) => route.abort());
    await start("load-failure-case", "load-failure-case-bid");
    await expect
      .poll(async () => await messages("load-failure-case"))
      .toContainEqual(
        expect.objectContaining({
          message: "TS APS Render Failed",
          reason: "runner_no_load",
        }),
      );
  });
});

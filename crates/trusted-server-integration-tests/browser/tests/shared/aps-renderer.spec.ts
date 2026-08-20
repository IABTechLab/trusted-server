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

function descriptor() {
  const tagType = "iframe";
  const creativeUrl = IFRAME_CREATIVE_URL;
  const envelope = {
    seatbid: [
      {
        bid: [
          {
            id: `fictional-${tagType}-bid`,
            price: 1.23,
            w: 300,
            h: 250,
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
    bidId: `fictional-${tagType}-bid`,
    creativeId: `fictional-${tagType}-creative`,
    tagType,
    creativeUrl,
    aaxResponse: Buffer.from(JSON.stringify(envelope), "utf8").toString(
      "base64",
    ),
    width: 300,
    height: 250,
  };
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
  slot.innerHTML = '<span class="existing">existing publisher content</span>';
  document.getElementById('slots').appendChild(slot);
  var frame = document.createElement('iframe');
  frame.setAttribute('sandbox', ${JSON.stringify(SANDBOX)});
  frame.src = ${JSON.stringify(APS_TEST_RENDERER_URL)} + '#' + options.bootstrapNonce;
  frame.style.display = 'none';
  var record = {messages: [], frame: frame, port: null};
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
      frame.contentWindow.postMessage(JSON.stringify({
        message: 'TS APS Bootstrap Configure',
        version: 2,
        bootstrapNonce: options.bootstrapNonce,
        rendererNonce: options.rendererNonce,
        creativeOrigin: creativeOrigin,
        tagType: options.renderer.tagType
      }), '*', []);
      return;
    }
    if (value.message !== 'TS APS Container Ready' || value.version !== 1 ||
        value.bootstrapNonce !== options.bootstrapNonce ||
        value.rendererNonce !== options.rendererNonce || event.ports.length !== 1) return;
    window.removeEventListener('message', receive, true);
    record.port = event.ports[0];
    record.port.onmessage = function(portEvent) {
      record.messages.push(portEvent.data);
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

    const makeDescriptor = (bidId: string) => {
      const value = descriptor();
      value.bidId = bidId;
      const envelope = JSON.parse(
        Buffer.from(value.aaxResponse, "base64").toString("utf8"),
      ) as { seatbid: Array<{ bid: Array<{ id: string }> }> };
      envelope.seatbid[0].bid[0].id = bidId;
      value.aaxResponse = Buffer.from(
        JSON.stringify(envelope),
        "utf8",
      ).toString("base64");
      return value;
    };
    const start = async (
      slotId: string,
      bidId: string,
      rendererOverrides: Record<string, unknown> = {},
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
          renderer: {
            ...makeDescriptor(bidId),
            ...rendererOverrides,
          },
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

    await start("nested-case", "nested-case-bid");
    await expect
      .poll(async () => await messages("nested-case"))
      .toContainEqual(
        expect.objectContaining({
          message: "TS APS Render Completed",
        }),
      );

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

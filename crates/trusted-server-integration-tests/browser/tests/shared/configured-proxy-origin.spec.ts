import { test, expect } from "@playwright/test";
import { createServer, type Server } from "node:http";
import { resolve } from "node:path";
import { startViceroy, stopViceroy } from "../../helpers/infra.js";

let runtime: Awaited<ReturnType<typeof startViceroy>>;
let assets: Server;
let assetOrigin: string;

// Use real loopback origins, with normal browser security and actual proxy token
// verification. The publisher bundle stays on 127.0.0.1 while configured proxy
// URLs use localhost on the same listener. No fulfilled browser routes or CORS
// exceptions stand in for a reachable server.
test.beforeAll(async () => {
  assets = createServer((request, response) => {
    if (request.url?.startsWith("/pixel.gif")) {
      response.writeHead(200, { "content-type": "image/gif" });
      response.end(
        Buffer.from(
          "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7",
          "base64",
        ),
      );
    } else {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(
        "<!doctype html><html><head><title>Proxy frame</title></head><body>signed frame</body></html>",
      );
    }
  });
  await new Promise<void>((resolve, reject) => {
    assets.once("error", reject);
    assets.listen(0, "127.0.0.1", resolve);
  });
  const address = assets.address();
  if (!address || typeof address === "string")
    throw new Error("Missing asset listener");
  assetOrigin = `http://127.0.0.1:${address.port}`;
  runtime = await startViceroy(
    process.env.WASM_BINARY_PATH ||
      resolve(
        __dirname,
        "../../../../../target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm",
      ),
    process.env.VICEROY_CONFIG_PATH ||
      resolve(
        __dirname,
        "../../../../../target/integration-test-artifacts/configs/viceroy.toml",
      ),
    { publicOriginHostname: "localhost" },
  );
});

test.afterAll(async () => {
  if (runtime?.process.pid) await stopViceroy(runtime.process.pid);
  if (assets)
    await new Promise<void>((resolve, reject) =>
      assets.close((error) => (error ? reject(error) : resolve())),
    );
});

test("does not double-proxy images or iframes on a distinct configured origin", async ({
  page,
}, testInfo) => {
  const publicOrigin = new URL(runtime.baseUrl);
  publicOrigin.hostname = "localhost";
  const signed: Record<string, string> = {};
  for (const phase of ["initial", "mutation"]) {
    for (const [kind, path] of [
      ["image", "/pixel.gif"],
      ["iframe", "/frame.html"],
    ]) {
      const target = `${assetOrigin}${path}?phase=${phase}`;
      const response = await page.request.post(
        `${runtime.baseUrl}/first-party/sign`,
        { data: { url: target } },
      );
      expect(response.ok()).toBe(true);
      const href = ((await response.json()) as { href: string }).href;
      const url = new URL(href);
      expect(url.origin).toBe(publicOrigin.origin);
      expect(url.pathname).toBe("/first-party/proxy");
      const targetBase = new URL(target);
      targetBase.search = "";
      expect(url.searchParams.get("tsurl")).toBe(targetBase.href);
      expect(url.searchParams.get("phase")).toBe(phase);
      expect(url.searchParams.get("tstoken")).toBeTruthy();
      signed[`${phase}-${kind}`] = href;
    }
  }
  // A negative control proves the real proxy validates tokens, rather than a
  // fixture accepting arbitrary requests at a matching pathname.
  const invalid = new URL(signed["initial-image"]);
  invalid.searchParams.set("tstoken", "invalid-token");
  const invalidTokenStatus = (await page.request.get(invalid.href)).status();
  expect(invalidTokenStatus).toBe(403);

  const requests: string[] = [];
  const responses: { url: string; status: number; resourceType: string }[] = [];
  page.on("request", (request) => requests.push(request.url()));
  page.on("response", (response) =>
    responses.push({
      url: response.url(),
      status: response.status(),
      resourceType: response.request().resourceType(),
    }),
  );
  await page.goto(runtime.baseUrl, { waitUntil: "load" });
  const metadata = await page
    .locator('script[src*="/static/tsjs="]')
    .first()
    .evaluate((script) => ({
      src: script.getAttribute("src"),
      origin: new URL((script as HTMLScriptElement).src).origin,
      configured: script.getAttribute("data-ts-public-origin"),
    }));
  expect(metadata.src).toMatch(/^\/static\/tsjs=/);
  expect(metadata.origin).toBe(runtime.baseUrl);
  expect(metadata.configured).toBe(publicOrigin.origin);
  await page.evaluate(() => {
    // Keep the server-injected runtime, but remove framework fixture resources
    // before enabling the guard so only this test's images and frames are signed.
    document.body.replaceChildren();
    (
      window as unknown as {
        tscreative: { setConfig(config: { renderGuard: boolean }): void };
      }
    ).tscreative.setConfig({ renderGuard: true });
  });

  // Prove renderGuard is active before testing already-proxied resources. A
  // disabled guard would otherwise pass the zero-sign assertions vacuously.
  const rawControl = `${assetOrigin}/pixel.gif?phase=control`;
  const controlSignResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/first-party/sign" &&
      response.request().method() === "POST",
  );
  await page.evaluate((src) => {
    const image = new Image();
    image.id = "active-guard-control";
    image.src = src;
    document.body.appendChild(image);
  }, rawControl);
  const signResponse = await controlSignResponse;
  expect(signResponse.ok()).toBe(true);
  expect(new URL(signResponse.url()).origin).toBe(runtime.baseUrl);
  expect(signResponse.request().postDataJSON()).toEqual({ url: rawControl });
  const controlHref = ((await signResponse.json()) as { href: string }).href;
  expect(new URL(controlHref).origin).toBe(publicOrigin.origin);
  expect(new URL(controlHref).pathname).toBe("/first-party/proxy");
  await expect
    .poll(() =>
      responses.some(
        (response) =>
          response.url === controlHref &&
          response.status === 200 &&
          response.resourceType === "image",
      ),
    )
    .toBe(true);
  await expect(page.locator("#active-guard-control")).toHaveAttribute(
    "src",
    controlHref,
  );
  await expect
    .poll(() =>
      page
        .locator("#active-guard-control")
        .evaluate((image) => (image as HTMLImageElement).naturalWidth),
    )
    .toBe(1);
  expect(
    requests.filter((url) => new URL(url).pathname === "/first-party/sign"),
  ).toEqual([`${runtime.baseUrl}/first-party/sign`]);
  const control = {
    raw: rawControl,
    signed: controlHref,
    requests: [...requests],
    responses: [...responses],
  };
  await page
    .locator("#active-guard-control")
    .evaluate((image) => image.remove());
  requests.length = 0;
  responses.length = 0;

  for (const phase of ["initial", "mutation"]) {
    const expected = {
      image: signed[`${phase}-image`],
      iframe: signed[`${phase}-iframe`],
    };
    await page.evaluate(
      ({ phase, expected }) => {
        if (phase === "initial") {
          const image = new Image();
          image.id = "configured-proxy-image";
          image.src = expected.image;
          document.body.appendChild(image);
          // innerHTML exercises MutationObserver processing, not just src setters.
          const container = document.createElement("div");
          container.innerHTML = `<iframe id="configured-proxy-iframe" src="${expected.iframe}"></iframe>`;
          document.body.appendChild(container);
        } else {
          document
            .querySelector("#configured-proxy-image")!
            .setAttribute("src", expected.image);
          document
            .querySelector("#configured-proxy-iframe")!
            .setAttribute("src", expected.iframe);
        }
      },
      { phase, expected },
    );
    for (const [kind, href] of Object.entries(expected)) {
      await expect
        .poll(() =>
          responses.some(
            (response) =>
              response.url === href &&
              response.status === 200 &&
              response.resourceType ===
                (kind === "image" ? "image" : "document"),
          ),
        )
        .toBe(true);
      await expect(page.locator(`#configured-proxy-${kind}`)).toHaveAttribute(
        "src",
        href,
      );
    }
    await expect
      .poll(() =>
        page
          .locator("#configured-proxy-image")
          .evaluate((image) => (image as HTMLImageElement).naturalWidth),
      )
      .toBe(1);
    const frameScript = page
      .frameLocator("#configured-proxy-iframe")
      .locator('script[src*="/static/tsjs="]')
      .first();
    await expect(frameScript).toHaveAttribute(
      "data-ts-public-origin",
      publicOrigin.origin,
    );
    expect(new URL((await frameScript.getAttribute("src"))!).origin).toBe(
      publicOrigin.origin,
    );
  }
  expect(
    requests.filter((url) => new URL(url).pathname === "/first-party/sign"),
  ).toEqual([]);
  const proxyRequests = requests.filter(
    (url) => new URL(url).pathname === "/first-party/proxy",
  );
  expect(new Set(proxyRequests)).toEqual(new Set(Object.values(signed)));
  for (const href of proxyRequests) {
    const target = new URL(new URL(href).searchParams.get("tsurl")!);
    expect(target.origin).toBe(assetOrigin);
    expect(target.pathname).not.toBe("/first-party/proxy");
  }
  await testInfo.attach("configured-proxy-network", {
    body: JSON.stringify(
      { metadata, invalidTokenStatus, control, signed, requests, responses },
      null,
      2,
    ),
    contentType: "application/json",
  });
});

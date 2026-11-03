import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  expect,
  test,
  type Page,
  type ElementHandle,
  type Frame,
  type Route,
  type TestInfo,
} from "@playwright/test";
import type { Controller } from "../../../fixtures/frameworks/nextjs/app/publisher-registration-real-gpt/RealGptPublisher";

const {
  loadInventory,
  sha256,
  assertEvidence,
} = require("../../publisher-registration/real-gpt.cjs");
const { rendererAsset } = require("../../cwv/html.cjs");
const baselineMode =
  process.env.PUBLISHER_REGISTRATION_REAL_GPT_MODE === "legacy";
const inventory = loadInventory(
  process.env.PUBLISHER_REGISTRATION_REAL_GPT_INVENTORY_FILE,
);
const paths = [inventory.registered.path, inventory.unrelated.path];
type Browser = Window & {
  realGptPublisher: Controller;
  realGptObservation: { events: Array<Record<string, unknown>> };
  tsjs?: {
    tsRenderBridgeVersion?: number;
    navGeneration?: number;
    publisherSlotRegistrationVersion?: number;
  };
  googletag: { apiReady?: boolean; pubads(): { __tsRefreshWrapped?: boolean } };
  pbjs?: { registerBidAdapter?: unknown };
};
type Network = {
  index: number;
  url: string;
  method: string;
  postData: string | null;
  resourceType: string;
  failure?: string;
};
type Session = {
  network: Network[];
  responses: Array<Record<string, unknown>>;
  errors: string[];
  pending: Promise<void>[];
  captureErrors: string[];
  interventions: Array<Record<string, unknown>>;
  runtimeRoutes: Route[];
  hydrationRoutes: Route[];
  releaseHydration(): Promise<void>;
  releaseRuntime(): Promise<void>;
};
const sessions = new WeakMap<Page, Session>();
const baseUrl = () => {
  expect(
    process.env.PUBLISHER_REGISTRATION_REAL_GPT_BASE_URL,
    "Use the real GPT launcher, not the ordinary browser suite",
  ).toBeTruthy();
  return process.env.PUBLISHER_REGISTRATION_REAL_GPT_BASE_URL!;
};
const publisher = (page: Page) =>
  page.evaluate(() => (window as unknown as Browser).realGptObservation.events);

function consent() {
  const bytes = Buffer.alloc(32);
  const bits = (offset: number, width: number, value: number) => {
    const binary = value.toString(2).padStart(width, "0");
    for (let index = 0; index < width; index++)
      if (binary[index] === "1")
        bytes[(offset + index) >> 3] |= 1 << (7 - ((offset + index) % 8));
  };
  const now = Math.floor(Date.now() / 100);
  bits(0, 6, 2);
  bits(6, 36, now);
  bits(42, 36, now);
  bits(152, 24, 1 << 23);
  return bytes.toString("base64url");
}

async function install(
  page: Page,
  info: TestInfo,
  runtime: "normal" | "failed" | "late" = "normal",
) {
  const session: Session = {
    network: [],
    responses: [],
    errors: [],
    pending: [],
    captureErrors: [],
    interventions: [],
    runtimeRoutes: [],
    hydrationRoutes: [],
    async releaseHydration() {
      holdHydration = false;
      for (const route of session.hydrationRoutes.splice(0))
        await route.continue();
    },
    async releaseRuntime() {
      holdRuntime = false;
      for (const route of session.runtimeRoutes.splice(0))
        await route.continue();
    },
  };
  sessions.set(page, session);
  const reset = await page.request.delete(
    `${process.env.PUBLISHER_REGISTRATION_REAL_GPT_ORIGIN}/api/fixture-auction`,
  );
  expect(
    reset.ok(),
    "actual local provider ledger must reset before each case",
  ).toBe(true);
  let holdRuntime = runtime === "late";
  let holdHydration = baselineMode;
  await page.addInitScript(() => {
    if (window !== window.top) return;
    const browser = window as unknown as {
      realGptObservation: Browser["realGptObservation"];
      googletag?: {
        cmd: { push(callback: () => void): unknown };
        pubads?(): {
          addEventListener(
            type: string,
            callback: (event: {
              slot: {
                getSlotElementId(): string;
                getAdUnitPath(): string;
                getTargetingKeys(): string[];
                getTargeting(key: string): string[];
              };
              creativeId?: number;
              sourceAgnosticCreativeId?: number;
              isEmpty?: boolean;
              size?: number[];
            }) => void,
          ): void;
        };
      };
    };
    browser.realGptObservation = { events: [] };
    browser.googletag = browser.googletag || { cmd: [] };
    browser.googletag.cmd.push(() => {
      for (const type of [
        "slotRequested",
        "slotResponseReceived",
        "slotRenderEnded",
        "slotOnload",
      ])
        browser.googletag!.pubads!().addEventListener(type, (event) => {
          const slot = event.slot;
          browser.realGptObservation.events.push({
            type,
            time: performance.now(),
            id: slot.getSlotElementId(),
            path: slot.getAdUnitPath(),
            targeting: Object.fromEntries(
              slot
                .getTargetingKeys()
                .map((key) => [key, slot.getTargeting(key)]),
            ),
            creativeId: event.creativeId ?? event.sourceAgnosticCreativeId,
            isEmpty: event.isEmpty,
            size: event.size,
          });
        });
    });
  });
  const indexes = new WeakMap<object, number>();
  page.on("request", (request) => {
    const index = session.network.length;
    indexes.set(request, index);
    session.network.push({
      index,
      url: request.url(),
      method: request.method(),
      postData: request.postData(),
      resourceType: request.resourceType(),
    });
  });
  page.on("requestfailed", (request) => {
    const index = indexes.get(request);
    if (index !== undefined)
      session.network[index].failure = request.failure()?.errorText;
  });
  page.on("pageerror", (error) => session.errors.push(error.message));
  page.on("response", (response) => {
    const url = new URL(response.url());
    if (
      response.request().resourceType() !== "script" &&
      response.request().resourceType() !== "document" &&
      !url.pathname.endsWith("/gampad/ads") &&
      url.pathname !== "/_ts/page-bids"
    )
      return;
    const index = indexes.get(response.request());
    session.pending.push(
      (async () => {
        const bytes = await response.body();
        const filename = `network-${index}-${sha256(bytes)}.bin`;
        writeFileSync(info.outputPath(filename), bytes);
        session.responses.push({
          index,
          url: response.url(),
          status: response.status(),
          headers: response.headers(),
          sha256: sha256(bytes),
          artifact: filename,
        });
      })().catch((error) => {
        session.captureErrors.push(`${response.url()}: ${String(error)}`);
      }),
    );
  });
  await page
    .context()
    .addCookies([{ name: "euconsent-v2", value: consent(), url: baseUrl() }]);
  const prebidDir = process.env.PUBLISHER_REGISTRATION_PREBID_DIR;
  expect(prebidDir, "--prebid verified actual bundle is required").toBeTruthy();
  const manifest = JSON.parse(
    readFileSync(resolve(prebidDir!, "manifest.json"), "utf8"),
  );
  const bytes = readFileSync(resolve(prebidDir!, manifest.filename));
  expect(sha256(bytes)).toBe(manifest.sha256);
  await page.route("**/*", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin !== new URL(baseUrl()).origin) return route.continue();
    if (
      holdHydration &&
      url.pathname.startsWith("/_next/") &&
      request.resourceType() === "script"
    ) {
      session.hydrationRoutes.push(route);
      return;
    }
    if (url.pathname === "/integrations/prebid/bundle.js") {
      expect(url.searchParams.get("v")).toBe(manifest.sha256);
      return route.fulfill({
        contentType: "application/javascript",
        body: bytes,
      });
    }
    if ((runtime === "failed" || holdRuntime) && rendererAsset(url.pathname)) {
      session.interventions.push({ type: runtime, url: url.href });
      if (holdRuntime) {
        session.runtimeRoutes.push(route);
        return;
      }
      return route.abort("failed");
    }
    return route.continue();
  });
  return session;
}

async function ready(page: Page, runtime = true, variant = "") {
  await page.goto(`${baseUrl()}/publisher-registration-real-gpt${variant}`, {
    waitUntil: "commit",
  });
  await expect
    .poll(() =>
      page.evaluate(() => {
        const browser = window as unknown as Browser;
        return (
          !!browser.realGptPublisher?.ready &&
          browser.googletag.apiReady === true &&
          browser.tsjs?.publisherSlotRegistrationVersion === 1
        );
      }),
    )
    .toBe(true);
  if (runtime)
    await expect
      .poll(() =>
        page.evaluate(() => {
          const browser = window as unknown as Browser;
          return (
            browser.tsjs?.tsRenderBridgeVersion === 1 &&
            typeof browser.pbjs?.registerBidAdapter === "function" &&
            browser.googletag.pubads().__tsRefreshWrapped === true
          );
        }),
      )
      .toBe(true);
  expect(
    (await publisher(page)).filter((event) => event.type === "slotRequested"),
  ).toHaveLength(0);
  expect(
    sessions
      .get(page)!
      .network.filter((entry) =>
        new URL(entry.url).pathname.endsWith("/gampad/ads"),
      ),
  ).toHaveLength(0);
  expect(await page.locator("#real-gpt-placeholder").count()).toBe(1);
}

async function mount(page: Page, eligible = true, id = "real-gpt-final") {
  await page.evaluate(
    ({ id, eligible }) =>
      (window as unknown as Browser).realGptPublisher.mount(id, eligible),
    { id, eligible },
  );
  await expect(page.locator(`#${id}`)).toHaveCount(1);
}

async function visiblyPainted(element: ElementHandle) {
  return element.evaluate((node) => {
    if (!(node instanceof HTMLElement) || !node.isConnected || document.hidden)
      return false;
    const rect = node.getBoundingClientRect();
    let left = Math.max(0, rect.left);
    let top = Math.max(0, rect.top);
    let right = Math.min(innerWidth, rect.right);
    let bottom = Math.min(innerHeight, rect.bottom);
    for (
      let ancestor: HTMLElement | null = node;
      ancestor;
      ancestor = ancestor.parentElement
    ) {
      const css = getComputedStyle(ancestor);
      if (
        css.display === "none" ||
        css.visibility === "hidden" ||
        css.visibility === "collapse" ||
        Number(css.opacity) === 0 ||
        css.contentVisibility === "hidden"
      )
        return false;
      const clip = ancestor.getBoundingClientRect();
      if (css.overflowX !== "visible") {
        left = Math.max(left, clip.left);
        right = Math.min(right, clip.right);
      }
      if (css.overflowY !== "visible") {
        top = Math.max(top, clip.top);
        bottom = Math.min(bottom, clip.bottom);
      }
    }
    return right > left && bottom > top;
  });
}

async function marker(page: Page, text: string, id: string) {
  for (const element of await page.locator(`#${id} iframe`).elementHandles()) {
    const root = await element.contentFrame();
    if (!root) continue;
    const frames = [root];
    for (let index = 0; index < frames.length; index++) {
      const frame = frames[index];
      frames.push(...frame.childFrames());
      try {
        let ancestorsVisible = true;
        for (
          let ancestor: Frame | null = frame;
          ancestor && ancestor !== page.mainFrame();
          ancestor = ancestor.parentFrame()
        ) {
          if (!(await visiblyPainted(await ancestor.frameElement()))) {
            ancestorsVisible = false;
            break;
          }
        }
        if (!ancestorsVisible) continue;
        for (const textElement of await frame
          .getByText(text, { exact: false })
          .elementHandles()) {
          if (await visiblyPainted(textElement)) return true;
        }
      } catch {
        /* Detached, hidden, collapsed and offscreen frames are not evidence. */
      }
    }
  }
  return false;
}

async function nativeSlots(page: Page) {
  return page.evaluate(() => {
    const tag = (
      window as unknown as {
        googletag: {
          pubads(): {
            getSlots(): Array<{
              getSlotElementId(): string;
              getAdUnitPath(): string;
            }>;
          };
        };
      }
    ).googletag;
    return tag
      .pubads()
      .getSlots()
      .map((slot) => ({
        id: slot.getSlotElementId(),
        path: slot.getAdUnitPath(),
        connected: !!document.getElementById(slot.getSlotElementId())
          ?.isConnected,
      }));
  });
}

async function delivered(
  page: Page,
  candidate: boolean,
  id = "real-gpt-final",
  count = 1,
) {
  const expectedMarker = candidate
    ? inventory.registered.tsMarker
    : inventory.registered.ordinaryMarker;
  await expect
    .poll(
      async () =>
        (await publisher(page)).filter(
          (event) => event.type === "slotRenderEnded" && event.id === id,
        ).length,
    )
    .toBe(count);
  await expect
    .poll(() => marker(page, expectedMarker, id), {
      message: `Creative marker ${expectedMarker} must render inside an actual GPT frame`,
    })
    .toBe(true);
  return {
    path:
      id === "real-gpt-unrelated"
        ? inventory.unrelated.path
        : inventory.registered.path,
    id,
    candidate,
    creativeId: candidate
      ? inventory.registered.tsCreativeId
      : inventory.registered.ordinaryCreativeId,
    markerObserved: true,
  };
}

async function verify(page: Page, expected: Array<Record<string, unknown>>) {
  const session = sessions.get(page)!;
  const events = await publisher(page);
  const requests = session.network.filter((entry) =>
    new URL(entry.url).pathname.endsWith("/gampad/ads"),
  );
  assertEvidence({ requests, events, expected, paths });
  await verifyProxy(page);
}

async function verifyProxy(page: Page) {
  const session = sessions.get(page)!;
  await Promise.all(session.pending);
  const scripts = session.responses.filter((entry) =>
    new URL(String(entry.url)).pathname.startsWith("/integrations/gpt/"),
  );
  expect(
    scripts.some(
      (entry) =>
        new URL(String(entry.url)).pathname === "/integrations/gpt/script",
    ),
    "actual Rust GPT bootstrap proxy response",
  ).toBe(true);
  expect(
    scripts.some((entry) => String(entry.url).includes("pubads_impl")),
    "actual GPT dynamic script guard cascade",
  ).toBe(true);
  for (const script of scripts) {
    expect(script.status).toBe(200);
    expect((script.headers as Record<string, string>)["x-gpt-proxy"]).toBe(
      "true",
    );
  }
  const bypassed = session.network.filter(
    (entry) =>
      entry.resourceType === "script" &&
      new URL(entry.url).origin !== new URL(baseUrl()).origin &&
      /(?:\/tag\/js\/gpt\.js|\/pubads_impl)/.test(new URL(entry.url).pathname),
  );
  expect(
    bypassed,
    "GPT must execute through production proxy/interception, not direct external scripts",
  ).toEqual([]);
  expect(session.captureErrors).toEqual([]);
  const unexpectedErrors = baselineMode
    ? session.errors.filter(
        (message) =>
          !/Hydration failed|hydrating|Text content does not match/i.test(
            message,
          ),
      )
    : session.errors;
  expect(unexpectedErrors).toEqual([]);
}

test.afterEach(async ({ page }, info) => {
  const session = sessions.get(page);
  if (!session) return;
  try {
    // Always retain failure evidence, even if a renderer or inventory assertion failed.
    const events = await publisher(page).catch(() => []);
    const providerResponse = await page.request.get(
      `${process.env.PUBLISHER_REGISTRATION_REAL_GPT_ORIGIN}/api/fixture-auction`,
    );
    const providerText = await providerResponse.text();
    writeFileSync(info.outputPath("provider-raw.json"), providerText);
    const provider = JSON.parse(providerText);
    await Promise.all(session.pending);
    const evidence = {
      status: info.status,
      expectedStatus: info.expectedStatus,
      inventory,
      network: session.network,
      responses: session.responses,
      events,
      provider,
      errors: session.errors,
      captureErrors: session.captureErrors,
      interventions: session.interventions,
    };
    const file = info.outputPath("real-gpt-evidence.json");
    writeFileSync(file, JSON.stringify(evidence, null, 2));
    writeFileSync(
      info.outputPath("real-gpt-evidence.sha256"),
      `${sha256(readFileSync(file))}  real-gpt-evidence.json\n`,
    );
    await info.attach("real GPT network and public lifecycle", {
      path: file,
      contentType: "application/json",
    });
    expect(providerResponse.ok()).toBe(true);
    expect(
      provider.exchanges.length,
      "each scenario must actually reach local provider through Rust, not silently fall back on transport failure",
    ).toBeGreaterThan(0);
    for (const exchange of provider.exchanges) {
      expect(exchange.path).toBe("/api/fixture-auction");
      for (const impression of exchange.request.imp) {
        expect([
          "fixture-header",
          "real-gpt-final",
          "real-gpt-unrelated",
          "real-gpt-final-replacement",
          "real-gpt-navigation-final",
          "real-gpt-ssr",
          "real-gpt-baseline-final",
        ]).toContain(impression.id);
        expect(impression.ext.prebid.bidder["fixture-bidder"]).toBeTruthy();
      }
      const noBid = exchange.mode.noBid;
      expect(
        exchange.mode.forcedNoBid ||
          new URL(exchange.request.site.page).pathname.endsWith("/no-bid"),
      ).toBe(noBid);
      if (noBid) expect(exchange.response.seatbid).toEqual([]);
      else
        expect(exchange.response.seatbid[0].bid[0].adm).toContain(
          "Fixture publisher registration winner",
        );
    }
  } finally {
    await session.releaseHydration();
    await session.releaseRuntime();
  }
});

if (!baselineMode) {
  test("eligible first request uses TS creative and leaves unrelated inventory untouched", async ({
    page,
  }, info) => {
    await install(page, info);
    await ready(page);
    await mount(page);
    const initial = await delivered(page, true);
    await verify(page, [initial]);
  });

  for (const scenario of ["no-bid", "ineligible"] as const)
    test(`${scenario} uses ordinary GAM fallback on the first request`, async ({
      page,
    }, info) => {
      await install(page, info);
      await ready(page, true, scenario === "no-bid" ? "/no-bid" : "");
      await mount(page, scenario !== "ineligible");
      await verify(page, [await delivered(page, false)]);
    });

  for (const runtime of ["failed", "late"] as const)
    test(`${runtime} deferred runtime preserves eligible early TS delivery without a repeat at takeover`, async ({
      page,
    }, info) => {
      const session = await install(page, info, runtime);
      await ready(page, false);
      await mount(page);
      expect(
        await page.evaluate(
          () => (window as unknown as Browser).tsjs?.tsRenderBridgeVersion,
        ),
        "actual early renderer must survive the deferred-runtime fault",
      ).toBe(1);
      const initial = await delivered(page, true);
      expect(
        session.interventions.some((entry) => entry.type === runtime),
      ).toBe(true);
      if (runtime === "late") {
        await session.releaseRuntime();
        await expect
          .poll(() =>
            page.evaluate(() => {
              const browser = window as unknown as Browser;
              return (
                typeof browser.pbjs?.registerBidAdapter === "function" &&
                browser.googletag.pubads().__tsRefreshWrapped === true
              );
            }),
          )
          .toBe(true);
        // The completed main-runtime installation is the observation boundary, not a sleep.
      }
      await verify(page, [initial]);
    });

  test("eligible TS-first candidate retires on real no-bid publisher refresh with SRA isolation", async ({
    page,
  }, info) => {
    const session = await install(page, info);
    await ready(page);
    await mount(page);
    const initial = await delivered(page, true);
    await verify(page, [initial]);
    const providerUrl = `${process.env.PUBLISHER_REGISTRATION_REAL_GPT_ORIGIN}/api/fixture-auction`;
    const beforeResponse = await page.request.get(providerUrl);
    const before = await beforeResponse.json();
    expect(before.exchanges.length).toBeGreaterThan(0);
    expect(
      before.exchanges.every(
        (exchange: { mode: { noBid: boolean } }) =>
          exchange.mode.noBid === false,
      ),
    ).toBe(true);
    const transition = await page.request.put(providerUrl, {
      data: { noBid: true },
    });
    expect(transition.ok()).toBe(true);
    session.interventions.push({
      type: "actualProviderNoBidTransition",
      priorExchanges: before.exchanges.length,
      response: await transition.json(),
    });
    await page.evaluate(() =>
      (window as unknown as Browser).realGptPublisher.refresh(true),
    );
    const subsequent = await delivered(page, false, "real-gpt-final", 2);
    const unrelated = await delivered(page, false, "real-gpt-unrelated");
    await verify(page, [initial, subsequent, unrelated]);
    const afterResponse = await page.request.get(providerUrl);
    const after = await afterResponse.json();
    const fresh = after.exchanges.slice(before.exchanges.length);
    expect(
      fresh.length,
      "refresh must produce new actual provider exchanges, not reuse the initial SSR ledger",
    ).toBeGreaterThan(0);
    const freshImpressions = new Set<string>();
    for (const exchange of fresh) {
      expect(exchange.mode).toMatchObject({
        noBid: true,
        forcedNoBid: true,
        source: "control",
      });
      expect(exchange.response.seatbid).toEqual([]);
      for (const impression of exchange.request.imp) {
        freshImpressions.add(impression.id);
        expect(impression.ext.prebid.bidder["fixture-bidder"]).toBeTruthy();
      }
    }
    expect([...freshImpressions].sort()).toEqual([
      "real-gpt-final",
      "real-gpt-unrelated",
    ]);
    writeFileSync(
      info.outputPath("refresh-provider-transition.json"),
      JSON.stringify({ before, fresh }, null, 2),
    );
    const {
      decodeAdRequest,
    } = require("../../publisher-registration/real-gpt.cjs");
    expect(
      session.network.some(
        (request) =>
          new URL(request.url).pathname.endsWith("/gampad/ads") &&
          decodeAdRequest(request, paths).length === 2,
      ),
      "must observe actual two-slot SRA network request",
    ).toBe(true);
    expect(
      session.network.some(
        (request) => new URL(request.url).pathname === "/auction",
      ),
      "later refresh must run the actual Prebid client auction",
    ).toBe(true);
  });

  test("replacing the SSR placeholder before registration uses only the final native mount", async ({
    page,
  }, info) => {
    await install(page, info);
    await ready(page);
    const placeholder = await page
      .locator("#real-gpt-placeholder")
      .elementHandle();
    await page.evaluate(() =>
      (window as unknown as Browser).realGptPublisher.replacePlaceholder(),
    );
    await expect(page.locator("#real-gpt-placeholder")).toHaveAttribute(
      "data-revision",
      "1",
    );
    expect(await placeholder!.evaluate((element) => element.isConnected)).toBe(
      false,
    );
    await mount(page, true, "real-gpt-final-replacement");
    await verify(page, [
      await delivered(page, true, "real-gpt-final-replacement"),
    ]);
  });

  test("publisher cancellation before dispatch destroys connected native slot without a request", async ({
    page,
  }, info) => {
    await install(page, info);
    await ready(page);
    // Hold only the dispatch scheduling primitive. Do not replace GPT or server bids.
    await page.evaluate(() => {
      const browser = window as unknown as Browser & {
        realGptFrames?: FrameRequestCallback[];
      };
      browser.realGptFrames = [];
      window.requestAnimationFrame = (callback) => {
        browser.realGptFrames!.push(callback);
        return browser.realGptFrames!.length;
      };
    });
    await mount(page);
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as Browser).realGptPublisher.handles.length,
        ),
      )
      .toBe(1);
    await page.evaluate(async () => {
      const browser = window as unknown as Browser & {
        realGptFrames: FrameRequestCallback[];
      };
      await browser.realGptPublisher.remove();
      for (const callback of browser.realGptFrames.splice(0))
        callback(performance.now());
    });
    await expect(page.locator("#real-gpt-final")).toHaveCount(0);
    await verify(page, []);
    const destroyed = (await publisher(page)).filter(
      (event) => event.type === "destroyBeforeRemove",
    );
    expect(destroyed).toHaveLength(1);
    expect(destroyed[0]).toMatchObject({
      id: "real-gpt-final",
      connected: true,
      destroyed: true,
      nativeStillRegistered: false,
    });
    expect(
      (await nativeSlots(page)).filter(
        (slot) => slot.path === inventory.registered.path,
      ),
    ).toEqual([]);
  });

  test("Next navigation retires old native placement and admits ordinary no-bid replacement", async ({
    page,
  }, info) => {
    await install(page, info);
    await ready(page);
    await mount(page);
    const initial = await delivered(page, true);
    await page.evaluate(() =>
      (window as unknown as Browser).realGptPublisher.navigate(),
    );
    await expect(page.getByTestId("real-gpt-route")).toHaveText(
      "/publisher-registration-real-gpt/no-bid",
    );
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as Browser).tsjs?.navGeneration ?? 0,
        ),
      )
      .toBeGreaterThan(0);
    // Registration waits for the actual successful current SPA page-bids response.
    await mount(page, true, "real-gpt-navigation-final");
    const next = await delivered(page, false, "real-gpt-navigation-final");
    await verify(page, [initial, next]);
    const destroyed = (await publisher(page)).filter(
      (event) => event.type === "destroyBeforeRemove",
    );
    expect(destroyed).toHaveLength(1);
    expect(destroyed[0]).toMatchObject({
      id: "real-gpt-final",
      connected: true,
      destroyed: true,
      nativeStillRegistered: false,
    });
    const live = await nativeSlots(page);
    expect(
      live.filter((slot) => slot.path === inventory.registered.path),
    ).toEqual([
      {
        id: "real-gpt-navigation-final",
        path: inventory.registered.path,
        connected: true,
      },
    ]);
    expect(
      live.filter((slot) => slot.path === inventory.unrelated.path),
    ).toEqual([
      {
        id: "real-gpt-unrelated",
        path: inventory.unrelated.path,
        connected: true,
      },
    ]);
  });
} else {
  test("original main real GPT reproduces duplicate native membership or lost final TS creative after subtree replacement", async ({
    page,
  }, info) => {
    const session = await install(page, info);
    await page.goto(`${baseUrl()}/publisher-registration-real-gpt`, {
      waitUntil: "commit",
    });
    await expect
      .poll(
        async () =>
          (await publisher(page)).filter(
            (event) =>
              event.type === "slotRenderEnded" && event.id === "real-gpt-ssr",
          ).length,
      )
      .toBe(1);
    await expect
      .poll(() => marker(page, inventory.registered.tsMarker, "real-gpt-ssr"))
      .toBe(true);
    const initialMarkerObserved = true;
    expect(
      session.hydrationRoutes.length,
      "original SSR creative must precede real Next hydration",
    ).toBeGreaterThan(0);
    await session.releaseHydration();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as Browser).realGptPublisher?.ready,
        ),
      )
      .toBe(true);
    await mount(page, true, "real-gpt-baseline-final");
    await expect
      .poll(
        async () =>
          (await publisher(page)).filter(
            (event) =>
              event.type === "slotRenderEnded" &&
              event.id === "real-gpt-baseline-final",
          ).length,
      )
      .toBe(1);
    const events = await publisher(page);
    const finalRender = events.find(
      (event) =>
        event.type === "slotRenderEnded" &&
        event.id === "real-gpt-baseline-final",
    )!;
    let finalMarkerObserved = false;
    if (!finalRender.isEmpty) {
      const text =
        finalRender.creativeId === inventory.registered.tsCreativeId
          ? inventory.registered.tsMarker
          : inventory.registered.ordinaryMarker;
      await expect
        .poll(() => marker(page, text, "real-gpt-baseline-final"))
        .toBe(true);
      finalMarkerObserved = true;
    }
    const liveSlots = await nativeSlots(page);
    const {
      assertBaselineEvidence,
    } = require("../../publisher-registration/real-gpt.cjs");
    const reproduction = assertBaselineEvidence({
      requests: session.network.filter((request) =>
        new URL(request.url).pathname.endsWith("/gampad/ads"),
      ),
      events,
      paths,
      liveSlots,
      registered: inventory.registered,
      initialMarkerObserved,
      finalMarkerObserved,
    });
    const reproductionFile = info.outputPath("original-main-reproduction.json");
    writeFileSync(reproductionFile, JSON.stringify(reproduction, null, 2));
    await info.attach("original main reproduction", {
      path: reproductionFile,
      contentType: "application/json",
    });
    await verifyProxy(page);
  });
}

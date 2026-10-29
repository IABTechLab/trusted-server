import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  expect,
  test,
  type Page,
  type Route,
  type TestInfo,
} from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";
import type { PublisherController } from "../../../fixtures/frameworks/nextjs/app/publisher-registration/RegisteredSlot";

const { creative, pucPage } = require("../../publisher-registration/pages.cjs");
const {
  scriptBlocks,
  withoutEarlyRenderer,
  rendererAsset,
} = require("../../cwv/html.cjs");
const enabled = process.env.PUBLISHER_REGISTRATION_RUN === "1";
test.skip(
  !enabled,
  "Publisher registration requires its dedicated app config and launcher",
);

interface RequestSnapshot {
  index: number;
  native: number;
  element: number;
  id: string;
  source: string;
  targeting: Record<string, string[]>;
}
interface NativeSlot {
  getSlotElementId(): string;
  getTargeting(key: string): string[];
}
interface FixtureWindow extends Window {
  publisherRegistrationFixture: PublisherController;
  tsjs: {
    publisherSlotRegistrationVersion?: number;
    tsRenderBridgeVersion?: number;
    navGeneration?: number;
    adSlots?: Array<{ id: string; initialization?: string }>;
    bids?: Record<string, { hb_adid?: string; debug_bid?: unknown }>;
    divToSlotId?: Record<string, string>;
  };
  googletag: { pubads(): { __tsRefreshWrapped?: boolean } };
  pbjs: {
    onEvent(event: string, callback: () => void): void;
    registerBidAdapter?: unknown;
  };
  fixture: {
    requests: RequestSnapshot[];
    trace: Array<Record<string, unknown>>;
    nativeSlots: NativeSlot[];
    elements: Element[];
    render(index: number, options?: { noFill?: boolean }): void;
    record(event: string, details?: Record<string, unknown>): void;
  };
  frameworkFrames: {
    hold: boolean;
    pending(): number;
    flush(reverse?: boolean): void;
  };
  frameworkVisibility: { hidden: boolean; set(hidden: boolean): void };
  __tsjsPrebidShimInstalled?: boolean;
}
interface Session {
  html: string[];
  payloads: unknown[];
  errors: string[];
  consoleErrors: string[];
  external: string[];
  auctions: string[];
  mainBundle?: string;
  blockedRuntime: string[];
  rendererInterventions: Array<{
    originalSha256: string;
    servedSha256: string;
    removedEarly: boolean;
  }>;
  pending: Promise<void>[];
  held: Route[];
  expectedHydrationFailure?: boolean;
  releaseHydration?: () => Promise<void>;
}
const sessions = new WeakMap<Page, Session>();
const sha256 = (bytes: Buffer | string) =>
  createHash("sha256").update(bytes).digest("hex");

function currentConsent(purposeOne = true): string {
  const bytes = Buffer.alloc(32);
  const bits = (offset: number, width: number, value: number) => {
    const binary = value.toString(2).padStart(width, "0");
    for (let index = 0; index < width; index++) {
      if (binary[index] === "1")
        bytes[(offset + index) >> 3] |= 1 << (7 - ((offset + index) % 8));
    }
  };
  const now = Math.floor(Date.now() / 100);
  bits(0, 6, 2);
  bits(6, 36, now);
  bits(42, 36, now);
  if (purposeOne) bits(152, 24, 1 << 23);
  return bytes.toString("base64url");
}

async function install(
  page: Page,
  options: {
    holdFrames?: boolean;
    blockRenderer?: boolean;
    blockRuntime?: boolean;
    failHydration?: boolean;
    holdHydration?: boolean;
  } = {},
) {
  const session: Session = {
    html: [],
    payloads: [],
    errors: [],
    consoleErrors: [],
    external: [],
    auctions: [],
    blockedRuntime: [],
    rendererInterventions: [],
    pending: [],
    held: [],
    expectedHydrationFailure: options.failHydration,
  };
  sessions.set(page, session);
  let holdHydration = !!options.holdHydration;
  const hydrationRoutes: Route[] = [];
  session.releaseHydration = async () => {
    holdHydration = false;
    for (const route of hydrationRoutes.splice(0)) await route.continue();
  };
  page.on("pageerror", (error) => session.errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") session.consoleErrors.push(message.text());
  });
  page.on("response", (response) => {
    if (
      response.request().resourceType() === "document" &&
      response.url().startsWith(runtimeUrl("/"))
    ) {
      session.pending.push(
        response.text().then((html) => {
          session.html.push(html);
        }),
      );
    }
    if (new URL(response.url()).pathname === "/_ts/page-bids") {
      session.pending.push(
        response.json().then((payload) => {
          session.payloads.push(payload);
        }),
      );
    }
  });
  const prebidDirectory = process.env.PUBLISHER_REGISTRATION_PREBID_DIR!;
  const manifest = JSON.parse(
    readFileSync(resolve(prebidDirectory, "manifest.json"), "utf8"),
  );
  const external = readFileSync(resolve(prebidDirectory, manifest.filename));
  expect(sha256(external)).toBe(manifest.sha256);
  expect(
    `sha384-${createHash("sha384").update(external).digest("base64")}`,
  ).toBe(manifest.sri);
  const puc = readFileSync(
    resolve(
      __dirname,
      "../../node_modules/prebid-universal-creative/dist/banner.js",
    ),
  );
  const publisherOrigin = new URL(runtimeUrl("/")).origin;
  await page
    .context()
    .addCookies([
      { name: "euconsent-v2", value: currentConsent(), url: publisherOrigin },
    ]);
  await page.addInitScript(
    ({ holdFrames }) => {
      if (window !== window.top) return;
      const current = window as unknown as FixtureWindow;
      const raf = window.requestAnimationFrame.bind(window);
      const cancel = window.cancelAnimationFrame.bind(window);
      const frames: Array<{ id: number; callback: FrameRequestCallback }> = [];
      let id = -1;
      current.frameworkFrames = {
        hold: holdFrames,
        pending: () => frames.length,
        // Deliberately force cancelled/old callbacks too. The registration token,
        // not the browser's cancellation behavior, must reject stale work.
        flush(reverse = false) {
          this.hold = false;
          const queued = frames.splice(0);
          if (reverse) queued.reverse();
          for (const frame of queued) frame.callback(performance.now());
        },
      };
      window.requestAnimationFrame = (callback) => {
        if (!current.frameworkFrames.hold) return raf(callback);
        frames.push({ id: id--, callback });
        return frames.at(-1)!.id;
      };
      window.cancelAnimationFrame = (handle) => {
        if (handle >= 0) cancel(handle);
      };
      current.frameworkVisibility = {
        hidden: false,
        set(hidden) {
          this.hidden = hidden;
          document.dispatchEvent(new Event("visibilitychange"));
        },
      };
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => current.frameworkVisibility.hidden,
      });
      Object.defineProperty(document, "visibilityState", {
        configurable: true,
        get: () => (current.frameworkVisibility.hidden ? "hidden" : "visible"),
      });
    },
    { holdFrames: !!options.holdFrames },
  );
  const stub = readFileSync(
    resolve(__dirname, "../../publisher-registration/gpt-fixture.js"),
    "utf8",
  );
  await page.addInitScript({
    content: `if (window === window.top) {${stub}\n}`,
  });
  await page.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin === "https://creative.example.com") {
      if (url.pathname === "/fixture-puc.js")
        return route.fulfill({
          contentType: "application/javascript",
          body: puc,
        });
      if (url.pathname === "/fixture-puc")
        return route.fulfill({
          contentType: "text/html",
          body: pucPage(publisherOrigin),
        });
      if (url.pathname === "/fixture-publisher")
        return route.fulfill({
          contentType: "text/html",
          body: creative("Publisher result"),
        });
    }
    if (url.origin === "https://ads.example.com")
      return route.fulfill({ contentType: "application/javascript", body: "" });
    if (url.origin !== publisherOrigin) {
      session.external.push(url.href);
      return route.abort();
    }
    if (url.pathname === "/integrations/prebid/bundle.js") {
      expect(url.searchParams.get("v")).toBe(manifest.sha256);
      return route.fulfill({
        contentType: "application/javascript",
        body: external,
      });
    }
    if (
      options.failHydration &&
      url.pathname.startsWith("/_next/") &&
      route.request().resourceType() === "script"
    )
      return route.abort();
    if (
      holdHydration &&
      url.pathname.startsWith("/_next/") &&
      route.request().resourceType() === "script"
    ) {
      hydrationRoutes.push(route);
      session.held.push(route);
      return;
    }
    if (
      (options.blockRenderer || options.blockRuntime) &&
      rendererAsset(url.pathname)
    ) {
      const response = await route.fetch();
      session.mainBundle = await response.text();
      session.blockedRuntime.push(url.pathname);
      return route.fulfill({ response, body: "" });
    }
    if (url.pathname === "/auction")
      session.auctions.push(route.request().postData() || "");
    if (
      (process.env.PUBLISHER_REGISTRATION_ASSEMBLY === "shared" ||
        options.blockRenderer) &&
      route.request().resourceType() === "document"
    ) {
      // Request interception adds browser no-cache semantics. Exercise an
      // ordinary reload through the actual Rust interface instead, preserving
      // its complete response and per-reader admitted slots/bids unchanged.
      const headers = await route.request().allHeaders();
      if (process.env.PUBLISHER_REGISTRATION_ASSEMBLY === "shared") {
        delete headers.pragma;
        headers["cache-control"] = "max-age=0";
      }
      const response = await route.fetch({ headers });
      if (process.env.PUBLISHER_REGISTRATION_ASSEMBLY === "shared")
        expect(response.headers()["x-ts-template-cache"]).not.toBe(
          "bypass-request",
        );
      if (options.blockRenderer) {
        const original = await response.text();
        const body = withoutEarlyRenderer(original);
        session.rendererInterventions.push({
          originalSha256: sha256(original),
          servedSha256: sha256(body),
          removedEarly: original !== body,
        });
        return route.fulfill({ response, body });
      }
      return route.fulfill({ response });
    }
    return route.continue();
  });
}

async function ready(page: Page, renderer = true) {
  // Node-side polling still observes the real readiness conditions when a case
  // deliberately holds the page's animation frames to control dispatch order.
  await expect
    .poll(() =>
      page.evaluate(() => {
        const current = window as unknown as FixtureWindow;
        return Boolean(
          current.publisherRegistrationFixture &&
          current.tsjs?.publisherSlotRegistrationVersion === 1 &&
          typeof current.pbjs?.registerBidAdapter === "function" &&
          current.__tsjsPrebidShimInstalled === true &&
          current.googletag.pubads().__tsRefreshWrapped === true,
        );
      }),
    )
    .toBe(true);
  expect(
    await page.evaluate(
      () => (window as unknown as FixtureWindow).tsjs.tsRenderBridgeVersion,
    ),
  ).toBe(renderer ? 1 : undefined);
  await page.evaluate(() => {
    const current = window as unknown as FixtureWindow;
    current.pbjs.onEvent("auctionInit", () =>
      current.fixture.record("realPrebidAuctionInit"),
    );
    current.fixture.record("refreshWrapperReady", {
      ready: current.googletag.pubads().__tsRefreshWrapped,
    });
  });
}

async function headerRequests(page: Page) {
  return page.evaluate(() =>
    (window as unknown as FixtureWindow).fixture.requests.filter((request) =>
      request.id.startsWith("publisher-final"),
    ),
  );
}

async function mount(page: Page, id = "publisher-final-header") {
  await page.evaluate(
    (finalId) =>
      (window as unknown as FixtureWindow).publisherRegistrationFixture.mount(
        finalId,
      ),
    id,
  );
  await expect(page.getByTestId("final-placement")).toHaveAttribute("id", id);
}

async function delivered(page: Page, count = 1) {
  await expect
    .poll(async () => (await headerRequests(page)).length)
    .toBe(count);
  return (await headerRequests(page)).at(-1)!;
}

async function render(page: Page, snapshot: RequestSnapshot, label: string) {
  await page.evaluate(
    (index) => (window as unknown as FixtureWindow).fixture.render(index),
    snapshot.index,
  );
  const selector = `#${snapshot.id} > iframe`;
  const frame = page.frameLocator(selector);
  if (label === "Publisher result")
    await expect(frame.locator(".marker")).toHaveText(label);
  else {
    // Universal Creative may create a nested frame. Inspect real content in that
    // frame, independently of fixture markers and native request counts.
    await expect
      .poll(async () => {
        const outer = await (
          await page.locator(selector).elementHandle()
        )?.contentFrame();
        if (!outer) return false;
        const descendants = [outer];
        for (let index = 0; index < descendants.length; index++)
          descendants.push(...descendants[index].childFrames());
        const matches = await Promise.all(
          descendants.map(async (candidate) => {
            const creative = candidate.locator(
              '[data-fixture-creative="winner"]',
            );
            return (
              (await creative.count()) > 0 &&
              (await creative.isVisible()) &&
              (await creative.textContent()) === label
            );
          }),
        );
        return matches.some(Boolean);
      })
      .toBe(true);
    const facts = await page.evaluate((id) => {
      const element = document.getElementById(id)!;
      const current = window as unknown as FixtureWindow;
      const instance = current.publisherRegistrationFixture.instances.at(-1)!;
      return {
        sameElement: element === instance.element,
        connected: instance.element.isConnected,
        frames: element.querySelectorAll("iframe").length,
      };
    }, snapshot.id);
    expect(facts).toMatchObject({
      sameElement: true,
      connected: true,
      frames: 1,
    });
  }
  await expect(page.locator(selector)).toBeVisible();
}

async function assertIdentity(page: Page, snapshot: RequestSnapshot) {
  const facts = await page.evaluate((request) => {
    const current = window as unknown as FixtureWindow;
    const instance = current.publisherRegistrationFixture.instances.at(-1)!;
    return {
      native: instance.slot === current.fixture.nativeSlots[request.native],
      element: instance.element === current.fixture.elements[request.element],
      lookup: document.getElementById(request.id) === instance.element,
      connected: instance.element.isConnected,
      generation: instance.generation,
      currentGeneration: current.tsjs.navGeneration ?? 0,
      unrelatedRequests: current.fixture.requests.filter(
        (entry) => entry.id === "publisher-unrelated",
      ).length,
      defines: current.fixture.trace.filter(
        (entry) =>
          entry.event === "nativeDefine" &&
          entry.native === request.native &&
          entry.id === request.id,
      ).length,
      displays: current.fixture.trace.filter(
        (entry) =>
          entry.event === "nativeDisplay" &&
          entry.native === request.native &&
          entry.id === request.id,
      ).length,
      refresh: current.fixture.trace.filter(
        (entry) =>
          entry.event === "nativeRefresh" &&
          (entry.natives as number[]).includes(request.native),
      ),
    };
  }, snapshot);
  expect(facts).toMatchObject({
    native: true,
    element: true,
    lookup: true,
    connected: true,
    unrelatedRequests: 0,
    defines: 1,
    displays: 1,
  });
  expect(facts.generation).toBe(facts.currentGeneration);
  expect(facts.refresh[0]).toMatchObject({
    scoped: true,
    natives: [snapshot.native],
  });
}

async function holdNavigationPayload(page: Page) {
  let received!: (route: Route) => void;
  const arrived = new Promise<Route>((resolve) => {
    received = resolve;
  });
  await page.route("**/_ts/page-bids?**", (route) => {
    sessions.get(page)!.held.push(route);
    received(route);
  });
  return {
    arrived,
    async release() {
      const route = await arrived;
      const actualResponse = await route.fetch();
      const payload = await actualResponse.json();
      expect(
        payload.slots.some(
          (slot: { id: string; initialization?: string }) =>
            slot.id === "fixture-header" &&
            slot.initialization === "publisher_registered",
        ),
      ).toBe(true);
      await route.fulfill({ response: actualResponse });
      await page.unroute("**/_ts/page-bids?**");
    },
  };
}

async function saveEvidence(page: Page, info: TestInfo) {
  const session = sessions.get(page);
  if (!session) return;
  for (const route of session.held) await route.abort().catch(() => {});
  await Promise.allSettled(session.pending);
  await info.attach("actual-rust-responses", {
    body: JSON.stringify(
      {
        html: session.html,
        htmlSha256: session.html.map(sha256),
        bootstrapSha256: session.html
          .map(
            (html) =>
              scriptBlocks(html).find((script: { content: string }) =>
                script.content.includes("publisherSlotRegistrationVersion"),
              )?.content,
          )
          .map((bootstrap) => (bootstrap ? sha256(bootstrap) : null)),
        payloads: session.payloads,
      },
      null,
      2,
    ),
    contentType: "application/json",
  });
  await info.attach("network-and-console", {
    body: JSON.stringify(
      {
        html: session.html,
        payloads: session.payloads,
        errors: session.errors,
        consoleErrors: session.consoleErrors,
        external: session.external,
        auctions: session.auctions,
        blockedRuntime: session.blockedRuntime,
        rendererInterventions: session.rendererInterventions,
        expectedHydrationFailure: session.expectedHydrationFailure,
      },
      null,
      2,
    ),
    contentType: "application/json",
  });
  if (!page.isClosed()) {
    const facts = await page
      .evaluate(() => {
        const current = window as unknown as FixtureWindow;
        return {
          requests: current.fixture?.requests,
          trace: current.fixture?.trace,
          api: current.tsjs?.publisherSlotRegistrationVersion,
          renderer: current.tsjs?.tsRenderBridgeVersion,
          slots: current.tsjs?.adSlots,
          mapping: current.tsjs?.divToSlotId,
          instances: current.publisherRegistrationFixture?.instances.map(
            (instance) => ({
              generation: instance.generation,
              id: instance.slot.getSlotElementId(),
              connected: instance.element.isConnected,
              statuses: instance.handles.map((handle) => handle.getStatus()),
              eligibleFormats: instance.eligibleFormats,
            }),
          ),
        };
      })
      .catch((error: Error) => ({ unavailable: error.message }));
    await info.attach("native-registration-facts", {
      body: JSON.stringify(facts, null, 2),
      contentType: "application/json",
    });
  }
  const origin = `http://127.0.0.1:${process.env.INTEGRATION_ORIGIN_PORT || "8888"}`;
  const exchanges = await page.request.get(`${origin}/api/fixture-auction`);
  await info.attach("actual-provider-exchanges", {
    body: await exchanges.body(),
    contentType: "application/json",
  });
}

test.beforeEach(async ({ request }) => {
  const origin = `http://127.0.0.1:${process.env.INTEGRATION_ORIGIN_PORT || "8888"}`;
  await request.delete(`${origin}/api/fixture-auction`);
});
test.afterEach(async ({ page }, info) => {
  await saveEvidence(page, info);
  const session = sessions.get(page);
  if (session) {
    expect(session.external).toEqual([]);
    if (!session.expectedHydrationFailure) expect(session.errors).toEqual([]);
  }
});

test("actual Rust HTML, admitted payload, delayed mount and later real Prebid refresh", async ({
  page,
}) => {
  await install(page);
  const response = await page.goto(runtimeUrl("/publisher-registration"));
  const html = (await response!.text()).replaceAll('\\"', '"');
  expect(html).toContain('"initialization":"publisher_registered"');
  expect(html).toContain("scheduleInitialAdInit");
  expect(html.indexOf("publisherSlotRegistrationVersion")).toBeGreaterThan(-1);
  expect(html.indexOf("publisherSlotRegistrationVersion")).toBeLessThan(
    html.indexOf('id="fixture-header-placeholder"'),
  );
  if (process.env.PUBLISHER_REGISTRATION_ASSEMBLY === "shared")
    expect(html).toContain('if(typeof s==="function")s(b,a)');
  else expect(html).toContain('if(typeof s==="function")s(b);');
  await ready(page);
  expect(await headerRequests(page)).toHaveLength(0);
  expect(
    await page.evaluate(() =>
      (window as unknown as FixtureWindow).fixture.trace.some(
        (entry) =>
          entry.event === "nativeDefine" &&
          String(entry.id).startsWith("fixture-header-"),
      ),
    ),
  ).toBe(false);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as FixtureWindow).fixture.requests.filter(
            (entry) => entry.id.startsWith("fixture-auto-"),
          ).length,
      ),
    )
    .toBe(1);
  await page.setViewportSize({ width: 500, height: 800 });
  await mount(page, "publisher-final-changed-before-registration");
  const initial = await delivered(page);
  expect(initial.targeting).toMatchObject({
    ts_initial: ["1"],
    hb_adid: ["fixture-win-fixture-header"],
    publisher_section: ["fiction"],
  });
  await assertIdentity(page, initial);
  await render(page, initial, "Fixture publisher registration winner");
  const session = sessions.get(page)!;
  expect(session.auctions).toHaveLength(0);
  await page.getByRole("button", { name: "Later publisher refresh" }).click();
  await expect.poll(() => session.auctions.length).toBe(1);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as FixtureWindow).fixture.trace.filter(
            (entry) => entry.event === "realPrebidAuctionInit",
          ).length,
      ),
    )
    .toBe(1);
  const later = await delivered(page, 2);
  expect(later.targeting.publisher_section).toEqual(["fiction"]);
  const traces = await page.evaluate(
    () => (window as unknown as FixtureWindow).fixture.trace,
  );
  expect(
    traces
      .filter(
        (entry) =>
          entry.event === "nativeRefresh" &&
          (entry.natives as number[]).includes(initial.native),
      )
      .at(-1),
  ).toMatchObject({ scoped: true, options: { changeCorrelator: false } });
  expect(session.errors).toEqual([]);
  expect(session.external).toEqual([]);
});

test("registered dispatch before automatic frames preserves both mapping owners", async ({
  page,
}) => {
  await install(page, { holdFrames: true });
  await page.goto(runtimeUrl("/publisher-registration"));
  await ready(page);
  await mount(page);
  expect(await headerRequests(page)).toHaveLength(0);
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).frameworkFrames.flush(true),
  );
  const initial = await delivered(page);
  await assertIdentity(page, initial);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as FixtureWindow).fixture.requests.filter(
            (entry) => entry.id.startsWith("fixture-auto-"),
          ).length,
      ),
    )
    .toBe(1);
  const mappings = await page.evaluate(
    () => (window as unknown as FixtureWindow).tsjs.divToSlotId,
  );
  expect(mappings?.[initial.id]).toBe("fixture-header");
  expect(Object.values(mappings || {})).toContain("fixture-auto");
  const order = await page.evaluate(() =>
    (window as unknown as FixtureWindow).fixture.requests.map(
      (entry) => entry.id,
    ),
  );
  expect(order.indexOf(initial.id)).toBeLessThan(
    order.findIndex((id) => id.startsWith("fixture-auto-")),
  );
  await render(page, initial, "Fixture publisher registration winner");
});

test("real pathname navigation registers while actual page-bids is held, then retires old mappings", async ({
  page,
}) => {
  await install(page);
  await page.goto(runtimeUrl("/publisher-registration"));
  await ready(page);
  const oldGeneration = await page.evaluate(
    () => (window as unknown as FixtureWindow).tsjs.navGeneration ?? 0,
  );
  const held = await holdNavigationPayload(page);
  await page.getByRole("button", { name: "Registered-only route" }).click();
  await page.waitForURL("**/publisher-registration/registered-only");
  await held.arrived;
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as FixtureWindow).tsjs.navGeneration ?? 0,
      ),
    )
    .toBeGreaterThan(oldGeneration);
  await mount(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as FixtureWindow
          ).publisherRegistrationFixture.status()?.status,
      ),
    )
    .toBe("waiting_payload");
  expect(await headerRequests(page)).toHaveLength(0);
  await held.release();
  const initial = await delivered(page);
  await assertIdentity(page, initial);
  await render(page, initial, "Fixture publisher registration winner");
  const before = await page.evaluate(
    () => (window as unknown as FixtureWindow).fixture.trace.length,
  );
  const childGeneration = await page.evaluate(
    () => (window as unknown as FixtureWindow).tsjs.navGeneration ?? 0,
  );
  await page.getByRole("button", { name: "Mixed route", exact: true }).click();
  await page.waitForURL("**/publisher-registration");
  await expect(page.getByTestId("ssr-placeholder")).toBeVisible();
  await mount(page, "publisher-final-next-generation");
  const next = await delivered(page, 2);
  await assertIdentity(page, next);
  expect(
    await page.evaluate(
      () => (window as unknown as FixtureWindow).tsjs.navGeneration ?? 0,
    ),
  ).toBeGreaterThan(childGeneration);
  expect(next.native).not.toBe(initial.native);
  expect(next.element).not.toBe(initial.element);
  const facts = await page.evaluate(
    ({ previous, before }) => {
      const current = window as unknown as FixtureWindow;
      return {
        oldConnected: current.fixture.elements[previous.element].isConnected,
        mapping: current.tsjs.divToSlotId,
        trace: current.fixture.trace.slice(before),
      };
    },
    { previous: initial, before },
  );
  expect(facts.oldConnected).toBe(false);
  expect(facts.mapping?.[initial.id]).toBeUndefined();
  expect(facts.mapping?.[next.id]).toBe("fixture-header");
  expect(
    facts.trace.find(
      (entry) =>
        entry.event === "nativeDestroy" && entry.native === initial.native,
    ),
  ).toMatchObject({ connected: true });
});

test("hidden breakpoint narrowing cancels old eligibility and cannot resurrect a tall winner", async ({
  page,
}) => {
  await install(page);
  await page.goto(runtimeUrl("/publisher-registration/tall"));
  await ready(page);
  await page.evaluate(() => {
    const current = window as unknown as FixtureWindow;
    current.frameworkFrames.hold = true;
  });
  await mount(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as FixtureWindow
          ).publisherRegistrationFixture.status()?.status,
      ),
    )
    .toBe("waiting_delivery");
  expect(
    await page.evaluate(() =>
      (window as unknown as FixtureWindow).frameworkFrames.pending(),
    ),
  ).toBeGreaterThan(0);
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).frameworkVisibility.set(true),
  );
  await page.setViewportSize({ width: 500, height: 800 });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as FixtureWindow
          ).publisherRegistrationFixture.instances.at(-1)!.handles.length,
      ),
    )
    .toBe(2);
  const replacement = await page.evaluate(() => {
    const current = window as unknown as FixtureWindow;
    const instance = current.publisherRegistrationFixture.instances.at(-1)!;
    instance.handles[0].cancel();
    return {
      oldStatus: instance.handles[0].getStatus(),
      formats: instance.eligibleFormats,
    };
  });
  expect(replacement).toMatchObject({
    oldStatus: { status: "cancelled" },
    formats: [[300, 250]],
  });
  expect(await headerRequests(page)).toHaveLength(0);
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).frameworkFrames.flush(),
  );
  expect(await headerRequests(page)).toHaveLength(0);
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).frameworkVisibility.set(false),
  );
  const initial = await delivered(page);
  expect(initial.targeting.hb_adid).toBeUndefined();
  expect(initial.targeting.ts_initial).toEqual(["1"]);
  await assertIdentity(page, initial);
  await render(page, initial, "Publisher result");
  await page.setViewportSize({ width: 900, height: 800 });
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).frameworkFrames.flush(),
  );
  expect(await headerRequests(page)).toHaveLength(1);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as FixtureWindow
        ).publisherRegistrationFixture.instances.at(-1)!.handles.length,
    ),
  ).toBe(2);
});

test("pending payload cancellation destroys before React removes the element", async ({
  page,
}) => {
  await install(page);
  await page.goto(runtimeUrl("/publisher-registration"));
  await ready(page);
  const held = await holdNavigationPayload(page);
  await page.getByRole("button", { name: "Registered-only route" }).click();
  await page.waitForURL("**/publisher-registration/registered-only");
  await held.arrived;
  await mount(page);
  await page.getByRole("button", { name: "Destroy then remove" }).click();
  await expect(page.getByTestId("ssr-placeholder")).toBeVisible();
  await held.release();
  expect(await headerRequests(page)).toHaveLength(0);
  const destroyed = await page.evaluate(() =>
    (window as unknown as FixtureWindow).fixture.trace.filter(
      (entry) =>
        entry.event === "nativeDestroy" &&
        entry.id === "publisher-final-header",
    ),
  );
  expect(destroyed).toHaveLength(1);
  expect(destroyed[0]).toMatchObject({ connected: true });
  await mount(page);
  const replacement = await delivered(page);
  await assertIdentity(page, replacement);
  expect(replacement.native).not.toBe(destroyed[0].native);
  await render(page, replacement, "Fixture publisher registration winner");
});

test("legal no-bid still performs one scoped ordinary GAM delivery", async ({
  page,
}) => {
  await install(page);
  await page.goto(runtimeUrl("/publisher-registration/no-bid"));
  await ready(page);
  await mount(page);
  const initial = await delivered(page);
  expect(initial.targeting.hb_adid).toBeUndefined();
  await assertIdentity(page, initial);
  await render(page, initial, "Publisher result");
  expect(
    await page.evaluate(
      () =>
        (window as unknown as FixtureWindow).tsjs.bids?.["fixture-header"]
          ?.debug_bid,
    ),
  ).toBeUndefined();
});

test("bootstrap-only ordinary fill survives late renderer installation without replacement", async ({
  page,
}) => {
  await install(page, { blockRenderer: true });
  await page.goto(runtimeUrl("/publisher-registration/registered-only"));
  await ready(page, false);
  await mount(page);
  const initial = await delivered(page);
  expect(initial.targeting.hb_adid).toBeUndefined();
  await assertIdentity(page, initial);
  await render(page, initial, "Publisher result");
  const session = sessions.get(page)!;
  expect(session.rendererInterventions).toHaveLength(1);
  expect(session.rendererInterventions[0].removedEarly).toBe(true);
  expect(
    session.blockedRuntime.some((url) => url.includes("tsjs-gpt.min.js")),
  ).toBe(true);
  expect(session.mainBundle).toBeTruthy();
  await page.addScriptTag({ content: session.mainBundle! });
  await page.waitForFunction(
    () => (window as unknown as FixtureWindow).tsjs.tsRenderBridgeVersion === 1,
  );
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  expect(
    await page.evaluate(
      () =>
        (window as unknown as FixtureWindow).tsjs.bids?.["fixture-header"]
          ?.hb_adid,
    ),
  ).toBe("fixture-win-fixture-header");
  expect(await headerRequests(page)).toHaveLength(1);
  expect(
    await page.evaluate(() =>
      (window as unknown as FixtureWindow).fixture.nativeSlots
        .find((slot) => slot.getSlotElementId() === "publisher-final-header")!
        .getTargeting("hb_adid"),
    ),
  ).toEqual([]);
  await expect(
    page.frameLocator("#publisher-final-header > iframe").locator(".marker"),
  ).toHaveText("Publisher result");
});

test("failed deferred runtime retains the early renderer and server candidate", async ({
  page,
}) => {
  await install(page, { blockRuntime: true });
  await page.goto(runtimeUrl("/publisher-registration/registered-only"));
  await ready(page);
  expect(sessions.get(page)!.blockedRuntime).toContain(
    "/static/tsjs=tsjs-gpt.min.js",
  );
  expect(sessions.get(page)!.rendererInterventions).toEqual([]);
  await expect(page.locator("#trustedserver-bootstrap")).toHaveCount(1);
  await mount(page);
  const initial = await delivered(page);
  expect(initial.targeting.hb_adid).toEqual(["fixture-win-fixture-header"]);
  await assertIdentity(page, initial);
  await render(page, initial, "Fixture publisher registration winner");
  expect(await headerRequests(page)).toHaveLength(1);
});

for (const { width, noFill } of [
  { width: 500, noFill: false },
  { width: 600, noFill: false },
  { width: 601, noFill: false },
  { width: 1200, noFill: false },
  { width: 500, noFill: true },
  { width: 1200, noFill: true },
])
  test(`Rust reservation precedes real Next hydration at ${width}px, ${noFill ? "no-fill" : "fill"}`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1100 });
    await install(page, { holdHydration: true });
    await page.goto(runtimeUrl("/publisher-registration/registered-only"), {
      waitUntil: "domcontentloaded",
    });
    const wrapper = page.getByTestId("reserved-space");
    await expect(page.getByTestId("ssr-placeholder")).toBeVisible();
    const expected = width <= 600 ? 250 : 600;
    expect(
      await wrapper.evaluate((element) => getComputedStyle(element).minHeight),
    ).toBe(`${expected}px`);
    expect(await wrapper.getAttribute("style")).toBeNull();
    expect(
      await page.evaluate(
        () =>
          !!(window as unknown as FixtureWindow).publisherRegistrationFixture,
      ),
    ).toBe(false);
    const original = await wrapper.elementHandle();
    const before = await wrapper.boundingBox();
    expect(before!.height).toBe(expected);
    await sessions.get(page)!.releaseHydration!();
    await ready(page);
    await mount(page);
    const initial = await delivered(page);
    await assertIdentity(page, initial);
    expect(
      await original!.evaluate(
        (element) =>
          element === document.getElementById("fixture-header-space"),
      ),
    ).toBe(true);
    expect(
      await page
        .getByTestId("final-placement")
        .evaluate((element) => element.style.minHeight),
    ).toBe("");
    const beforeFill = await wrapper.boundingBox();
    const flowBefore = await page
      .getByTestId("automatic-placement")
      .evaluate((element) => element.getBoundingClientRect().top);
    if (noFill) {
      await page.evaluate(
        (index) =>
          (window as unknown as FixtureWindow).fixture.render(index, {
            noFill: true,
          }),
        initial.index,
      );
      await expect(
        page.getByTestId("final-placement").locator("iframe"),
      ).toHaveCount(0);
    } else await render(page, initial, "Fixture publisher registration winner");
    const afterFill = await wrapper.boundingBox();
    expect(afterFill!.height).toBe(expected);
    expect(afterFill).toEqual(beforeFill);
    expect(
      await page
        .getByTestId("automatic-placement")
        .evaluate((element) => element.getBoundingClientRect().top),
    ).toBe(flowBefore);
    expect(await headerRequests(page)).toHaveLength(1);
  });

test("delayed real Next hydration cannot trigger a guessed registered delivery", async ({
  page,
}) => {
  await install(page, { holdHydration: true });
  await page.goto(runtimeUrl("/publisher-registration/registered-only"), {
    waitUntil: "domcontentloaded",
  });
  await expect(page.getByTestId("ssr-placeholder")).toBeVisible();
  expect(
    await page.evaluate(
      () => !!(window as unknown as FixtureWindow).publisherRegistrationFixture,
    ),
  ).toBe(false);
  expect(await headerRequests(page)).toHaveLength(0);
  await sessions.get(page)!.releaseHydration!();
  await ready(page);
  await mount(page);
  const initial = await delivered(page);
  await assertIdentity(page, initial);
  await render(page, initial, "Fixture publisher registration winner");
});

test("failed Next hydration never authorizes registered-placeholder delivery", async ({
  page,
}) => {
  await install(page, { failHydration: true });
  await page.goto(runtimeUrl("/publisher-registration/registered-only"));
  await expect(page.getByTestId("ssr-placeholder")).toBeVisible();
  expect(
    await page.evaluate(
      () => !!(window as unknown as FixtureWindow).publisherRegistrationFixture,
    ),
  ).toBe(false);
  expect(await headerRequests(page)).toHaveLength(0);
  expect(
    await page.evaluate(() =>
      (window as unknown as FixtureWindow).fixture.trace.some(
        (entry) =>
          entry.event === "nativeDefine" &&
          String(entry.id).startsWith("fixture-header-"),
      ),
    ),
  ).toBe(false);
});

test("breakpoint change while waiting for actual SPA payload replaces eligibility synchronously", async ({
  page,
}) => {
  await install(page);
  await page.goto(runtimeUrl("/publisher-registration"));
  await ready(page);
  const held = await holdNavigationPayload(page);
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).publisherRegistrationFixture.navigate(
      "/publisher-registration/tall",
    ),
  );
  await page.waitForURL("**/publisher-registration/tall");
  await held.arrived;
  await mount(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as FixtureWindow
          ).publisherRegistrationFixture.status()?.status,
      ),
    )
    .toBe("waiting_payload");
  await page.setViewportSize({ width: 500, height: 800 });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as FixtureWindow
          ).publisherRegistrationFixture.instances.at(-1)!.handles.length,
      ),
    )
    .toBe(2);
  await page.evaluate(() =>
    (window as unknown as FixtureWindow).publisherRegistrationFixture.instances
      .at(-1)!
      .handles[0].cancel(),
  );
  expect(await headerRequests(page)).toHaveLength(0);
  await held.release();
  const initial = await delivered(page);
  expect(initial.targeting.hb_adid).toBeUndefined();
  await assertIdentity(page, initial);
  await render(page, initial, "Publisher result");
});

test("post-request destroy/remove does not reopen the consumed placement", async ({
  page,
}) => {
  await install(page);
  await page.goto(runtimeUrl("/publisher-registration/registered-only"));
  await ready(page);
  await mount(page);
  const first = await delivered(page);
  await render(page, first, "Fixture publisher registration winner");
  await page.getByRole("button", { name: "Destroy then remove" }).click();
  await expect(page.getByTestId("ssr-placeholder")).toBeVisible();
  expect(
    await page.evaluate(
      (element) =>
        (window as unknown as FixtureWindow).fixture.elements[element]
          .isConnected,
      first.element,
    ),
  ).toBe(false);
  await mount(page, "publisher-final-replacement-after-consumed");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as FixtureWindow).publisherRegistrationFixture
            .instances.length,
      ),
    )
    .toBe(2);
  expect(await headerRequests(page)).toHaveLength(1);
  const traces = await page.evaluate(
    () => (window as unknown as FixtureWindow).fixture.trace,
  );
  expect(
    traces.find(
      (entry) =>
        entry.event === "nativeDestroy" && entry.native === first.native,
    ),
  ).toMatchObject({ connected: true });
  expect(
    traces.some(
      (entry) =>
        entry.event === "nativeDisplay" &&
        entry.id === "publisher-final-replacement-after-consumed",
    ),
  ).toBe(false);
});

test("actual page-bids with denied consent settles with no admitted placements", async ({
  request,
}) => {
  const response = await request.get(
    runtimeUrl("/_ts/page-bids?path=/publisher-registration"),
    {
      headers: {
        "X-TSJS-Page-Bids": "1",
        "Sec-Fetch-Site": "same-origin",
        "User-Agent": "Mozilla/5.0 FixturePublisherRegistration",
        Cookie: `euconsent-v2=${currentConsent(false)}`,
      },
    },
  );
  expect(response.ok()).toBe(true);
  expect(await response.json()).toEqual({ slots: [], bids: {} });
});

test("served runtime modules exactly match the selected current distribution", async ({
  request,
}) => {
  const distribution = process.env.PUBLISHER_REGISTRATION_DIST!;
  // This isolated config enables GPT and deferred Prebid; core and the creative
  // module are unconditional. Match bundle.rs's documented IIFE separator.
  const immediate = ["core", "creative", "gpt"]
    .map((module) =>
      readFileSync(resolve(distribution, `tsjs-${module}.js`), "utf8"),
    )
    .join(";\n");
  const main = await request.get(
    runtimeUrl("/static/tsjs=tsjs-unified.min.js"),
  );
  expect(main.ok()).toBe(true);
  expect(
    sha256(await main.body()),
    "served immediate modules must match selected current distribution",
  ).toBe(sha256(immediate));
  const prebid = await request.get(
    runtimeUrl("/static/tsjs=tsjs-prebid.min.js"),
  );
  expect(prebid.ok()).toBe(true);
  expect(
    sha256(await prebid.body()),
    "served deferred module must match selected current distribution",
  ).toBe(sha256(readFileSync(resolve(distribution, "tsjs-prebid.js"))));
});

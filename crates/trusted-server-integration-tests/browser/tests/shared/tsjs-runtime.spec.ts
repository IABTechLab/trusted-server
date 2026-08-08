import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";

const TSJS_CRATE = resolve(__dirname, "../../../../trusted-server-js");
const CORE_BUNDLE = resolve(TSJS_CRATE, "dist/tsjs-core.js");
const GPT_BUNDLE = resolve(TSJS_CRATE, "dist/tsjs-gpt.js");
const RELEASE = JSON.parse(
  readFileSync(resolve(TSJS_CRATE, "dist/tsjs-release-v1.json"), "utf8"),
) as { releaseId: string };

function boot(releaseId: string) {
  return {
    abi: 1,
    releaseId,
    manifest: { version: 1, releaseId, integrations: [] },
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: "browser-initial", results: [] },
      slots: [],
      bids: [],
    },
    creative: {
      version: 1,
      enabled: false,
      clickGuard: false,
      renderGuard: false,
    },
    diagnostics: {
      version: 1,
      renderTraceOverlay: false,
      gpt: { active: false },
    },
  };
}

async function waitForRuntime(page: Page, state: "kernel" | "fallback") {
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              tsjs?: { _internal?: { state?: string } };
            }
          ).tsjs?._internal?.state,
      ),
    )
    .toBe(state);
}

async function openRuntimePage(page: Page) {
  await page.route("https://runtime.test/fixture", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/html",
      body: '<!doctype html><div id="runtime-slot"></div>',
    }),
  );
  await page.goto("https://runtime.test/fixture");
}

test.describe("TSJS hard-cutover runtime", () => {
  test("publishes only the kernel API and drains a hostile preload queue once", async ({
    page,
  }) => {
    await openRuntimePage(page);
    await page.evaluate((initialBoot) => {
      const browserWindow = window as unknown as {
        queueOrder: string[];
        tsjs: Record<string, unknown> & { que: Array<() => void> };
      };
      browserWindow.queueOrder = [];
      const que = [
        () => {
          browserWindow.queueOrder.push("first");
          browserWindow.tsjs.que.push(() =>
            browserWindow.queueOrder.push("nested"),
          );
        },
        () => {
          browserWindow.queueOrder.push("throw");
          throw new Error("publisher callback failure");
        },
        () => browserWindow.queueOrder.push("last"),
      ];
      browserWindow.tsjs = {
        boot: initialBoot,
        que,
        _integrationConfig: {},
        bids: { legacy: true },
        renderAdUnit() {},
        renderAllAdUnits() {},
        setConfig() {},
        getConfig() {},
      };
    }, boot(RELEASE.releaseId));

    await page.addScriptTag({ path: CORE_BUNDLE });
    await waitForRuntime(page, "kernel");

    const state = await page.evaluate(() => {
      const api = (window as unknown as { tsjs: Record<string, unknown> }).tsjs;
      return {
        names: Object.getOwnPropertyNames(api).sort(),
        queueOrder: (
          window as unknown as { queueOrder: string[] }
        ).queueOrder.slice(),
        queueFrozen: Object.isFrozen(api.que),
        bootFrozen: Object.isFrozen(api.boot),
        releaseId: api.releaseId,
        legacy: [
          "bids",
          "renderAdUnit",
          "renderAllAdUnits",
          "setConfig",
          "getConfig",
          "adInit",
          "renders",
          "gptDiagnostics",
        ].filter((name) => Object.prototype.hasOwnProperty.call(api, name)),
      };
    });

    expect(state.names).toEqual([
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
    ]);
    expect(state.queueOrder).toEqual(
      expect.arrayContaining(["first", "nested", "throw", "last"]),
    );
    expect(new Set(state.queueOrder).size).toBe(4);
    expect(state.queueFrozen).toBe(true);
    expect(state.bootFrozen).toBe(true);
    expect(state.releaseId).toBe(RELEASE.releaseId);
    expect(state.legacy).toEqual([]);
  });

  test("terminal fallback cannot be revived by a late integration bundle", async ({
    page,
  }) => {
    await openRuntimePage(page);
    await page.evaluate((initialBoot) => {
      const browserWindow = window as unknown as {
        tsjs: Record<string, unknown>;
        fallbackEffects: { messageListeners: number; timeouts: number };
      };
      browserWindow.fallbackEffects = { messageListeners: 0, timeouts: 0 };
      const nativeAddEventListener = window.addEventListener.bind(window);
      window.addEventListener = ((
        type: string,
        listener: EventListenerOrEventListenerObject,
      ) => {
        if (type === "message")
          browserWindow.fallbackEffects.messageListeners += 1;
        nativeAddEventListener(type, listener);
      }) as typeof window.addEventListener;
      const nativeSetTimeout = window.setTimeout.bind(window);
      window.setTimeout = ((handler: TimerHandler, timeout?: number) => {
        browserWindow.fallbackEffects.timeouts += 1;
        return nativeSetTimeout(handler, timeout);
      }) as typeof window.setTimeout;
      browserWindow.tsjs = {
        boot: initialBoot,
        que: [],
        _integrationConfig: Object.create({ hostile: true }),
      };
    }, boot(RELEASE.releaseId));

    await page.addScriptTag({ path: CORE_BUNDLE });
    await waitForRuntime(page, "fallback");
    const before = await page.evaluate(() => ({
      effects: {
        ...(
          window as unknown as {
            fallbackEffects: { messageListeners: number; timeouts: number };
          }
        ).fallbackEffects,
      },
      names: Object.getOwnPropertyNames(
        (window as unknown as { tsjs: object }).tsjs,
      ).sort(),
    }));

    await page.addScriptTag({ path: GPT_BUNDLE });
    await page.evaluate(() => {
      window.dispatchEvent(
        new MessageEvent("message", { data: { message: "Prebid Request" } }),
      );
    });
    await page.waitForTimeout(25);

    const after = await page.evaluate(() => {
      const browserWindow = window as unknown as {
        tsjs: {
          _internal: { state: string; reason: string };
          _registerIntegration(value: unknown): boolean;
        };
        fallbackEffects: { messageListeners: number; timeouts: number };
        googletag?: unknown;
      };
      return {
        internal: browserWindow.tsjs._internal,
        registrationAccepted: browserWindow.tsjs._registerIntegration({}),
        effects: { ...browserWindow.fallbackEffects },
        frames: document.querySelectorAll("iframe").length,
        scripts: document.querySelectorAll("script").length,
        hasGoogletag: Object.prototype.hasOwnProperty.call(window, "googletag"),
      };
    });

    expect(before.names).toEqual([
      "_internal",
      "_registerIntegration",
      "addAdUnits",
      "boot",
      "log",
      "que",
      "releaseId",
      "requestAds",
      "version",
    ]);
    expect(after.internal).toMatchObject({
      state: "fallback",
      reason: "abi_mismatch",
    });
    expect(after.registrationAccepted).toBe(false);
    expect(after.effects.messageListeners).toBe(
      before.effects.messageListeners,
    );
    expect(after.effects.timeouts).toBe(before.effects.timeouts);
    expect(after.frames).toBe(0);
    expect(after.scripts).toBe(2);
    expect(after.hasGoogletag).toBe(false);
  });
});

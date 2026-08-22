import { expect, test, type Page } from "@playwright/test";
import { installGptStub } from "../../helpers/gpt-stub.js";
import {
  firstDisplayTsjsFixture,
  loadRuntimeTsjsFixture,
  runtimeTsjsFixture,
  serverBootTransportLiteralV1,
  type RuntimeTsjsFixture,
} from "../../helpers/tsjs-fixture.js";

const KERNEL_FIXTURE = runtimeTsjsFixture(["render_runtime"]);
const FALLBACK_FIXTURE = runtimeTsjsFixture([]);
const FIRST_DISPLAY_SLOT = "first-display-owner-slot";
const FIRST_DISPLAY_FIXTURE = firstDisplayTsjsFixture({
  firstDisplayIds: ["first_display", "render_owner_initial", "gpt_initial"],
  takeoverIds: ["render_runtime", "gpt"],
  auctionProjection: {
    version: 1,
    auction: {
      version: 1,
      auctionId: "browser-first-display-owner",
      results: [
        {
          slot: FIRST_DISPLAY_SLOT,
          outcome: "winner",
          candidateId: "AAAAAAAAAAAA",
        },
      ],
    },
    slots: [
      {
        slot: FIRST_DISPLAY_SLOT,
        gamUnitPath: `/123/${FIRST_DISPLAY_SLOT}`,
        divId: FIRST_DISPLAY_SLOT,
        formats: [[300, 250]],
        targeting: {},
      },
    ],
    bids: [
      {
        candidateId: "AAAAAAAAAAAA",
        slot: FIRST_DISPLAY_SLOT,
        provider: "fictional",
        upstreamBidId: "first-display-owner-bid",
        cpm: 1.25,
        currency: "USD",
        targeting: { hb_bidder: "fictional" },
        rendererReservationId: "r1_AAAAAAAAAAAAAAAAAAAAAA",
        renderSource: {
          type: "adm",
          version: 1,
          adm: "<main>fictional first-display creative</main>",
          width: 300,
          height: 250,
        },
      },
    ],
  },
  integrations: {
    version: 1,
    entries: [
      {
        id: "gpt",
        config: {
          gamAttributionEnabled: false,
          pageBidsEnabled: true,
        },
      },
    ],
  },
});

function boot(fixture: RuntimeTsjsFixture) {
  return {
    abi: 1,
    releaseId: fixture.releaseId,
    manifest: fixture.manifest,
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: "browser-initial", results: [] },
      slots: [],
      bids: [],
    },
    integrations: { version: 1, entries: [] },
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

async function installFirstDisplayResourceProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const events: Array<{ kind: string; beforePaint: boolean }> = [];
    const record = (kind: string): void => {
      events.push({
        kind,
        beforePaint:
          performance.getEntriesByName("tsjs:first-display-paint", "mark")
            .length === 0,
      });
    };

    const nativeCreateElement = Document.prototype.createElement;
    Document.prototype.createElement = function (
      qualifiedName: string,
      options?: ElementCreationOptions,
    ): HTMLElement {
      const normalized = String(qualifiedName).toLowerCase();
      if (normalized === "script" || normalized === "link") {
        record(`create:${normalized}`);
      }
      return nativeCreateElement.call(this, qualifiedName, options);
    };

    const nativeFetch = window.fetch;
    window.fetch = ((...arguments_: Parameters<typeof window.fetch>) => {
      record("fetch");
      return Reflect.apply(nativeFetch, window, arguments_);
    }) as typeof window.fetch;

    const nativeXhrOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (...arguments_: unknown[]): void {
      record("xhr");
      Reflect.apply(nativeXhrOpen, this, arguments_);
    } as typeof XMLHttpRequest.prototype.open;

    const wrapConstructor = (name: "Worker" | "SharedWorker"): void => {
      const nativeConstructor = Reflect.get(window, name);
      if (typeof nativeConstructor !== "function") return;
      const wrapped = function (this: unknown, ...arguments_: unknown[]) {
        record(name.toLowerCase());
        return Reflect.construct(nativeConstructor, arguments_, new.target);
      };
      Object.setPrototypeOf(wrapped, nativeConstructor);
      Object.defineProperty(wrapped, "prototype", {
        value: nativeConstructor.prototype,
      });
      Reflect.set(window, name, wrapped);
    };
    wrapConstructor("Worker");
    wrapConstructor("SharedWorker");

    const nativeCreateObjectUrl = URL.createObjectURL;
    URL.createObjectURL = ((object: Blob | MediaSource): string => {
      record("blob-url");
      return Reflect.apply(nativeCreateObjectUrl, URL, [object]) as string;
    }) as typeof URL.createObjectURL;

    Object.defineProperty(window, "__firstDisplayResourceProbe", {
      value: () => events.map((entry) => ({ ...entry })),
    });
  });
}

test.describe("TSJS hard-cutover runtime", () => {
  test("loads the render owner only inside one parser-blocking first-display request before paint", async ({
    page,
  }) => {
    await installGptStub(page);
    await installFirstDisplayResourceProbe(page);
    const firstDisplayUrl = new URL(
      FIRST_DISPLAY_FIXTURE.firstDisplaySrc,
      "https://runtime.test",
    ).toString();
    const runtimeUrl = new URL(
      FIRST_DISPLAY_FIXTURE.runtimeSrc,
      "https://runtime.test",
    ).toString();
    const firstDisplayRequests: string[] = [];
    const allRequests: string[] = [];
    const pageErrors: string[] = [];
    const consoleMessages: string[] = [];
    page.on("request", (request) => allRequests.push(request.url()));
    page.on("pageerror", (error) => pageErrors.push(error.message));
    page.on("console", (message) =>
      consoleMessages.push(`${message.type()}: ${message.text()}`),
    );
    await page.route(firstDisplayUrl, (route) => {
      firstDisplayRequests.push(route.request().url());
      return route.fulfill({
        status: 200,
        contentType: "application/javascript; charset=utf-8",
        headers: { "x-content-type-options": "nosniff" },
        body: FIRST_DISPLAY_FIXTURE.firstDisplayBody,
      });
    });
    await page.route(runtimeUrl, (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/javascript; charset=utf-8",
        headers: { "x-content-type-options": "nosniff" },
        body: FIRST_DISPLAY_FIXTURE.runtimeBody,
      }),
    );
    await page.route("https://runtime.test/first-display-owner", (route) =>
      route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: `<!doctype html><html><head><meta charset="utf-8"><link rel="icon" href="data:,"><script>window.tsjs={que:[]};window.__gptDiagnosticsStub.emitRequestStartOnDisplay();const __TSJS_SERVER_BOOT_TRANSPORT_V1__=${serverBootTransportLiteralV1(FIRST_DISPLAY_FIXTURE.boot, FIRST_DISPLAY_FIXTURE.outline)};${FIRST_DISPLAY_FIXTURE.bootstrapBody}</script></head><body><div id="${FIRST_DISPLAY_SLOT}"></div><script src="${FIRST_DISPLAY_FIXTURE.firstDisplaySrc}" id="trustedserver-js"></script><script>window.__firstDisplayParserObservation={async:document.querySelector("script#trustedserver-js").async,defer:document.querySelector("script#trustedserver-js").defer,firstAction:performance.getEntriesByName("tsjs:first-display","mark").length};window.__gptDiagnosticsStub.emit("slotRenderEnded",${JSON.stringify(FIRST_DISPLAY_SLOT)},{isEmpty:true,responseIdentifier:"fictional-empty-gam"});</script></body></html>`,
      }),
    );

    await page.goto("https://runtime.test/first-display-owner");
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            performance.getEntriesByName("tsjs:first-display-paint", "mark")
              .length,
        ),
      )
      .toBe(1);
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
      .toMatch(/^(?:kernel|fallback)$/u);
    const runtimeDiagnostic = await page.evaluate(() => ({
      internal: (
        window as unknown as {
          tsjs?: { _internal?: unknown };
        }
      ).tsjs?._internal,
      marks: performance
        .getEntriesByType("mark")
        .map((entry) => ({ name: entry.name, startTime: entry.startTime })),
      frames: [...document.querySelectorAll("iframe")].map((frame) => ({
        connected: frame.isConnected,
        parent: frame.parentElement?.id ?? null,
        title: frame.title,
      })),
    }));
    expect(
      runtimeDiagnostic.internal,
      JSON.stringify({
        ...runtimeDiagnostic,
        allRequests,
        consoleMessages,
        firstDisplayRequests,
        pageErrors,
      }),
    ).toMatchObject({ state: "kernel" });

    const observation = await page.evaluate((slotId) => {
      const browserWindow = window as unknown as {
        __firstDisplayParserObservation: {
          async: boolean;
          defer: boolean;
          firstAction: number;
        };
        __firstDisplayResourceProbe(): Array<{
          kind: string;
          beforePaint: boolean;
        }>;
      };
      return {
        parser: browserWindow.__firstDisplayParserObservation,
        loaderCallsBeforePaint: browserWindow
          .__firstDisplayResourceProbe()
          .filter((entry) => entry.beforePaint)
          .map((entry) => entry.kind),
        loaderCallsAfterPaint: browserWindow
          .__firstDisplayResourceProbe()
          .filter((entry) => !entry.beforePaint)
          .map((entry) => entry.kind),
        firstDisplayScripts: document.querySelectorAll(
          "script#trustedserver-js",
        ).length,
        creativeFrames: document.querySelectorAll(
          `#${slotId} > iframe[title="Ad content"]`,
        ).length,
      };
    }, FIRST_DISPLAY_SLOT);

    expect(firstDisplayRequests).toEqual([firstDisplayUrl]);
    expect(allRequests).toEqual([
      "https://runtime.test/first-display-owner",
      firstDisplayUrl,
      runtimeUrl,
    ]);
    expect(
      allRequests.filter((url) => /tsjs-render_owner_initial/u.test(url)),
    ).toEqual([]);
    expect(observation.parser).toEqual({
      async: false,
      defer: false,
      firstAction: 1,
    });
    expect(observation.loaderCallsBeforePaint).toEqual([]);
    expect(observation.loaderCallsAfterPaint).toEqual(["create:script"]);
    expect(observation.firstDisplayScripts).toBe(1);
    expect(observation.creativeFrames).toBe(1);
  });

  test("generated bootstrap transfers one direct-runtime watchdog to the persistent owner", async ({
    page,
  }) => {
    const runtimeRequests: string[] = [];
    const runtimeUrl = new URL(
      KERNEL_FIXTURE.runtimeSrc,
      "https://runtime.test",
    ).toString();
    await page.route(runtimeUrl, (route) => {
      runtimeRequests.push(route.request().url());
      return route.fulfill({
        status: 200,
        contentType: "application/javascript; charset=utf-8",
        headers: { "x-content-type-options": "nosniff" },
        body: `queueMicrotask(()=>window.__runtimeOrder.push("publisher-microtask"));${KERNEL_FIXTURE.runtimeBody}`,
      });
    });
    await page.route("https://runtime.test/direct", (route) =>
      route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: `<!doctype html><head><script>window.__runtimeOrder=[];window.tsjs={que:[()=>window.__runtimeOrder.push("kernel-commit")]};const __TSJS_SERVER_BOOT_TRANSPORT_V1__=${serverBootTransportLiteralV1(boot(KERNEL_FIXTURE))};${KERNEL_FIXTURE.bootstrapBody}</script><script src="${KERNEL_FIXTURE.runtimeSrc}" id="trustedserver-js"></script><script>window.__runtimeOrder.push("publisher-parser:"+window.tsjs?._internal?.state)</script></head><body></body>`,
      }),
    );

    await page.goto("https://runtime.test/direct");
    await waitForRuntime(page, "kernel");

    const observation = await page.evaluate(() => ({
      claimPresent: Object.prototype.hasOwnProperty.call(
        (window as unknown as { tsjs: object }).tsjs,
        "_claimDirectRuntime",
      ),
      runtimeScripts: document.querySelectorAll("script#trustedserver-js")
        .length,
      bidsMarks: performance.getEntriesByName("tsjs:bids-script", "mark")
        .length,
      order: (
        window as unknown as { __runtimeOrder: string[] }
      ).__runtimeOrder.slice(),
      state: (window as unknown as { tsjs: { _internal: { state: string } } })
        .tsjs._internal.state,
    }));

    expect(runtimeRequests).toEqual([runtimeUrl]);
    expect(observation).toEqual({
      claimPresent: false,
      runtimeScripts: 1,
      bidsMarks: 1,
      order: [
        "kernel-commit",
        "publisher-microtask",
        "publisher-parser:kernel",
      ],
      state: "kernel",
    });
  });

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
        bids: { legacy: true },
        renderAdUnit() {},
        renderAllAdUnits() {},
        setConfig() {},
        getConfig() {},
      };
    }, boot(KERNEL_FIXTURE));

    await loadRuntimeTsjsFixture(page, KERNEL_FIXTURE);
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
    expect(state.releaseId).toBe(KERNEL_FIXTURE.releaseId);
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
      };
    }, boot(FALLBACK_FIXTURE));

    await loadRuntimeTsjsFixture(page, FALLBACK_FIXTURE);
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

    await page.addScriptTag({
      content: "window.tsjs._registerIntegration({});",
    });
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
    expect(after.scripts).toBe(3);
    expect(after.hasGoogletag).toBe(false);
  });
});

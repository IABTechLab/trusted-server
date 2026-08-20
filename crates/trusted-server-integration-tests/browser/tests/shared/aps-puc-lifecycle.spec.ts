import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { installGptStub } from "../../helpers/gpt-stub.js";
import { runtimeUrl } from "../../helpers/state.js";
import {
  loadRuntimeTsjsFixture,
  runtimeTsjsFixture,
} from "../../helpers/tsjs-fixture.js";

const GPT_FIXTURE = runtimeTsjsFixture(["render_runtime", "aps", "gpt"]);

const SLOT = "puc-lifecycle-slot";
const RESERVATION_ID = "r1_AAAAAAAAAAAAAAAAAAAAAA";
const APS_RENDERER_URL = "https://puc.test/integrations/aps/renderer/v2";
const APS_RUNNER_URL = "https://puc.test/integrations/aps/runner.js";
const FICTIONAL_APS_RUNNER = readFileSync(
  resolve(__dirname, "../../fixtures/fictional-aps-runner.js"),
  "utf8",
);

function apsDescriptor() {
  const bid = {
    id: "duplicate-puc-top-mount-bid",
    price: 1.25,
    w: 300,
    h: 250,
    ext: {
      creativeurl: "https://creative.example/render",
      tagtype: "iframe",
    },
  };
  return {
    type: "aps",
    version: 1,
    accountId: "fictional-account",
    bidId: bid.id,
    creativeId: "fictional-creative",
    tagType: bid.ext.tagtype,
    creativeUrl: bid.ext.creativeurl,
    width: bid.w,
    height: bid.h,
    aaxResponse: Buffer.from(
      JSON.stringify({ seatbid: [{ bid: [bid] }] }),
      "utf8",
    ).toString("base64"),
  };
}

function boot() {
  return {
    abi: 1,
    releaseId: GPT_FIXTURE.releaseId,
    manifest: GPT_FIXTURE.manifest,
    auctionProjection: {
      version: 1,
      auction: {
        version: 1,
        auctionId: "puc-browser-lifecycle",
        results: [
          { slot: SLOT, outcome: "winner", candidateId: "AAAAAAAAAAAA" },
        ],
      },
      slots: [
        {
          slot: SLOT,
          gamUnitPath: `/123/${SLOT}`,
          divId: SLOT,
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: [
        {
          candidateId: "AAAAAAAAAAAA",
          slot: SLOT,
          provider: "fictional",
          upstreamBidId: "puc-browser-bid",
          cpm: 1.25,
          currency: "USD",
          targeting: { hb_bidder: "fictional" },
          rendererReservationId: RESERVATION_ID,
          renderSource: apsDescriptor(),
        },
      ],
    },
    integrations: {
      version: 1,
      entries: [
        { id: "aps", config: {} },
        {
          id: "gpt",
          config: {
            gamAttributionEnabled: false,
            pageBidsEnabled: true,
          },
        },
      ],
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

async function openLifecyclePage(
  page: Page,
  nonemptyCompletionOnDisplay = false,
): Promise<void> {
  await installGptStub(page);
  const rendererResponse = await page.request.get(
    runtimeUrl("/integrations/aps/renderer/v2"),
  );
  expect(rendererResponse.status()).toBe(200);
  const rendererDocument = await rendererResponse.text();
  await page.route(APS_RENDERER_URL, (route) =>
    route.fulfill({
      status: 200,
      body: rendererDocument,
      headers: {
        "content-type": "text/html; charset=utf-8",
        "content-security-policy":
          rendererResponse.headers()["content-security-policy"] ?? "",
        "referrer-policy": "no-referrer",
        "x-content-type-options": "nosniff",
      },
    }),
  );
  await page.route(APS_RUNNER_URL, (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/javascript",
      body: FICTIONAL_APS_RUNNER,
    }),
  );
  await page.route("https://puc.test/fixture", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/html",
      body: `<!doctype html><div id="${SLOT}"></div>`,
    }),
  );
  await page.goto("https://puc.test/fixture");
  await page.evaluate(
    ({ initialBoot, completeOnDisplay }) => {
      const browserWindow = window as unknown as {
        tsjs: Record<string, unknown>;
        __gptDiagnosticsStub: {
          emitNonemptyCompletionOnDisplay(): void;
          emitRequestStartOnDisplay(): void;
        };
      };
      browserWindow.__gptDiagnosticsStub.emitRequestStartOnDisplay();
      if (completeOnDisplay) {
        browserWindow.__gptDiagnosticsStub.emitNonemptyCompletionOnDisplay();
      }
      browserWindow.tsjs = {
        boot: initialBoot,
        que: [],
      };
    },
    {
      initialBoot: boot(),
      completeOnDisplay: nonemptyCompletionOnDisplay,
    },
  );
  await loadRuntimeTsjsFixture(page, GPT_FIXTURE);
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
    .toBe("kernel");
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as {
            __gptDiagnosticsStub: { displayCount(): number };
          }
        ).__gptDiagnosticsStub.displayCount(),
      ),
    )
    .toBe(1);
}

async function emitNonemptyGam(page: Page): Promise<void> {
  await page.evaluate((slot) => {
    const stub = (
      window as unknown as {
        __gptDiagnosticsStub: {
          emit(
            name: string,
            slotId: string,
            facts?: Record<string, unknown>,
          ): void;
        };
      }
    ).__gptDiagnosticsStub;
    stub.emit("slotRenderEnded", slot, {
      isEmpty: false,
      responseIdentifier: "fictional-response-1",
    });
  }, SLOT);
}

async function startFictionalPuc(page: Page): Promise<void> {
  await page.evaluate(
    ({ slot, reservationId }) =>
      (
        window as unknown as {
          __gptDiagnosticsStub: {
            renderUniversalCreative(slotId: string, adId: string): void;
          };
        }
      ).__gptDiagnosticsStub.renderUniversalCreative(slot, reservationId),
    { slot: SLOT, reservationId: RESERVATION_ID },
  );
}

async function expectAcceptedLifecycle(page: Page): Promise<void> {
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as {
            __gptDiagnosticsStub: {
              universalCreativeSnapshot(): Readonly<Record<string, unknown>>;
            };
          }
        ).__gptDiagnosticsStub.universalCreativeSnapshot(),
      ),
    )
    .toEqual({
      lifecycle: ["accepted"],
      pucFrames: 1,
      ownerFrames: [0],
      ownerSources: [""],
      emissions: [
        { name: "slotRequested", listeners: 1, physical: true },
        { name: "slotRenderEnded", listeners: 1, physical: true },
      ],
      listeners: expect.objectContaining({
        slotRequested: 1,
        slotRenderEnded: 1,
      }),
      displays: [SLOT],
      refreshes: [],
      slotChildren: [
        { tag: "IFRAME", fictionalPuc: true, title: null, adm: false },
        {
          tag: "IFRAME",
          fictionalPuc: false,
          title: "Ad content",
          adm: false,
        },
      ],
    });

  expect(
    await page.evaluate((slot) => {
      const root = document.getElementById(slot);
      const puc = root?.querySelector<HTMLIFrameElement>(
        "iframe[data-fictional-puc]",
      );
      const creative = root?.querySelector<HTMLIFrameElement>(
        ':scope > iframe[title="Ad content"]',
      );
      return {
        hbAdId: (
          window as unknown as {
            __gptDiagnosticsStub: {
              targeting(slotId: string, key: string): readonly string[];
            };
          }
        ).__gptDiagnosticsStub.targeting(slot, "hb_adid"),
        pucCount: root?.querySelectorAll("iframe[data-fictional-puc]").length,
        ownerCreativeCount: puc?.contentDocument?.querySelectorAll(
          'iframe[title="Ad content"]',
        ).length,
        creativeCount: root?.querySelectorAll(
          ':scope > iframe[title="Ad content"]',
        ).length,
        creativeWidth: creative?.getAttribute("width"),
        creativeHeight: creative?.getAttribute("height"),
        creativeVisibility: creative?.style.visibility,
      };
    }, SLOT),
  ).toEqual({
    hbAdId: [RESERVATION_ID],
    pucCount: 1,
    creativeCount: 1,
    creativeWidth: "300",
    creativeHeight: "250",
    creativeVisibility: "visible",
    ownerCreativeCount: 0,
  });
}

test.describe("APS/PUC lifecycle over the hard-cutover runtime", () => {
  test("joins a PUC claim buffered before attributable GAM completion", async ({
    page,
  }) => {
    await openLifecyclePage(page);
    await startFictionalPuc(page);
    await emitNonemptyGam(page);
    await expectAcceptedLifecycle(page);
  });

  test("joins a PUC claim arriving after attributable GAM completion", async ({
    page,
  }) => {
    await openLifecyclePage(page, true);
    await startFictionalPuc(page);
    await expectAcceptedLifecycle(page);
  });

  test("contains a simultaneous duplicate PUC claim without duplicating the creative", async ({
    page,
  }) => {
    await openLifecyclePage(page);
    await page.evaluate(
      ({ slot, reservationId }) => {
        const stub = (
          window as unknown as {
            __gptDiagnosticsStub: {
              renderUniversalCreative(slotId: string, adId: string): void;
            };
          }
        ).__gptDiagnosticsStub;
        stub.renderUniversalCreative(slot, reservationId);
        stub.renderUniversalCreative(slot, reservationId);
      },
      { slot: SLOT, reservationId: RESERVATION_ID },
    );
    await emitNonemptyGam(page);

    await expect
      .poll(() =>
        page.evaluate(() =>
          (
            window as unknown as {
              __gptDiagnosticsStub: {
                universalCreativeSnapshot(): Readonly<{
                  lifecycle: readonly string[];
                  ownerFrames: readonly number[];
                  pucFrames: number;
                }>;
              };
            }
          ).__gptDiagnosticsStub.universalCreativeSnapshot(),
        ),
      )
      .toMatchObject({
        lifecycle: ["accepted", "failed:fictional PUC response refused"],
        ownerFrames: [0, 0],
        pucFrames: 2,
      });

    expect(
      await page.evaluate((slot) => {
        const root = document.getElementById(slot);
        const pucFrames = Array.from(
          root?.querySelectorAll<HTMLIFrameElement>(
            "iframe[data-fictional-puc]",
          ) ?? [],
        );
        return {
          pucFrames: pucFrames.length,
          ownerCreativeFrames: pucFrames.reduce(
            (total, frame) =>
              total +
              (frame.contentDocument?.querySelectorAll(
                'iframe[title="Ad content"]',
              ).length ?? 0),
            0,
          ),
          topCreativeFrames:
            root?.querySelectorAll(':scope > iframe[title="Ad content"]')
              .length ?? 0,
        };
      }, SLOT),
    ).toEqual({
      pucFrames: 2,
      ownerCreativeFrames: 0,
      topCreativeFrames: 1,
    });
  });

  test("retires the exact APS overlay before a publisher refresh returns", async ({
    page,
  }) => {
    await openLifecyclePage(page);
    await startFictionalPuc(page);
    await emitNonemptyGam(page);
    await expectAcceptedLifecycle(page);

    expect(
      await page.evaluate((slot) => {
        const stub = (
          window as unknown as {
            __gptDiagnosticsStub: {
              publisherRefresh(slotId: string): void;
              universalCreativeSnapshot(): Readonly<{
                lifecycle: readonly string[];
              }>;
            };
          }
        ).__gptDiagnosticsStub;
        stub.publisherRefresh(slot);
        const root = document.getElementById(slot);
        return {
          lifecycle: stub.universalCreativeSnapshot().lifecycle,
          position: root?.style.getPropertyValue("position"),
          pucFrames:
            root?.querySelectorAll("iframe[data-fictional-puc]").length ?? -1,
          topCreativeFrames:
            root?.querySelectorAll(':scope > iframe[title="Ad content"]')
              .length ?? -1,
        };
      }, SLOT),
    ).toEqual({
      lifecycle: ["accepted"],
      position: "",
      pucFrames: 1,
      topCreativeFrames: 0,
    });
  });

  test("keeps the overlay on failed publisher destroy and retires it on success", async ({
    page,
  }) => {
    await openLifecyclePage(page);
    await startFictionalPuc(page);
    await emitNonemptyGam(page);
    await expectAcceptedLifecycle(page);

    expect(
      await page.evaluate((slot) => {
        const stub = (
          window as unknown as {
            __gptDiagnosticsStub: {
              publisherDestroy(slotId: string, succeeds?: boolean): boolean;
            };
          }
        ).__gptDiagnosticsStub;
        const result = stub.publisherDestroy(slot, false);
        const root = document.getElementById(slot);
        return {
          result,
          position: root?.style.getPropertyValue("position"),
          topCreativeFrames:
            root?.querySelectorAll(':scope > iframe[title="Ad content"]')
              .length ?? -1,
        };
      }, SLOT),
    ).toEqual({ result: false, position: "relative", topCreativeFrames: 1 });

    expect(
      await page.evaluate(
        (slot) =>
          (
            window as unknown as {
              __gptDiagnosticsStub: {
                publisherDestroy(slotId: string, succeeds?: boolean): boolean;
              };
            }
          ).__gptDiagnosticsStub.publisherDestroy(slot, true),
        SLOT,
      ),
    ).toBe(true);
    await expect
      .poll(() =>
        page.evaluate((slot) => {
          const root = document.getElementById(slot);
          return {
            position: root?.style.getPropertyValue("position"),
            topCreativeFrames:
              root?.querySelectorAll(':scope > iframe[title="Ad content"]')
                .length ?? -1,
          };
        }, SLOT),
      )
      .toEqual({ position: "", topCreativeFrames: 0 });
  });

  test("retires a moved APS node and restores the exact host style", async ({
    page,
  }) => {
    await openLifecyclePage(page);
    await startFictionalPuc(page);
    await emitNonemptyGam(page);
    await expectAcceptedLifecycle(page);

    await page.evaluate((slot) => {
      const root = document.getElementById(slot);
      const overlay = root?.querySelector<HTMLIFrameElement>(
        ':scope > iframe[title="Ad content"]',
      );
      if (!overlay) throw new Error("missing fictional APS overlay");
      document.body.appendChild(overlay);
    }, SLOT);

    await expect
      .poll(() =>
        page.evaluate(
          (slot) => ({
            position: document
              .getElementById(slot)
              ?.style.getPropertyValue("position"),
            movedOverlays: document.body.querySelectorAll(
              ':scope > iframe[title="Ad content"]',
            ).length,
          }),
          SLOT,
        ),
      )
      .toEqual({ position: "", movedOverlays: 0 });
  });

  test("retires the detached artifact when the publisher replaces its host", async ({
    page,
  }) => {
    await openLifecyclePage(page);
    await startFictionalPuc(page);
    await emitNonemptyGam(page);
    await expectAcceptedLifecycle(page);

    await page.evaluate((slot) => {
      const current = document.getElementById(slot);
      if (!current) throw new Error("missing fictional APS host");
      const replacement = document.createElement("div");
      replacement.id = slot;
      current.replaceWith(replacement);
      (
        window as unknown as {
          __detachedFictionalApsHost?: HTMLElement;
        }
      ).__detachedFictionalApsHost = current;
    }, SLOT);

    await expect
      .poll(() =>
        page.evaluate((slot) => {
          const detached = (
            window as unknown as {
              __detachedFictionalApsHost?: HTMLElement;
            }
          ).__detachedFictionalApsHost;
          const replacement = document.getElementById(slot);
          return {
            detachedPosition: detached?.style.getPropertyValue("position"),
            detachedOverlays:
              detached?.querySelectorAll(':scope > iframe[title="Ad content"]')
                .length ?? -1,
            replacementChildren: replacement?.children.length ?? -1,
            replacementPosition:
              replacement?.style.getPropertyValue("position"),
          };
        }, SLOT),
      )
      .toEqual({
        detachedPosition: "",
        detachedOverlays: 0,
        replacementChildren: 0,
        replacementPosition: "",
      });
  });
});

import { expect, test, type Page } from "@playwright/test";
import { installGptStub } from "../../helpers/gpt-stub.js";
import {
  criticalTsjsFixture,
  loadCriticalTsjsFixture,
} from "../../helpers/tsjs-fixture.js";

const GPT_FIXTURE = criticalTsjsFixture(["render_runtime", "gpt"]);

const SLOT = "puc-lifecycle-slot";
const RESERVATION_ID = "r1_AAAAAAAAAAAAAAAAAAAAAA";

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
          renderSource: {
            type: "adm",
            version: 1,
            adm: '<main id="fictional-creative">fictional creative</main>',
            width: 300,
            height: 250,
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
        _integrationConfig: { gpt: {} },
      };
    },
    {
      initialBoot: boot(),
      completeOnDisplay: nonemptyCompletionOnDisplay,
    },
  );
  await loadCriticalTsjsFixture(page, GPT_FIXTURE);
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
      ownerFrames: [1],
      ownerSources: [expect.stringContaining("fictional creative")],
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
      ],
    });

  expect(
    await page.evaluate((slot) => {
      const root = document.getElementById(slot);
      const puc = root?.querySelector<HTMLIFrameElement>(
        "iframe[data-fictional-puc]",
      );
      const creative = puc?.contentDocument?.querySelector<HTMLIFrameElement>(
        'iframe[title="Ad content"]',
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
        creativeCount: puc?.contentDocument?.querySelectorAll(
          'iframe[title="Ad content"]',
        ).length,
        creativeWidth: creative?.getAttribute("width"),
        creativeHeight: creative?.getAttribute("height"),
        creativeSource: creative?.srcdoc,
      };
    }, SLOT),
  ).toEqual({
    hbAdId: [RESERVATION_ID],
    pucCount: 1,
    creativeCount: 1,
    creativeWidth: "300",
    creativeHeight: "250",
    creativeSource: expect.stringContaining("fictional creative"),
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
        ownerFrames: [1, 0],
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
          creativeFrames: pucFrames.reduce(
            (total, frame) =>
              total +
              (frame.contentDocument?.querySelectorAll(
                'iframe[title="Ad content"]',
              ).length ?? 0),
            0,
          ),
        };
      }, SLOT),
    ).toEqual({ pucFrames: 2, creativeFrames: 1 });
  });
});

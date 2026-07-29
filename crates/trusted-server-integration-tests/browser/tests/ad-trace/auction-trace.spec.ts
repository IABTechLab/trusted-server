import { expect, test, type Page } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

const ORIGIN_PORT = process.env.INTEGRATION_ORIGIN_PORT || "8888";

async function serveBuiltPrebid(page: Page): Promise<void> {
    const response = await fetch(
        `http://127.0.0.1:${ORIGIN_PORT}/prebid-bundle.js`,
    );
    if (!response.ok)
        throw new Error(`fixture Prebid bundle returned ${response.status}`);
    const body = await response.text();
    await page.route("**/integrations/prebid/bundle.js*", (route) =>
        route.fulfill({
            status: 200,
            contentType: "application/javascript",
            body,
        }),
    );
}

async function openTesterPage(page: Page): Promise<void> {
    await serveBuiltPrebid(page);
    await page.goto(runtimeUrl("/?ts_console=1"), {
        waitUntil: "domcontentloaded",
    });
    await expect(page).toHaveURL(runtimeUrl("/"));
    await expect
        .poll(() =>
            page.evaluate(() =>
                (
                    window as Window & {
                        tsjs?: { adTrace?: { export(): unknown } };
                    }
                ).tsjs?.adTrace?.export(),
            ),
        )
        .toBeTruthy();
    await expect
        .poll(() =>
            page.evaluate(() => {
                const result = (
                    window as Window & {
                        tsjs?: {
                            adTrace?: {
                                export(): {
                                    slots: Array<{
                                        slotId: string;
                                        stages: {
                                            creative: { outcome: string };
                                        };
                                    }>;
                                };
                            };
                        };
                    }
                ).tsjs?.adTrace?.export();
                return result?.slots.find(
                    (slot) => slot.slotId === "ad-trace-slot",
                )?.stages.creative.outcome;
            }),
        )
        .toBe("load_acknowledged");
    await expect
        .poll(() =>
            page.evaluate(() =>
                (
                    window as Window & {
                        tsjs?: {
                            adTrace?: {
                                getEvents(): Array<{ kind: string }>;
                            };
                        };
                    }
                ).tsjs?.adTrace
                    ?.getEvents()
                    .some((event) => event.kind === "gpt_slot_render_ended"),
            ),
        )
        .toBe(true);
}

async function exported(page: Page) {
    return page.evaluate(() =>
        (
            window as Window & {
                tsjs: {
                    adTrace: {
                        export(): { slots: Array<Record<string, unknown>> };
                    };
                };
            }
        ).tsjs.adTrace.export(),
    );
}

test.describe("tester-only auction trace contract", () => {
    test("config without an activated console session exposes no browser trace surface", async ({
        page,
    }) => {
        await serveBuiltPrebid(page);
        await page.goto(runtimeUrl("/"), { waitUntil: "domcontentloaded" });

        expect(
            await page.evaluate(
                () =>
                    typeof (window as Window & { tsjs?: { adTrace?: unknown } })
                        .tsjs?.adTrace,
            ),
        ).toBe("undefined");
    });

    test("console session supports true, persists privately, and can be disabled", async ({
        page,
    }) => {
        await serveBuiltPrebid(page);
        const activation = await page.goto(runtimeUrl("/?ts_console=true"), {
            waitUntil: "domcontentloaded",
        });
        await expect(page).toHaveURL(runtimeUrl("/"));
        expect(activation?.headers()["cache-control"]).toBe("private, no-store");
        await expect
            .poll(() =>
                page.evaluate(
                    () =>
                        typeof (
                            window as Window & { tsjs?: { adTrace?: unknown } }
                        ).tsjs?.adTrace,
                ),
            )
            .toBe("object");
        expect(
            (await page.context().cookies()).find(
                (cookie) => cookie.name === "__Host-ts-console",
            ),
        ).toMatchObject({
            value: "1",
            httpOnly: true,
            secure: true,
            sameSite: "Lax",
        });

        await page.reload({ waitUntil: "domcontentloaded" });
        expect(
            await page.evaluate(
                () =>
                    typeof (window as Window & { tsjs?: { adTrace?: unknown } })
                        .tsjs?.adTrace,
            ),
        ).toBe("object");

        await page.goto(runtimeUrl("/?ts_console=0"), {
            waitUntil: "domcontentloaded",
        });
        await expect(page).toHaveURL(runtimeUrl("/"));
        expect(
            await page.evaluate(
                () =>
                    typeof (window as Window & { tsjs?: { adTrace?: unknown } })
                        .tsjs?.adTrace,
            ),
        ).toBe("undefined");

        await page.reload({ waitUntil: "domcontentloaded" });
        expect(
            await page.evaluate(
                () =>
                    typeof (window as Window & { tsjs?: { adTrace?: unknown } })
                        .tsjs?.adTrace,
            ),
        ).toBe("undefined");
    });

    test("initial TS winner reaches direct GPT and source-validated creative acknowledgement", async ({
        page,
    }) => {
        await openTesterPage(page);

        await expect
            .poll(async () => {
                const result = await exported(page);
                const slot = result.slots.find(
                    (item) => item.slotId === "ad-trace-slot",
                ) as
                    | {
                          stages?: Record<
                              string,
                              { outcome?: string; confidence?: string }
                          >;
                      }
                    | undefined;
                return {
                    trustedServer: slot?.stages?.trustedServer?.outcome,
                    prebid: slot?.stages?.prebid?.outcome,
                    gam: slot?.stages?.gam?.outcome,
                    creative: slot?.stages?.creative?.outcome,
                };
            })
            .toEqual({
                trustedServer: "won",
                prebid: "not_run",
                gam: "trusted_server_won",
                creative: "load_acknowledged",
            });

        const session = await page.context().newCDPSession(page);
        const tree = (await session.send("Accessibility.getFullAXTree")) as {
            nodes: Array<{ name?: { value?: string } }>;
        };
        const visibleText = tree.nodes
            .map((node) => node.name?.value || "")
            .join("\n");
        expect(visibleText).toContain("TS winner: won · definitive");
        expect(visibleText).toContain(
            "Creative: load_acknowledged · definitive",
        );
    });

    test("direct auction API render reaches an exact iframe-load acknowledgement", async ({
        page,
    }) => {
        await openTesterPage(page);
        await page.evaluate(() => {
            const direct = document.createElement("div");
            direct.id = "direct-api-slot";
            document.body.appendChild(direct);
            const ts = (window as Window & {
                tsjs: {
                    addAdUnits(unit: unknown): void;
                    requestAds(): void;
                };
            }).tsjs;
            ts.addAdUnits({
                code: "direct-api-slot",
                mediaTypes: { banner: { sizes: [[300, 250]] } },
                bids: [{ bidder: "example", params: {} }],
            });
            ts.requestAds();
        });

        await expect
            .poll(() =>
                page.evaluate(() => {
                    const result = (
                        window as Window & {
                            tsjs: {
                                adTrace: {
                                    export(): {
                                        renders: Array<{
                                            slotId: string;
                                            source: string;
                                            outcome: string;
                                        }>;
                                    };
                                };
                            };
                        }
                    ).tsjs.adTrace.export();
                    return result.renders.find(
                        (render) => render.slotId === "direct-api-slot",
                    );
                }),
            )
            .toMatchObject({
                slotId: "direct-api-slot",
                source: "direct_auction",
                outcome: "confirmed",
            });
    });

    test("actual generated Prebid selects the traced TS bid before a probable GAM result", async ({
        page,
    }) => {
        await openTesterPage(page);
        await expect
            .poll(() =>
                page.evaluate(() => {
                    const win = window as Window & {
                        pbjs?: { requestBids?: unknown };
                        googletag?: {
                            pubads(): { __tsRefreshWrapped?: boolean };
                        };
                    };
                    return (
                        typeof win.pbjs?.requestBids === "function" &&
                        win.googletag?.pubads().__tsRefreshWrapped === true
                    );
                }),
            )
            .toBe(true);
        await page.evaluate(() => {
            const win = window as Window & {
                adTraceFixture: {
                    latestSlot(): unknown;
                    setSuppressCreative(value: boolean): void;
                };
                googletag: { pubads(): { refresh(slots: unknown[]): void } };
            };
            win.adTraceFixture.setSuppressCreative(true);
            win.googletag.pubads().refresh([win.adTraceFixture.latestSlot()]);
        });
        await expect
            .poll(async () => {
                const result = await exported(page);
                const slot = result.slots.find(
                    (item) => item.slotId === "ad-trace-slot",
                ) as
                    | {
                          stages?: Record<
                              string,
                              { outcome?: string; confidence?: string }
                          >;
                      }
                    | undefined;
                return {
                    prebid: slot?.stages?.prebid,
                    gam: slot?.stages?.gam,
                };
            })
            .toMatchObject({
                prebid: { outcome: "won", confidence: "definitive" },
                gam: {
                    outcome: "trusted_server_candidate",
                    confidence: "probable",
                },
            });
    });

    test("client selection, backfill, direct-or-unattributed, and retained generations stay independent", async ({
        page,
    }) => {
        await openTesterPage(page);

        await page.evaluate(() => {
            const win = window as Window & {
                adTraceFixture: { simulateClientSelection(): void };
            };
            win.adTraceFixture.simulateClientSelection();
        });
        await expect
            .poll(async () => {
                const result = await exported(page);
                const slot = result.slots.find(
                    (item) => item.slotId === "ad-trace-slot",
                ) as
                    | { stages?: Record<string, { outcome?: string }> }
                    | undefined;
                return {
                    prebid: slot?.stages?.prebid?.outcome,
                    gam: slot?.stages?.gam?.outcome,
                };
            })
            .toEqual({ prebid: "lost", gam: "client_prebid_candidate" });

        await page.evaluate(() => {
            const win = window as Window & {
                adTraceFixture: {
                    latestSlot(): unknown;
                    setNextRender(flags: { isBackfill: boolean }): void;
                    requestCurrent(): void;
                };
                tsjs: {
                    captureAdTraceRequest(slot: unknown, trigger: string): void;
                };
            };
            const slot = win.adTraceFixture.latestSlot();
            win.adTraceFixture.setNextRender({ isBackfill: true });
            win.tsjs.captureAdTraceRequest(slot, "fixture_backfill");
            win.adTraceFixture.requestCurrent();
        });
        await expect
            .poll(async () => {
                const result = await exported(page);
                const slot = result.slots.find(
                    (item) => item.slotId === "ad-trace-slot",
                ) as
                    | { stages?: Record<string, { outcome?: string }> }
                    | undefined;
                return slot?.stages?.gam?.outcome;
            })
            .toBe("backfill");

        await page.evaluate(() => {
            const win = window as Window & {
                adTraceFixture: {
                    latestSlot(): { clearTargeting(): void };
                    requestCurrent(): void;
                };
                tsjs: {
                    captureAdTraceRequest(slot: unknown, trigger: string): void;
                };
            };
            const slot = win.adTraceFixture.latestSlot();
            slot.clearTargeting();
            win.tsjs.captureAdTraceRequest(slot, "fixture_direct");
            win.adTraceFixture.requestCurrent();
        });
        await expect
            .poll(async () => {
                const result = await exported(page);
                const slot = result.slots.find(
                    (item) => item.slotId === "ad-trace-slot",
                ) as
                    | { stages?: Record<string, { outcome?: string }> }
                    | undefined;
                return slot?.stages?.gam?.outcome;
            })
            .toBe("direct_or_unattributed");

        const generations = await page.evaluate(() => {
            const win = window as Window & {
                adTraceFixture: {
                    simulateRetainedGenerationAcknowledgement(): unknown;
                };
            };
            return win.adTraceFixture.simulateRetainedGenerationAcknowledgement();
        });
        const result = await exported(page);
        const slot = result.slots.find(
            (item) => item.slotId === "ad-trace-slot",
        ) as {
            latestGeneration: number;
            generations: Array<{
                generation: number;
                stages: { creative: { outcome: string } };
            }>;
        };
        const retained = generations as { first: number; second: number };
        expect(slot.latestGeneration).toBe(retained.second);
        expect(
            slot.generations.find((item) => item.generation === retained.first)
                ?.stages.creative.outcome,
        ).toBe("load_acknowledged");
        expect(
            slot.generations.find((item) => item.generation === retained.second)
                ?.stages.creative.outcome,
        ).not.toBe("load_acknowledged");
    });
});

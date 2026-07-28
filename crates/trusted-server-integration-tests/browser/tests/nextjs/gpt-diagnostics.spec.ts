import { expect, test, type Page } from "@playwright/test";
import { installGptStub } from "../../helpers/gpt-stub.js";
import { readState, runtimeUrl } from "../../helpers/state.js";

const HOST_ID = "trusted-server-gpt-diagnostics";
const EVENT_NAMES = [
    "slotRequested",
    "slotResponseReceived",
    "slotRenderEnded",
    "slotOnload",
    "impressionViewable",
    "slotVisibilityChanged",
] as const;

test.beforeEach(async ({ page }, testInfo) => {
    const state = readState();
    if (state.framework !== "nextjs") testInfo.skip();
    await installGptStub(page);
});

async function waitForApi(page: Page): Promise<void> {
    await page.waitForFunction(() =>
        Boolean((window as any).tsjs?.gptDiagnostics),
    );
}

async function emit(
    page: Page,
    name: string,
    slotId: string,
    facts: Record<string, unknown> = {},
): Promise<void> {
    await page.evaluate(
        ({ eventName, id, eventFacts }) => {
            (window as any).__gptDiagnosticsStub.emit(
                eventName,
                id,
                eventFacts,
            );
        },
        { eventName: name, id: slotId, eventFacts: facts },
    );
}

test.describe("GPT runtime diagnostics", () => {
    test("is completely inactive without a tab directive", async ({ page }) => {
        const diagnosticNetworkRequests: string[] = [];
        page.on("request", (request) => {
            if (
                ["fetch", "xhr", "beacon"].includes(request.resourceType()) &&
                /diagnostic|trace/i.test(request.url())
            ) {
                diagnosticNetworkRequests.push(request.url());
            }
        });

        await page.goto(runtimeUrl("/gpt-diagnostics"), { waitUntil: "load" });

        expect(
            await page.evaluate(() => ({
                api: Boolean((window as any).tsjs?.gptDiagnostics),
                host: Boolean(
                    document.getElementById("trusted-server-gpt-diagnostics"),
                ),
                listenerCounts: (
                    window as any
                ).__gptDiagnosticsStub.listenerCounts(),
            })),
        ).toEqual({ api: false, host: false, listenerCounts: {} });
        expect(diagnosticNetworkRequests).toEqual([]);
    });

    test("activates, cleans the URL, persists in the tab, and deactivates", async ({
        browser,
        page,
    }) => {
        await page.goto(
            runtimeUrl(
                "/gpt-diagnostics?unrelated=kept&ts_console=true#fixture",
            ),
            { waitUntil: "load" },
        );
        await waitForApi(page);

        expect(new URL(page.url()).search).toBe("?unrelated=kept");
        expect(new URL(page.url()).hash).toBe("#fixture");
        const listenerCounts = await page.evaluate(() =>
            (window as any).__gptDiagnosticsStub.listenerCounts(),
        );
        for (const eventName of EVENT_NAMES)
            expect(listenerCounts[eventName]).toBe(1);

        await page.getByRole("link", { name: "Home" }).click();
        await page.waitForURL("**/");
        expect(
            await page.evaluate(() =>
                Boolean((window as any).tsjs?.gptDiagnostics),
            ),
        ).toBe(true);
        await page.goBack();
        await page.waitForURL("**/gpt-diagnostics?unrelated=kept#fixture");
        await waitForApi(page);

        await page.goto(
            runtimeUrl("/gpt-diagnostics?unrelated=second#persisted"),
            {
                waitUntil: "load",
            },
        );
        await waitForApi(page);
        expect(new URL(page.url()).search).toBe("?unrelated=second");
        expect(new URL(page.url()).hash).toBe("#persisted");

        await page.goto(
            runtimeUrl(
                "/gpt-diagnostics?unrelated=kept&ts_console=false#disabled",
            ),
            { waitUntil: "load" },
        );
        expect(new URL(page.url()).search).toBe("?unrelated=kept");
        expect(new URL(page.url()).hash).toBe("#disabled");
        expect(
            await page.evaluate(() => ({
                api: Boolean((window as any).tsjs?.gptDiagnostics),
                host: Boolean(
                    document.getElementById("trusted-server-gpt-diagnostics"),
                ),
                listeners: (
                    window as any
                ).__gptDiagnosticsStub.listenerCounts(),
            })),
        ).toEqual({ api: false, host: false, listeners: {} });

        const separateContext = await browser.newContext();
        const separatePage = await separateContext.newPage();
        await installGptStub(separatePage);
        await separatePage.goto(runtimeUrl("/gpt-diagnostics"), {
            waitUntil: "load",
        });
        expect(
            await separatePage.evaluate(() =>
                Boolean((window as any).tsjs?.gptDiagnostics),
            ),
        ).toBe(false);
        await separatePage.waitForLoadState("networkidle");
        await separateContext.close();
    });

    test("captures lifecycle truth, conservative overlap, binding changes, remount, and export", async ({
        page,
    }, testInfo) => {
        const pageErrors: string[] = [];
        const diagnosticNetworkRequests: string[] = [];
        page.on("pageerror", (error) => pageErrors.push(error.message));
        page.on("request", (request) => {
            if (
                ["fetch", "xhr", "beacon"].includes(request.resourceType()) &&
                /diagnostic|trace/i.test(request.url())
            ) {
                diagnosticNetworkRequests.push(request.url());
            }
        });

        await page.goto(runtimeUrl("/gpt-diagnostics?ts_console=1"), {
            waitUntil: "load",
        });
        await waitForApi(page);
        await page.evaluate(() =>
            (window as any).__gptDiagnosticsStub.captureReferences(),
        );
        await expect(page.locator(`#${HOST_ID}`)).toHaveCount(1);
        expect(
            await page
                .locator(`#${HOST_ID}`)
                .evaluate((host) => host.shadowRoot === null),
        ).toBe(true);

        await emit(page, "slotRequested", "gpt-diagnostics-slot-primary");
        await emit(
            page,
            "slotResponseReceived",
            "gpt-diagnostics-slot-primary",
        );
        await emit(page, "slotRenderEnded", "gpt-diagnostics-slot-primary", {
            isEmpty: false,
            size: [300, 250],
            isBackfill: true,
            slotContentChanged: true,
        });
        await emit(page, "slotOnload", "gpt-diagnostics-slot-primary");
        await emit(page, "impressionViewable", "gpt-diagnostics-slot-primary");
        await emit(
            page,
            "slotVisibilityChanged",
            "gpt-diagnostics-slot-primary",
            {
                inViewPercentage: 80,
            },
        );
        await emit(page, "slotRequested", "gpt-diagnostics-slot-secondary");
        await emit(
            page,
            "slotResponseReceived",
            "gpt-diagnostics-slot-secondary",
        );
        await emit(page, "slotRenderEnded", "gpt-diagnostics-slot-secondary", {
            isEmpty: true,
        });
        await emit(page, "slotRequested", "gpt-diagnostics-slot-primary");
        await emit(page, "slotRequested", "gpt-diagnostics-slot-primary");
        await emit(
            page,
            "slotResponseReceived",
            "gpt-diagnostics-slot-primary",
        );

        await page.waitForFunction(() => {
            const snapshot = (window as any).tsjs.gptDiagnostics.snapshot();
            return (
                snapshot.slots.length === 2 &&
                snapshot.callbackIssues.length > 0
            );
        });
        const snapshot = await page.evaluate(() =>
            (window as any).tsjs.gptDiagnostics.snapshot(),
        );
        expect(snapshot.slots).toHaveLength(2);
        const primary = snapshot.slots.find(
            (slot: any) =>
                slot.slotElementId === "gpt-diagnostics-slot-primary",
        );
        const secondary = snapshot.slots.find(
            (slot: any) =>
                slot.slotElementId === "gpt-diagnostics-slot-secondary",
        );
        expect(primary.binding).toEqual({ status: "bound" });
        expect(
            primary.requests.map((cycle: any) => cycle.requestNumber),
        ).toEqual([1, 2, 3]);
        expect(primary.requests[0]).toMatchObject({
            isEmpty: false,
            size: [300, 250],
            isBackfill: true,
            slotContentChanged: true,
        });
        expect(
            primary.requests[0].durations.requestToResponseMs,
        ).toBeGreaterThanOrEqual(0);
        expect(
            primary.requests[0].durations.responseToRenderMs,
        ).toBeGreaterThanOrEqual(0);
        expect(secondary.requests[0].isEmpty).toBe(true);
        expect(snapshot.callbackIssues).toContainEqual(
            expect.objectContaining({
                kind: "slotResponseReceived",
                disposition: "ambiguous",
                reason: "overlapping_request_cycles",
            }),
        );
        for (const counters of Object.values(snapshot.coverage) as any[]) {
            expect(counters.observed).toBe(
                counters.matched + counters.unmatched + counters.ambiguous,
            );
        }

        await page.getByRole("button", { name: "Toggle duplicate ID" }).click();
        await page.waitForFunction(
            () =>
                (window as any).tsjs.gptDiagnostics.snapshot().slots[0].binding
                    .status === "ambiguous",
        );
        expect(
            await page.evaluate(
                () =>
                    (window as any).tsjs.gptDiagnostics.snapshot().slots[0]
                        .binding,
            ),
        ).toEqual({ status: "ambiguous", reason: "duplicate_dom_id" });
        await page.getByRole("button", { name: "Toggle duplicate ID" }).click();
        await page
            .getByRole("button", { name: "Replace primary slot" })
            .click();
        await page.waitForFunction(
            () =>
                (window as any).tsjs.gptDiagnostics.snapshot().slots[0].binding
                    .status === "bound",
        );

        await page
            .getByRole("button", { name: "Remove diagnostics host" })
            .click();
        await expect(page.locator(`#${HOST_ID}`)).toHaveCount(1);

        await page.evaluate(() => (window as any).tsjs.gptDiagnostics.hide());
        await expect(page.locator(`#${HOST_ID}`)).toHaveCount(0);
        await emit(page, "slotRequested", "gpt-diagnostics-slot-secondary");
        await page.evaluate(() => (window as any).tsjs.gptDiagnostics.show());
        await expect(page.locator(`#${HOST_ID}`)).toHaveCount(1);
        const hiddenPeriodSnapshot = await page.evaluate(() =>
            (window as any).tsjs.gptDiagnostics.snapshot(),
        );
        expect(
            hiddenPeriodSnapshot.slots.find(
                (slot: any) =>
                    slot.slotElementId === "gpt-diagnostics-slot-secondary",
            ).requests,
        ).toHaveLength(2);

        const downloadPromise = page.waitForEvent("download");
        await page.evaluate(() => (window as any).tsjs.gptDiagnostics.export());
        const download = await downloadPromise;
        expect(download.suggestedFilename()).toMatch(
            /^trusted-server-gpt-diagnostics-.*\.json$/,
        );
        const stream = await download.createReadStream();
        const chunks: Buffer[] = [];
        for await (const chunk of stream) chunks.push(Buffer.from(chunk));
        const exported = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        expect(exported.version).toBe(1);
        expect(exported.slots).toHaveLength(hiddenPeriodSnapshot.slots.length);
        expect(exported.callbackIssues).toHaveLength(
            hiddenPeriodSnapshot.callbackIssues.length,
        );
        expect(exported.page).toEqual({
            origin: new URL(page.url()).origin,
            pathname: "/gpt-diagnostics",
        });
        expect(JSON.stringify(exported)).not.toMatch(
            /bidder|targeting|creativeMarkup|cookie|userId|auction/i,
        );

        expect(
            await page.evaluate(() =>
                (window as any).__gptDiagnosticsStub.referencesUnchanged(),
            ),
        ).toBe(true);
        expect(
            await page
                .locator("#gpt-diagnostics-slot-primary")
                .evaluate((element) => ({
                    className: element.className,
                    diagnosticAttributes: element
                        .getAttributeNames()
                        .filter((name) => /diagnostic|tsjs/i.test(name)),
                })),
        ).toEqual({ className: "", diagnosticAttributes: [] });
        const badgeEvidence = await page.screenshot();
        expect(badgeEvidence.byteLength).toBeGreaterThan(0);
        await testInfo.attach("gpt-diagnostics-badge-evidence", {
            body: badgeEvidence,
            contentType: "image/png",
        });
        expect(diagnosticNetworkRequests).toEqual([]);
        expect(pageErrors).toEqual([]);
    });
});

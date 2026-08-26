import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

const RUNNER_URL = "https://client.aps.amazon-adsystem.com/prebid-creative.js";
const PUBLISHER_CORE_URL = "https://tsjs.example/trusted-server-core.js";
const IFRAME_CREATIVE_URL = "https://creative.example/iframe";
const SCRIPT_CREATIVE_URL = "https://creative.example/script.js";
const SANDBOX =
    "allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation";
const TSJS_CRATE = resolve(__dirname, "../../../../trusted-server-js");
const PUC_BANNER = readFileSync(
    resolve(
        __dirname,
        "../../node_modules/prebid-universal-creative/dist/banner.js",
    ),
    "utf8",
);

function clientAuctionBundlePaths() {
    const manifestPath = resolve(TSJS_CRATE, "dist/prebid/manifest.json");
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as {
        filename: string;
    };
    return {
        core: resolve(TSJS_CRATE, "dist/tsjs-core.js"),
        gpt: resolve(TSJS_CRATE, "dist/tsjs-gpt.js"),
        prebid: resolve(TSJS_CRATE, "dist/prebid", manifest.filename),
        prebidShim: resolve(TSJS_CRATE, "dist/tsjs-prebid.js"),
    };
}

/**
 * Load the client auction scripts in the order the server serves them.
 *
 * A coupled external bundle ships Prebid.js with the tsjs shim (and the
 * trustedServer adapter) already installed. A decoupled external bundle is
 * pure Prebid.js and stamps its manifest on `window.__tsjs_prebid_bundle`;
 * the shim then arrives as the separately served deferred tsjs module, so
 * this helper loads it whenever the stamp is present.
 */
async function loadClientAuctionBundles(page: Page): Promise<void> {
    const bundles = clientAuctionBundlePaths();
    await page.addScriptTag({ path: bundles.core });
    await page.addScriptTag({ path: bundles.gpt });
    await page.addScriptTag({ path: bundles.prebid });
    const decoupledBundle = await page.evaluate(() =>
        Boolean(
            (window as unknown as { __tsjs_prebid_bundle?: unknown })
                .__tsjs_prebid_bundle,
        ),
    );
    if (decoupledBundle) {
        await page.addScriptTag({ path: bundles.prebidShim });
    }
}

function descriptor(
    tagType: "iframe" | "script",
    creativeUrl = tagType === "iframe"
        ? IFRAME_CREATIVE_URL
        : SCRIPT_CREATIVE_URL,
) {
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

function testPage(rendererUrl: string) {
    return `<!doctype html>
<meta charset="utf-8">
<div id="slots"></div>
<script>
window.apsMessages = [];
window.startApsFrame = function(options) {
  var slot = document.createElement('div');
  slot.id = options.slotId;
  slot.innerHTML = '<span class="existing">existing publisher content</span>';
  document.getElementById('slots').appendChild(slot);

  var frame = document.createElement('iframe');
  frame.width = '300';
  frame.height = '250';
  frame.style.display = 'none';
  if (!options.omitSandbox) {
    frame.setAttribute('sandbox', ${JSON.stringify(SANDBOX)});
  }
  frame.src = ${JSON.stringify(rendererUrl)} +
    (options.includeFragment === false ? '' : '#tsaps=' + options.fragmentNonce);

  function receive(event) {
    if (event.source !== frame.contentWindow || !event.data) return;
    window.apsMessages.push({ slotId: options.slotId, data: event.data });
    if (event.data.message === 'trusted-server/aps/renderer-ready' &&
        event.data.nonce === options.fragmentNonce) {
      var existing = slot.querySelector('.existing');
      if (existing) existing.remove();
      frame.style.display = '';
    }
  }
  window.addEventListener('message', receive);
  frame.onload = function() {
    frame.contentWindow.postMessage({
      nonce: options.messageNonce,
      renderer: options.renderer
    }, '*');
  };
  slot.appendChild(frame);
};
</script>`;
}

const FAKE_RUNNER = `(function(){
  var runnerRead = false;
  var runnerWrite = false;
  try { void top.document.body; runnerRead = true; } catch (_error) {}
  try { top.document.body.dataset.apsCompromised = 'runner'; runnerWrite = true; } catch (_error) {}
  parent.postMessage({
    message: 'fictional-runner-security',
    runnerRead: runnerRead,
    runnerWrite: runnerWrite,
    accountMap: window._aps instanceof Map
  }, '*');

  addEventListener('message', function(event) {
    if (event.data && event.data.message === 'fictional-creative-security') {
      parent.postMessage(event.data, '*');
    }
  });

  window._aps.forEach(function(account) {
    var events = account.queue.splice(0);
    events.forEach(function(event) {
      var response = JSON.parse(atob(event.detail.aaxResponse));
      var bid = response.seatbid[0].bid[0];
      if (bid.ext.tagtype === 'iframe') {
        var frame = document.createElement('iframe');
        frame.setAttribute('sandbox', 'allow-scripts allow-same-origin');
        frame.width = String(bid.w);
        frame.height = String(bid.h);
        frame.style.border = '0';
        frame.src = bid.ext.creativeurl;
        document.body.appendChild(frame);
      } else {
        var script = document.createElement('script');
        script.src = bid.ext.creativeurl;
        document.head.appendChild(script);
      }
    });
  });
})();`;

const IFRAME_CREATIVE = `<!doctype html><script>
var creativeRead = false;
var creativeWrite = false;
try { void top.document.body; creativeRead = true; } catch (_error) {}
try { top.document.body.dataset.apsCompromised = 'iframe'; creativeWrite = true; } catch (_error) {}
parent.postMessage({
  message: 'fictional-creative-security',
  tagType: 'iframe',
  creativeRead: creativeRead,
  creativeWrite: creativeWrite
}, '*');
<\/script>`;

const SCRIPT_CREATIVE = `(function(){
  var creativeRead = false;
  var creativeWrite = false;
  try { void top.document.body; creativeRead = true; } catch (_error) {}
  try { top.document.body.dataset.apsCompromised = 'script'; creativeWrite = true; } catch (_error) {}
  parent.postMessage({
    message: 'fictional-creative-security',
    tagType: 'script',
    creativeRead: creativeRead,
    creativeWrite: creativeWrite
  }, '*');
})();`;

test.describe("APS rendering", () => {
    test("renders through real PUC and expands only its authenticated 1x1 shell", async ({
        page,
    }) => {
        const adId = "fictional-inline-ad-id";
        const publisherOrigin = new URL(runtimeUrl("/")).origin;
        const outerCreativeUrl = runtimeUrl("/fictional-puc-shell");
        let creativeRequests = 0;

        await page.route(runtimeUrl("/aps-puc-topology-test"), (route) =>
            route.fulfill({
                status: 200,
                contentType: "text/html",
                body: '<!doctype html><div id="div-aps"></div><div id="div-other"></div>',
            }),
        );
        await page.route(outerCreativeUrl, (route) =>
            route.fulfill({
                status: 200,
                contentType: "text/html",
                body: `<!doctype html><script>${PUC_BANNER}</script><script>
window.ucTag.renderAd(document, { adId: ${JSON.stringify(adId)}, pubUrl: ${JSON.stringify(publisherOrigin)} });
</script>`,
            }),
        );
        await page.route(IFRAME_CREATIVE_URL, (route) => {
            creativeRequests += 1;
            return route.fulfill({
                status: 200,
                contentType: "text/html",
                body: IFRAME_CREATIVE,
            });
        });

        await page.goto(runtimeUrl("/aps-puc-topology-test"));
        await page.addScriptTag({ path: clientAuctionBundlePaths().gpt });
        await page.evaluate(
            ({ creativeUrl, outerUrl, selectedAdId }) => {
                const typedWindow = window as unknown as {
                    tsjs: Record<string, unknown>;
                    pucEvents: Array<Record<string, unknown>>;
                };
                typedWindow.tsjs = {
                    bids: {
                        "aps-slot": {
                            hb_adid: selectedAdId,
                            hb_bidder: "fictional",
                            hb_pb: "1.23",
                            adm: `<iframe src="${creativeUrl}" width="300" height="250"></iframe>`,
                            w: 300,
                            h: 250,
                        },
                    },
                    adSlots: [
                        {
                            id: "aps-slot",
                            div_id: "div-aps",
                            gam_unit_path: "/fictional/aps",
                            formats: [[300, 250]],
                        },
                    ],
                };
                typedWindow.pucEvents = [];
                const locator = document.createElement("iframe");
                locator.name = "__pb_locator__";
                document.body.appendChild(locator);
                window.addEventListener("message", (event) => {
                    try {
                        const message = JSON.parse(
                            String(event.data),
                        ) as Record<string, unknown>;
                        if (message.message === "Prebid Event") {
                            typedWindow.pucEvents.push(message);
                        }
                    } catch {
                        // Ignore unrelated publisher messages.
                    }
                });

                const slot = document.getElementById("div-aps")!;
                slot.style.width = "1px";
                slot.style.height = "1px";
                const frame = document.createElement("iframe");
                frame.id = "google_ads_iframe_fictional_0";
                frame.width = "1";
                frame.height = "1";
                frame.style.width = "1px";
                frame.style.height = "1px";
                frame.src = outerUrl;
                slot.appendChild(frame);

                const other = document.getElementById("div-other")!;
                const otherFrame = document.createElement("iframe");
                otherFrame.width = "1";
                otherFrame.height = "1";
                otherFrame.style.width = "1px";
                otherFrame.style.height = "1px";
                other.appendChild(otherFrame);
            },
            {
                creativeUrl: IFRAME_CREATIVE_URL,
                outerUrl: outerCreativeUrl,
                selectedAdId: adId,
            },
        );

        await expect.poll(() => creativeRequests).toBe(1);
        await expect
            .poll(() =>
                page.evaluate(() =>
                    (
                        window as unknown as {
                            pucEvents: Array<Record<string, unknown>>;
                        }
                    ).pucEvents.some(
                        (event) => event.event === "adRenderSucceeded",
                    ),
                ),
            )
            .toBe(true);
        await expect(page.locator("#google_ads_iframe_fictional_0")).toHaveCSS(
            "width",
            "300px",
        );
        await expect(page.locator("#google_ads_iframe_fictional_0")).toHaveCSS(
            "height",
            "250px",
        );
        await expect(page.locator("#div-aps")).toHaveCSS("width", "300px");
        await expect(page.locator("#div-aps")).toHaveCSS("height", "250px");
        await expect(page.locator("#div-other iframe")).toHaveCSS(
            "width",
            "1px",
        );
        await expect(page.locator("#div-other iframe")).toHaveCSS(
            "height",
            "1px",
        );
    });

    test("renders a trustedServer adapter bid using Prebid's generated GAM ad ID", async ({
        page,
    }) => {
        const apsRenderer = descriptor("iframe");
        const responseBody = {
            id: "fictional-auction",
            seatbid: [
                {
                    seat: "aps",
                    bid: [
                        {
                            id: apsRenderer.bidId,
                            impid: "div-aps",
                            price: 1.23,
                            crid: apsRenderer.creativeId,
                            w: 300,
                            h: 250,
                            ext: {
                                trusted_server: { renderer: apsRenderer },
                            },
                        },
                    ],
                },
            ],
            ext: {},
        };
        let auctionRequests = 0;
        await page.route(runtimeUrl("/aps-prebid-adapter-test"), (route) =>
            route.fulfill({
                status: 200,
                contentType: "text/html",
                body: '<!doctype html><div id="div-aps"></div><div id="div-other"></div>',
            }),
        );
        await page.route(runtimeUrl("/auction"), (route) => {
            auctionRequests += 1;
            return route.fulfill({
                status: 200,
                contentType: "application/json",
                body: JSON.stringify(responseBody),
            });
        });

        await page.goto(runtimeUrl("/aps-prebid-adapter-test"));
        await loadClientAuctionBundles(page);

        const result = await page.evaluate(async () => {
            type PrebidBid = {
                ad?: string;
                adId: string;
                bidderCode: string;
                status?: string;
            };
            type PrebidApi = {
                getAllWinningBids(): PrebidBid[];
                getBidResponsesForAdUnitCode(code: string): {
                    bids: PrebidBid[];
                };
                onEvent(
                    name: string,
                    callback: (value: Record<string, unknown>) => void,
                ): void;
                requestBids(options: Record<string, unknown>): void;
            };
            const pbjs = (window as unknown as { pbjs: PrebidApi }).pbjs;
            const bidWon: string[] = [];
            const renderSucceeded: string[] = [];
            pbjs.onEvent("bidWon", (bid) => bidWon.push(String(bid.adId)));
            pbjs.onEvent("adRenderSucceeded", (event) =>
                renderSucceeded.push(String(event.adId)),
            );

            const acceptedBid = await new Promise<PrebidBid | undefined>(
                (resolveBid) => {
                    pbjs.requestBids({
                        adUnits: [
                            {
                                code: "div-aps",
                                mediaTypes: { banner: { sizes: [[300, 250]] } },
                                bids: [],
                            },
                        ],
                        bidsBackHandler: () =>
                            resolveBid(
                                pbjs
                                    .getBidResponsesForAdUnitCode("div-aps")
                                    .bids.find(
                                        (bid) => bid.bidderCode === "aps",
                                    ),
                            ),
                        timeout: 1_000,
                    });
                },
            );
            if (!acceptedBid)
                throw new Error("APS bid was not accepted by Prebid");

            const foreignUniversalCreativeResponse = await new Promise<
                Record<string, unknown> | undefined
            >((resolveResponse) => {
                const frame = document.createElement("iframe");
                const adIdJson = JSON.stringify(acceptedBid.adId);
                frame.srcdoc = `<script>
const renderChannel = new MessageChannel();
renderChannel.port1.onmessage = function(event) {
  parent.postMessage({ type: 'captured-foreign-prebid-response', payload: event.data }, '*');
};
parent.postMessage(JSON.stringify({
  message: 'Prebid Request',
  adId: ${adIdJson}
}), '*', [renderChannel.port2]);
<\/script>`;
                let timeout = 0;
                const receive = (event: MessageEvent) => {
                    if (event.data?.type !== "captured-foreign-prebid-response")
                        return;
                    window.removeEventListener("message", receive);
                    window.clearTimeout(timeout);
                    resolveResponse(JSON.parse(String(event.data.payload)));
                };
                window.addEventListener("message", receive);
                document.getElementById("div-other")!.appendChild(frame);
                timeout = window.setTimeout(() => {
                    window.removeEventListener("message", receive);
                    resolveResponse(undefined);
                }, 200);
            });

            const universalCreativeResponse = await new Promise<
                Record<string, unknown>
            >((resolveResponse, rejectResponse) => {
                const frame = document.createElement("iframe");
                const adIdJson = JSON.stringify(acceptedBid.adId);
                frame.srcdoc = `<script>
const renderChannel = new MessageChannel();
renderChannel.port1.onmessage = function(event) {
  parent.postMessage({ type: 'captured-prebid-response', payload: event.data }, '*');
  const eventChannel = new MessageChannel();
  parent.postMessage(JSON.stringify({
    message: 'Prebid Event',
    adId: ${adIdJson},
    event: 'adRenderSucceeded'
  }), '*', [eventChannel.port2]);
};
parent.postMessage(JSON.stringify({
  message: 'Prebid Request',
  adId: ${adIdJson}
}), '*', [renderChannel.port2]);
<\/script>`;
                const receive = (event: MessageEvent) => {
                    if (event.data?.type !== "captured-prebid-response") return;
                    window.removeEventListener("message", receive);
                    resolveResponse(JSON.parse(String(event.data.payload)));
                };
                window.addEventListener("message", receive);
                document.getElementById("div-aps")!.appendChild(frame);
                window.setTimeout(
                    () =>
                        rejectResponse(
                            new Error("Universal Creative response timed out"),
                        ),
                    3_000,
                );
            });
            await new Promise((resolveTick) =>
                window.setTimeout(resolveTick, 50),
            );

            return {
                acceptedAd: acceptedBid.ad,
                acceptedAdId: acceptedBid.adId,
                acceptedStatus: acceptedBid.status,
                bidWon,
                foreignUniversalCreativeResponse,
                renderSucceeded,
                universalCreativeResponse,
                winningAdIds: pbjs.getAllWinningBids().map((bid) => bid.adId),
                registrySize: Object.keys(
                    (
                        window as unknown as {
                            tsjs?: {
                                apsPrebidRenderers?: Record<string, unknown>;
                            };
                        }
                    ).tsjs?.apsPrebidRenderers ?? {},
                ).length,
            };
        });

        expect(auctionRequests).toBe(1);
        expect(result.acceptedAd).toBe("");
        expect(result.acceptedAdId).not.toBe(apsRenderer.bidId);
        expect(result.foreignUniversalCreativeResponse).toBeUndefined();
        expect(result.universalCreativeResponse).toEqual(
            expect.objectContaining({
                message: "Prebid Response",
                adId: result.acceptedAdId,
                rendererVersion: 4,
                apsRenderer,
            }),
        );
        expect(result.bidWon).toEqual([result.acceptedAdId]);
        expect(result.renderSucceeded).toEqual([result.acceptedAdId]);
        expect(result.winningAdIds).toContain(result.acceptedAdId);
        expect(result.acceptedStatus).toBe("rendered");
        expect(result.registrySize).toBe(0);
    });

    test("enforces nonce gating and isolates iframe and script behavior under restrictive CSP", async ({
        page,
    }) => {
        const rendererResponse = await page.request.get(
            runtimeUrl("/integrations/aps/renderer"),
        );
        expect(rendererResponse.status()).toBe(200);
        expect(rendererResponse.headers()["content-type"]).toContain(
            "text/html",
        );
        const rendererCsp =
            rendererResponse.headers()["content-security-policy"];
        expect(rendererCsp).toContain("default-src 'none'");
        expect(rendererCsp).toContain("sandbox allow-forms");
        expect(rendererCsp).not.toContain("allow-same-origin");
        expect(rendererResponse.headers()["referrer-policy"]).toBe(
            "no-referrer",
        );

        let runnerRequests = 0;
        await page.route(RUNNER_URL, async (route) => {
            runnerRequests += 1;
            await route.fulfill({
                status: 200,
                contentType: "application/javascript",
                body: FAKE_RUNNER,
            });
        });
        await page.route(IFRAME_CREATIVE_URL, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                body: IFRAME_CREATIVE,
            });
        });
        await page.route(SCRIPT_CREATIVE_URL, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "application/javascript",
                body: SCRIPT_CREATIVE,
            });
        });
        await page.route(runtimeUrl("/aps-security-test"), async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                headers: {
                    "Content-Security-Policy":
                        "default-src 'none'; script-src 'unsafe-inline'; frame-src 'self'",
                },
                body: testPage(runtimeUrl("/integrations/aps/renderer")),
            });
        });

        await page.goto(runtimeUrl("/aps-security-test"));

        const validNonce = "ABCDEFGHIJKLMNOPQRSTUV";
        await page.evaluate(
            ({ renderer, nonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "iframe-slot",
                    fragmentNonce: nonce,
                    messageNonce: nonce,
                    renderer,
                });
            },
            { renderer: descriptor("iframe"), nonce: validNonce },
        );

        await expect
            .poll(async () =>
                page.evaluate(() =>
                    (
                        window as unknown as {
                            apsMessages: Array<{ data: { message?: string } }>;
                        }
                    ).apsMessages.some(
                        ({ data }) =>
                            data.message === "fictional-creative-security",
                    ),
                ),
            )
            .toBe(true);
        await expect(page.locator("#iframe-slot .existing")).toHaveCount(0);

        const validState = await page.evaluate(() => {
            const frame = document.querySelector<HTMLIFrameElement>(
                "#iframe-slot iframe",
            )!;
            const messages = (
                window as unknown as {
                    apsMessages: Array<{ data: Record<string, unknown> }>;
                }
            ).apsMessages.map(({ data }) => data);
            let publisherCanReadFrame = false;
            try {
                publisherCanReadFrame = Boolean(
                    frame.contentWindow?.document.body,
                );
            } catch (_error) {
                publisherCanReadFrame = false;
            }
            return {
                existing: Boolean(
                    document.querySelector("#iframe-slot .existing"),
                ),
                sandbox: frame.getAttribute("sandbox"),
                publisherCanReadFrame,
                compromised: document.body.dataset.apsCompromised,
                messages,
            };
        });

        expect(validState.existing).toBe(false);
        expect(validState.sandbox).toBe(SANDBOX);
        expect(validState.sandbox).not.toContain("allow-same-origin");
        expect(validState.publisherCanReadFrame).toBe(false);
        expect(validState.compromised).toBeUndefined();
        expect(validState.messages).toContainEqual(
            expect.objectContaining({
                message: "fictional-runner-security",
                runnerRead: false,
                runnerWrite: false,
                accountMap: true,
            }),
        );
        expect(validState.messages).toContainEqual(
            expect.objectContaining({
                message: "fictional-creative-security",
                tagType: "iframe",
                creativeRead: false,
                creativeWrite: false,
            }),
        );

        const cspNonce = "csp-sandbox-0123456789";
        await page.evaluate(
            ({ renderer, nonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "csp-sandbox-slot",
                    fragmentNonce: nonce,
                    messageNonce: nonce,
                    omitSandbox: true,
                    renderer,
                });
            },
            { renderer: descriptor("script"), nonce: cspNonce },
        );
        await expect
            .poll(async () =>
                page.evaluate(() =>
                    (
                        window as unknown as {
                            apsMessages: Array<{
                                slotId: string;
                                data: Record<string, unknown>;
                            }>;
                        }
                    ).apsMessages.some(
                        ({ slotId, data }) =>
                            slotId === "csp-sandbox-slot" &&
                            data.message === "fictional-creative-security" &&
                            data.tagType === "script",
                    ),
                ),
            )
            .toBe(true);
        await expect(page.locator("#csp-sandbox-slot .existing")).toHaveCount(
            0,
        );
        const cspState = await page.evaluate(() => {
            const frame = document.querySelector<HTMLIFrameElement>(
                "#csp-sandbox-slot iframe",
            )!;
            let publisherCanReadFrame = false;
            try {
                publisherCanReadFrame = Boolean(
                    frame.contentWindow?.document.body,
                );
            } catch (_error) {
                publisherCanReadFrame = false;
            }
            return {
                sandbox: frame.getAttribute("sandbox"),
                existing: Boolean(
                    document.querySelector("#csp-sandbox-slot .existing"),
                ),
                publisherCanReadFrame,
                compromised: document.body.dataset.apsCompromised,
                messages: (
                    window as unknown as {
                        apsMessages: Array<{
                            slotId: string;
                            data: Record<string, unknown>;
                        }>;
                    }
                ).apsMessages
                    .filter(({ slotId }) => slotId === "csp-sandbox-slot")
                    .map(({ data }) => data),
            };
        });
        expect(cspState.sandbox).toBeNull();
        expect(cspState.existing).toBe(false);
        expect(cspState.publisherCanReadFrame).toBe(false);
        expect(cspState.compromised).toBeUndefined();
        expect(cspState.messages).toContainEqual(
            expect.objectContaining({
                message: "fictional-runner-security",
                runnerRead: false,
                runnerWrite: false,
            }),
        );
        expect(cspState.messages).toContainEqual(
            expect.objectContaining({
                message: "fictional-creative-security",
                tagType: "script",
                creativeRead: false,
                creativeWrite: false,
            }),
        );

        const messagesBeforeReplay = await page.evaluate(
            () =>
                (
                    window as unknown as {
                        apsMessages: Array<{ data: Record<string, unknown> }>;
                    }
                ).apsMessages.length,
        );
        await page.evaluate(
            ({ renderer, nonce }) => {
                const frame = document.querySelector<HTMLIFrameElement>(
                    "#iframe-slot iframe",
                )!;
                frame.contentWindow!.postMessage({ nonce, renderer }, "*");
            },
            { renderer: descriptor("iframe"), nonce: validNonce },
        );
        await page.waitForTimeout(100);
        expect(
            await page.evaluate(
                () =>
                    (
                        window as unknown as {
                            apsMessages: Array<{
                                data: Record<string, unknown>;
                            }>;
                        }
                    ).apsMessages.length,
            ),
        ).toBe(messagesBeforeReplay);

        const requestsBeforeInvalid = runnerRequests;
        const wrongNonce = "ZYXWVUTSRQPONMLKJIHGFE";
        await page.evaluate(
            ({ renderer, fragmentNonce, messageNonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "mismatch-slot",
                    fragmentNonce,
                    messageNonce,
                    renderer,
                });
            },
            {
                renderer: descriptor("iframe"),
                fragmentNonce: validNonce,
                messageNonce: wrongNonce,
            },
        );
        await page.waitForTimeout(100);
        expect(runnerRequests).toBe(requestsBeforeInvalid);
        await expect(page.locator("#mismatch-slot .existing")).toHaveCount(1);

        await page.evaluate(
            ({ renderer, nonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "missing-fragment-slot",
                    fragmentNonce: nonce,
                    messageNonce: nonce,
                    includeFragment: false,
                    renderer,
                });
            },
            { renderer: descriptor("iframe"), nonce: validNonce },
        );
        await page.waitForTimeout(100);
        expect(runnerRequests).toBe(requestsBeforeInvalid);
        await expect(
            page.locator("#missing-fragment-slot .existing"),
        ).toHaveCount(1);

        await page.evaluate(
            ({ renderer, nonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "malformed-slot",
                    fragmentNonce: nonce,
                    messageNonce: nonce,
                    renderer: { ...renderer, unexpected: true },
                });
            },
            { renderer: descriptor("iframe"), nonce: wrongNonce },
        );
        await page.waitForTimeout(100);
        expect(runnerRequests).toBe(requestsBeforeInvalid);
        await expect(page.locator("#malformed-slot .existing")).toHaveCount(1);

        const scriptNonce = "0123456789abcdefghijkl";
        await page.evaluate(
            ({ renderer, nonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "script-slot",
                    fragmentNonce: nonce,
                    messageNonce: nonce,
                    renderer,
                });
            },
            { renderer: descriptor("script"), nonce: scriptNonce },
        );

        await expect
            .poll(async () =>
                page.evaluate(() =>
                    (
                        window as unknown as {
                            apsMessages: Array<{
                                data: Record<string, unknown>;
                            }>;
                        }
                    ).apsMessages.some(
                        ({ data }) =>
                            data.message === "fictional-creative-security" &&
                            data.tagType === "script",
                    ),
                ),
            )
            .toBe(true);
        await expect(page.locator("#script-slot .existing")).toHaveCount(0);

        const scriptState = await page.evaluate(() => ({
            existing: Boolean(document.querySelector("#script-slot .existing")),
            compromised: document.body.dataset.apsCompromised,
            scriptSecurity: (
                window as unknown as {
                    apsMessages: Array<{ data: Record<string, unknown> }>;
                }
            ).apsMessages.find(
                ({ data }) =>
                    data.message === "fictional-creative-security" &&
                    data.tagType === "script",
            )?.data,
        }));
        expect(scriptState.existing).toBe(false);
        expect(scriptState.compromised).toBeUndefined();
        expect(scriptState.scriptSecurity).toEqual(
            expect.objectContaining({
                creativeRead: false,
                creativeWrite: false,
            }),
        );
    });

    test("rejects same-origin creative URLs through the TSJS rendering path", async ({
        page,
    }) => {
        const publisherOrigin = "https://publisher.example";
        const rendererUrl = `${publisherOrigin}/integrations/aps/renderer`;
        const auctionUrl = `${publisherOrigin}/auction`;
        const testUrl = `${publisherOrigin}/aps-same-origin-test`;
        const runtimeRenderer = await page.request.get(
            runtimeUrl("/integrations/aps/renderer"),
        );
        const rendererDocument = await runtimeRenderer.text();
        let creativeUrl = SCRIPT_CREATIVE_URL;
        let runnerRequests = 0;

        await page.route(RUNNER_URL, async (route) => {
            runnerRequests += 1;
            await route.fulfill({
                status: 200,
                contentType: "application/javascript",
                body: FAKE_RUNNER,
            });
        });
        await page.route(rendererUrl, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                headers: {
                    "Content-Security-Policy":
                        runtimeRenderer.headers()["content-security-policy"],
                    "Referrer-Policy": "no-referrer",
                },
                body: rendererDocument,
            });
        });
        await page.route(auctionUrl, async (route) => {
            const renderer = descriptor("script", creativeUrl);
            await route.fulfill({
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({
                    id: "fictional-auction",
                    seatbid: [
                        {
                            seat: "aps",
                            bid: [
                                {
                                    id: renderer.bidId,
                                    impid: "same-origin-slot",
                                    price: 1.23,
                                    w: 300,
                                    h: 250,
                                    ext: {
                                        trusted_server: { renderer },
                                    },
                                },
                            ],
                        },
                    ],
                    ext: {},
                }),
            });
        });
        await page.route(testUrl, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                headers: {
                    "Content-Security-Policy":
                        "default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'; frame-src 'self'",
                },
                body: '<!doctype html><div id="same-origin-slot"><span class="existing">existing publisher content</span></div>',
            });
        });

        await page.goto(testUrl);
        await page.addScriptTag({ path: clientAuctionBundlePaths().core });
        await page.evaluate(() => {
            const tsjs = (
                window as unknown as {
                    tsjs: {
                        addAdUnits(units: Array<Record<string, unknown>>): void;
                        log: {
                            info(message: string, ...args: unknown[]): void;
                        };
                        requestAds(): void;
                    };
                    auctionRenderCompletions?: number;
                }
            ).tsjs;
            tsjs.addAdUnits([
                {
                    code: "same-origin-slot",
                    mediaTypes: { banner: { sizes: [[300, 250]] } },
                    bids: [],
                },
            ]);
            const originalInfo = tsjs.log.info.bind(tsjs.log);
            tsjs.log.info = (message: string, ...args: unknown[]) => {
                if (
                    message === "requestAds: rendered creatives from response"
                ) {
                    (
                        window as unknown as {
                            auctionRenderCompletions: number;
                        }
                    ).auctionRenderCompletions += 1;
                }
                originalInfo(message, ...args);
            };
            (
                window as unknown as {
                    auctionRenderCompletions: number;
                }
            ).auctionRenderCompletions = 0;
            tsjs.requestAds();
        });

        await expect.poll(() => runnerRequests).toBe(1);
        await expect
            .poll(() =>
                page.evaluate(
                    () =>
                        (
                            window as unknown as {
                                auctionRenderCompletions: number;
                            }
                        ).auctionRenderCompletions,
                ),
            )
            .toBe(1);
        await expect(page.locator("#same-origin-slot .existing")).toHaveCount(
            0,
        );
        const productSandbox = await page
            .locator("#same-origin-slot > iframe")
            .getAttribute("sandbox");
        expect(productSandbox).toBe(SANDBOX);
        expect(productSandbox).not.toContain("allow-same-origin");

        creativeUrl = `${publisherOrigin}/fictional-same-origin.js`;
        await page.locator("#same-origin-slot").evaluate((slot) => {
            slot.innerHTML =
                '<span class="existing">existing publisher content</span>';
        });
        await page.evaluate(() => {
            (
                window as unknown as {
                    tsjs: { requestAds(): void };
                }
            ).tsjs.requestAds();
        });

        await expect
            .poll(() =>
                page.evaluate(
                    () =>
                        (
                            window as unknown as {
                                auctionRenderCompletions: number;
                            }
                        ).auctionRenderCompletions,
                ),
            )
            .toBe(2);
        expect(runnerRequests).toBe(1);
        await expect(page.locator("#same-origin-slot iframe")).toHaveCount(0);
        await expect(page.locator("#same-origin-slot .existing")).toHaveCount(
            1,
        );
    });

    test("renders publisher-native mode through the injected friendly-frame runner", async ({
        page,
    }) => {
        const publisherOrigin = "https://publisher.example";
        const auctionUrl = `${publisherOrigin}/auction`;
        const testUrl = `${publisherOrigin}/aps-publisher-native-test`;
        const renderer = descriptor("iframe");
        const coreBundle = readFileSync(clientAuctionBundlePaths().core, "utf8");
        let runnerRequests = 0;
        let runnerReferrer: string | undefined;
        let creativeReferrer: string | undefined;

        await page.route(PUBLISHER_CORE_URL, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "application/javascript",
                body: coreBundle,
            });
        });
        await page.route(RUNNER_URL, async (route) => {
            runnerRequests += 1;
            runnerReferrer = route.request().headers()["referer"];
            await route.fulfill({
                status: 200,
                contentType: "application/javascript",
                body: FAKE_RUNNER,
            });
        });
        await page.route(IFRAME_CREATIVE_URL, async (route) => {
            creativeReferrer = route.request().headers()["referer"];
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                body: IFRAME_CREATIVE,
            });
        });
        await page.route(auctionUrl, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({
                    id: "fictional-native-auction",
                    seatbid: [
                        {
                            seat: "aps",
                            bid: [
                                {
                                    id: renderer.bidId,
                                    impid: "publisher-native-slot",
                                    price: 1.23,
                                    w: renderer.width,
                                    h: renderer.height,
                                    ext: { trusted_server: { renderer } },
                                },
                            ],
                        },
                    ],
                    ext: {},
                }),
            });
        });
        await page.route(testUrl, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                headers: {
                    "Content-Security-Policy":
                        "default-src 'none'; script-src https://tsjs.example https://client.aps.amazon-adsystem.com https://creative.example; connect-src 'self'; frame-src https://creative.example",
                },
                body: `<!doctype html>
<div id="publisher-native-slot"><span class="existing">existing publisher content</span></div>`,
            });
        });

        await page.goto(testUrl);
        await page.evaluate(async (scriptUrl) => {
            await new Promise<void>((resolveScript, rejectScript) => {
                const script = document.createElement("script");
                script.setAttribute(
                    "data-ts-aps-rendering-mode",
                    "publisher_native",
                );
                script.src = scriptUrl;
                script.addEventListener("load", () => resolveScript(), {
                    once: true,
                });
                script.addEventListener(
                    "error",
                    () => rejectScript(new Error("TSJS core failed to load")),
                    { once: true },
                );
                document.head.appendChild(script);
            });
        }, PUBLISHER_CORE_URL);
        await page.evaluate(() => {
            const tsjs = (
                window as unknown as {
                    tsjs: {
                        addAdUnits(units: Array<Record<string, unknown>>): void;
                        requestAds(): void;
                    };
                }
            ).tsjs;
            tsjs.addAdUnits([
                {
                    code: "publisher-native-slot",
                    mediaTypes: { banner: { sizes: [[300, 250]] } },
                    bids: [],
                },
            ]);
            tsjs.requestAds();
        });

        await expect.poll(() => runnerRequests).toBe(1);
        const frame = page.locator("#publisher-native-slot > iframe");
        await expect(frame).toHaveCount(1);
        await expect(frame).toBeVisible();
        expect(await frame.getAttribute("sandbox")).toBeNull();
        await expect(
            frame
                .contentFrame()
                .locator(`iframe[src="${IFRAME_CREATIVE_URL}"]`),
        ).toHaveCount(1);
        await expect(
            page.locator("#publisher-native-slot .existing"),
        ).toHaveCount(0);
        expect(runnerReferrer).toBeUndefined();
        expect(creativeReferrer).toBeUndefined();
        expect(
            await frame.evaluate((element: HTMLIFrameElement) => {
                const document = element.contentDocument!;
                const creative = document.body.querySelector("iframe")!;
                return {
                    bodyMargin: getComputedStyle(document.body).margin,
                    bodyPadding: getComputedStyle(document.body).padding,
                    creativeDisplay: getComputedStyle(creative).display,
                    clientWidth: document.documentElement.clientWidth,
                    clientHeight: document.documentElement.clientHeight,
                    scrollWidth: document.documentElement.scrollWidth,
                    scrollHeight: document.documentElement.scrollHeight,
                };
            }),
        ).toEqual({
            bodyMargin: "0px",
            bodyPadding: "0px",
            creativeDisplay: "block",
            clientWidth: 300,
            clientHeight: 250,
            scrollWidth: 300,
            scrollHeight: 250,
        });
        expect(
            await page
                .locator("#publisher-native-slot")
                .evaluate(
                    (slot) =>
                        slot.querySelectorAll(
                            'iframe[src*="/integrations/aps/renderer"]',
                        ).length,
                ),
        ).toBe(0);
    });
});

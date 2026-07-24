import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";
import { runtimeUrl } from "../../helpers/state.js";

const RUNNER_URL = "https://client.aps.amazon-adsystem.com/prebid-creative.js";
const IFRAME_CREATIVE_URL = "https://creative.example/iframe";
const SCRIPT_CREATIVE_URL = "https://creative.example/script.js";
const SANDBOX =
    "allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation";
const TSJS_CRATE = resolve(__dirname, "../../../../trusted-server-js");

function clientAuctionBundlePaths() {
    const manifestPath = resolve(TSJS_CRATE, "dist/prebid/manifest.json");
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as {
        filename: string;
    };
    return {
        gpt: resolve(TSJS_CRATE, "dist/tsjs-gpt.js"),
        prebid: resolve(TSJS_CRATE, "dist/prebid", manifest.filename),
    };
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

test.describe("APS opaque renderer", () => {
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
                body: '<!doctype html><div id="div-aps"></div>',
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
        const bundles = clientAuctionBundlePaths();
        await page.addScriptTag({ path: bundles.gpt });
        await page.addScriptTag({ path: bundles.prebid });

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

    test("rejects same-origin creative URLs during static validation", async ({
        page,
    }) => {
        const publisherOrigin = "https://publisher.example";
        const rendererUrl = `${publisherOrigin}/integrations/aps/renderer`;
        const testUrl = `${publisherOrigin}/aps-same-origin-test`;
        const runtimeRenderer = await page.request.get(
            runtimeUrl("/integrations/aps/renderer"),
        );
        const rendererDocument = await runtimeRenderer.text();
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
        await page.route(testUrl, async (route) => {
            await route.fulfill({
                status: 200,
                contentType: "text/html",
                headers: {
                    "Content-Security-Policy":
                        "default-src 'none'; script-src 'unsafe-inline'; frame-src 'self'",
                },
                body: testPage(rendererUrl),
            });
        });

        await page.goto(testUrl);
        const nonce = "same-origin-0123456789";
        await page.evaluate(
            ({ renderer, nonce }) => {
                (
                    window as unknown as {
                        startApsFrame(options: Record<string, unknown>): void;
                    }
                ).startApsFrame({
                    slotId: "same-origin-slot",
                    fragmentNonce: nonce,
                    messageNonce: nonce,
                    omitSandbox: true,
                    renderer,
                });
            },
            {
                renderer: descriptor(
                    "script",
                    `${publisherOrigin}/fictional-same-origin.js`,
                ),
                nonce,
            },
        );

        await page.waitForTimeout(100);
        expect(runnerRequests).toBe(0);
        await expect(page.locator("#same-origin-slot .existing")).toHaveCount(
            1,
        );
    });
});

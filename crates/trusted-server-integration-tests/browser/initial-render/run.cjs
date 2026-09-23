const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const { execFileSync } = require("node:child_process");
const os = require("node:os");
const { parseArgs } = require("node:util");
const { values: options } = parseArgs({
  options: {
    baseline: { type: "boolean" },
    out: { type: "string" },
    prebid: { type: "string" },
    variant: { type: "string", default: "both" },
  },
});
assert(
  ["both", "runtime", "bootstrap"].includes(options.variant),
  "--variant must be both, runtime, or bootstrap",
);
const repo = path.resolve(__dirname, "../../../..");
const { chromium } = require(
  path.join(
    repo,
    "crates/trusted-server-integration-tests/browser/node_modules/playwright",
  ),
);
const outputRoot = options.out
  ? path.resolve(options.out)
  : fs.mkdtempSync(path.join(os.tmpdir(), "ts-initial-render-"));
fs.mkdirSync(outputRoot, { recursive: true });
console.log("Evidence directory:", outputRoot);
let prebidDirectory = options.prebid && path.resolve(options.prebid);
if (!prebidDirectory) {
  prebidDirectory = fs.mkdtempSync(
    path.join(os.tmpdir(), "ts-initial-render-prebid-"),
  );
  execFileSync(
    process.execPath,
    [
      "build-prebid-external.mjs",
      "--modules-json",
      JSON.stringify({
        bidder: ["rubiconBidAdapter"],
        userId: ["sharedIdSystem"],
        analytics: [],
      }),
      "--out",
      prebidDirectory,
    ],
    { cwd: path.join(repo, "crates/trusted-server-js/lib"), stdio: "inherit" },
  );
}
const dist = path.join(repo, "crates/trusted-server-js/dist");
const manifest = JSON.parse(
  fs.readFileSync(path.join(prebidDirectory, "manifest.json")),
);
const external = path.join(prebidDirectory, manifest.filename);
const puc = fs.readFileSync(
  path.join(
    repo,
    "crates/trusted-server-integration-tests/browser/node_modules/prebid-universal-creative/dist/banner.js",
  ),
  "utf8",
);
// Keep every scenario on the same artifacts, even if a separate build runs.
const scriptContents = new Map(
  [
    path.join(
      repo,
      "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
    ),
    path.join(dist, "tsjs-core.js"),
    path.join(dist, "tsjs-gpt.js"),
    path.join(dist, "tsjs-prebid.js"),
    external,
  ].map((file) => [file, fs.readFileSync(file, "utf8")]),
);
const origin = "https://publisher.example.com";
const { creative, pucPage } = require("./pages.cjs");
const scenarios = [
  "publisher-first",
  "before-request-event",
  "request-before-render",
  "after-render",
  "subsequent-refresh",
];
(async () => {
  const browser = await chromium.launch({ headless: true });
  const reports = [];
  // Tracked across the whole run so a throw anywhere in a scenario still writes
  // that scenario's trace into the evidence directory CI uploads on failure.
  let pendingTrace = null;
  let openContext = null;
  try {
    for (const variant of options.variant === "both"
      ? ["runtime", "bootstrap"]
      : [options.variant]) {
      const out = path.join(outputRoot, variant);
      fs.mkdirSync(out, { recursive: true });
      for (const scenario of scenarios) {
        const context = await browser.newContext({
          viewport: { width: 720, height: 500 },
        });
        openContext = context;
        pendingTrace = {
          context,
          path: path.join(out, scenario + "-trace.zip"),
        };
        await context.tracing.start({
          screenshots: true,
          snapshots: true,
          sources: true,
        });
        const page = await context.newPage();
        const errors = [];
        const held = [];
        page.on("pageerror", (e) => {
          errors.push(e.message);
          console.log("Page error:", e.message);
        });
        page.on("console", (m) => {
          if (m.type() === "error") console.log("Browser error:", m.text());
        });
        let notifyAuction;
        const auctionArrived = new Promise(
          (resolve) => (notifyAuction = resolve),
        );
        await page.route("**/*", async (route) => {
          const url = new URL(route.request().url());
          if (
            url.origin !== origin &&
            url.origin !== "https://creative.example.com"
          )
            return route.abort();
          if (url.pathname === "/auction") {
            held.push(route);
            notifyAuction();
            return;
          }
          if (url.pathname === "/fixture-puc.js")
            return route.fulfill({
              contentType: "application/javascript",
              body: puc,
            });
          if (url.pathname === "/fixture-puc")
            return route.fulfill({
              contentType: "text/html",
              body: pucPage(),
            });
          if (url.pathname === "/")
            return route.fulfill({
              contentType: "text/html",
              body: '<!doctype html><title>Initial delivery reproduction</title><h1>Initial delivery reproduction</h1><div id="example-slot" style="width:300px;height:250px"></div>',
            });
          return route.fulfill({ status: 404, body: "" });
        });
        await page.goto(origin);
        await page.addScriptTag({
          path: path.join(__dirname, "gpt-fixture.js"),
        });
        const scripts = [path.join(dist, "tsjs-core.js")];
        if (variant === "bootstrap")
          scripts.push(
            path.join(
              repo,
              "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
            ),
          );
        scripts.push(
          path.join(dist, "tsjs-gpt.js"),
          external,
          path.join(dist, "tsjs-prebid.js"),
        );
        for (const file of scripts)
          await page.addScriptTag({ content: scriptContents.get(file) });
        assert.equal(
          await page.evaluate(() => typeof pbjs.onEvent),
          "function",
          "Real Prebid API must be installed",
        );
        await page.evaluate((serverCreative) => {
          tsjs.adSlots = [
            {
              id: "example-server-slot",
              gam_unit_path: "/123/example-placement",
              div_id: "example-slot",
              formats: [[300, 250]],
            },
          ];
          tsjs.bids = {
            "example-server-slot": {
              hb_adid: "example-server-ad",
              hb_bidder: "exampleServer",
              hb_pb: "1.00",
              w: 300,
              h: 250,
              adm: serverCreative,
            },
          };
          pbjs.onEvent("auctionInit", (e) =>
            fixture.record("auctionInit", { auctionId: e.auctionId }),
          );
          pbjs.onEvent("auctionEnd", (e) =>
            fixture.record("auctionEnd", { auctionId: e.auctionId }),
          );
          window.beginPublisherAuction = () => {
            fixture.snapshot("beforePublisher");
            fixture.record("publisherAuctionCall");
            window.publisherDone = false;
            pbjs.requestBids({
              adUnits: [
                {
                  code: "example-slot",
                  mediaTypes: { banner: { sizes: [[300, 250]] } },
                  bids: [],
                },
              ],
              timeout: 15000,
              bidsBackHandler: () => {
                fixture.record("publisherCallback");
                pbjs.setTargetingForGPTAsync(["example-slot"]);
                googletag.pubads().refresh([fixture.slot]);
                window.publisherDone = true;
              },
            });
            fixture.snapshot("afterPublisher");
          };
        }, creative("Server result"));
        const waitForAuction = async () => {
          let timer;
          try {
            await Promise.race([
              auctionArrived,
              new Promise((_, reject) => {
                timer = setTimeout(
                  () => reject(new Error("Auction route did not arrive")),
                  10000,
                );
              }),
            ]);
          } finally {
            clearTimeout(timer);
          }
        };
        const startPublisher = async () => {
          await page.evaluate(() => beginPublisherAuction());
          await waitForAuction();
        };
        const releaseAuction = async () => {
          assert.equal(
            held.length,
            1,
            "One real Prebid network auction must be held",
          );
          await held[0].fulfill({
            status: 200,
            contentType: "application/json",
            body: JSON.stringify({
              id: "example-auction",
              seatbid: [
                {
                  seat: "exampleClient",
                  bid: [
                    {
                      id: "example-client-bid",
                      impid: "example-slot",
                      price: 2,
                      crid: "example-client-creative",
                      w: 300,
                      h: 250,
                      adm: creative("Client result"),
                      adomain: ["example.com"],
                    },
                  ],
                },
              ],
              ext: {},
            }),
          });
        };
        const renderAndObserve = async (index, label) => {
          await page.evaluate((i) => fixture.render(i), index);
          try {
            await page.waitForFunction(
              (expected) => fixture.creatives.at(-1) === expected,
              label,
              { timeout: 10000 },
            );
          } catch (error) {
            console.log(
              "Render failure:",
              scenario,
              JSON.stringify(
                await page.evaluate(() => ({
                  trace: fixture.trace,
                  bids: tsjs.bids,
                  mapping: tsjs.divToSlotId,
                  frames: [...document.querySelectorAll("iframe")].map((f) => ({
                    src: f.src,
                    html: f.contentDocument?.body?.innerHTML,
                  })),
                })),
              ),
            );
            for (const frame of page.frames())
              console.log(
                "Frame:",
                frame.url(),
                (await frame.content()).slice(0, 1000),
              );
            throw error;
          }
          const content = await Promise.all(
            page.frames().map(async (frame) => {
              try {
                return await frame
                  .locator(".marker")
                  .textContent({ timeout: 150 });
              } catch {
                return null;
              }
            }),
          );
          assert(
            content.includes(label),
            "A browser iframe must contain the actual creative marker",
          );
        };
        if (scenario === "publisher-first") {
          await startPublisher();
          await page.evaluate(() => tsjs.adInit());
          assert.equal(
            await page.evaluate(() => fixture.requests.length),
            0,
            "TS must defer to existing publisher claim",
          );
        } else {
          if (scenario === "before-request-event")
            await page.evaluate(() => (fixture.holdRequestEvent = true));
          await page.evaluate(() => tsjs.adInit());
          assert.equal(
            await page.evaluate(() => fixture.requests.length),
            1,
            "TS must submit the initial request",
          );
          if (scenario === "after-render" || scenario === "subsequent-refresh")
            await renderAndObserve(0, "Server result");
          if (scenario === "subsequent-refresh") {
            await page.evaluate(() => {
              fixture.record("independentRefreshCall");
              googletag.pubads().refresh([fixture.slot]);
            });
            await waitForAuction();
          } else await startPublisher();
          if (scenario === "before-request-event")
            await page.evaluate(() => {
              fixture.holdRequestEvent = false;
              fixture.emit("slotRequested");
            });
          if (
            scenario === "request-before-render" ||
            scenario === "before-request-event"
          )
            await renderAndObserve(0, "Server result");
          await page.screenshot({
            path: path.join(out, scenario + "-before.png"),
          });
        }
        await releaseAuction();
        if (scenario === "subsequent-refresh")
          await page.waitForFunction(() => fixture.requests.length === 2);
        else await page.waitForFunction(() => window.publisherDone);
        const count = await page.evaluate(() => fixture.requests.length);
        if (scenario === "publisher-first")
          await renderAndObserve(0, "Client result");
        else if (count > 1) await renderAndObserve(1, "Client result");
        await page.screenshot({
          path: path.join(out, scenario + "-after.png"),
        });
        const report = await page.evaluate(() => ({
          requests: fixture.requests,
          creatives: fixture.creatives,
          trace: fixture.trace,
          claim: tsjs.firstImpression?.slots["example-slot"]?.owner,
          prebidVersion: pbjs.version,
        }));
        report.scenario = scenario;
        report.variant = variant;
        report.errors = errors;
        reports.push(report);
        fs.writeFileSync(
          path.join(outputRoot, "evidence.json"),
          JSON.stringify(reports, null, 2),
        );
        await context.tracing.stop({ path: pendingTrace.path });
        pendingTrace = null;
        assert.equal(
          errors.length,
          0,
          "No unrelated browser error should invalidate evidence",
        );
        const pos = (event) => report.trace.findIndex((e) => e.event === event);
        if (
          scenario === "request-before-render" ||
          scenario === "before-request-event"
        ) {
          const call = pos("publisherAuctionCall"),
            requested = pos("slotRequested"),
            rendered = pos("slotRenderEnded"),
            callback = pos("publisherCallback");
          assert(
            pos("nativeRequest") < call &&
              call < rendered &&
              rendered < callback,
            "Auction must start after native refresh but before render, and finish after render",
          );
          assert(
            scenario === "before-request-event"
              ? call < requested
              : requested < call,
            "Requested event must be on the intended side of publisher registration",
          );
        }
        const replaces =
          ["after-render", "subsequent-refresh"].includes(scenario) ||
          (options.baseline && scenario === "request-before-render");
        assert.equal(
          report.requests.length,
          replaces ? 2 : 1,
          `${variant}/${scenario}: native request count`,
        );
        assert.deepEqual(
          report.creatives,
          scenario === "publisher-first"
            ? ["Client result"]
            : replaces
              ? ["Server result", "Client result"]
              : ["Server result"],
          `${variant}/${scenario}: rendered creative sequence`,
        );
        console.log(
          JSON.stringify({
            variant,
            scenario,
            nativeRequests: count,
            creatives: report.creatives,
            prebidVersion: report.prebidVersion,
          }),
        );
        await context.close();
        openContext = null;
      }
    }
    fs.writeFileSync(
      path.join(outputRoot, "artifacts.json"),
      JSON.stringify(
        {
          browserVersion: browser.version(),
          sourceCommit: execFileSync("git", ["rev-parse", "HEAD"], {
            cwd: repo,
            encoding: "utf8",
          }).trim(),
          worktreeStatus: execFileSync("git", ["status", "--short"], {
            cwd: repo,
            encoding: "utf8",
          }),
          externalPrebidVersion: manifest.prebidVersion,
          pucVersion: require(
            path.join(
              repo,
              "crates/trusted-server-integration-tests/browser/node_modules/prebid-universal-creative/package.json",
            ),
          ).version,
          files: [
            path.join(
              repo,
              "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
            ),
            path.join(dist, "tsjs-core.js"),
            path.join(dist, "tsjs-gpt.js"),
            path.join(dist, "tsjs-prebid.js"),
            external,
            __filename,
            path.join(__dirname, "pages.cjs"),
            path.join(__dirname, "gpt-fixture.js"),
            path.join(
              repo,
              "crates/trusted-server-integration-tests/browser/node_modules/prebid-universal-creative/dist/banner.js",
            ),
          ].map((file) => ({
            file,
            sha256: crypto
              .createHash("sha256")
              .update(scriptContents.get(file) ?? fs.readFileSync(file))
              .digest("hex"),
          })),
        },
        null,
        2,
      ),
    );
    console.log("Controlled comparison assertions passed.");
  } finally {
    if (pendingTrace) {
      await pendingTrace.context.tracing
        .stop({ path: pendingTrace.path })
        .catch(() => {});
    }
    if (openContext) await openContext.close().catch(() => {});
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});

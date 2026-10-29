const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const crypto = require("node:crypto");
const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const { parseArgs } = require("node:util");
const { documentPage, payload, creative, pucPage } = require("./pages.cjs");
const { values: options } = parseArgs({
  options: {
    baseline: { type: "boolean" },
    implementation: { type: "string", default: "all" },
    out: { type: "string" },
    prebid: { type: "string" },
  },
});
assert(
  ["runtime", "bootstrap", "handoff", "all"].includes(options.implementation),
  "Invalid --implementation",
);
const repo = path.resolve(__dirname, "../../../..");
const browserRoot = path.join(
  repo,
  "crates/trusted-server-integration-tests/browser",
);
const { chromium } = require(path.join(browserRoot, "node_modules/playwright"));
const outputRoot = options.out
  ? path.resolve(options.out)
  : fs.mkdtempSync(path.join(os.tmpdir(), "ts-publisher-registration-"));
fs.mkdirSync(outputRoot, { recursive: true });
const sha256 = (content) =>
  crypto.createHash("sha256").update(content).digest("hex");
let prebidDirectory = options.prebid && path.resolve(options.prebid);
if (!prebidDirectory) {
  prebidDirectory = fs.mkdtempSync(
    path.join(os.tmpdir(), "ts-publisher-registration-prebid-"),
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
    {
      cwd: path.join(repo, "crates/trusted-server-js/lib"),
      stdio: "inherit",
    },
  );
}
const manifest = JSON.parse(
  fs.readFileSync(path.join(prebidDirectory, "manifest.json"), "utf8"),
);
const external = path.join(prebidDirectory, manifest.filename);
const dist = path.join(repo, "crates/trusted-server-js/dist");
const bootstrap = path.join(
  repo,
  "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
);
const core = path.join(dist, "tsjs-core.js");
const gpt = path.join(dist, "tsjs-gpt.js");
const prebid = path.join(dist, "tsjs-prebid.js");
const stub = path.join(__dirname, "gpt-fixture.js");
const puc = path.join(
  browserRoot,
  "node_modules/prebid-universal-creative/dist/banner.js",
);
// Snapshot before launching. A concurrent coordinator build cannot mix releases.
const sources = new Map(
  [
    bootstrap,
    core,
    gpt,
    prebid,
    external,
    stub,
    puc,
    __filename,
    path.join(__dirname, "pages.cjs"),
    path.join(__dirname, "../initial-render/pages.cjs"),
    path.join(prebidDirectory, "manifest.json"),
  ].map((file) => [file, fs.readFileSync(file, "utf8")]),
);
assert.equal(
  sha256(sources.get(external)),
  manifest.sha256,
  "External Prebid manifest hash mismatch",
);
assert.equal(
  "sha384-" +
    crypto.createHash("sha384").update(sources.get(external)).digest("base64"),
  manifest.sri,
  "External Prebid manifest SRI mismatch",
);
assert(
  manifest.modules.bidder.includes("rubiconBidAdapter"),
  "Real Rubicon adapter required",
);
assert(
  manifest.modules.userId.includes("sharedIdSystem"),
  "Real SharedId module required",
);
const save = (name, value) =>
  fs.writeFileSync(path.join(outputRoot, name), JSON.stringify(value, null, 2));
const artifacts = {
  startedAt: new Date().toISOString(),
  sourceCommit: execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: repo,
    encoding: "utf8",
  }).trim(),
  dirtyInputs: execFileSync("git", ["status", "--short"], {
    cwd: repo,
    encoding: "utf8",
  }),
  node: process.version,
  externalPrebidManifest: manifest,
  pucVersion: require(
    path.join(
      browserRoot,
      "node_modules/prebid-universal-creative/package.json",
    ),
  ).version,
  provenance:
    "Offline direct payload from pages.cjs. Not Rust provider/framework admission proof.",
  files: [...sources].map(([file, content]) => ({
    file,
    sha256: sha256(content),
  })),
};
save("artifacts.json", artifacts);
console.log("Evidence directory:", outputRoot);
const reports = [];
const implementations =
  options.implementation === "all"
    ? ["runtime", "bootstrap", "handoff"]
    : [options.implementation];

async function install(page, implementation) {
  const files = [core];
  if (implementation !== "runtime") files.push(bootstrap);
  if (
    implementation === "runtime" ||
    (implementation === "handoff" && options.baseline)
  )
    files.push(gpt);
  files.push(external, prebid);
  for (const file of files)
    await page.addScriptTag({ content: sources.get(file) });
  assert.equal(
    await page.evaluate(() => typeof pbjs.onEvent),
    "function",
    "Real Prebid must load",
  );
  await page.evaluate(() => {
    pbjs.onEvent("auctionInit", (event) =>
      fixture.record("auctionInit", { auctionId: event.auctionId }),
    );
    pbjs.onEvent("auctionEnd", (event) =>
      fixture.record("auctionEnd", { auctionId: event.auctionId }),
    );
  });
}

async function render(page, index, label) {
  await page.evaluate((index) => fixture.render(index), index);
  await page.waitForFunction(
    ({ index, label }) =>
      fixture.creatives.some(
        (creative) => creative.index === index && creative.label === label,
      ),
    { index, label },
    { timeout: 10000 },
  );
  const entry = await page.evaluate((index) => fixture.requests[index], index);
  const root = page.locator(`[id=${JSON.stringify(entry.id)}]`);
  await root.locator("iframe").waitFor({ state: "visible" });
  const frame = await root
    .locator("iframe")
    .elementHandle()
    .then((element) => element.contentFrame());
  const descendants = (parent) => [
    parent,
    ...parent.childFrames().flatMap(descendants),
  ];
  const texts = await Promise.all(
    descendants(frame).map(async (child) => {
      const marker = child.locator(".marker");
      return (await marker.count()) ? marker.textContent() : null;
    }),
  );
  assert(
    texts.includes(label),
    "Actual creative text inside the final element's separate-origin frame tree",
  );
}

async function baseline(page, scenario) {
  const [mutation, sizes] = scenario.split("-");
  await page.evaluate(() => {
    const root = document.createElement("div");
    root.id = "example-header-ssr";
    root.style.cssText = "width:300px;height:250px";
    document.getElementById("mount").append(root);
    window.oldElement = root;
  });
  await page.evaluate(
    ({ slots, bids }) => {
      tsjs.scheduleInitialAdInit(bids, slots);
    },
    payload({ registered: false, candidate: "none" }),
  );
  await page.waitForFunction(() => fixture.requests.length > 0);
  await render(page, 0, "Publisher result");
  const formats =
    sizes === "identical"
      ? [
          [300, 250],
          [300, 600],
        ]
      : sizes === "reordered"
        ? [
            [300, 600],
            [300, 250],
          ]
        : [[300, 250]];
  await page.evaluate(
    ({ mutation, formats }) => {
      const root =
        mutation === "replace" ? document.createElement("div") : oldElement;
      root.id = "example-header-final";
      root.style.cssText = "width:300px;height:250px";
      if (mutation === "replace") oldElement.replaceWith(root);
      window.finalElement = root;
      window.offeredSlot = googletag
        .defineSlot("/123/example-placement", formats, root.id)
        .addService(googletag.pubads());
      googletag.display(root.id);
      fixture.record("publisherDefinitionResult", {
        sameNative: offeredSlot === fixture.nativeSlots[0],
        sameElement: oldElement === root,
        nativeId: offeredSlot.getSlotElementId(),
        finalId: root.id,
        finalFrames: root.querySelectorAll("iframe").length,
      });
    },
    { mutation, formats },
  );
  if (sizes !== "identical") {
    await page.waitForFunction(() => fixture.requests.length === 2);
    await render(page, 1, "Publisher result");
  }
  const facts = await page.evaluate(() => ({
    count: fixture.requests.length,
    sameNative: offeredSlot === fixture.nativeSlots[0],
    sameElement: finalElement === oldElement,
    finalFrames: finalElement.querySelectorAll("iframe").length,
    nativeId: offeredSlot.getSlotElementId(),
    finalId: finalElement.id,
  }));
  assert.equal(
    facts.count,
    sizes === "identical" ? 1 : 2,
    "Old automatic native request reproduction",
  );
  assert.equal(
    facts.sameNative,
    sizes === "identical",
    "Old changed-ID matching depends on exact format order",
  );
  if (mutation === "replace" && sizes === "identical")
    assert.equal(
      facts.finalFrames,
      0,
      "Aliased old native object is not visible delivery after subtree removal",
    );
  return {
    ...facts,
    diagnosticOnly: true,
    supportedTeardown: false,
    note: "Old-mode mutation intentionally omits destroy-before-removal to reproduce the unsupported lifecycle. Native alias alone is not visible delivery.",
  };
}

// These barriers drain already scheduled work. They never infer publisher mount
// readiness. Mount, admission, visibility and network release are explicit controls.
async function drain(page) {
  await page.evaluate(
    () =>
      new Promise((resolve) => {
        requestAnimationFrame(() =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        );
      }),
  );
}

async function runtime(page) {
  await page.addScriptTag({ content: sources.get(gpt) });
  assert.equal(
    await page.evaluate(() => tsjs.tsRenderBridgeVersion),
    1,
    "The actual runtime bridge must be installed",
  );
}

async function mount(
  page,
  slotId = "header",
  eligible = [[300, 250]],
  register = true,
  nativeFormats,
) {
  return page.evaluate(
    ({ slotId, eligible, register, nativeFormats }) => {
      const element = document.createElement("div");
      // Deliberately outside the configured prefix. Render lookup must be exact.
      element.id = "publisher-final-" + slotId;
      element.style.cssText = "width:300px;height:250px";
      document.getElementById("mount").append(element);
      const native = googletag
        .defineSlot(
          "/123/example-placement",
          nativeFormats ?? (eligible.length ? eligible : [[300, 250]]),
          element.id,
        )
        .addService(googletag.pubads());
      native.setTargeting("publisher_key", "keep");
      const options = {
        slotId,
        gptSlot: native,
        navigationGeneration: tsjs.navGeneration ?? 0,
        eligibleFormats: eligible,
      };
      const reference = { element, native, options };
      if (register) reference.handle = tsjs.registerPublisherSlot(options);
      window.publisherRefs[slotId] = reference;
      return reference.handle?.getStatus();
    },
    { slotId, eligible, register, nativeFormats },
  );
}

async function accept(page, data, seam = "shared") {
  await page.evaluate(
    ({ slots, bids, seam }) => {
      if (seam === "inline") {
        tsjs.adSlots = slots;
        tsjs.scheduleInitialAdInit(bids);
      } else tsjs.scheduleInitialAdInit(bids, slots);
    },
    { ...data, seam },
  );
}

async function noEarly(page, label) {
  await drain(page);
  const facts = await page.evaluate(() => ({
    requests: fixture.requests.length,
    definitions: fixture.trace.filter(
      (entry) => entry.event === "nativeDefine" && entry.id !== "unrelated",
    ).length,
  }));
  assert.equal(facts.requests, 0, label + ": no request");
  assert.equal(facts.definitions, 0, label + ": no fallback definition");
}

async function status(page, slotId = "header") {
  return page.evaluate(
    (slotId) => publisherRefs[slotId].handle.getStatus(),
    slotId,
  );
}

async function assertDelivery(
  page,
  slotId,
  candidate,
  { serviceChanges = false, noFill = false } = {},
) {
  await page.waitForFunction(
    (slotId) =>
      fixture.requests.some(
        (entry) => entry.id === publisherRefs[slotId].element.id,
      ),
    slotId,
  );
  await drain(page);
  const facts = await page.evaluate((slotId) => {
    const { native, element, handle } = publisherRefs[slotId];
    const nativeIndex = fixture.nativeSlots.indexOf(native);
    const entries = fixture.requests.filter(
      (entry) => entry.native === nativeIndex,
    );
    const calls = fixture.trace.slice(window.publisherReadyTrace);
    return {
      entries,
      nativeIndex,
      exactNative: entries.every(
        (entry) => fixture.nativeSlots[entry.native] === native,
      ),
      exactElement: entries.every(
        (entry) =>
          fixture.elements[entry.element] === element &&
          document.getElementById(entry.id) === element,
      ),
      displays: calls.filter(
        (entry) =>
          entry.event === "nativeDisplay" &&
          entry.id === element.id &&
          entry.native === nativeIndex,
      ),
      refreshes: calls.filter(
        (entry) =>
          entry.event === "nativeRefresh" &&
          entry.natives.includes(nativeIndex),
      ),
      definitions: calls.filter(
        (entry) =>
          entry.event === "nativeDefine" && entry.native === nativeIndex,
      ),
      serviceChanges: calls.filter((entry) =>
        [
          "enableServices",
          "enableSingleRequest",
          "disableInitialLoad",
          "setConfig",
        ].includes(entry.event),
      ),
      destroyed: calls.filter(
        (entry) =>
          entry.event === "nativeDestroy" && entry.native === nativeIndex,
      ),
      mapped: tsjs.divToSlotId?.[element.id],
      status: handle.getStatus(),
      owned: (tsjs.prevGptSlots ?? []).includes(native),
      unrelatedRequests: fixture.requests.filter(
        (entry) => entry.id === "unrelated",
      ).length,
      auctions: calls.filter((entry) => entry.event === "auctionInit").length,
    };
  }, slotId);
  assert.equal(facts.entries.length, 1, "One per-slot native initial request");
  assert(
    facts.exactNative && facts.exactElement,
    "Native object and final element identity must both match independently",
  );
  assert.equal(
    facts.definitions.length,
    1,
    "Only the publisher definition, no TS fallback",
  );
  assert.equal(
    facts.displays.length,
    1,
    "Exactly one native display(actualId)",
  );
  assert.equal(
    facts.refreshes.length,
    1,
    "Exactly one explicit native refresh",
  );
  assert.deepEqual(
    facts.refreshes[0].natives,
    [facts.nativeIndex],
    "Refresh list contains only the exact offered native object",
  );
  assert(facts.refreshes[0].scoped, "No global refresh");
  assert.equal(
    facts.entries[0].source,
    "refresh",
    "Disabled-load display must not fetch",
  );
  assert.equal(
    facts.destroyed.length,
    0,
    "TS never destroys publisher native slots",
  );
  assert.equal(
    facts.owned,
    false,
    "Publisher slot excluded from TS destruction list",
  );
  assert.equal(
    facts.unrelatedRequests,
    0,
    "Unrelated unfetched SRA slot isolated",
  );
  assert.equal(
    facts.auctions,
    0,
    "Real Prebid bypasses the delegated initial refresh",
  );
  if (!serviceChanges)
    assert.deepEqual(
      facts.serviceChanges,
      [],
      "Registered-only delivery makes no service or policy calls",
    );
  assert.equal(
    facts.mapped,
    slotId,
    "Exact outside-prefix final ID retained in shared mapping",
  );
  const targeting = facts.entries[0].targeting;
  assert.deepEqual(
    targeting.publisher_key,
    ["keep"],
    "Publisher targeting preserved at native invocation",
  );
  assert.deepEqual(
    targeting.ts_initial,
    ["1"],
    "Initial targeting present before native invocation",
  );
  assert.deepEqual(targeting.example_section, ["fiction"]);
  assert.equal(
    !!targeting.hb_adid,
    candidate,
    "Only eligible candidates get TS targeting",
  );
  if (!candidate)
    assert(
      !Object.keys(targeting).some((key) => key.startsWith("hb_")),
      "Rejected candidates leave no managed candidate keys",
    );
  assert(
    ["requested", "rendered"].includes(facts.status.status),
    "Discriminated handle status after native delivery",
  );
  if (noFill) {
    await page.evaluate(
      (index) => fixture.render(index, { noFill: true }),
      facts.entries[0].index,
    );
    assert.equal(
      await page.locator("#publisher-final-" + slotId + " iframe").count(),
      0,
      "Explicit GAM no-fill is empty, not a TS rendering failure",
    );
  } else
    await render(
      page,
      facts.entries[0].index,
      candidate ? "Server result" : "Publisher result",
    );
  return facts;
}

const commonCases = [
  "registration-first-inline",
  "registration-first-shared",
  "payload-first-inline",
  "payload-first-shared",
  "missing-placeholder",
  "ssr-placeholder",
  "head-only",
  "admitted-empty",
  "repeated-registration",
  "reordered-formats",
  "narrowed-incompatible",
  "missing-bid",
  "unknown-dimensions",
  "empty-eligibility",
  "fluid-only",
  "explicit-no-fill",
  "two-same-path",
  "cancel-pending",
  "cancel-replace-eligibility",
  "hidden-change-eligibility",
  "cancel-frame-replacement",
  "policy-enabled",
  "policy-unknown",
  "policy-changed-pending",
  "element-lost",
  "native-destroyed",
  "cancel-after-dispatch",
  "registered-before-auto",
  "auto-before-registered",
  "empty-automatic-pass",
  "later-publisher-refresh",
  "publisher-first",
];
function fixedCases(implementation) {
  return [
    ...commonCases,
    ...(implementation !== "bootstrap"
      ? ["navigation-stale", "mixed-navigation-cleanup", "runtime-reinstall"]
      : ["stale-generation", "bootstrap-history-a-b-a"]),
    ...(implementation === "handoff"
      ? [
          "late-runtime-no-resurrection",
          "cancel-across-handoff",
          "history-runtime-catchup",
          "history-catchup-stale-b",
          "history-catchup-failure-retry",
        ]
      : []),
  ];
}

async function fixed(page, scenario, implementation, transport) {
  assert.equal(
    await page.evaluate(() => tsjs.publisherSlotRegistrationVersion),
    1,
    "Publisher registration API version 1 must exist",
  );
  assert.equal(
    await page.evaluate(() => typeof tsjs.registerPublisherSlot),
    "function",
    "Publisher registration API must exist",
  );
  assert.equal(
    await page.evaluate(() => !!googletag.pubads().__tsRefreshWrapped),
    true,
    "Actual Prebid refresh wrapper installed",
  );
  await page.evaluate(() => {
    window.publisherRefs = {};
    window.publisherReadyTrace = fixture.trace.length;
  });
  const data = payload({
    two: scenario === "two-same-path",
    candidate: [
      "narrowed-incompatible",
      "cancel-replace-eligibility",
      "hidden-change-eligibility",
      "cancel-frame-replacement",
    ].includes(scenario)
      ? "incompatible"
      : scenario === "missing-bid"
        ? "none"
        : scenario === "unknown-dimensions"
          ? "unknown"
          : "fit",
  });
  let candidate =
    implementation !== "bootstrap" &&
    ![
      "narrowed-incompatible",
      "missing-bid",
      "unknown-dimensions",
      "empty-eligibility",
      "fluid-only",
      "cancel-replace-eligibility",
      "hidden-change-eligibility",
      "cancel-frame-replacement",
    ].includes(scenario);
  const takeover = async () => {
    if (implementation === "handoff") await runtime(page);
  };

  if (
    scenario === "bootstrap-history-a-b-a" ||
    scenario === "history-runtime-catchup" ||
    scenario === "history-catchup-stale-b" ||
    scenario === "history-catchup-failure-retry"
  ) {
    // A was admitted, but no publisher registered or requested on A. A stale
    // generation would therefore be able to spend that payload on B.
    await accept(page, data);
    await noEarly(page, "Admitted A before any publisher registration");
    await page.evaluate(() => history.pushState({}, "", "/route-b"));
    assert.equal(
      await page.evaluate(() => tsjs.navGeneration),
      1,
      "Bootstrap history push advances generation before runtime exists",
    );
    await mount(page);
    await page.evaluate(() => {
      window.pendingBHandle = publisherRefs.header.handle;
      window.pendingBToken =
        tsjs.publisherSlotRegistrationState.records.header.token;
      window.pendingBNative = publisherRefs.header.native;
      window.pendingBElement = publisherRefs.header.element;
      window.bootstrapPushState = history.pushState;
      window.bootstrapReplaceState = history.replaceState;
    });
    await drain(page);
    assert.equal((await status(page)).status, "waiting_payload");
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      0,
      "B cannot spend admitted A payload",
    );
    assert.equal(
      transport.network.filter(
        (entry) => new URL(entry.url).pathname === "/_ts/page-bids",
      ).length,
      0,
      "Bootstrap history invalidation does not fetch",
    );

    if (scenario === "bootstrap-history-a-b-a") {
      await page.evaluate(() => history.back());
      await page.waitForFunction(
        () => location.pathname === "/" && tsjs.navGeneration === 2,
      );
      await page.evaluate(() => {
        googletag.destroySlots([publisherRefs.header.native]);
        publisherRefs.header.element.remove();
      });
      await mount(page);
      // Force a stale document seam after real popstate. It cannot settle A2.
      await accept(page, data);
      await drain(page);
      assert.equal(
        await page.evaluate(() => pendingBHandle.getStatus().status),
        "cancelled",
      );
      assert.equal((await status(page)).status, "waiting_payload");
      const facts = await page.evaluate(() => ({
        path: location.pathname,
        generation: tsjs.navGeneration,
        requests: fixture.requests.length,
        nativeDeliveryCalls: fixture.trace.filter((entry) =>
          ["nativeDisplay", "nativeRefresh"].includes(entry.event),
        ).length,
      }));
      assert.equal(
        facts.requests,
        0,
        "Returning to A requires generation-2 payload, not retained generation-0 SSR data",
      );
      assert.equal(facts.nativeDeliveryCalls, 0);
      assert.equal(
        transport.network.filter(
          (entry) => new URL(entry.url).pathname === "/_ts/page-bids",
        ).length,
        0,
      );
      return {
        facts,
        status: await status(page),
        oldBStatus: await page.evaluate(() => pendingBHandle.getStatus()),
      };
    }

    if (
      scenario === "history-catchup-stale-b" ||
      scenario === "history-catchup-failure-retry"
    )
      await page.evaluate((scenario) => {
        // Record real response completion. Only the stale-response case ignores
        // B's AbortController so its actual response can arrive after C.
        const nativeFetch = window.fetch.bind(window);
        window.fetch = (input, init) => {
          const url = new URL(
            typeof input === "string" ? input : input.url,
            location.href,
          );
          const heldB =
            url.pathname === "/_ts/page-bids" &&
            url.searchParams.get("path") === "/route-b";
          return nativeFetch(
            input,
            heldB && scenario === "history-catchup-stale-b"
              ? { ...init, signal: undefined }
              : init,
          ).then((response) => {
            if (heldB)
              fixture.record("controlledCatchupResponse", {
                path: "/route-b",
                status: response.status,
              });
            return response;
          });
        };
      }, scenario);
    await runtime(page);
    const bRoute = await transport.next("/_ts/page-bids");
    assert.equal(
      new URL(bRoute.request().url()).searchParams.get("path"),
      "/route-b",
      "Late runtime catches up the current path",
    );
    const adoption = await page.evaluate(() => ({
      generation: tsjs.navGeneration,
      sameHandle: publisherRefs.header.handle === pendingBHandle,
      sameToken:
        tsjs.publisherSlotRegistrationState.records.header.token ===
        pendingBToken,
      samePush: history.pushState === bootstrapPushState,
      sameReplace: history.replaceState === bootstrapReplaceState,
      status: pendingBHandle.getStatus(),
      requests: fixture.requests.length,
    }));
    assert.equal(
      adoption.generation,
      1,
      "Takeover cannot invalidate B or advance generation twice",
    );
    assert(
      adoption.sameHandle && adoption.sameToken,
      "Takeover adopts B record and handle",
    );
    assert(
      adoption.samePush && adoption.sameReplace,
      "One shared history bridge, not a second runtime wrapper",
    );
    assert.equal(adoption.status.status, "waiting_payload");
    assert.equal(
      adoption.requests,
      0,
      "Catchup does not admit before the accepted response",
    );
    const bData = structuredClone(data);
    bData.bids.header.hb_adid = "example-b-header";
    if (scenario === "history-catchup-failure-retry") {
      await bRoute.fulfill({
        status: 500,
        contentType: "application/json",
        body: JSON.stringify({ error: "fixture-unavailable" }),
      });
      await page.waitForFunction(() =>
        fixture.trace.some(
          (entry) =>
            entry.event === "controlledCatchupResponse" && entry.status === 500,
        ),
      );
      await drain(page);
      assert.equal(
        (await status(page)).status,
        "waiting_payload",
        "Failed catchup does not admit B",
      );
      assert.equal(await page.evaluate(() => fixture.requests.length), 0);
      await page.evaluate(() =>
        history.replaceState({}, "", "/route-b?retry=1#pending"),
      );
      const retryRoute = await transport.next("/_ts/page-bids");
      assert.equal(
        new URL(retryRoute.request().url()).searchParams.get("path"),
        "/route-b",
      );
      const retry = await page.evaluate(() => ({
        generation: tsjs.navGeneration,
        sameHandle: publisherRefs.header.handle === pendingBHandle,
        sameToken:
          tsjs.publisherSlotRegistrationState.records.header.token ===
          pendingBToken,
        sameNative:
          publisherRefs.header.native === pendingBNative &&
          googletag.pubads().getSlots().includes(pendingBNative),
        sameElement:
          publisherRefs.header.element === pendingBElement &&
          document.getElementById(pendingBElement.id) === pendingBElement,
        status: pendingBHandle.getStatus(),
      }));
      assert.equal(
        retry.generation,
        1,
        "Same-path retry preserves the provisional generation",
      );
      assert(
        retry.sameHandle &&
          retry.sameToken &&
          retry.sameNative &&
          retry.sameElement,
        "Retry preserves pending B record, native and element instead of invalidating again",
      );
      assert.equal(retry.status.status, "waiting_payload");
      await retryRoute.fulfill({
        contentType: "application/json",
        body: JSON.stringify(bData),
      });
      const facts = await assertDelivery(page, "header", true);
      assert.deepEqual(facts.entries[0].targeting.hb_adid, [
        "example-b-header",
      ]);
      assert.equal(
        await page.evaluate(() => pendingBHandle.getStatus().status),
        "rendered",
      );
      await page.evaluate(() =>
        history.replaceState({}, "", "/route-b?retry=2"),
      );
      await drain(page);
      assert.equal(await page.evaluate(() => tsjs.navGeneration), 1);
      assert.equal(await page.evaluate(() => fixture.requests.length), 1);
      assert.equal(
        transport.network.filter(
          (entry) => new URL(entry.url).pathname === "/_ts/page-bids",
        ).length,
        2,
        "One failed catchup and one explicit same-path retry, not a post-delivery refetch",
      );
      return { adoption, retry, facts };
    }
    if (scenario === "history-runtime-catchup") {
      await bRoute.fulfill({
        contentType: "application/json",
        body: JSON.stringify(bData),
      });
      const facts = await assertDelivery(page, "header", true);
      assert.deepEqual(facts.entries[0].targeting.hb_adid, [
        "example-b-header",
      ]);
      assert.equal(
        await page.evaluate(() => pendingBHandle.getStatus().status),
        "rendered",
        "The original waiting handle observes the adopted delivery",
      );
      await runtime(page);
      await page.evaluate(() => history.replaceState({}, "", "/route-b"));
      await drain(page);
      assert.equal(await page.evaluate(() => tsjs.navGeneration), 1);
      assert.equal(
        await page.evaluate(() => fixture.requests.length),
        1,
        "Reinstallation/same-path history cannot repeat initial delivery",
      );
      assert.equal(
        transport.network.filter(
          (entry) => new URL(entry.url).pathname === "/_ts/page-bids",
        ).length,
        1,
        "Exactly one catchup fetch for the already advanced generation",
      );
      return { adoption, facts };
    }
    await page.evaluate(() => history.pushState({}, "", "/route-c"));
    const cRoute = await transport.next("/_ts/page-bids");
    assert.equal(
      new URL(cRoute.request().url()).searchParams.get("path"),
      "/route-c",
    );
    assert.equal(
      await page.evaluate(() => tsjs.navGeneration),
      2,
      "Runtime owns C transition once after bootstrap takeover",
    );
    await page.evaluate(() => {
      googletag.destroySlots([publisherRefs.header.native]);
      publisherRefs.header.element.remove();
    });
    await mount(page);
    await bRoute.fulfill({
      contentType: "application/json",
      body: JSON.stringify(bData),
    });
    await page.waitForFunction(() =>
      fixture.trace.some(
        (entry) => entry.event === "controlledCatchupResponse",
      ),
    );
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      0,
      "A forced successful late B response cannot authorize C",
    );
    assert.equal((await status(page)).status, "waiting_payload");
    assert.equal(
      await page.evaluate(() => pendingBHandle.getStatus().status),
      "cancelled",
    );
    const cData = structuredClone(data);
    cData.bids.header.hb_adid = "example-c-header";
    await cRoute.fulfill({
      contentType: "application/json",
      body: JSON.stringify(cData),
    });
    const facts = await assertDelivery(page, "header", true);
    assert.deepEqual(facts.entries[0].targeting.hb_adid, ["example-c-header"]);
    assert.equal(
      transport.network.filter(
        (entry) => new URL(entry.url).pathname === "/_ts/page-bids",
      ).length,
      2,
      "Only B catchup and C navigation fetch",
    );
    return {
      adoption,
      facts,
      oldBStatus: await page.evaluate(() => pendingBHandle.getStatus()),
    };
  }

  if (scenario.startsWith("policy-") && scenario !== "policy-changed-pending") {
    await takeover();
    await accept(page, data);
    const result = await mount(page);
    assert.equal(
      result.status,
      "rejected",
      "Enabled or unverifiable load policy rejects before request",
    );
    await drain(page);
    assert.equal(await page.evaluate(() => fixture.requests.length), 0);
    assert.equal(
      await page.evaluate(
        () =>
          fixture.trace.filter((entry) => entry.event === "nativeDisplay")
            .length,
      ),
      0,
    );
    return { status: result };
  }
  if (scenario === "head-only") {
    await takeover();
    await page.evaluate(({ slots }) => {
      tsjs.adSlots = slots;
      tsjs.scheduleInitialAdInit();
    }, data);
    await mount(page);
    await drain(page);
    assert.equal(
      (await status(page)).status,
      "waiting_payload",
      "Head configuration/noargs scheduler does not authorize registered delivery",
    );
    assert.equal(await page.evaluate(() => fixture.requests.length), 0);
    return { status: await status(page) };
  }
  if (scenario === "admitted-empty") {
    await mount(page);
    await takeover();
    await accept(page, { slots: [], bids: {} });
    await drain(page);
    assert.equal((await status(page)).status, "inactive");
    assert.equal(await page.evaluate(() => fixture.requests.length), 0);
    return { status: await status(page) };
  }
  if (scenario === "missing-placeholder" || scenario === "ssr-placeholder") {
    if (scenario === "ssr-placeholder")
      await page.evaluate(() => {
        const placeholder = document.createElement("div");
        placeholder.id = "example-header-ssr";
        placeholder.style.cssText = "width:300px;height:250px";
        document.getElementById("mount").append(placeholder);
      });
    await accept(page, data);
    await noEarly(
      page,
      "Admitted registered slot without final publisher mount",
    );
    await takeover();
    await page.evaluate(() =>
      document.getElementById("example-header-ssr")?.remove(),
    );
    await mount(page);
  } else if (
    scenario === "payload-first-shared" ||
    scenario === "payload-first-inline"
  ) {
    await accept(
      page,
      data,
      scenario === "payload-first-inline" ? "inline" : "shared",
    );
    await noEarly(page, "Shared seam admitted before registration");
    await takeover();
    await mount(page);
  } else if (
    scenario === "registered-before-auto" ||
    scenario === "auto-before-registered" ||
    scenario === "mixed-navigation-cleanup"
  ) {
    const automatic = {
      ...data.slots[0],
      id: "automatic",
      div_id: "automatic-slot",
    };
    delete automatic.initialization;
    data.slots.push(automatic);
    data.bids.automatic = {
      ...data.bids.header,
      hb_adid: "example-ad-automatic",
    };
    await page.evaluate(() => {
      const element = document.createElement("div");
      element.id = "automatic-slot";
      element.style.cssText = "width:300px;height:250px";
      document.getElementById("mount").append(element);
    });
    if (scenario === "registered-before-auto") await mount(page);
    await takeover();
    await accept(page, data);
    if (scenario !== "registered-before-auto") {
      await page.waitForFunction(() =>
        fixture.requests.some((entry) => entry.id === "automatic-slot"),
      );
      assert.equal(
        await page.evaluate(() =>
          fixture.requests.some(
            (entry) => entry.id === "publisher-final-header",
          ),
        ),
        false,
      );
      await mount(page);
    }
    await page.waitForFunction(() => fixture.requests.length === 2);
    const facts = await assertDelivery(page, "header", candidate, {
      serviceChanges: true,
    });
    const mapping = await page.evaluate(() => tsjs.divToSlotId);
    assert.equal(
      mapping["automatic-slot"],
      "automatic",
      "Registered writer retains automatic mapping",
    );
    assert.equal(
      mapping["publisher-final-header"],
      "header",
      "Automatic writer retains registered mapping",
    );
    const order = await page.evaluate(() =>
      fixture.requests.map((entry) => entry.id),
    );
    assert.equal(
      order[0],
      scenario === "registered-before-auto"
        ? "publisher-final-header"
        : "automatic-slot",
    );
    if (scenario === "mixed-navigation-cleanup") {
      await page.evaluate(() => history.pushState({}, "", "/next-route"));
      const route = await transport.next("/_ts/page-bids");
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ slots: [], bids: {} }),
      });
      await page.waitForFunction(
        () => !publisherRefs.header.native.getTargeting("ts_initial").length,
      );
      const cleanup = await page.evaluate(() => ({
        targeting: fixture.snapshot(publisherRefs.header.native).targeting,
        mapped: tsjs.divToSlotId,
        live: googletag
          .pubads()
          .getSlots()
          .includes(publisherRefs.header.native),
      }));
      assert.equal(
        cleanup.live,
        true,
        "Navigation does not destroy publisher slot",
      );
      assert.deepEqual(cleanup.targeting.publisher_key, ["keep"]);
      assert.equal(cleanup.targeting.hb_adid, undefined);
      assert.equal(cleanup.targeting.example_section, undefined);
      assert.equal(cleanup.mapped["publisher-final-header"], undefined);
      return { facts, order, cleanup };
    }
    return { facts, order, mapping };
  } else if (
    scenario === "cancel-pending" ||
    scenario === "cancel-across-handoff"
  ) {
    await mount(page);
    await page.evaluate(() => {
      publisherRefs.header.handle.cancel();
      googletag.destroySlots([publisherRefs.header.native]);
      fixture.record("publisherRemove", {
        connectedBefore: publisherRefs.header.element.isConnected,
      });
      publisherRefs.header.element.remove();
    });
    await takeover();
    await accept(page, data);
    await drain(page);
    assert.equal((await status(page)).status, "cancelled");
    assert.equal(await page.evaluate(() => fixture.requests.length), 0);
    const events = await page.evaluate(() =>
      fixture.trace.filter((entry) =>
        ["nativeDestroy", "publisherRemove"].includes(entry.event),
      ),
    );
    assert.deepEqual(
      events.map((entry) => entry.event),
      ["nativeDestroy", "publisherRemove"],
    );
    assert(
      events[0].connected && events[1].connectedBefore,
      "Destroy occurs before DOM removal",
    );
    return { status: await status(page), events };
  } else if (
    scenario === "cancel-replace-eligibility" ||
    scenario === "hidden-change-eligibility" ||
    scenario === "cancel-frame-replacement"
  ) {
    await takeover();
    await page.evaluate(() => {
      window.testVisibilityCallbacks = [];
      for (const target of [window, document]) {
        const add = target.addEventListener.bind(target);
        target.addEventListener = (name, fn, options) => {
          if (name === "visibilitychange") testVisibilityCallbacks.push(fn);
          return add(name, fn, options);
        };
      }
      window.fixtureHidden = false;
      Object.defineProperty(document, "hidden", {
        configurable: true,
        get: () => fixtureHidden,
      });
      Object.defineProperty(document, "visibilityState", {
        configurable: true,
        get: () => (fixtureHidden ? "hidden" : "visible"),
      });
    });
    if (scenario === "hidden-change-eligibility")
      await page.evaluate(() => {
        window.fixtureHidden = true;
      });
    await mount(page, "header", [
      [300, 250],
      [300, 600],
    ]);
    if (scenario === "hidden-change-eligibility") {
      await accept(page, data);
      await drain(page);
      assert.equal(
        await page.evaluate(() => fixture.requests.length),
        0,
        "Hidden document cannot dispatch",
      );
    }
    await page.evaluate(
      ({ data, frameCase }) => {
        window.oldHandle = publisherRefs.header.handle;
        const staleVisibility = [...testVisibilityCallbacks];
        const oldRaf = window.requestAnimationFrame;
        const staleFrames = [];
        if (frameCase) {
          window.requestAnimationFrame = (fn) => {
            staleFrames.push(fn);
            return oldRaf.call(window, fn);
          };
          tsjs.scheduleInitialAdInit(data.bids, data.slots);
        }
        oldHandle.cancel();
        publisherRefs.header.options.eligibleFormats = [[300, 250]];
        publisherRefs.header.handle = tsjs.registerPublisherSlot(
          publisherRefs.header.options,
        );
        // Force old token callbacks and old-handle cancellation after replacement.
        window.fixtureHidden = false;
        for (const fn of staleVisibility) fn(new Event("visibilitychange"));
        for (const fn of staleFrames) fn(performance.now());
        window.requestAnimationFrame = oldRaf;
        oldHandle.cancel();
        document.dispatchEvent(
          new Event("visibilitychange", { bubbles: true }),
        );
      },
      { data, frameCase: scenario === "cancel-frame-replacement" },
    );
    if (scenario === "cancel-replace-eligibility") await accept(page, data);
    await assertDelivery(page, "header", false);
    assert.equal(
      await page.evaluate(() => oldHandle.getStatus().status),
      "cancelled",
    );
    assert.notEqual(
      (await status(page)).status,
      "cancelled",
      "Old handle cannot cancel replacement",
    );
    return {
      status: await status(page),
      oldStatus: await page.evaluate(() => oldHandle.getStatus()),
    };
  } else if (
    [
      "policy-changed-pending",
      "element-lost",
      "native-destroyed",
      "stale-generation",
    ].includes(scenario)
  ) {
    await takeover();
    await mount(page);
    await page.evaluate((scenario) => {
      if (scenario === "policy-changed-pending") fixture.setPolicy(false);
      if (scenario === "element-lost") publisherRefs.header.element.remove();
      if (scenario === "native-destroyed")
        googletag.destroySlots([publisherRefs.header.native]);
      if (scenario === "stale-generation") tsjs.navGeneration = 1;
    }, scenario);
    await accept(page, data);
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      0,
      "Dispatch rechecks stale policy, element, native and generation",
    );
    assert(["rejected", "cancelled"].includes((await status(page)).status));
    return { status: await status(page) };
  } else if (scenario === "navigation-stale") {
    await takeover();
    await mount(page);
    await page.evaluate(() => {
      window.oldHandle = publisherRefs.header.handle;
      history.pushState({}, "", "/route-a");
    });
    const oldRoute = await transport.next("/_ts/page-bids");
    await page.evaluate(() => history.pushState({}, "", "/route-b"));
    const newRoute = await transport.next("/_ts/page-bids");
    await page.evaluate(() => {
      googletag.destroySlots([publisherRefs.header.native]);
      publisherRefs.header.element.remove();
    });
    await mount(page);
    await oldRoute
      .fulfill({ contentType: "application/json", body: JSON.stringify(data) })
      .catch(() => {});
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      0,
      "Stale old navigation cannot authorize delivery",
    );
    await newRoute.fulfill({
      contentType: "application/json",
      body: JSON.stringify(data),
    });
    await page.evaluate(() => oldHandle.cancel());
    const facts = await assertDelivery(page, "header", true);
    assert.equal(
      await page.evaluate(() => oldHandle.getStatus().status),
      "cancelled",
    );
    return {
      facts,
      oldStatus: await page.evaluate(() => oldHandle.getStatus()),
    };
  } else if (scenario === "publisher-first") {
    await takeover();
    await mount(page, "header", [[300, 250]], false);
    await page.evaluate(() => {
      pbjs.requestBids({
        adUnits: [
          {
            code: publisherRefs.header.element.id,
            mediaTypes: { banner: { sizes: [[300, 250]] } },
            bids: [],
          },
        ],
        timeout: 15000,
        bidsBackHandler() {
          window.publisherAuctionDone = true;
        },
      });
    });
    const auction = await transport.next("/auction");
    await accept(page, data);
    await page.evaluate(() => {
      publisherRefs.header.handle = tsjs.registerPublisherSlot(
        publisherRefs.header.options,
      );
    });
    await drain(page);
    assert.equal((await status(page)).status, "publisher_owned");
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      0,
      "No competing TS request after publisher claim",
    );
    assert.equal(
      await page.evaluate(
        () => publisherRefs.header.native.getTargeting("ts_initial").length,
      ),
      0,
      "No overwritten targeting",
    );
    await auction.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ id: "example-auction", seatbid: [], ext: {} }),
    });
    await page.waitForFunction(() => window.publisherAuctionDone === true);
    return { status: await status(page) };
  } else if (scenario === "late-runtime-no-resurrection") {
    await mount(page);
    await accept(page, data);
    const before = await assertDelivery(page, "header", false);
    assert.equal(
      await page.evaluate(() => tsjs.tsRenderBridgeVersion),
      undefined,
      "Bootstrap did not claim a renderer",
    );
    await runtime(page);
    await runtime(page);
    await page.evaluate(() => {
      tsjs.adInit();
    });
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      1,
      "Late runtime cannot request again",
    );
    assert.equal(
      transport.network.filter(
        (entry) => new URL(entry.url).pathname === "/_ts/page-bids",
      ).length,
      0,
      "No navigation means no startup catchup fetch",
    );
    assert.equal(
      await page.evaluate(
        () => publisherRefs.header.native.getTargeting("hb_adid").length,
      ),
      0,
      "No retroactive TS candidate targeting",
    );
    // Real MessageChannel request from the already displayed ordinary creative.
    // A two-frame message-processing fence observes the inline bid-response path;
    // no network/cache response is needed because this retained bid has inline ADM.
    const ordinary = await page
      .locator("#publisher-final-header iframe")
      .elementHandle()
      .then((element) => element.contentFrame());
    const response = await ordinary.evaluate(
      () =>
        new Promise((resolve) => {
          const channel = new MessageChannel();
          let answered = false;
          channel.port1.onmessage = () => {
            answered = true;
          };
          top.postMessage(
            { message: "Prebid Request", adId: "example-ad-header" },
            "*",
            [channel.port2],
          );
          requestAnimationFrame(() =>
            requestAnimationFrame(() => {
              channel.port1.close();
              resolve(answered);
            }),
          );
        }),
    );
    assert.equal(
      response,
      false,
      "Late bridge must not resurrect the excluded retained candidate",
    );
    assert.equal(
      transport.network.filter((entry) =>
        new URL(entry.url).pathname.startsWith("/fixture-notice/"),
      ).length,
      0,
      "Excluded retained candidate cannot fire win or billing beacons",
    );
    return {
      before,
      excludedCandidateResponse: response,
      status: await status(page),
    };
  } else {
    const eligible =
      scenario === "reordered-formats"
        ? [
            [300, 600],
            [300, 250],
          ]
        : scenario === "empty-eligibility" || scenario === "fluid-only"
          ? []
          : [[300, 250]];
    await mount(
      page,
      "header",
      eligible,
      true,
      scenario === "fluid-only" ? ["fluid"] : undefined,
    );
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      0,
      "Registration alone cannot authorize delivery",
    );
    await takeover();
    await accept(
      page,
      data,
      scenario === "registration-first-inline" ? "inline" : "shared",
    );
    if (scenario === "two-same-path")
      await mount(page, "sidebar", [
        [300, 600],
        [300, 250],
      ]);
  }

  if (scenario === "repeated-registration") {
    await page.evaluate(() => {
      window.repeatedHandle = tsjs.registerPublisherSlot({
        ...publisherRefs.header.options,
        eligibleFormats: [
          [300, 250],
          [300, 250],
        ],
      });
    });
  }
  if (scenario === "empty-automatic-pass") {
    await page.waitForFunction(() => fixture.requests.length === 1);
    await page.evaluate(() => tsjs.adInit());
    await drain(page);
  }
  const facts = await assertDelivery(page, "header", candidate, {
    noFill: scenario === "explicit-no-fill",
  });
  if (scenario === "two-same-path") {
    const sidebar = await assertDelivery(page, "sidebar", candidate);
    assert.notEqual(
      facts.nativeIndex,
      sidebar.nativeIndex,
      "Same GAM path does not deduplicate separate placements",
    );
    return { header: facts, sidebar };
  }
  if (scenario === "repeated-registration") {
    assert.equal(
      await page.evaluate(() => repeatedHandle.getStatus().status),
      (await status(page)).status,
    );
    await drain(page);
    assert.equal(await page.evaluate(() => fixture.requests.length), 1);
  }
  if (scenario === "runtime-reinstall") {
    await runtime(page);
    await runtime(page);
    await page.evaluate(() =>
      tsjs.registerPublisherSlot(publisherRefs.header.options),
    );
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      1,
      "Repeated runtime installation adopts consumed registration",
    );
  }
  if (scenario === "cancel-after-dispatch") {
    await page.evaluate(() => {
      publisherRefs.header.handle.cancel();
      googletag.destroySlots([publisherRefs.header.native]);
      publisherRefs.header.element.remove();
    });
    await mount(page);
    await drain(page);
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      1,
      "Removed consumed placement cannot reopen an initial request",
    );
    return {
      facts,
      replacementStatus: await status(page),
      supportedBoundary:
        "Publisher owns teardown; no ad-preservation promise after subtree replacement.",
    };
  }
  if (scenario === "later-publisher-refresh") {
    await page.evaluate(() =>
      googletag
        .pubads()
        .refresh([publisherRefs.header.native], { changeCorrelator: false }),
    );
    const auction = await transport.next("/auction");
    assert.equal(
      await page.evaluate(() => fixture.requests.length),
      1,
      "Later refresh waits for real Prebid auction",
    );
    const request = auction.request().postDataJSON();
    const impid = request.adUnits?.[0]?.code;
    assert(impid, "Real trustedServer adapter sends AdRequest ad-unit codes");
    await auction.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        id: request.id ?? "example-auction",
        seatbid: [
          {
            seat: "exampleClient",
            bid: [
              {
                id: "example-client-bid",
                impid,
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
    await page.waitForFunction(() => fixture.requests.length === 2);
    const later = await page.evaluate(() => ({
      request: fixture.requests[1],
      auctions: fixture.trace.filter((entry) => entry.event === "auctionInit")
        .length,
    }));
    assert.equal(
      later.auctions,
      1,
      "A genuinely later refresh traverses actual Prebid",
    );
    assert.deepEqual(later.request.options, { changeCorrelator: false });
    assert.equal(later.request.native, facts.nativeIndex);
    assert.deepEqual(later.request.targeting.publisher_key, ["keep"]);
    assert.equal(
      await page.evaluate(
        () =>
          fixture.requests.filter((entry) => entry.id === "unrelated").length,
      ),
      0,
    );
    await render(page, 1, "Client result");
    return { facts, later, auctionRequest: request };
  }
  return { facts };
}

(async () => {
  const browser = await chromium.launch({ headless: true });
  artifacts.browserVersion = browser.version();
  save("artifacts.json", artifacts);
  try {
    for (const implementation of implementations) {
      const scenarios = options.baseline
        ? [
            "replace-identical",
            "replace-reordered",
            "replace-narrowed",
            "rename-identical",
            "rename-reordered",
            "rename-narrowed",
          ]
        : fixedCases(implementation);
      for (const scenario of scenarios) {
        const started = performance.now();
        const name = implementation + "-" + scenario;
        const context = await browser.newContext({
          viewport: { width: 1000, height: 900 },
        });
        await context.tracing.start({
          screenshots: true,
          snapshots: true,
          sources: true,
        });
        const page = await context.newPage();
        const errors = [];
        const consoleErrors = [];
        const network = [];
        const held = [];
        const waiters = [];
        const transport = {
          network,
          next(pathname) {
            const index = held.findIndex(
              (route) => new URL(route.request().url()).pathname === pathname,
            );
            if (index !== -1) return Promise.resolve(held.splice(index, 1)[0]);
            return new Promise((resolve, reject) => {
              const waiter = { pathname, resolve };
              waiter.timer = setTimeout(() => {
                waiters.splice(waiters.indexOf(waiter), 1);
                reject(
                  new Error("Controlled route did not arrive: " + pathname),
                );
              }, 10000);
              waiters.push(waiter);
            });
          },
          hold(route, pathname) {
            const index = waiters.findIndex(
              (waiter) => waiter.pathname === pathname,
            );
            if (index === -1) held.push(route);
            else {
              const waiter = waiters.splice(index, 1)[0];
              clearTimeout(waiter.timer);
              waiter.resolve(route);
            }
          },
        };
        const report = {
          implementation,
          scenario,
          baseline: !!options.baseline,
          errors,
          consoleErrors,
          network,
        };
        page.on("pageerror", (error) => errors.push(error.message));
        page.on("console", (message) => {
          if (message.type() === "error") consoleErrors.push(message.text());
        });
        await page.route("**/*", async (route) => {
          const url = new URL(route.request().url());
          network.push({
            url: url.href,
            method: route.request().method(),
            ...(url.pathname === "/auction" || url.pathname === "/_ts/page-bids"
              ? { body: route.request().postData() }
              : {}),
          });
          if (
            ![
              "https://publisher.example.com",
              "https://creative.example.com",
            ].includes(url.origin)
          )
            return route.abort();
          if (
            url.pathname === "/auction" ||
            url.pathname === "/_ts/page-bids"
          ) {
            transport.hold(route, url.pathname);
            return;
          }
          if (url.pathname.startsWith("/fixture-notice/"))
            return route.fulfill({ status: 204, body: "" });
          if (url.pathname === "/fixture-puc.js")
            return route.fulfill({
              contentType: "application/javascript",
              body: sources.get(puc),
            });
          if (url.pathname === "/fixture-puc")
            return route.fulfill({ contentType: "text/html", body: pucPage() });
          if (url.pathname === "/fixture-publisher")
            return route.fulfill({
              contentType: "text/html",
              body: creative("Publisher result"),
            });
          if (url.pathname === "/")
            return route.fulfill({
              contentType: "text/html",
              body: documentPage({
                initialLoad:
                  options.baseline || scenario === "policy-enabled"
                    ? "enabled"
                    : scenario === "policy-unknown"
                      ? "unknown"
                      : "disabled",
              }),
            });
          return route.fulfill({ status: 404, body: "" });
        });
        try {
          await page.goto("https://publisher.example.com");
          await page.addScriptTag({ content: sources.get(stub) });
          if (!options.baseline)
            await page.evaluate(() => {
              const element = document.createElement("div");
              element.id = "unrelated";
              document.body.append(element);
              googletag
                .defineSlot("/123/unrelated", [[300, 250]], element.id)
                .addService(googletag.pubads());
              if (googletag.pubads().isInitialLoadDisabled?.())
                googletag.pubads().disableInitialLoad();
              googletag.pubads().enableSingleRequest();
              googletag.enableServices();
            });
          await install(page, implementation);
          report.facts = options.baseline
            ? await baseline(page, scenario)
            : await fixed(page, scenario, implementation, transport);
          assert.deepEqual(errors, [], "No unrelated browser errors");
          report.outcome = "passed";
        } catch (error) {
          report.outcome = "failed";
          report.failure = error.stack;
          process.exitCode = 1;
        } finally {
          report.elapsedMs = Math.round(performance.now() - started);
          Object.assign(
            report,
            await page
              .evaluate(() => ({
                requests: window.fixture?.requests,
                creatives: window.fixture?.creatives,
                trace: window.fixture?.trace,
                prebidVersion: window.pbjs?.version,
              }))
              .catch(() => ({})),
          );
          await page
            .screenshot({
              path: path.join(outputRoot, name + ".png"),
              fullPage: true,
            })
            .catch((error) => {
              report.screenshotError = error.message;
              report.outcome = "failed";
              report.failure ??= error.stack;
              process.exitCode = 1;
            });
          await context.tracing
            .stop({
              path: path.join(outputRoot, name + "-trace.zip"),
            })
            .catch((error) => {
              report.traceError = error.message;
              report.outcome = "failed";
              report.failure ??= error.stack;
              process.exitCode = 1;
            });
          reports.push(report);
          save("results.json", reports);
          for (const waiter of waiters) clearTimeout(waiter.timer);
          for (const route of held) await route.abort().catch(() => {});
          await context.close();
        }
        console.log(
          JSON.stringify({
            implementation,
            scenario,
            outcome: report.outcome,
            requests: report.requests?.length,
            elapsedMs: report.elapsedMs,
            failure: report.failure?.split("\n")[0],
          }),
        );
      }
    }
  } finally {
    await browser.close();
    artifacts.finishedAt = new Date().toISOString();
    save("artifacts.json", artifacts);
  }
})().catch((error) => {
  save("fatal.json", { failure: error.stack });
  console.error(error);
  process.exitCode = 1;
});

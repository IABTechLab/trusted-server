const fs = require("node:fs");
const path = require("node:path");
const http = require("node:http");
const crypto = require("node:crypto");
const zlib = require("node:zlib");
const assert = require("node:assert/strict");
const { parseOptions, conditions } = require("./options.cjs");
const {
  assertRuntimeOrdering,
  assertCacheUse,
  assertReservationControl,
  assertCspRuntimeApis,
  assertCandidateRendererVersion,
  assertVisibleCreative,
} = require("./evidence.cjs");
const { sessionCls, distribution, observePage } = require("./metrics.cjs");
const { fixtureProxy } = require("./proxy.cjs");
const options = parseOptions();
const diagnosticScreen = options["diagnostic-screen"];
const diagnosticOutages = options["diagnostic-outages"];
const unifiedDiagnostic = diagnosticScreen || diagnosticOutages;
const settings = conditions(options);
const runs = options.runs;
const out = path.resolve(options.out);
fs.mkdirSync(out, { recursive: true });
const save = (name, value) =>
  fs.writeFileSync(path.join(out, name), JSON.stringify(value, null, 2));
const acceptance = {
  schemaVersion: 1,
  status: "running",
  scope: diagnosticScreen
    ? "diagnostic-screen"
    : diagnosticOutages
      ? "diagnostic-outages"
      : options["regressions-only"]
        ? "regressions"
        : options["correctness-only"]
          ? "correctness"
          : "measurement-and-correctness",
  startedAt: new Date().toISOString(),
  conditions: settings,
  samples: 0,
  profileSamples: {},
  negativeControls: Object.fromEntries(
    ["parser-blocking", "reservation-removed"].map((id) => [
      id,
      {
        status:
          unifiedDiagnostic || options["regressions-only"]
            ? "not-requested"
            : "pending",
        ...(unifiedDiagnostic || options["regressions-only"]
          ? {
              reason: diagnosticScreen
                ? "Diagnostic synchronous-unified measurement only; deferred parser control is inapplicable and reservation-removal correctness is outside this screen. Full acceptance remains unexecuted."
                : diagnosticOutages
                  ? "Four critical-runtime availability diagnostics only; parser-defer and reservation-removal controls are not requested. Changed outage behavior is observed, not approved."
                  : "--regressions-only selects the four review regressions, not the negative controls",
            }
          : {}),
      },
    ]),
  ),
};
let evidence;
const saveOutcome = () => {
  save("acceptance.json", acceptance);
  save("artifacts.json", { ...evidence, outcome: acceptance });
};
const recordFailure = (error) => {
  acceptance.status =
    ["ENOENT", "MODULE_NOT_FOUND"].includes(error.code) ||
    /Executable doesn't exist/.test(error.message)
      ? "blocked"
      : "failed";
  acceptance.finishedAt = new Date().toISOString();
  acceptance.error = {
    name: error.name,
    message: error.message,
    stack: error.stack,
  };
  for (const control of Object.values(acceptance.negativeControls))
    if (control.status === "running") control.status = acceptance.status;
  saveOutcome();
};
// Persist before reading exports/dependencies so a failed rerun cannot leave an
// older successful outcome in the same output directory.
saveOutcome();
process.on("uncaughtExceptionMonitor", recordFailure);
const { chromium } = require("playwright");
const { JSDOM } = require("jsdom");
const {
  baselineRevision,
  expectedArchitecture,
  assertScreenDocument,
  installEarlyProbe,
  assertEarlyProbe,
  screenCases,
  assertScreenSamples,
  outageCases,
  assertOutageSamples,
  assertCriticalOutage,
} = require("./diagnostic.cjs");
if (diagnosticScreen) {
  acceptance.diagnostic = {
    expectedArchitecture,
    expectedSamples: 8 * runs,
    fullAcceptancePassed: false,
    excluded:
      "Full correctness, controls, framework lifecycle, CSP/outage matrix and real-GPT acceptance are not executed by this screen. Missing authorized Google inventory remains blocked outside this scope.",
  };
  saveOutcome();
}
if (diagnosticOutages) {
  acceptance.diagnostic = {
    expectedArchitecture,
    expectedSamples: 4,
    fullAcceptancePassed: false,
    changedOutageBehaviorApproved: false,
    excluded:
      "Observes successful and failed critical unified loading with and without enforced CSP. Loss of inline fallback is a changed outage boundary, not compatibility approval. No retry/recovery, full correctness, controller, framework or live acceptance is established. Missing authorized Google inventory remains blocked outside this scope.",
  };
  saveOutcome();
}
const {
  reservationStyles,
  withoutEarlyRenderer,
  withoutRuntimeDefer,
  withoutScripts,
  rendererAsset,
} = require("./html.cjs");
const sha256 = (value) =>
  crypto.createHash("sha256").update(value).digest("hex");
const sizes = (bytes) => ({
  raw: bytes.length,
  gzip: zlib.gzipSync(bytes).length,
  brotli: zlib.brotliCompressSync(bytes).length,
});
const pucPath = require.resolve("prebid-universal-creative/dist/banner.js");
const puc = fs.readFileSync(pucPath);
const fixturePath = path.join(
  __dirname,
  "../publisher-registration/gpt-fixture.js",
);
const fixtureSource = fs.readFileSync(fixturePath, "utf8");

function snapshot(directory, label) {
  const root = path.resolve(directory);
  const manifestBytes = fs.readFileSync(path.join(root, "manifest.json"));
  const manifest = JSON.parse(manifestBytes);
  assert.equal(manifest.schemaVersion, 1);
  const retained = path.join(out, "inputs", label);
  fs.mkdirSync(retained, { recursive: true });
  fs.writeFileSync(path.join(retained, "manifest.json"), manifestBytes);
  const files = new Map();
  for (const filename of [
    ...manifest.documents.map((doc) => doc.file),
    ...Object.values(manifest.assets),
    "app.toml",
  ]) {
    const bytes = fs.readFileSync(path.join(root, filename));
    files.set(filename, bytes);
    fs.mkdirSync(path.dirname(path.join(retained, filename)), {
      recursive: true,
    });
    fs.writeFileSync(path.join(retained, filename), bytes);
  }
  return {
    root,
    manifest,
    files,
    evidence: {
      root,
      manifestSha256: sha256(manifestBytes),
      manifest,
      files: Object.fromEntries(
        [...files].map(([file, bytes]) => [
          file,
          { sha256: sha256(bytes), ...sizes(bytes) },
        ]),
      ),
    },
  };
}
const loading = snapshot(options.loading, "loading");
const combined = snapshot(options.combined, "combined");
const baseline = options.baseline && snapshot(options.baseline, "baseline");
if (unifiedDiagnostic)
  assert.equal(
    baseline.manifest.sourceRevision,
    baselineRevision,
    "Diagnostic baseline must remain pinned original-main, not the failing optimized PR snapshot",
  );
const profiles = {
  loading,
  combined,
  ...(baseline ? { baseline, reservation: baseline } : {}),
};
evidence = {
  node: process.version,
  platform: process.platform,
  architecture: process.arch,
  command: process.argv,
  startedAt: new Date().toISOString(),
  settings: {
    ...settings,
    runs,
    hostContention: "Shared host may run concurrent builds",
    cacheSemantics:
      options.cache === "warm"
        ? "Fresh per-sample context, prime then navigate again in same page; static assets cacheable for 3600 seconds; documents and creatives no-store"
        : "Fresh contexts, HTTP cache disabled, all responses no-store",
    externalNetwork:
      "HTTP proxy forwards only exact publisher/creative origins; all other origins and CONNECT rejected; no browser bypass or Playwright routing",
  },
  inputs: {
    loading: loading.evidence,
    combined: combined.evidence,
    ...(baseline ? { baseline: baseline.evidence } : {}),
  },
  transport: {
    fixtureSha256: sha256(fixtureSource),
    pucSha256: sha256(puc),
    pucVersion: require("prebid-universal-creative/package.json").version,
  },
  runnerSha256: sha256(fs.readFileSync(__filename)),
  metricsSha256: sha256(fs.readFileSync(path.join(__dirname, "metrics.cjs"))),
  htmlSha256: sha256(fs.readFileSync(path.join(__dirname, "html.cjs"))),
  optionsSha256: sha256(fs.readFileSync(path.join(__dirname, "options.cjs"))),
  evidenceSha256: sha256(fs.readFileSync(path.join(__dirname, "evidence.cjs"))),
  proxySha256: sha256(fs.readFileSync(path.join(__dirname, "proxy.cjs"))),
  diagnosticSha256: sha256(
    fs.readFileSync(path.join(__dirname, "diagnostic.cjs")),
  ),
  proxyRequests: [],
  interventions: [
    {
      type: "runtime delay, CPU/network and HTTP cache configuration",
      ...settings,
    },
  ],
  boundary:
    "Controlled loopback lab fixture, simulated GPT and fictional bids. First-visible is an animation-frame/DOM estimate, not hardware paint, GAM impression time or field CWV.",
};
saveOutcome();

function documentInput(profile, name) {
  const source = profiles[profile];
  const doc = source.manifest.documents.find((entry) => entry.name === name);
  assert(doc, `Missing required exported document ${profile}/${name}`);
  let html = source.files.get(doc.file).toString("utf8");
  if (profile === "reservation") {
    const reservedDoc = combined.manifest.documents.find(
      (entry) => entry.name === name,
    );
    const styles = reservationStyles(
      combined.files.get(reservedDoc.file).toString("utf8"),
    );
    if (doc.matched && !doc.absentWrapper)
      assert(styles.length, "Reserved export must supply production CSS");
    const css = styles.join("");
    html = html.replace("<head>", "<head>" + css);
    evidence.interventions.push({
      profile,
      name,
      type: "CSS-only isolation derivative",
      source: combined.root,
      cssSha256: sha256(css),
      htmlSha256: sha256(html),
    });
  }
  const dom = new JSDOM(html).window.document;
  const runtime = dom.querySelector("script#trustedserver-js");
  assert(runtime?.src, `${profile}/${name} must have a production runtime tag`);
  const runtimeSrc = runtime.getAttribute("src");
  const inline = [...dom.querySelectorAll("script:not([src])")]
    .map((script) => script.textContent)
    .join("\n");
  return {
    ...doc,
    html,
    source,
    runtimeSrc,
    deferred: runtime.defer,
    bytes: {
      html: sizes(Buffer.from(html)),
      inlineScript: sizes(Buffer.from(inline)),
      inlineTags: [...dom.querySelectorAll("script:not([src])")].map(
        (script, index) => ({
          id: script.id || `anonymous-${index}`,
          ...sizes(Buffer.from(script.textContent)),
        }),
      ),
      runtime: sizes(source.files.get(source.manifest.assets[runtimeSrc])),
    },
  };
}

let active;
let publisherOrigin;
let creativeOrigin;
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function respond(request, response) {
  const url = new URL(request.url, `http://${request.headers.host}`);
  const send = (type, bytes, status = 200) => {
    const cacheable =
      status === 200 &&
      (active.input.source.manifest.assets[request.url] ||
        ["/hero.svg", "/fixture-puc.js"].includes(url.pathname));
    active.responses.push({
      url: url.href,
      status,
      cacheable: !!cacheable,
      phase: active.phase,
      runtimeDelayMs:
        request.url === active.input.runtimeSrc && active.runtimeAttempts === 1
          ? settings.runtimeDelayMs
          : 0,
    });
    response.writeHead(status, {
      "Content-Type": type,
      "Cache-Control":
        options.cache === "warm" && cacheable
          ? "public, max-age=3600"
          : "no-store",
      ...(url.pathname === active.input.requestPath
        ? active.responseHeaders
        : {}),
    });
    response.end(bytes);
  };
  if (url.pathname === "/fixture-puc.js")
    return send("application/javascript", puc);
  if (url.pathname === "/fixture-puc")
    return send(
      "text/html",
      `<!doctype html><script src="/fixture-puc.js"></script><script>ucTag.renderAd(document,{adId:new URL(location.href).searchParams.get('adId'),pubUrl:${JSON.stringify(publisherOrigin)}})</script>`,
    );
  if (url.pathname === "/fixture-publisher")
    return send(
      "text/html",
      `<!doctype html><div class="marker">Publisher result</div><script>requestAnimationFrame(()=>top.postMessage({type:"fixture-creative-ready",label:"Publisher result"},"*"));</script>`,
    );
  if (url.pathname === "/hero.svg")
    return send(
      "image/svg+xml",
      '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="180"><rect width="800" height="180" fill="#456f91"/><text x="24" y="96" fill="white" font-size="30">Fictional article image</text></svg>',
    );
  if (
    url.pathname === "/favicon.ico" ||
    url.pathname.startsWith("/fixture-notice/")
  )
    return send("text/plain", "", 204);
  if (request.url === active.input.runtimeSrc) {
    active.runtimeAttempts++;
    const retry = active.runtimeAttempts > 1;
    if (!retry && settings.runtimeDelayMs > 0)
      await wait(settings.runtimeDelayMs);
    if (!retry && ["failed", "late-runtime"].includes(active.mode))
      return send("text/plain", "Deliberate fixture runtime failure", 404);
  }
  if (active.mode === "renderer-absent" && rendererAsset(url.pathname)) {
    active.blockedRenderer.push(url.pathname);
    return send("text/plain", "Deliberate fixture renderer absence", 404);
  }
  const asset = active.input.source.manifest.assets[request.url];
  if (asset)
    return send("application/javascript", active.input.source.files.get(asset));
  if (url.pathname === active.input.requestPath)
    return send("text/html", active.html);
  active.unexpected.push(request.url);
  return send("text/plain", "Undeclared fixture URL", 404);
}
const servers = [];
async function listen() {
  const server = http.createServer((req, res) =>
    respond(req, res).catch((error) => {
      console.error(error);
      res.destroy(error);
    }),
  );
  servers.push(server);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return `http://127.0.0.1:${server.address().port}`;
}

function installDelivery({ height, noFill }) {
  const state = window.cwv;
  const scheduled = new Set();
  const schedule = () => {
    for (const request of fixture.requests) {
      if (scheduled.has(request.index)) continue;
      scheduled.add(request.index);
      setTimeout(() => {
        state.geometrySnapshot("before-fill");
        fixture.render(request.index, { noFill });
        const frame = document.querySelector(
          `iframe[data-request="${request.index}"]`,
        );
        if (frame) frame.height = String(height);
        if (noFill) state.delivered = true;
      }, 150);
    }
  };
  const service = googletag.pubads();
  for (const [object, name] of [
    [service, "refresh"],
    [googletag, "display"],
  ]) {
    const original = object[name];
    object[name] = function (...args) {
      const result = original.apply(this, args);
      schedule();
      return result;
    };
  }
  window.addEventListener("message", (event) => {
    if (event.data?.type !== "fixture-creative-ready") return;
    requestAnimationFrame(() => {
      const frame = document.querySelector("iframe[data-request]");
      const rect = frame?.getBoundingClientRect();
      if (
        rect &&
        rect.width > 0 &&
        rect.height > 0 &&
        rect.top < innerHeight &&
        rect.bottom > 0
      ) {
        state.firstVisibleEstimateMs ??= performance.now();
        state.delivered = true;
        state.geometrySnapshot("creative-visible");
      }
    });
  });
}

const samples = [];
let stage =
  options["correctness-only"] ||
  options["regressions-only"] ||
  diagnosticOutages
    ? "correctness"
    : "measurement";
async function sample(
  browser,
  profile,
  name,
  viewport,
  mode = "slow",
  noFill = false,
  control = null,
) {
  if (control) {
    acceptance.negativeControls[control].status = "running";
    saveOutcome();
  }
  const input = documentInput(profile, name);
  if (diagnosticOutages)
    assert.equal(
      input.enforcedCsp,
      name === "csp",
      "Outage matrix must separately exercise normal and enforced-CSP production documents",
    );
  const runtimeUnavailable =
    (input.enforcedCsp || diagnosticOutages) && mode === "failed";
  // Keep the existing publisher-side probe. Its core call deliberately fails
  // when the synchronous runtime is unavailable; no other page error is allowed.
  const expectedErrors = runtimeUnavailable
    ? ["tsjs.setConfig is not a function"]
    : [];
  if (unifiedDiagnostic) assertScreenDocument(input.html, profile);
  const expectedDeferred = unifiedDiagnostic
    ? false
    : !input.enforcedCsp && (profile === "loading" || profile === "combined");
  assert.equal(
    input.deferred,
    expectedDeferred,
    `${profile} must exercise its intended production loading mode`,
  );
  const index = samples.length;
  const label = `${index}-${profile}-${name}-${viewport.width}-${mode}${noFill ? "-nofill" : ""}${control ? "-" + control : ""}`;
  active = {
    input,
    mode,
    runtimeAttempts: 0,
    unexpected: [],
    blockedRenderer: [],
    responseHeaders: {},
    responses: [],
    phase: options.cache === "warm" ? "warmup" : "measurement",
    html: input.html,
  };
  if (control === "parser-blocking") {
    active.html = withoutRuntimeDefer(input.html);
    assert.notEqual(
      active.html,
      input.html,
      "Parser negative control must remove defer",
    );
  }
  if (control === "reservation-removed") {
    const styles = reservationStyles(input.html);
    assert(
      styles.length > 0,
      "Reservation negative control requires production CSS",
    );
    for (const style of styles) active.html = active.html.replace(style, "");
  }
  if (control) {
    const file = label + ".html";
    fs.writeFileSync(path.join(out, file), active.html);
    evidence.interventions.push({
      index,
      profile,
      name,
      type: control,
      file,
      htmlSha256: sha256(active.html),
    });
  }
  if (input.enforcedCsp) {
    const legacy = baseline ?? input.source;
    const legacyDoc = legacy.manifest.documents.find(
      (doc) => doc.name === (baseline ? "inline" : "csp"),
    );
    const legacyHtml = legacy.files.get(legacyDoc.file).toString("utf8");
    assert(
      !legacyHtml.includes('id="trustedserver-bootstrap"'),
      "CSP hashes must not come from the new compiled inline entry",
    );
    const inline = [
      ...new JSDOM(legacyHtml).window.document.querySelectorAll(
        "script:not([src])",
      ),
    ].map((script) => script.textContent);
    const hashes = [
      ...inline,
      ...(input.source.manifest.creativeInlineScripts ?? []),
    ].map(
      (script) =>
        `'sha256-${crypto.createHash("sha256").update(script).digest("base64")}'`,
    );
    active.responseHeaders["Content-Security-Policy"] =
      `script-src 'self' ${hashes.join(" ")}`;
    evidence.interventions.push({
      profile,
      name,
      type: "enforced old-inline-hash and self CSP",
      policySource: baseline
        ? baseline.root
        : "current synchronous legacy-inline output",
      responseHeaders: active.responseHeaders,
    });
    assert(
      !input.html.includes('id="trustedserver-bootstrap"'),
      "Enforced CSP must use synchronous production fallback",
    );
    assert(
      !input.html.includes("// Edge-injected GPT auction bootstrap."),
      "Enforced CSP must not depend on the provisional raw bootstrap's inline hash",
    );
    if (runtimeUnavailable)
      evidence.interventions.push({
        index,
        profile,
        name,
        type: "enforced-CSP synchronous runtime failure; no provisional fallback",
        expectedErrors,
      });
  }
  if (mode === "renderer-absent") {
    active.html = withoutEarlyRenderer(input.html);
    assert.notEqual(
      active.html,
      input.html,
      "Renderer absence must remove the real compiled early entry",
    );
    // This fixture-only probe calls core APIs deliberately absent in this case.
    active.html = withoutScripts(active.html, (script) =>
      script.content.trimStart().startsWith("window.fixtureEarly="),
    );
    assert(
      active.html.includes("publisherSlotRegistrationVersion"),
      "Raw protocol bootstrap must remain",
    );
    evidence.interventions.push({
      profile,
      name,
      type: "true renderer absence, early entry and core probe removed; GPT/unified requests blocked",
      htmlSha256: sha256(active.html),
    });
  }
  if (mode === "absent") {
    active.html = withoutScripts(
      input.html,
      (script) => script.id === "trustedserver-js",
    );
    assert.notEqual(
      active.html,
      input.html,
      "Absent-runtime control must remove the runtime tag",
    );
    evidence.interventions.push({
      profile,
      name,
      type: "absent runtime tag control",
      htmlSha256: sha256(active.html),
    });
  }
  const context = await browser.newContext({
    viewport,
    serviceWorkers: "block",
  });
  let traceStarted = false;
  let result;
  let page;
  let warmup = null;
  const requests = new Map();
  const errors = [];
  const network = [];
  try {
    await context.tracing.start({
      screenshots: true,
      snapshots: true,
      sources: true,
    });
    traceStarted = true;
    // One init script preserves setup order; Playwright does not order separate init scripts.
    await context.addInitScript({
      content: `if(window===window.top){
      (${observePage.toString()})();
      ${unifiedDiagnostic ? `(${installEarlyProbe.toString()})();` : ""}
      window.publisherFixtureOptions={initialLoad:${JSON.stringify(input.registered ? "disabled" : "enabled")}};
      ${fixtureSource.replaceAll("https://creative.example.com", creativeOrigin)}
      (${installDelivery.toString()})(${JSON.stringify({ height: input.creativeHeight, noFill })});
    }`,
    });
    page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    await cdp.send("Network.enable");
    await cdp.send("Network.setCacheDisabled", {
      cacheDisabled: options.cache === "cold",
    });
    await cdp.send("Emulation.setCPUThrottlingRate", {
      rate: settings.cpuRate,
    });
    await cdp.send("Network.emulateNetworkConditions", settings.network);
    cdp.on("Network.requestWillBeSent", ({ requestId, request }) => {
      requests.set(requestId, { url: request.url, method: request.method });
    });
    cdp.on("Network.requestServedFromCache", ({ requestId }) => {
      const entry = requests.get(requestId);
      if (entry) entry.servedFromCache = true;
    });
    cdp.on("Network.responseReceived", ({ requestId, response }) => {
      const entry = requests.get(requestId);
      if (entry)
        Object.assign(entry, {
          status: response.status,
          fromDiskCache: response.fromDiskCache ?? false,
          fromServiceWorker: response.fromServiceWorker ?? false,
          headers: response.headers,
        });
    });
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("request", (request) => {
      const url = request.url();
      network.push({ url, method: request.method() });
      if (
        !url.startsWith(publisherOrigin + "/") &&
        !url.startsWith(creativeOrigin + "/")
      )
        errors.push("Unexpected external request: " + url);
    });
    const navigate = () =>
      page.goto(publisherOrigin + input.requestPath, {
        waitUntil: "load",
        timeout: 15000,
      });
    if (options.cache === "warm") {
      await navigate();
      if (input.matched && !runtimeUnavailable)
        await page.waitForFunction(() => cwv.delivered, null, {
          timeout: 10000,
        });
      await wait(250);
      assert.deepEqual(
        errors,
        expectedErrors,
        "Cache priming must have only declared fixture errors",
      );
      assert.deepEqual(
        active.unexpected,
        [],
        "Cache priming URLs must be declared",
      );
      warmup = {
        requests: [...requests.values()],
        network: [...network],
        responses: [...active.responses],
        raw: await page.evaluate(() => cwv.finish()),
      };
      requests.clear();
      network.length = 0;
      errors.length = 0;
      active.runtimeAttempts = 0;
      active.blockedRenderer = [];
      active.responses = [];
      active.phase = "measurement";
    }
    await navigate();
    if (input.matched && !runtimeUnavailable)
      await page.waitForFunction(() => cwv.delivered, null, { timeout: 10000 });
    if (mode === "late-runtime") {
      await page.evaluate(
        (src) =>
          new Promise((resolve, reject) => {
            const script = document.createElement("script");
            script.src = src;
            script.onload = resolve;
            script.onerror = reject;
            document.head.append(script);
          }),
        input.runtimeSrc,
      );
    }
    // Multiple frames include fill-induced layout, then a declared quiet window.
    await page.evaluate(
      () =>
        new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        ),
    );
    await wait(250);
    const raw = await page.evaluate(() => cwv.finish());
    const cls = sessionCls(raw.shifts);
    const fcp = raw.paint.find(
      (entry) => entry.name === "first-contentful-paint",
    )?.startTime;
    const lcp = raw.lcp.at(-1)?.startTime;
    for (const type of ["paint", "largest-contentful-paint", "layout-shift"])
      assert(raw.supported.includes(type), `Missing ${type} observer support`);
    assert(Number.isFinite(fcp), "FCP must be observed");
    assert(Number.isFinite(lcp), "LCP must be observed");
    assert.deepEqual(
      errors,
      expectedErrors,
      "Only declared fixture page errors are allowed",
    );
    if (input.enforcedCsp || diagnosticOutages)
      assertCspRuntimeApis(raw, !runtimeUnavailable);
    if (diagnosticOutages && runtimeUnavailable) {
      assertCriticalOutage(
        raw,
        active.responses,
        new URL(input.runtimeSrc, publisherOrigin).href,
      );
      assert.equal(
        active.runtimeAttempts,
        1,
        "Critical outage must issue one failed runtime request without retry",
      );
    }
    if (input.enforcedCsp)
      assert.deepEqual(
        raw.cspViolations,
        [],
        "Old-inline-hash/self CSP must permit synchronous fallback without violations",
      );
    assert.deepEqual(
      active.unexpected,
      [],
      "Every loopback URL must be declared",
    );
    if (mode === "renderer-absent") {
      assert.equal(
        raw.rendererVersion,
        undefined,
        "No actual render bridge may remain installed",
      );
      assert.equal(
        raw.candidateAdId,
        "fixture-server-ad",
        "Absent renderer must not erase the admitted candidate",
      );
      assert.deepEqual(
        active.blockedRenderer,
        [new URL(input.runtimeSrc, publisherOrigin).pathname],
        "Actual standalone GPT renderer must be blocked",
      );
    } else if (!runtimeUnavailable) {
      assert(
        raw.early.core && raw.early.queued,
        unifiedDiagnostic
          ? "Direct core API and queued publisher callback must work at the first publisher script"
          : "Direct core API and queued publisher callback must work before deferred runtime",
      );
      assertCandidateRendererVersion(raw, profile);
      if (unifiedDiagnostic) assertEarlyProbe(raw, profile);
    }
    assert.equal(
      raw.fixture.requests.length,
      input.matched && !runtimeUnavailable ? 1 : 0,
      "Exactly one native initial request requires a matched slot and available runtime or early bridge",
    );
    if (input.matched && !runtimeUnavailable) {
      assert.equal(
        raw.fixture.trace.filter((entry) => entry.event === "nativeDefine")
          .length,
        1,
        "One native slot definition must survive takeover",
      );
      assert.equal(
        raw.fixture.trace.filter((entry) => entry.event === "nativeDisplay")
          .length,
        1,
        "Initial delivery must display exactly once",
      );
      if (input.registered) {
        const refreshes = raw.fixture.trace.filter(
          (entry) => entry.event === "nativeRefresh",
        );
        assert.equal(
          refreshes.length,
          1,
          "Registered initial delivery must refresh exactly once",
        );
        assert(
          refreshes[0].scoped,
          "Registered initial refresh must remain scoped",
        );
        assert.deepEqual(
          refreshes[0].natives,
          [raw.registeredNative],
          "Refresh must select only the publisher native object",
        );
        assert.equal(
          raw.fixture.requests[0].native,
          raw.registeredNative,
          "Registered delivery must use the publisher native object",
        );
        assert.deepEqual(
          raw.fixture.requests[0].targeting.publisher_key,
          ["keep"],
          "Publisher targeting must survive handoff",
        );
        assert.equal(
          raw.registration.status,
          "rendered",
          "Registration must settle through the real renderer",
        );
      }
      assert.equal(
        raw.fixture.requests[0].targeting.hb_adid?.[0],
        mode === "renderer-absent" ? undefined : "fixture-server-ad",
        "Candidate targeting requires an actual renderer",
      );
      if (!noFill) {
        const text = await Promise.all(
          page
            .frames()
            .map((frame) => frame.locator(".marker").allTextContents()),
        );
        assertVisibleCreative(
          text,
          raw.firstVisibleEstimateMs,
          mode === "renderer-absent" ? "Publisher result" : "Server result",
        );
      }
    }
    const resources = raw.resources.filter(
      (entry) =>
        new URL(entry.name).pathname ===
        new URL(input.runtimeSrc, publisherOrigin).pathname,
    );
    const responseEnd = resources[0]?.responseEnd;
    if (mode === "slow") {
      assert(
        Number.isFinite(responseEnd),
        "Runtime resource timing must exist",
      );
      assertRuntimeOrdering(
        raw,
        new URL(input.runtimeSrc, publisherOrigin).href,
        expectedDeferred && control !== "parser-blocking",
      );
      if (!expectedDeferred || control === "parser-blocking")
        assert(
          fcp >= responseEnd,
          "Parser-blocking runtime must finish before FCP",
        );
      if (
        expectedDeferred &&
        control !== "parser-blocking" &&
        options.cache === "cold" &&
        settings.runtimeDelayMs === 750
      )
        assert(
          fcp < responseEnd,
          "Changed content must paint during the default 750ms runtime hold",
        );
      // A deferred cache hit or 0ms response may legitimately finish before FCP.
      // Native script-load/parser observations above are the ordering gate.
    }
    const cacheRequests = [...requests.values()];
    const cacheHits =
      options.cache === "warm"
        ? assertCacheUse(
            cacheRequests,
            new URL(input.runtimeSrc, publisherOrigin).href,
            mode === "slow",
          )
        : [];
    const before = raw.geometry.find((entry) => entry.phase === "before-fill");
    const after = raw.geometry.at(-1);
    const expectedHeight = viewport.width >= 1024 ? 600 : 250;
    const reserved =
      ["combined", "reservation"].includes(profile) &&
      control !== "reservation-removed" &&
      input.matched &&
      !input.absentWrapper;
    if (reserved) {
      assert.equal(
        Number.parseFloat(after.minHeight),
        expectedHeight,
        "Responsive reservation must match viewport",
      );
      if (runtimeUnavailable) {
        assert.equal(
          cls,
          0,
          diagnosticOutages
            ? "Unavailable critical runtime must not cause ad-induced layout shift"
            : "Unavailable CSP runtime must not cause ad-induced layout shift",
        );
      } else if (noFill || input.creativeHeight <= expectedHeight) {
        assert.equal(
          after.article.top,
          before.article.top,
          "Reserved article must not move during compatible fill or no-fill",
        );
        assert.equal(
          cls,
          0,
          "Compatible reservation must prevent controlled ad-induced CLS",
        );
      }
    } else if (input.absentWrapper) {
      assert.equal(
        after.wrapper,
        null,
        "Absent configured wrapper must not be synthesized",
      );
    } else if (after.wrapper) {
      assert.equal(
        after.minHeight,
        "0px",
        "Unmatched and unreserved wrappers must remain untouched",
      );
    }
    if (control === "reservation-removed") assertReservationControl(raw, cls);
    result = {
      index,
      control,
      runtimeUnavailableBoundary: runtimeUnavailable,
      ...(diagnosticOutages
        ? {
            diagnosticOutage: {
              policy: input.enforcedCsp
                ? "enforced-old-hash-self-csp"
                : "normal",
              expectedRuntimeAvailable: !runtimeUnavailable,
              changedOutageBehaviorApproved: false,
              interpretation:
                "Observed critical-script availability boundary, not compatibility approval or recovery proof",
            },
          }
        : {}),
      conditions: settings,
      cache: {
        mode: options.cache,
        hits: cacheHits,
        requests: cacheRequests,
        warmup,
      },
      responses: active.responses,
      stage,
      profile,
      name,
      viewport,
      mode,
      noFill,
      bytes: input.bytes,
      metrics: {
        fcp,
        lcp,
        cls,
        domInteractive: raw.navigation[0].domInteractive,
        domContentLoaded: raw.navigation[0].domContentLoadedEventEnd,
        requestMs:
          raw.fixture.trace.find((entry) => entry.event === "nativeRequest")
            ?.time ?? null,
        firstVisibleEstimateMs: raw.firstVisibleEstimateMs,
        runtimeResponseEnd: responseEnd ?? null,
        reservationHeight: after.wrapper?.height ?? null,
        whitespace: after.wrapper
          ? Math.max(
              0,
              after.wrapper.height - (noFill ? 0 : input.creativeHeight),
            )
          : null,
      },
      raw,
      errors,
      network,
      responseHeaders: active.responseHeaders,
      blockedRenderer: active.blockedRenderer,
    };
    samples.push(result);
    acceptance.samples = samples.length;
    acceptance.profileSamples[profile] =
      (acceptance.profileSamples[profile] ?? 0) + 1;
    if (control)
      Object.assign(acceptance.negativeControls[control], {
        status: "passed",
        sampleIndex: index,
        htmlFile: label + ".html",
      });
    save("samples.json", samples);
    saveOutcome();
    console.log(JSON.stringify({ label, ...result.metrics }));
    await page.screenshot({
      path: path.join(out, label + ".png"),
      fullPage: true,
    });
    return result;
  } catch (error) {
    await page
      ?.screenshot({
        path: path.join(out, label + "-failure.png"),
        fullPage: true,
      })
      .catch(() => {});
    save(label + "-failure.json", {
      error: error.stack,
      sample: result,
      raw: await page?.evaluate(() => window.cwv?.finish()).catch(() => null),
      conditions: settings,
      cache: { mode: options.cache, requests: [...requests.values()], warmup },
      errors,
      network,
      server: {
        responses: active.responses,
        runtimeAttempts: active.runtimeAttempts,
        unexpected: active.unexpected,
      },
    });
    throw error;
  } finally {
    if (traceStarted)
      await context.tracing.stop({
        path: path.join(out, label + "-trace.zip"),
      });
    await context.close();
  }
}

(async () => {
  publisherOrigin = await listen();
  creativeOrigin = await listen();
  let browser;
  try {
    const allowedOrigins = [publisherOrigin, creativeOrigin];
    evidence.interventions.push({
      type: "exact-origin forwarding HTTP proxy",
      allowedOrigins,
      rejected: "all other origins and CONNECT; no browser bypass",
    });
    const proxy = fixtureProxy(allowedOrigins, (entry) => {
      evidence.proxyRequests.push(
        Object.assign(entry, {
          sampleIndex: samples.length,
          phase: active?.phase ?? "startup",
        }),
      );
    });
    servers.push(proxy);
    await new Promise((resolve) => proxy.listen(0, "127.0.0.1", resolve));
    browser = await chromium.launch({
      headless: true,
      proxy: {
        server: `http://127.0.0.1:${proxy.address().port}`,
      },
    });
    evidence.browser = browser.version();
    evidence.origins = { publisherOrigin, creativeOrigin };
    const mobile = { width: 390, height: 844 };
    const desktop = { width: 1280, height: 1000 };
    if (diagnosticOutages) {
      for (const { profile, name, viewport, mode } of outageCases())
        await sample(browser, profile, name, viewport, mode);
      assertOutageSamples(samples);
    } else if (diagnosticScreen) {
      for (const { profile, name, viewport } of screenCases(runs))
        await sample(browser, profile, name, viewport);
      assertScreenSamples(samples, runs);
      assert(
        samples
          .filter((entry) => ["baseline", "loading"].includes(entry.profile))
          .every((entry) => entry.metrics.cls > 0),
        "Unreserved diagnostic arms must reproduce ad-induced layout shift",
      );
    } else if (!options["correctness-only"] && !options["regressions-only"]) {
      for (const viewport of [mobile, desktop]) {
        for (let run = 0; run < runs; run++) {
          const order = ["baseline", "loading", "reservation", "combined"];
          if (run % 2) order.reverse();
          for (const profile of order)
            await sample(browser, profile, "inline", viewport);
        }
      }
      const shiftControls = samples.filter((entry) =>
        ["baseline", "loading"].includes(entry.profile),
      );
      assert(
        shiftControls.every((entry) => entry.metrics.cls > 0),
        "Unreserved controls must visibly reproduce ad-induced layout shift",
      );
    }
    if (!unifiedDiagnostic) {
      stage = "correctness";
      if (options["correctness-only"]) {
        for (const viewport of [mobile, desktop]) {
          const control = await sample(browser, "loading", "inline", viewport);
          assert(
            control.metrics.cls > 0,
            "Unreserved control must visibly shift after fill",
          );
        }
      }
      await sample(
        browser,
        "combined",
        "registered",
        mobile,
        "renderer-absent",
      );
      await sample(browser, "combined", "registered", mobile, "failed");
      await sample(browser, "combined", "csp", mobile);
      await sample(browser, "combined", "csp", mobile, "failed");
      if (!options["regressions-only"]) {
        await sample(
          browser,
          "combined",
          "inline",
          mobile,
          "slow",
          false,
          "parser-blocking",
        );
        await sample(
          browser,
          "combined",
          "inline",
          mobile,
          "slow",
          false,
          "reservation-removed",
        );
        for (const mode of ["slow", "failed", "absent", "late-runtime"]) {
          await sample(browser, "combined", "inline", mobile, mode);
          await sample(browser, "combined", "registered", mobile, mode);
        }
        for (const name of [
          "shared",
          "absent-wrapper",
          "unmatched",
          "child-replacement",
          "unknown-height",
        ])
          await sample(browser, "combined", name, mobile);
        await sample(browser, "combined", "shared", desktop);
        await sample(browser, "loading", "unknown-height", mobile);
        for (const width of [1023, 1024])
          await sample(browser, "combined", "tall", { width, height: 1000 });
        await sample(browser, "combined", "inline", mobile, "slow", true);
      }
    }
    const groups = {};
    for (const entry of samples) {
      const key = `${entry.stage}/${entry.profile}/${entry.name}/${entry.viewport.width}/${entry.mode}/${entry.noFill ? "nofill" : "fill"}${entry.control ? "/" + entry.control : ""}`;
      (groups[key] ??= []).push(entry.metrics);
    }
    const summary = Object.fromEntries(
      Object.entries(groups).map(([key, entries]) => [
        key,
        Object.fromEntries(
          Object.keys(entries[0]).map((metric) => [
            metric,
            distribution(entries.map((entry) => entry[metric])),
          ]),
        ),
      ]),
    );
    save("summary.json", summary);
    const comparisons = {};
    if (baseline)
      for (const viewport of [mobile, desktop]) {
        const reference =
          summary[`measurement/baseline/inline/${viewport.width}/slow/fill`];
        for (const profile of ["loading", "reservation", "combined"]) {
          const changed =
            summary[
              `measurement/${profile}/inline/${viewport.width}/slow/fill`
            ];
          if (!reference || !changed) continue;
          comparisons[`${profile}/${viewport.width}`] = Object.fromEntries(
            Object.keys(reference).map((metric) => {
              const before = reference[metric].median;
              const after = changed[metric].median;
              return [
                metric,
                {
                  baseline: before,
                  changed: after,
                  delta:
                    before === null || after === null ? null : after - before,
                  percent:
                    before === null || after === null || before === 0
                      ? null
                      : ((after - before) / before) * 100,
                },
              ];
            }),
          );
        }
      }
    save("comparisons.json", comparisons);
    console.log(
      diagnosticScreen
        ? "Diagnostic screen completed; raw timings retained. Full correctness, negative controls, framework/live gates and combined acceptance remain unexecuted."
        : diagnosticOutages
          ? "Four availability diagnostics completed. Changed critical-outage behavior is observed, not approved. Full acceptance remains unexecuted."
          : "CWV functional contracts passed; raw timings and ranges retained, no timing-improvement threshold applied.",
    );
  } finally {
    evidence.finishedAt = new Date().toISOString();
    saveOutcome();
    await browser?.close();
    await Promise.all(
      servers.map((server) => new Promise((resolve) => server.close(resolve))),
    );
  }
  acceptance.status = "passed";
  acceptance.finishedAt = new Date().toISOString();
  acceptance.reports = {
    artifacts: "artifacts.json",
    samples: "samples.json",
    summary: "summary.json",
    comparisons: "comparisons.json",
  };
  saveOutcome();
})().catch((error) => {
  recordFailure(error);
  console.error(error);
  process.exitCode = 1;
});

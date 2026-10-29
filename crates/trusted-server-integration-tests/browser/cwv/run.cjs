const fs = require("node:fs");
const path = require("node:path");
const http = require("node:http");
const crypto = require("node:crypto");
const zlib = require("node:zlib");
const assert = require("node:assert/strict");
const { parseArgs } = require("node:util");
const { chromium } = require("playwright");
const { JSDOM } = require("jsdom");
const { sessionCls, distribution, observePage } = require("./metrics.cjs");
const {
  reservationStyles,
  withoutEarlyRenderer,
  rendererAsset,
} = require("./html.cjs");

const { values: options } = parseArgs({
  options: {
    baseline: { type: "string" },
    loading: { type: "string" },
    combined: { type: "string" },
    out: { type: "string" },
    runs: { type: "string", default: "5" },
    "correctness-only": { type: "boolean" },
    "regressions-only": { type: "boolean" },
  },
});
assert(
  options.loading && options.combined && options.out,
  "Required: --loading CHANGED_UNRESERVED --combined CHANGED_RESERVED --out DIR; --baseline enables four-way measurement",
);
assert(
  options.baseline ||
    options["correctness-only"] ||
    options["regressions-only"],
  "Measurement requires --baseline; CI may use --correctness-only",
);
const runs = Number(options.runs);
assert(Number.isInteger(runs) && runs > 0, "--runs must be a positive integer");
const out = path.resolve(options.out);
fs.mkdirSync(out, { recursive: true });
const save = (name, value) =>
  fs.writeFileSync(path.join(out, name), JSON.stringify(value, null, 2));
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
const profiles = {
  loading,
  combined,
  ...(baseline ? { baseline, reservation: baseline } : {}),
};
const evidence = {
  node: process.version,
  platform: process.platform,
  architecture: process.arch,
  command: process.argv,
  startedAt: new Date().toISOString(),
  settings: {
    cpuThrottling: "none; shared host may run concurrent builds",
    network: "loopback; runtime response held 750ms",
    cache: "fresh contexts, no-store",
    runs,
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
  interventions: [],
  boundary:
    "Controlled loopback lab fixture, simulated GPT and fictional bids. First-visible is an animation-frame/DOM estimate, not hardware paint, GAM impression time or field CWV.",
};
save("artifacts.json", evidence);

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
  const url = new URL(request.url, publisherOrigin);
  const send = (type, bytes, status = 200) => {
    response.writeHead(status, {
      "Content-Type": type,
      "Cache-Control": "no-store",
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
    if (!retry) await wait(750);
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
  options["correctness-only"] || options["regressions-only"]
    ? "correctness"
    : "measurement";
async function sample(
  browser,
  profile,
  name,
  viewport,
  mode = "slow",
  noFill = false,
) {
  const input = documentInput(profile, name);
  assert.equal(
    input.deferred,
    !input.enforcedCsp && (profile === "loading" || profile === "combined"),
    `${profile} must exercise its intended production loading mode`,
  );
  const index = samples.length;
  const label = `${index}-${profile}-${name}-${viewport.width}-${mode}${noFill ? "-nofill" : ""}`;
  active = {
    input,
    mode,
    runtimeAttempts: 0,
    unexpected: [],
    blockedRenderer: [],
    responseHeaders: {},
    html: input.html,
  };
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
  }
  if (mode === "renderer-absent") {
    active.html = withoutEarlyRenderer(input.html);
    assert.notEqual(
      active.html,
      input.html,
      "Renderer absence must remove the real compiled early entry",
    );
    // This fixture-only probe calls core APIs deliberately absent in this case.
    active.html = active.html.replace(
      /<script\b[^>]*>\s*window\.fixtureEarly=[\s\S]*?<\/script>/i,
      "",
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
    active.html = input.html.replace(
      /<script\b[^>]*id="trustedserver-js"[^>]*><\/script>/i,
      "",
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
      window.publisherFixtureOptions={initialLoad:${JSON.stringify(input.registered ? "disabled" : "enabled")}};
      ${fixtureSource.replaceAll("https://creative.example.com", creativeOrigin)}
      (${installDelivery.toString()})(${JSON.stringify({ height: input.creativeHeight, noFill })});
    }`,
    });
    page = await context.newPage();
    const errors = [];
    const network = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await context.route("**/*", (route) => {
      const url = route.request().url();
      network.push({ url, method: route.request().method() });
      if (
        url.startsWith(publisherOrigin + "/") ||
        url.startsWith(creativeOrigin + "/")
      )
        return route.continue();
      errors.push("Unexpected external request: " + url);
      return route.abort();
    });
    await page.goto(publisherOrigin + input.requestPath, {
      waitUntil: "load",
      timeout: 15000,
    });
    if (input.matched)
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
    assert.equal(errors.length, 0, "No unexpected page or network errors");
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
    } else {
      assert(
        raw.early.core && raw.early.queued,
        "Direct core API and queued publisher callback must work before deferred runtime",
      );
      assert.equal(
        raw.rendererVersion,
        1,
        "Real renderer must remain installed when only deferred runtime fails",
      );
    }
    assert.equal(
      raw.fixture.requests.length,
      input.matched ? 1 : 0,
      "Exactly one native initial request for a matched slot",
    );
    if (input.matched) {
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
        assert(
          text
            .flat()
            .includes(
              mode === "renderer-absent" ? "Publisher result" : "Server result",
            ),
          "Actual creative iframe DOM must contain server creative",
        );
        assert(
          Number.isFinite(raw.firstVisibleEstimateMs),
          "Visible ad estimate must be observed",
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
      if (input.deferred)
        assert(
          fcp < responseEnd,
          "Changed content must paint while runtime response is held",
        );
      else
        assert(
          fcp >= responseEnd,
          "Baseline must exercise parser-blocking runtime",
        );
    }
    const before = raw.geometry.find((entry) => entry.phase === "before-fill");
    const after = raw.geometry.at(-1);
    const expectedHeight = viewport.width >= 1024 ? 600 : 250;
    const reserved =
      ["combined", "reservation"].includes(profile) &&
      input.matched &&
      !input.absentWrapper;
    if (reserved) {
      assert.equal(
        Number.parseFloat(after.minHeight),
        expectedHeight,
        "Responsive reservation must match viewport",
      );
      if (noFill || input.creativeHeight <= expectedHeight) {
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
    result = {
      index,
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
    save("samples.json", samples);
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
      server: {
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
    browser = await chromium.launch({ headless: true });
    evidence.browser = browser.version();
    evidence.origins = { publisherOrigin, creativeOrigin };
    const mobile = { width: 390, height: 844 };
    const desktop = { width: 1280, height: 1000 };
    if (!options["correctness-only"] && !options["regressions-only"]) {
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
    await sample(browser, "combined", "registered", mobile, "renderer-absent");
    await sample(browser, "combined", "registered", mobile, "failed");
    await sample(browser, "combined", "csp", mobile);
    if (!options["regressions-only"]) {
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
    const groups = {};
    for (const entry of samples) {
      const key = `${entry.stage}/${entry.profile}/${entry.name}/${entry.viewport.width}/${entry.mode}/${entry.noFill ? "nofill" : "fill"}`;
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
      "CWV functional contracts passed; raw timings and ranges retained, no timing-improvement threshold applied.",
    );
  } finally {
    evidence.finishedAt = new Date().toISOString();
    save("artifacts.json", evidence);
    await browser?.close();
    await Promise.all(
      servers.map((server) => new Promise((resolve) => server.close(resolve))),
    );
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});

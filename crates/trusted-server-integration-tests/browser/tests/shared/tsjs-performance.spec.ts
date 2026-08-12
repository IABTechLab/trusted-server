import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import {
  expect,
  test,
  type Browser,
  type BrowserContext,
  type Page,
} from "@playwright/test";

const REPO_ROOT = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();
const WARMUPS = 5;
const SAMPLES = 50;
const PERCENTILE = 90;
const MAXIMUM_P90_RATIO = 1.1;
const HARD_P90_CEILING_MS = 100;
const REFERENCE_SHA = "62421ee44c62f24534ea8782a46dfa5bfbcea950";
const EXPECTED_CHROMIUM = "145.0.7632.6";
const MACHINE_CLASS = "github-hosted:ubuntu-24.04";
const RUNNER_IMAGE = "ubuntu-24.04";
const FIXTURE_ID = "tsjs-generated-loopback-paired-v2";
const CONTROLLER_ID = "generated-server-v1";
const CRITICAL_IDS = ["render_runtime", "gpt"] as const;
const DEFERRED_IDS = ["diagnostics_presentation", "gpt_later"] as const;
const HEAP_CEILINGS = {
  afterBoot: 1_329_697,
  afterFirstRender: 1_333_217,
  afterRefresh: 1_333_217,
  afterSpaNavigation: 1_341_419,
} as const;

type HeapCheckpoint = keyof typeof HEAP_CEILINGS;

interface ReleaseArtifact {
  id: string;
  role: "bootstrap" | "core" | "integration";
  phase: "critical" | "deferred" | null;
  trigger: "first_display_or_idle" | null;
  file: string;
  hash: string;
}

interface Release {
  version: 1;
  releaseId: string;
  artifacts: ReleaseArtifact[];
}

interface FixtureResources {
  release: Release;
  controllerDocument: string;
  criticalBody: string;
  criticalSrc: string;
  deferred: Map<string, { body: string; src: string }>;
}

interface FixtureRun {
  context: BrowserContext;
  page: Page;
  criticalRequests: string[];
  deferredRequests: string[];
  pageErrors: string[];
  consoleMessages: string[];
  close(): Promise<void>;
}

interface FixtureServer {
  origin: string;
  close(): Promise<void>;
}

interface BrowserObservation {
  timingMs: number;
  markTimingMs: number;
  bidsScriptCount: number;
  firstDisplayCount: number;
  firstDisplayPaintCount: number;
  measureCount: number;
  runtimeState: string | undefined;
  releaseId: string | undefined;
  displayCount: number;
  diagnosticsPresentationCount: number;
  criticalScriptCount: number;
  deferred: Array<{
    id: string;
    startTime: number;
    responseEnd: number;
    loadTime: number;
    preparationTime: number;
    executionTime: number;
  }>;
  paintTime: number;
  preloadBeforePaintCount: number;
}

function exactArtifact(release: Release, id: string): ReleaseArtifact {
  const matches = release.artifacts.filter((artifact) => artifact.id === id);
  if (matches.length !== 1)
    throw new Error(`expected one release artifact for ${id}`);
  return matches[0]!;
}

function loadFixtureResources(repositoryRoot: string): FixtureResources {
  const tsjsCrate = resolve(repositoryRoot, "crates/trusted-server-js");
  const dist = resolve(tsjsCrate, "dist");
  const releaseFile = resolve(dist, "tsjs-release-v1.json");
  const release = JSON.parse(readFileSync(releaseFile, "utf8")) as Release;
  expect(release.version).toBe(1);
  const criticalArtifacts = [exactArtifact(release, "core")].concat(
    CRITICAL_IDS.map((id) => exactArtifact(release, id)),
  );
  for (const artifact of criticalArtifacts.slice(1)) {
    expect(artifact.phase).toBe("critical");
  }
  const criticalBody = criticalArtifacts
    .map((artifact) => readFileSync(resolve(dist, artifact.file), "utf8"))
    .join(";\n");
  const criticalHash = createHash("sha256").update(criticalBody).digest("hex");
  const criticalSrc = `/static/tsjs=tsjs-unified.min.js?v=${criticalHash}`;
  const deferred = new Map<string, { body: string; src: string }>();
  for (const id of DEFERRED_IDS) {
    const artifact = exactArtifact(release, id);
    expect(artifact.phase).toBe("deferred");
    expect(artifact.trigger).toBe("first_display_or_idle");
    deferred.set(id, {
      body: readFileSync(resolve(dist, artifact.file), "utf8"),
      src: `/static/tsjs=tsjs-${id}.min.js?v=${artifact.hash}`,
    });
  }
  const directory = mkdtempSync(join(tmpdir(), "tsjs-performance-controller-"));
  const projectionPath = resolve(directory, "projection.json");
  let controllerDocument: string;
  try {
    writeFileSync(projectionPath, `${JSON.stringify(initialProjection())}\n`);
    const host = /^host: (.+)$/mu.exec(
      execFileSync("rustc", ["-vV"], { encoding: "utf8" }),
    )?.[1];
    if (!host) throw new Error("Rust host target is unavailable");
    controllerDocument = execFileSync(
      "cargo",
      [
        "run",
        "--quiet",
        "--package",
        "trusted-server-integration-tests",
        "--bin",
        "generate-tsjs-prospective-fixture",
        "--target",
        host,
        "--",
        "--projection",
        projectionPath,
        "--ids",
        [...CRITICAL_IDS, ...DEFERRED_IDS].join(","),
      ],
      {
        cwd: repositoryRoot,
        encoding: "utf8",
        env: { ...process.env, TSJS_SKIP_BUILD: "1" },
      },
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
  const criticalTag = `<script src="${criticalSrc}" id="trustedserver-js"></script>`;
  expect(controllerDocument.match(/tsjs:bids-script/gu)).toHaveLength(1);
  expect(controllerDocument.match(/id="trustedserver-js"/gu)).toHaveLength(1);
  expect(controllerDocument).toContain(criticalTag);
  expect(controllerDocument).toContain(`"releaseId":"${release.releaseId}"`);
  for (const { src } of deferred.values())
    expect(controllerDocument).toContain(src);
  return {
    release,
    controllerDocument,
    criticalBody,
    criticalSrc,
    deferred,
  };
}

function initialProjection() {
  const candidateId = "AAAAAAAAAAAA";
  return {
    version: 1,
    auction: {
      version: 1,
      auctionId: "performance-initial",
      results: [{ slot: "perf-slot", outcome: "winner", candidateId }],
    },
    slots: [
      {
        slot: "perf-slot",
        gamUnitPath: "/123/performance",
        divId: "perf-slot",
        formats: [[300, 250]],
        targeting: {},
      },
    ],
    bids: [
      {
        candidateId,
        slot: "perf-slot",
        provider: "trusted",
        upstreamBidId: "performance-upstream",
        cpm: 1,
        currency: "USD",
        targeting: { hb_bidder: "trusted" },
        rendererReservationId: `r1_${"a".repeat(22)}`,
        renderSource: {
          type: "adm",
          version: 1,
          adm: "<main>fictional performance creative</main>",
          width: 300,
          height: 250,
        },
      },
    ],
  };
}

function fixtureDocument(
  resources: FixtureResources,
  manualDisplay: boolean,
): string {
  const criticalTag = `<script src="${resources.criticalSrc}" id="trustedserver-js"></script>`;
  const controlledCriticalTag = `<script>window.__fixtureGpt.setManual(true);</script>${criticalTag}`;
  const withControlledGpt = resources.controllerDocument.replace(
    criticalTag,
    controlledCriticalTag,
  );
  if (withControlledGpt === resources.controllerDocument)
    throw new Error("generated controller critical tag is unavailable");
  if (manualDisplay) return withControlledGpt;
  const released = withControlledGpt.replace(
    "</body>",
    "<script>window.__fixtureGpt.release();</script></body>",
  );
  if (released === withControlledGpt)
    throw new Error("generated controller body is unavailable");
  return released;
}

async function installFixtureGpt(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    type Listener = (event: Record<string, unknown>) => void;
    type Slot = {
      addService(service: object): Slot;
      clearTargeting(key?: string): Slot;
      getAdUnitPath(): string;
      getSlotElementId(): string;
      getTargeting(key: string): string[];
      setTargeting(key: string, value: string | string[]): Slot;
    };
    const listeners = new Map<string, Set<Listener>>();
    const slots = new Map<string, Slot>();
    const physical = new Set<Slot>();
    const commands: Array<() => void> = [];
    const calls: string[] = [];
    let ready = true;
    let displayCount = 0;
    const createSlot = (id: string, path: string): Slot => {
      const targeting = new Map<string, string[]>();
      const slot: Slot = {
        addService: () => slot,
        clearTargeting: (key?: string) => {
          calls.push(`clearTargeting:${key ?? "*"}`);
          if (key === undefined) targeting.clear();
          else targeting.delete(key);
          return slot;
        },
        getAdUnitPath: () => path,
        getSlotElementId: () => id,
        getTargeting: (key: string) => {
          calls.push(`getTargeting:${key}`);
          return [...(targeting.get(key) ?? [])];
        },
        setTargeting: (key: string, value: string | string[]) => {
          calls.push(`setTargeting:${key}`);
          targeting.set(key, Array.isArray(value) ? [...value] : [value]);
          return slot;
        },
      };
      slots.set(id, slot);
      return slot;
    };
    const emit = (
      name: string,
      slot: Slot,
      facts: Record<string, unknown> = {},
    ) => {
      for (const listener of listeners.get(name) ?? [])
        listener({ slot, ...facts });
    };
    const pubads = {
      addEventListener(name: string, listener: Listener) {
        calls.push(`addEventListener:${name}`);
        const registered = listeners.get(name) ?? new Set<Listener>();
        registered.add(listener);
        listeners.set(name, registered);
      },
      removeEventListener(name: string, listener: Listener) {
        listeners.get(name)?.delete(listener);
      },
      disableInitialLoad() {},
      enableSingleRequest() {},
      getConfig: () => ({ disableInitialLoad: false }),
      getSlots: () => [...physical],
      refresh(requested?: Slot[]) {
        calls.push("refresh");
        for (const slot of requested ?? [...physical]) {
          queueMicrotask(() => {
            emit("slotRequested", slot);
            emit("slotRenderEnded", slot, { isEmpty: true });
          });
        }
      },
    };
    const googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: {
        push(command: () => void) {
          if (ready) command();
          else commands.push(command);
          return commands.length;
        },
      },
      defineSlot(path: string, _sizes: unknown, id: string) {
        calls.push(`defineSlot:${id}`);
        const slot = slots.get(id) ?? createSlot(id, path);
        physical.add(slot);
        return slot;
      },
      destroySlots(requested?: Slot[]) {
        for (const slot of requested ?? [...physical]) physical.delete(slot);
        return true;
      },
      display(target: string | Slot) {
        calls.push("display");
        const slot = typeof target === "string" ? slots.get(target) : target;
        if (!slot) return;
        displayCount += 1;
        queueMicrotask(() => {
          emit("slotRequested", slot);
          emit("slotRenderEnded", slot, { isEmpty: true });
        });
      },
      getConfig: () => ({ disableInitialLoad: false }),
      pubads: () => pubads,
      setConfig() {},
    };
    const loadTimes = new Map<string, number>();
    const preparationTimes = new Map<string, number>();
    const executionTimes = new Map<string, number>();
    const preloadTimes: number[] = [];
    new MutationObserver((records) => {
      for (const record of records) {
        for (const added of record.addedNodes) {
          if (added instanceof HTMLLinkElement) {
            if (
              added.relList.contains("preload") &&
              /tsjs-(?:diagnostics_presentation|gpt_later)\.min\.js/u.test(
                added.href,
              )
            ) {
              preloadTimes.push(performance.now());
            }
            continue;
          }
          if (!(added instanceof HTMLScriptElement)) continue;
          const match = /tsjs-([a-z0-9_-]+)\.min\.js/u.exec(added.src);
          if (!match?.[1]) continue;
          added.addEventListener(
            "load",
            () => {
              const id = match[1]!;
              loadTimes.set(id, performance.now());
              // The runtime's property onload handler was registered before this
              // observer listener. It invokes synchronous module preparation before
              // its first await, so reaching this listener proves preparation ran.
              preparationTimes.set(id, performance.now());
              queueMicrotask(() => {
                // The loader's await continuation was queued before this listener;
                // these deferred modules activate synchronously in that continuation.
                executionTimes.set(id, performance.now());
              });
            },
            {
              once: true,
            },
          );
        }
      }
    }).observe(document, { childList: true, subtree: true });
    window.googletag = googletag;
    Object.defineProperty(window, "__fixtureGpt", {
      configurable: true,
      value: {
        displayCount: () => displayCount,
        calls: () => [...calls],
        executionTime: (id: string) => executionTimes.get(id),
        loadTime: (id: string) => loadTimes.get(id),
        preparationTime: (id: string) => preparationTimes.get(id),
        preloadTimes: () => [...preloadTimes],
        publisherRefresh() {
          const slot = [...physical][0];
          if (!slot)
            return Promise.reject(
              new Error("publisher GPT slot is unavailable"),
            );
          const service = googletag.pubads();
          return new Promise<void>((resolveRefresh, rejectRefresh) => {
            const timeout = window.setTimeout(() => {
              service.removeEventListener("slotRenderEnded", onRendered);
              rejectRefresh(
                new Error("publisher GPT refresh did not complete"),
              );
            }, 5_000);
            const onRendered: Listener = (event) => {
              if (event.slot !== slot) return;
              window.clearTimeout(timeout);
              service.removeEventListener("slotRenderEnded", onRendered);
              resolveRefresh();
            };
            service.addEventListener("slotRenderEnded", onRendered);
            service.refresh([slot]);
          });
        },
        release() {
          ready = true;
          googletag.apiReady = true;
          commands.splice(0).forEach((command) => command());
        },
        setManual(manual: boolean) {
          ready = !manual;
          googletag.apiReady = !manual;
        },
      },
    });
  });
}

function noBidAuction(requestBody: unknown) {
  const request = requestBody as {
    id?: unknown;
    imp?: Array<{ id?: unknown }>;
  };
  const auctionId =
    typeof request.id === "string" ? request.id : "performance-refresh";
  const results = (Array.isArray(request.imp) ? request.imp : []).map(
    (imp) => ({
      slot: typeof imp.id === "string" ? imp.id : "perf-slot",
      outcome: "no_bid",
    }),
  );
  return {
    id: auctionId,
    cur: "USD",
    seatbid: [],
    ext: {
      trusted_server: { slot_results: { version: 1, auctionId, results } },
    },
  };
}

function sendResponse(
  response: ServerResponse,
  status: number,
  contentType: string,
  body: string,
  script = false,
): void {
  response.writeHead(status, {
    "Content-Type": contentType,
    ...(script ? { "X-Content-Type-Options": "nosniff" } : {}),
  });
  response.end(body);
}

async function requestJson(request: IncomingMessage): Promise<unknown> {
  request.setEncoding("utf8");
  let body = "";
  for await (const chunk of request) body += chunk;
  return JSON.parse(body) as unknown;
}

async function serveFixtureRequest(
  request: IncomingMessage,
  response: ServerResponse,
  resources: FixtureResources,
): Promise<void> {
  const url = new URL(request.url ?? "/", "http://127.0.0.1");
  if (url.pathname === "/fixture") {
    sendResponse(
      response,
      200,
      "text/html; charset=utf-8",
      fixtureDocument(resources, url.searchParams.get("manual") === "1"),
    );
    return;
  }
  if (`${url.pathname}${url.search}` === resources.criticalSrc) {
    sendResponse(
      response,
      200,
      "application/javascript; charset=utf-8",
      resources.criticalBody,
      true,
    );
    return;
  }
  for (const [id, artifact] of resources.deferred) {
    if (`${url.pathname}${url.search}` !== artifact.src) continue;
    if (id === "gpt_later")
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 200));
    sendResponse(
      response,
      200,
      "application/javascript; charset=utf-8",
      artifact.body,
      true,
    );
    return;
  }
  if (url.pathname === "/_ts/auction") {
    sendResponse(
      response,
      200,
      "application/json",
      JSON.stringify(noBidAuction(await requestJson(request))),
    );
    return;
  }
  if (url.pathname === "/_ts/page-bids") {
    sendResponse(
      response,
      200,
      "application/json",
      JSON.stringify({
        version: 1,
        auction: {
          version: 1,
          auctionId: "performance-navigation",
          results: [{ slot: "perf-slot", outcome: "no_bid" }],
        },
        slots: [
          {
            slot: "perf-slot",
            gamUnitPath: "/123/performance",
            divId: "perf-slot",
            formats: [[300, 250]],
            targeting: {},
          },
        ],
        bids: [],
      }),
    );
    return;
  }
  sendResponse(response, 404, "text/plain; charset=utf-8", "not found");
}

async function startFixtureServer(
  resources: FixtureResources,
): Promise<FixtureServer> {
  const server = createServer((request, response) => {
    void serveFixtureRequest(request, response, resources).catch((error) => {
      const failure = error instanceof Error ? error : new Error(String(error));
      if (response.headersSent) {
        response.destroy(failure);
        return;
      }
      sendResponse(
        response,
        500,
        "text/plain; charset=utf-8",
        "fixture server failure",
      );
    });
  });
  await new Promise<void>((resolveListen, rejectListen) => {
    const onError = (error: Error) => {
      server.off("listening", onListening);
      rejectListen(error);
    };
    const onListening = () => {
      server.off("error", onError);
      resolveListen();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(0, "127.0.0.1");
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    server.close();
    throw new Error("fixture server address is unavailable");
  }
  let closed = false;
  return {
    origin: `http://127.0.0.1:${address.port}`,
    close: async () => {
      if (closed) return;
      closed = true;
      await new Promise<void>((resolveClose, rejectClose) => {
        server.close((error) => {
          if (error) rejectClose(error);
          else resolveClose();
        });
        server.closeAllConnections();
      });
    },
  };
}

async function openFixture(
  browser: Browser,
  fixtureServer: FixtureServer,
  manualDisplay = false,
): Promise<FixtureRun> {
  const context = await browser.newContext();
  await installFixtureGpt(context);
  const page = await context.newPage();
  const criticalRequests: string[] = [];
  const deferredRequests: string[] = [];
  const pageErrors: string[] = [];
  const consoleMessages: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (url.includes("/static/tsjs=tsjs-unified.min.js"))
      criticalRequests.push(url);
    if (
      DEFERRED_IDS.some((id) => url.includes(`/static/tsjs=tsjs-${id}.min.js`))
    ) {
      deferredRequests.push(url);
    }
  });
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("console", (message) =>
    consoleMessages.push(`${message.type()}: ${message.text()}`),
  );
  const fixtureUrl = `${fixtureServer.origin}/fixture${manualDisplay ? "?manual=1" : ""}`;
  await page.goto(fixtureUrl, { waitUntil: "load" });
  return {
    context,
    page,
    criticalRequests,
    deferredRequests,
    pageErrors,
    consoleMessages,
    close: () => context.close(),
  };
}

async function waitForMark(run: FixtureRun, name: string): Promise<void> {
  const { page } = run;
  try {
    await expect
      .poll(() =>
        page.evaluate(
          (mark) => performance.getEntriesByName(mark).length,
          name,
        ),
      )
      .toBe(1);
  } catch (error) {
    const diagnostics = await page.evaluate(() => ({
      runtimeState: window.tsjs?._internal?.state,
      runtimeReason: (window.tsjs?._internal as { reason?: string } | undefined)
        ?.reason,
      names: window.tsjs ? Object.getOwnPropertyNames(window.tsjs) : [],
      marks: performance
        .getEntriesByType("mark")
        .map((entry) => ({ name: entry.name, startTime: entry.startTime })),
      displayCount: window.__fixtureGpt.displayCount(),
      gptCalls: window.__fixtureGpt.calls(),
      renderTrace: window.tsjs?.diagnostics?.renderTrace?.current(),
      renderHistory: window.tsjs?.diagnostics?.renderTrace?.history(),
    }));
    throw new Error(
      `${name} was not recorded: ${JSON.stringify({ ...diagnostics, consoleMessages: run.consoleMessages, criticalRequests: run.criticalRequests, deferredRequests: run.deferredRequests, pageErrors: run.pageErrors })}; ${String(error)}`,
    );
  }
}

async function observeFixture(run: FixtureRun): Promise<BrowserObservation> {
  await waitForMark(run, "tsjs:first-display");
  await waitForMark(run, "tsjs:first-display-paint");
  for (const id of DEFERRED_IDS) {
    await expect
      .poll(() =>
        run.page.evaluate(
          (moduleId) => window.__fixtureGpt.executionTime(moduleId),
          id,
        ),
      )
      .not.toBeUndefined();
  }
  const observation = await run.page.evaluate((deferredIds) => {
    const entries = (name: string) => performance.getEntriesByName(name);
    const bids = entries("tsjs:bids-script");
    const display = entries("tsjs:first-display");
    const paint = entries("tsjs:first-display-paint");
    const measure = entries("tsjs:boot-to-first-display");
    const resources = performance.getEntriesByType(
      "resource",
    ) as PerformanceResourceTiming[];
    const deferred = deferredIds.map((id) => {
      const resource = resources.find((entry) =>
        entry.name.includes(`tsjs-${id}.min.js`),
      );
      if (!resource)
        throw new Error(`missing real deferred resource entry for ${id}`);
      const loadTime = window.__fixtureGpt.loadTime(id);
      const preparationTime = window.__fixtureGpt.preparationTime(id);
      const executionTime = window.__fixtureGpt.executionTime(id);
      if (
        loadTime === undefined ||
        preparationTime === undefined ||
        executionTime === undefined
      ) {
        throw new Error(
          `missing real deferred lifecycle observation for ${id}`,
        );
      }
      return {
        id,
        startTime: resource.startTime,
        responseEnd: resource.responseEnd,
        loadTime,
        preparationTime,
        executionTime,
      };
    });
    const paintTime = paint[0]?.startTime ?? -1;
    return {
      timingMs: measure[0]?.duration ?? Number.NaN,
      markTimingMs:
        (display[0]?.startTime ?? Number.NaN) -
        (bids[0]?.startTime ?? Number.NaN),
      bidsScriptCount: bids.length,
      firstDisplayCount: display.length,
      firstDisplayPaintCount: paint.length,
      measureCount: measure.length,
      runtimeState: window.tsjs?._internal?.state,
      releaseId: window.tsjs?.releaseId,
      displayCount: window.__fixtureGpt.displayCount(),
      diagnosticsPresentationCount: document.querySelectorAll(
        "#ts-render-trace-panel",
      ).length,
      criticalScriptCount: document.querySelectorAll("script#trustedserver-js")
        .length,
      deferred,
      paintTime,
      preloadBeforePaintCount: window.__fixtureGpt
        .preloadTimes()
        .filter((time) => time < paintTime).length,
    };
  }, DEFERRED_IDS);
  expect(observation.bidsScriptCount).toBe(1);
  expect(observation.firstDisplayCount).toBe(1);
  expect(observation.firstDisplayPaintCount).toBe(1);
  expect(observation.measureCount).toBe(1);
  expect(observation.runtimeState).toBe("kernel");
  expect(observation.displayCount).toBe(1);
  expect(observation.diagnosticsPresentationCount).toBe(1);
  expect(observation.criticalScriptCount).toBe(1);
  expect(run.criticalRequests).toHaveLength(1);
  expect(run.deferredRequests).toHaveLength(DEFERRED_IDS.length);
  expect(run.pageErrors).toEqual([]);
  expect(Number.isFinite(observation.timingMs)).toBe(true);
  expect(observation.timingMs).toBeGreaterThanOrEqual(0);
  expect(observation.timingMs).toBeCloseTo(observation.markTimingMs, 8);
  expect(observation.preloadBeforePaintCount).toBe(0);
  expect(
    observation.deferred.every(
      (entry) => entry.startTime >= observation.paintTime,
    ),
  ).toBe(true);
  expect(
    observation.deferred.every(
      (entry) => entry.preparationTime >= observation.paintTime,
    ),
  ).toBe(true);
  expect(
    observation.deferred.every(
      (entry) => entry.executionTime >= observation.paintTime,
    ),
  ).toBe(true);
  expect(
    observation.deferred.every(
      (entry) =>
        entry.startTime <= entry.responseEnd &&
        entry.responseEnd <= entry.loadTime &&
        entry.loadTime <= entry.preparationTime &&
        entry.preparationTime <= entry.executionTime,
    ),
  ).toBe(true);
  expect(
    Math.max(...observation.deferred.map((entry) => entry.startTime)) -
      Math.min(...observation.deferred.map((entry) => entry.startTime)),
  ).toBeLessThanOrEqual(50);
  const fast = observation.deferred.find(
    ({ id }) => id === "diagnostics_presentation",
  )!;
  const slow = observation.deferred.find(({ id }) => id === "gpt_later")!;
  expect(fast.responseEnd).toBeLessThan(slow.responseEnd);
  expect(fast.loadTime).toBeLessThan(slow.loadTime);
  expect(fast.executionTime).toBeLessThan(slow.executionTime);
  expect(fast.executionTime).toBeLessThan(slow.responseEnd);
  return observation;
}

async function collectHeap(page: Page): Promise<number> {
  const session = await page.context().newCDPSession(page);
  try {
    await session.send("HeapProfiler.collectGarbage");
    const usage = await session.send("Runtime.getHeapUsage");
    return usage.usedSize as number;
  } finally {
    await session.detach();
  }
}

function p90(values: readonly number[]): number {
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.ceil((PERCENTILE / 100) * ordered.length) - 1]!;
}

function packageVersion(packagePath: string): string {
  return (JSON.parse(readFileSync(packagePath, "utf8")) as { version: string })
    .version;
}

declare global {
  interface Window {
    googletag: unknown;
    __fixtureGpt: {
      displayCount(): number;
      calls(): string[];
      executionTime(id: string): number | undefined;
      loadTime(id: string): number | undefined;
      preparationTime(id: string): number | undefined;
      preloadTimes(): number[];
      publisherRefresh(): Promise<void>;
      release(): void;
      setManual(manual: boolean): void;
    };
    tsjs?: {
      _internal?: { state?: string };
      diagnostics?: {
        renderTrace?: { current(): unknown; history(): unknown };
      };
      releaseId?: string;
      requestAds(options?: { slots?: readonly string[] }): Promise<unknown>;
    };
  }
}

test.describe("TSJS first-display performance gate", () => {
  test.describe.configure({ retries: 0, mode: "serial" });
  const activeFixtureServers: FixtureServer[] = [];

  test.afterEach(async () => {
    await Promise.all(
      activeFixtureServers.splice(0).map((server) => server.close()),
    );
  });

  test("records complete real timing, heap, and request-ordering evidence", async ({
    browser,
    browserName,
  }) => {
    test.setTimeout(240_000);
    const mode = process.env.TSJS_PERF_MODE;
    test.skip(
      mode !== "preswitch" && mode !== "postswitch",
      "performance evidence run only",
    );
    expect(browserName).toBe("chromium");
    expect(browser.version()).toBe(EXPECTED_CHROMIUM);
    expect(process.env.TSJS_PERF_MACHINE_CLASS).toBe(MACHINE_CLASS);
    expect(process.env.TSJS_PERF_RUNNER_IMAGE).toBe(RUNNER_IMAGE);
    const referenceRoot = process.env.TSJS_PERF_REFERENCE_ROOT;
    expect(referenceRoot, "TSJS_PERF_REFERENCE_ROOT is required").toBeTruthy();
    expect(
      execFileSync("git", ["-C", referenceRoot!, "rev-parse", "HEAD"], {
        encoding: "utf8",
      }).trim(),
    ).toBe(REFERENCE_SHA);
    const referenceResources = loadFixtureResources(referenceRoot!);
    const currentResources = loadFixtureResources(REPO_ROOT);
    const referenceServer = await startFixtureServer(referenceResources);
    const currentServer = await startFixtureServer(currentResources);
    activeFixtureServers.push(referenceServer, currentServer);

    const observeVariant = async (
      fixtureServer: FixtureServer,
      resources: FixtureResources,
    ): Promise<BrowserObservation> => {
      const run = await openFixture(browser, fixtureServer);
      try {
        const observation = await observeFixture(run);
        expect(observation.releaseId).toBe(resources.release.releaseId);
        return observation;
      } finally {
        await run.close();
      }
    };

    for (let index = 0; index < WARMUPS; index += 1) {
      const variants =
        index % 2 === 0
          ? ([
              [referenceServer, referenceResources],
              [currentServer, currentResources],
            ] as const)
          : ([
              [currentServer, currentResources],
              [referenceServer, referenceResources],
            ] as const);
      for (const [server, resources] of variants) {
        await observeVariant(server, resources);
      }
    }

    const referenceSamples: number[] = [];
    const currentSamples: number[] = [];
    let representative: BrowserObservation | undefined;
    for (let index = 0; index < SAMPLES; index += 1) {
      const variants =
        index % 2 === 0
          ? ([
              ["reference", referenceServer, referenceResources],
              ["current", currentServer, currentResources],
            ] as const)
          : ([
              ["current", currentServer, currentResources],
              ["reference", referenceServer, referenceResources],
            ] as const);
      for (const [variant, server, resources] of variants) {
        const observation = await observeVariant(server, resources);
        if (variant === "reference") {
          referenceSamples.push(observation.timingMs);
        } else {
          currentSamples.push(observation.timingMs);
          representative = observation;
        }
      }
    }
    expect(referenceSamples).toHaveLength(SAMPLES);
    expect(currentSamples).toHaveLength(SAMPLES);
    const referenceP90 = p90(referenceSamples);
    const currentP90 = p90(currentSamples);
    expect(referenceP90).toBeGreaterThan(0);
    expect(referenceP90).toBeLessThanOrEqual(HARD_P90_CEILING_MS);
    expect(currentP90).toBeLessThanOrEqual(HARD_P90_CEILING_MS);
    expect(currentP90).toBeLessThanOrEqual(referenceP90 * MAXIMUM_P90_RATIO);

    const heapRun = await openFixture(browser, currentServer, true);
    const retainedHeapBytes = {} as Record<HeapCheckpoint, number>;
    try {
      await expect
        .poll(() => heapRun.page.evaluate(() => window.tsjs?._internal?.state))
        .toBe("kernel");
      retainedHeapBytes.afterBoot = await collectHeap(heapRun.page);
      await heapRun.page.evaluate(() => window.__fixtureGpt.release());
      await waitForMark(heapRun, "tsjs:first-display-paint");
      retainedHeapBytes.afterFirstRender = await collectHeap(heapRun.page);
      await heapRun.page.evaluate(() => window.__fixtureGpt.publisherRefresh());
      retainedHeapBytes.afterRefresh = await collectHeap(heapRun.page);
      for (const id of DEFERRED_IDS) {
        await expect
          .poll(() =>
            heapRun.page.evaluate(
              (moduleId) => window.__fixtureGpt.executionTime(moduleId),
              id,
            ),
          )
          .not.toBeUndefined();
      }
      const navigationResponse = heapRun.page.waitForResponse((response) =>
        response.url().includes("/_ts/page-bids"),
      );
      await heapRun.page.evaluate(() =>
        history.pushState({}, "", "/fixture?navigation=1"),
      );
      const response = await navigationResponse;
      expect(await response.finished()).toBeNull();
      // Replacement disposal removes the old physical slot before page-bids fetch.
      // Its return proves the response was parsed, committed, and reconciled.
      await expect
        .poll(() =>
          heapRun.page.evaluate(() => {
            const googletag = window.googletag as {
              pubads(): {
                getSlots(): Array<{ getSlotElementId(): string }>;
              };
            };
            return googletag
              .pubads()
              .getSlots()
              .map((slot) => slot.getSlotElementId());
          }),
        )
        .toEqual(["perf-slot"]);
      retainedHeapBytes.afterSpaNavigation = await collectHeap(heapRun.page);
    } finally {
      await heapRun.close();
    }
    for (const [name, ceiling] of Object.entries(HEAP_CEILINGS) as Array<
      [HeapCheckpoint, number]
    >) {
      expect(
        retainedHeapBytes[name],
        `${name} retained heap`,
      ).toBeLessThanOrEqual(ceiling);
    }

    const evidenceId = process.env.TSJS_EVIDENCE_ID;
    const headSha = process.env.GITHUB_SHA;
    const outputArgument = process.env.TSJS_PERF_OUTPUT;
    expect(evidenceId, "TSJS_EVIDENCE_ID is required").toMatch(
      /^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/u,
    );
    expect(headSha, "GITHUB_SHA is required").toMatch(/^[0-9a-f]{40}$/u);
    expect(outputArgument, "TSJS_PERF_OUTPUT is required").toBeTruthy();
    const outputPath = isAbsolute(outputArgument!)
      ? outputArgument!
      : resolve(REPO_ROOT, outputArgument!);
    const deferredBeforePaint = representative!.deferred.filter(
      ({ startTime }) => startTime < representative!.paintTime,
    ).length;
    const deferredPreparationBeforePaint = representative!.deferred.filter(
      ({ preparationTime }) => preparationTime < representative!.paintTime,
    ).length;
    const deferredExecutionBeforePaint = representative!.deferred.filter(
      ({ executionTime }) => executionTime < representative!.paintTime,
    ).length;
    const deferredStarts = representative!.deferred.map(
      ({ startTime }) => startTime,
    );
    const fastDeferred = representative!.deferred.find(
      ({ id }) => id === "diagnostics_presentation",
    )!;
    const slowDeferred = representative!.deferred.find(
      ({ id }) => id === "gpt_later",
    )!;
    const npmVersion = execFileSync("npm", ["--version"], {
      encoding: "utf8",
    }).trim();
    const evidence = {
      schemaVersion: 3,
      evidenceId,
      mode,
      headSha,
      environment: {
        chromium: browser.version(),
        controller: CONTROLLER_ID,
        machineClass: process.env.TSJS_PERF_MACHINE_CLASS,
        runnerImage: process.env.TSJS_PERF_RUNNER_IMAGE,
        fixture: FIXTURE_ID,
        node: process.version,
        npm: npmVersion,
        typescript: packageVersion(
          resolve(
            REPO_ROOT,
            "crates/trusted-server-js/lib/node_modules/typescript/package.json",
          ),
        ),
      },
      sampling: {
        warmupsPerVariant: WARMUPS,
        samplesPerVariant: SAMPLES,
        percentile: PERCENTILE,
        interleaving: "alternating-reference-current",
      },
      marks: {
        source: "performance-entry",
        bidsScript: representative!.bidsScriptCount === 1,
        firstDisplay: representative!.firstDisplayCount === 1,
        firstDisplayPaint: representative!.firstDisplayPaintCount === 1,
      },
      performance: {
        bootToFirstDisplayMs: {
          reference: {
            sha: REFERENCE_SHA,
            samples: referenceSamples,
            p90: referenceP90,
          },
          current: { samples: currentSamples, p90: currentP90 },
          percentile: PERCENTILE,
          maximumRatio: MAXIMUM_P90_RATIO,
          observedRatio: currentP90 / referenceP90,
          hardCeilingMs: HARD_P90_CEILING_MS,
        },
      },
      heap: {
        collection: "one-collectGarbage-then-immediate-getHeapUsage",
        checkpoints: Object.fromEntries(
          (
            Object.entries(HEAP_CEILINGS) as Array<[HeapCheckpoint, number]>
          ).map(([name, ceilingBytes]) => [
            name,
            { usedSize: retainedHeapBytes[name], ceilingBytes },
          ]),
        ),
      },
      requests: {
        critical: { count: 1 },
        deferred: {
          count: representative!.deferred.length,
          requestBeforePaintCount: deferredBeforePaint,
          preloadBeforePaintCount: representative!.preloadBeforePaintCount,
          preparationBeforePaintCount: deferredPreparationBeforePaint,
          executionBeforePaintCount: deferredExecutionBeforePaint,
          independentlyTriggered:
            Math.max(...deferredStarts) - Math.min(...deferredStarts) <= 50,
          headOfLineBlocking: !(
            fastDeferred.responseEnd < slowDeferred.responseEnd &&
            fastDeferred.loadTime < slowDeferred.loadTime &&
            fastDeferred.executionTime < slowDeferred.executionTime &&
            fastDeferred.executionTime < slowDeferred.responseEnd
          ),
        },
      },
      assertions: { correctness: true, loadOrder: true },
      provenance: {
        workflowName: process.env.TSJS_PERF_WORKFLOW_NAME,
        workflowFile: process.env.TSJS_PERF_WORKFLOW_FILE,
        runId: Number(process.env.GITHUB_RUN_ID),
        runAttempt: Number(process.env.GITHUB_RUN_ATTEMPT),
        artifactName: process.env.TSJS_PERF_ARTIFACT_NAME,
        headSha,
      },
      result: "complete",
    };
    mkdirSync(dirname(outputPath), { recursive: true });
    writeFileSync(outputPath, `${JSON.stringify(evidence, null, 2)}\n`);
  });
});

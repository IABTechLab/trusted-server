import { execFileSync } from "node:child_process";
import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import {
  existsSync,
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
import { measureBytes } from "../../../../trusted-server-js/lib/scripts/bundle-metrics.mjs";

const REPO_ROOT = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();
const WARMUPS = 5;
const SAMPLES = 50;
const PERCENTILE = 90;
const MAXIMUM_P90_RATIO = 1.1;
const EXPECTED_CHROMIUM = "145.0.7632.6";
const MACHINE_CLASS = "github-hosted:ubuntu-24.04";
const RUNNER_IMAGE = "ubuntu-24.04";
const FIXTURE_ID = "tsjs-main-paired-network-v2";
const CONTROLLER_ID = "generated-server-v1+production-main-v1";
const COMPARISON_START_MARK = "tsjs:comparison-start";
const FIRST_OBSERVABLE_ACTION_MARK = "tsjs:first-observable-action";
const NETWORK_PROFILE = Object.freeze({
  offline: false,
  latency: 150,
  downloadThroughput: 200_000,
  uploadThroughput: 93_750,
  packetLoss: 0,
});
// Current main always ships creative with core, and production's default
// creative guard is enabled. Keep both comparison sides on that same shape.
const SELECTED_IDS = ["render_runtime", "creative", "gpt"] as const;
const DEFERRED_IDS = ["diagnostics_presentation", "gpt_later"] as const;
const LEGACY_AD_INIT_INLINE = "window.tsjs.adInit();";
const HEAP_CHECKPOINTS = [
  "afterBoot",
  "afterFirstRender",
  "afterRefresh",
  "afterSpaNavigation",
] as const;
const MAXIMUM_HEAP_RATIO = 1.1;
const HARD_HEAP_CEILING_BYTES = 4 * 1024 * 1024;

type HeapCheckpoint = (typeof HEAP_CHECKPOINTS)[number];

interface ReleaseArtifact {
  id: string;
  role:
    | "bootstrap"
    | "first_display_base"
    | "first_display_slice"
    | "core"
    | "integration";
  phase: "first_display" | "takeover" | "deferred" | null;
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
  artifactModel: "release-v1" | "legacy-main-v1";
  release: Release | null;
  controllerDocument: string | null;
  selectedBody: string;
  selectedTransferBytes: number;
  selectedSrc: string;
  runtimeBody: string | null;
  runtimeSrc: string | null;
  deferred: Map<string, { body: string; src: string }>;
  referenceTransfer: ReferenceTransfer;
}

interface ReferenceTransferSource {
  semanticEndpoint: string;
  delivery: "inline" | "external";
  rawBytes: number;
  gzipBytes: number;
  brotliBytes: number;
  sha256: string;
}

interface ReferenceTransfer {
  sources: ReferenceTransferSource[];
  rawBytes: number;
  gzipBytes: number;
  brotliBytes: number;
}

interface FixtureRun {
  context: BrowserContext;
  page: Page;
  selectedRequests: string[];
  runtimeRequests: string[];
  deferredRequests: string[];
  pageErrors: string[];
  consoleMessages: string[];
  close(): Promise<void>;
}

interface FixtureServer {
  origin: string;
  close(): Promise<void>;
}

interface FixtureServerOptions {
  rejectRuntime?: boolean;
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
  selectedScriptCount: number;
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

interface ComparisonObservation {
  timingMs: number;
  displayCount: number;
  releaseId: string | undefined;
}

function exactArtifact(release: Release, id: string): ReleaseArtifact {
  const matches = release.artifacts.filter((artifact) => artifact.id === id);
  if (matches.length !== 1)
    throw new Error(`expected one release artifact for ${id}`);
  return matches[0]!;
}

function legacyBootInline(): string {
  const slots = [
    {
      id: "perf-slot",
      gam_unit_path: "/123/performance",
      div_id: "perf-slot",
      formats: [[300, 250]],
      targeting: {},
    },
  ];
  return `window.tsjs={adSlots:${JSON.stringify(slots)},bids:{},navGeneration:0};window.__tsjs_gpt_enabled=true;`;
}

function exactControllerInline(document: string): string {
  const normalized = document.toLowerCase();
  const opening = "<script>";
  const closing = "</script";
  const inline: string[] = [];
  let cursor = 0;
  for (;;) {
    const start = normalized.indexOf(opening, cursor);
    if (start < 0) break;
    const bodyStart = start + opening.length;
    let closeStart = normalized.indexOf(closing, bodyStart);
    let closeEnd = -1;
    while (closeStart >= 0) {
      closeEnd = closeStart + closing.length;
      while (
        closeEnd < normalized.length &&
        (normalized[closeEnd] === " " ||
          normalized[closeEnd] === "\t" ||
          normalized[closeEnd] === "\n" ||
          normalized[closeEnd] === "\r" ||
          normalized[closeEnd] === "\f")
      ) {
        closeEnd += 1;
      }
      if (normalized[closeEnd] === ">") break;
      closeStart = normalized.indexOf(closing, closeEnd);
    }
    if (closeStart < 0 || closeEnd < 0)
      throw new Error(
        "production controller contains an unclosed inline script",
      );
    inline.push(document.slice(bodyStart, closeStart));
    cursor = closeEnd + 1;
  }
  if (
    inline.length !== 1 ||
    !inline[0]?.includes('performance.mark("tsjs:bids-script")')
  ) {
    throw new Error(
      "production controller must contain one TSJS bids-script inline source",
    );
  }
  return inline[0];
}

function measureReferenceTransfer(
  sources: ReadonlyArray<{
    semanticEndpoint: string;
    delivery: "inline" | "external";
    body: string;
  }>,
): ReferenceTransfer {
  if (sources.length === 0)
    throw new Error("semantic transfer must not be empty");
  const endpoints = new Set<string>();
  const measured = sources.map(({ semanticEndpoint, delivery, body }) => {
    if (
      !semanticEndpoint ||
      endpoints.has(semanticEndpoint) ||
      body.length === 0
    ) {
      throw new Error("semantic transfer source is empty or duplicated");
    }
    endpoints.add(semanticEndpoint);
    return {
      semanticEndpoint,
      delivery,
      ...measureBytes(Buffer.from(body, "utf8")),
    };
  });
  return {
    sources: measured,
    rawBytes: measured.reduce((total, source) => total + source.rawBytes, 0),
    gzipBytes: measured.reduce((total, source) => total + source.gzipBytes, 0),
    brotliBytes: measured.reduce(
      (total, source) => total + source.brotliBytes,
      0,
    ),
  };
}

function loadReleaseFixtureResources(repositoryRoot: string): FixtureResources {
  const tsjsCrate = resolve(repositoryRoot, "crates/trusted-server-js");
  const dist = resolve(tsjsCrate, "dist");
  const releaseFile = resolve(dist, "tsjs-release-v1.json");
  const release = JSON.parse(readFileSync(releaseFile, "utf8")) as Release;
  expect(release.version).toBe(1);
  const runtimeArtifacts = [exactArtifact(release, "core")].concat(
    SELECTED_IDS.map((id) => exactArtifact(release, id)),
  );
  for (const artifact of runtimeArtifacts.slice(1)) {
    expect(artifact.phase).toBe("takeover");
  }
  expect(
    runtimeArtifacts.map(({ id }) => id),
    "release-v1 performance shape must stay core + render + creative + GPT",
  ).toEqual(["core", "render_runtime", "creative", "gpt"]);
  const runtimeBody = runtimeArtifacts
    .map((artifact) => readFileSync(resolve(dist, artifact.file), "utf8"))
    .join(";\n");
  const runtimeHash = createHash("sha256").update(runtimeBody).digest("hex");
  const runtimeSrc = `/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}`;
  const firstDisplayIds = [
    "first_display",
    "creative_initial",
    "gpt_initial",
  ] as const;
  const firstDisplayArtifacts = firstDisplayIds.map((id) =>
    exactArtifact(release, id),
  );
  for (const artifact of firstDisplayArtifacts)
    expect(artifact.phase).toBe("first_display");
  const selectedBody = firstDisplayArtifacts
    .map((artifact) => readFileSync(resolve(dist, artifact.file), "utf8"))
    .join(";\n");
  const selectedHash = createHash("sha256").update(selectedBody).digest("hex");
  const selectedSrc = `/static/tsjs=tsjs-first-display.min.js?m=0045&v=${selectedHash}`;
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
        "generate-tsjs-fixture",
        "--target",
        host,
        "--",
        "--projection",
        projectionPath,
        "--ids",
        [...SELECTED_IDS, ...DEFERRED_IDS].join(","),
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
  const selectedTag = `<script src="${selectedSrc}" id="trustedserver-js"></script>`;
  // The single generated bootstrap carries mutually exclusive direct-runtime and
  // first-display branches; each branch owns one mark callsite.
  expect(controllerDocument.match(/tsjs:bids-script/gu)).toHaveLength(2);
  expect(controllerDocument.match(/id="trustedserver-js"/gu)).toHaveLength(1);
  expect(controllerDocument).toContain(selectedTag);
  expect(controllerDocument).toContain(`"releaseId":"${release.releaseId}"`);
  expect(controllerDocument).toContain(`"runtimeSrc":"${runtimeSrc}"`);
  expect(controllerDocument).not.toContain(
    `<script src="${runtimeSrc}" id="trustedserver-js"></script>`,
  );
  for (const { src } of deferred.values())
    expect(controllerDocument).toContain(src);
  const referenceTransfer = measureReferenceTransfer([
    {
      semanticEndpoint: "inline:boot-controller",
      delivery: "inline",
      body: exactControllerInline(controllerDocument),
    },
    {
      semanticEndpoint: `external:${selectedSrc}`,
      delivery: "external",
      body: selectedBody,
    },
  ]);
  return {
    artifactModel: "release-v1",
    release,
    controllerDocument,
    selectedBody,
    selectedTransferBytes: Buffer.byteLength(selectedBody, "utf8"),
    selectedSrc,
    runtimeBody,
    runtimeSrc,
    deferred,
    referenceTransfer,
  };
}

function loadLegacyMainFixtureResources(
  repositoryRoot: string,
): FixtureResources {
  const dist = resolve(repositoryRoot, "crates/trusted-server-js/dist");
  const selectedBody = ["tsjs-core.js", "tsjs-creative.js", "tsjs-gpt.js"]
    .map((file) => readFileSync(resolve(dist, file), "utf8"))
    .join(";\n");
  const selectedHash = createHash("sha256").update(selectedBody).digest("hex");
  const selectedSrc = `/static/tsjs=tsjs-unified.min.js?v=${selectedHash}`;
  const referenceTransfer = measureReferenceTransfer([
    {
      semanticEndpoint: "inline:legacy-boot",
      delivery: "inline",
      body: legacyBootInline(),
    },
    {
      semanticEndpoint: "inline:legacy-ad-init",
      delivery: "inline",
      body: LEGACY_AD_INIT_INLINE,
    },
    {
      semanticEndpoint: `external:${selectedSrc}`,
      delivery: "external",
      body: selectedBody,
    },
  ]);
  return {
    artifactModel: "legacy-main-v1",
    release: null,
    controllerDocument: null,
    selectedBody,
    selectedTransferBytes: Buffer.byteLength(selectedBody, "utf8"),
    selectedSrc,
    runtimeBody: null,
    runtimeSrc: null,
    deferred: new Map(),
    referenceTransfer,
  };
}

function loadMainFixtureResources(repositoryRoot: string): FixtureResources {
  const releaseFile = resolve(
    repositoryRoot,
    "crates/trusted-server-js/dist/tsjs-release-v1.json",
  );
  return existsSync(releaseFile)
    ? loadReleaseFixtureResources(repositoryRoot)
    : loadLegacyMainFixtureResources(repositoryRoot);
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
  const selectedTag = `<script src="${resources.selectedSrc}" id="trustedserver-js"></script>`;
  const comparisonSetup = `<script>window.__fixtureGpt.setManual(true);performance.mark("${COMPARISON_START_MARK}");</script>`;
  if (resources.artifactModel === "legacy-main-v1") {
    const release = manualDisplay ? "" : "window.__fixtureGpt.release();";
    return `<!doctype html><html><head><meta charset="utf-8"><script>${legacyBootInline()}</script>${comparisonSetup}${selectedTag}</head><body><div id="perf-slot"></div><script>${LEGACY_AD_INIT_INLINE}${release}</script></body></html>`;
  }
  if (!resources.controllerDocument)
    throw new Error("generated controller document is unavailable");
  const controlledSelectedTag = `${comparisonSetup}${selectedTag}`;
  const withControlledGpt = resources.controllerDocument.replace(
    selectedTag,
    controlledSelectedTag,
  );
  if (withControlledGpt === resources.controllerDocument)
    throw new Error("generated controller selected tag is unavailable");
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
    const markFirstObservableAction = () => {
      if (
        performance.getEntriesByName("tsjs:first-observable-action").length ===
        0
      ) {
        performance.mark("tsjs:first-observable-action");
      }
    };
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
        markFirstObservableAction();
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
        markFirstObservableAction();
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

async function installAttemptResourceProbe(
  context: BrowserContext,
): Promise<void> {
  await context.addInitScript(() => {
    const activeMessageListeners =
      new Set<EventListenerOrEventListenerObject>();
    const activeTimeouts = new Set<number>();
    const activePorts = new Set<MessagePort>();
    let createdMessageListeners = 0;
    let createdTimeouts = 0;
    let createdPorts = 0;

    const nativeAddEventListener = window.addEventListener.bind(window);
    const nativeRemoveEventListener = window.removeEventListener.bind(window);
    window.addEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions,
    ) => {
      if (type === "message" && !activeMessageListeners.has(listener)) {
        activeMessageListeners.add(listener);
        createdMessageListeners += 1;
      }
      nativeAddEventListener(type, listener, options);
    }) as typeof window.addEventListener;
    window.removeEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | EventListenerOptions,
    ) => {
      if (type === "message") activeMessageListeners.delete(listener);
      nativeRemoveEventListener(type, listener, options);
    }) as typeof window.removeEventListener;

    const nativeSetTimeout = window.setTimeout.bind(window);
    const nativeClearTimeout = window.clearTimeout.bind(window);
    window.setTimeout = ((
      handler: TimerHandler,
      timeout?: number,
      ...arguments_: unknown[]
    ) => {
      let handle = 0;
      const run = () => {
        activeTimeouts.delete(handle);
        if (typeof handler === "function") {
          Reflect.apply(handler, window, arguments_);
        } else {
          window.eval(handler);
        }
      };
      handle = nativeSetTimeout(run, timeout);
      activeTimeouts.add(handle);
      createdTimeouts += 1;
      return handle;
    }) as typeof window.setTimeout;
    window.clearTimeout = ((handle?: number) => {
      if (typeof handle === "number") activeTimeouts.delete(handle);
      nativeClearTimeout(handle);
    }) as typeof window.clearTimeout;

    const NativeMessageChannel = window.MessageChannel;
    const TrackedMessageChannel = function (): MessageChannel {
      const channel = new NativeMessageChannel();
      for (const port of [channel.port1, channel.port2]) {
        activePorts.add(port);
        createdPorts += 1;
        const nativeClose = port.close.bind(port);
        let closed = false;
        Object.defineProperty(port, "close", {
          configurable: true,
          value: () => {
            if (!closed) {
              closed = true;
              activePorts.delete(port);
            }
            nativeClose();
          },
        });
      }
      return channel;
    } as unknown as typeof MessageChannel;
    Object.setPrototypeOf(TrackedMessageChannel, NativeMessageChannel);
    TrackedMessageChannel.prototype = NativeMessageChannel.prototype;
    window.MessageChannel = TrackedMessageChannel;

    Object.defineProperty(window, "__tsjsAttemptResources", {
      configurable: false,
      value: () => ({
        activeMessageListeners: activeMessageListeners.size,
        activePorts: activePorts.size,
        activeTimeouts: activeTimeouts.size,
        createdMessageListeners,
        createdPorts,
        createdTimeouts,
      }),
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
  options: FixtureServerOptions,
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
  if (`${url.pathname}${url.search}` === resources.selectedSrc) {
    sendResponse(
      response,
      200,
      "application/javascript; charset=utf-8",
      resources.selectedBody,
      true,
    );
    return;
  }
  if (
    resources.runtimeBody !== null &&
    resources.runtimeSrc !== null &&
    `${url.pathname}${url.search}` === resources.runtimeSrc
  ) {
    if (options.rejectRuntime) {
      sendResponse(
        response,
        503,
        "text/plain; charset=utf-8",
        "persistent runtime rejected by test fixture",
      );
      return;
    }
    sendResponse(
      response,
      200,
      "application/javascript; charset=utf-8",
      resources.runtimeBody,
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
  options: FixtureServerOptions = {},
): Promise<FixtureServer> {
  const server = createServer((request, response) => {
    void serveFixtureRequest(request, response, resources, options).catch(
      (error) => {
        const failure =
          error instanceof Error ? error : new Error(String(error));
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
      },
    );
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
  resources: FixtureResources,
  manualDisplay = false,
  probeAttemptResources = false,
): Promise<FixtureRun> {
  const context = await browser.newContext();
  if (probeAttemptResources) await installAttemptResourceProbe(context);
  await installFixtureGpt(context);
  const page = await context.newPage();
  const networkSession = await context.newCDPSession(page);
  await networkSession.send("Network.enable");
  await networkSession.send("Network.emulateNetworkConditions", {
    offline: false,
    latency: 150,
    downloadThroughput: 200_000,
    uploadThroughput: 93_750,
    packetLoss: 0,
  });
  const selectedRequests: string[] = [];
  const runtimeRequests: string[] = [];
  const deferredRequests: string[] = [];
  const pageErrors: string[] = [];
  const consoleMessages: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (url.endsWith(resources.selectedSrc)) selectedRequests.push(url);
    if (resources.runtimeSrc && url.endsWith(resources.runtimeSrc))
      runtimeRequests.push(url);
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
    selectedRequests,
    runtimeRequests,
    deferredRequests,
    pageErrors,
    consoleMessages,
    close: () => context.close(),
  };
}

async function observeComparisonFixture(
  run: FixtureRun,
  resources: FixtureResources,
): Promise<ComparisonObservation> {
  await expect
    .poll(() =>
      run.page.evaluate(
        (mark) => performance.getEntriesByName(mark).length,
        FIRST_OBSERVABLE_ACTION_MARK,
      ),
    )
    .toBe(1);
  const observation = await run.page.evaluate(
    ({ startMark, actionMark }) => {
      const start = performance.getEntriesByName(startMark);
      const action = performance.getEntriesByName(actionMark);
      return {
        timingMs:
          (action[0]?.startTime ?? Number.NaN) -
          (start[0]?.startTime ?? Number.NaN),
        displayCount: window.__fixtureGpt.displayCount(),
        releaseId: window.tsjs?.releaseId,
      };
    },
    {
      startMark: COMPARISON_START_MARK,
      actionMark: FIRST_OBSERVABLE_ACTION_MARK,
    },
  );
  expect(run.selectedRequests).toHaveLength(1);
  const selectedRequest = new URL(run.selectedRequests[0]!);
  expect(`${selectedRequest.pathname}${selectedRequest.search}`).toBe(
    resources.selectedSrc,
  );
  expect(run.pageErrors).toEqual([]);
  expect(observation.displayCount).toBe(1);
  expect(Number.isFinite(observation.timingMs)).toBe(true);
  expect(observation.timingMs).toBeGreaterThan(0);
  return observation;
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
      `${name} was not recorded: ${JSON.stringify({ ...diagnostics, consoleMessages: run.consoleMessages, selectedRequests: run.selectedRequests, runtimeRequests: run.runtimeRequests, deferredRequests: run.deferredRequests, pageErrors: run.pageErrors })}; ${String(error)}`,
    );
  }
}

async function waitForKernel(run: FixtureRun): Promise<void> {
  try {
    await expect
      .poll(() => run.page.evaluate(() => window.tsjs?._internal?.state))
      .toBe("kernel");
  } catch (error) {
    const diagnostics = await run.page.evaluate(() => ({
      runtimeState: window.tsjs?._internal?.state,
      runtimeReason: (window.tsjs?._internal as { reason?: string } | undefined)
        ?.reason,
      names: window.tsjs ? Object.getOwnPropertyNames(window.tsjs) : [],
      marks: performance
        .getEntriesByType("mark")
        .map((entry) => ({ name: entry.name, startTime: entry.startTime })),
    }));
    throw new Error(
      `persistent runtime did not commit: ${JSON.stringify({ ...diagnostics, consoleMessages: run.consoleMessages, selectedRequests: run.selectedRequests, runtimeRequests: run.runtimeRequests, deferredRequests: run.deferredRequests, pageErrors: run.pageErrors })}; ${String(error)}`,
    );
  }
}

async function observeFixture(
  run: FixtureRun,
  resources: FixtureResources,
): Promise<BrowserObservation> {
  const comparison = await observeComparisonFixture(run, resources);
  await waitForMark(run, "tsjs:first-display");
  await waitForMark(run, "tsjs:first-display-paint");
  await waitForKernel(run);
  for (const id of DEFERRED_IDS) {
    try {
      await expect
        .poll(() =>
          run.page.evaluate(
            (moduleId) => window.__fixtureGpt.executionTime(moduleId),
            id,
          ),
        )
        .not.toBeUndefined();
    } catch (error) {
      const lifecycle = await run.page.evaluate(
        (deferredIds) =>
          deferredIds.map((moduleId) => ({
            id: moduleId,
            loadTime: window.__fixtureGpt.loadTime(moduleId),
            preparationTime: window.__fixtureGpt.preparationTime(moduleId),
            executionTime: window.__fixtureGpt.executionTime(moduleId),
          })),
        DEFERRED_IDS,
      );
      throw new Error(
        `deferred ${id} did not execute: ${JSON.stringify({ lifecycle, deferredRequests: run.deferredRequests, consoleMessages: run.consoleMessages, pageErrors: run.pageErrors })}; ${String(error)}`,
      );
    }
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
      timingMs: Number.NaN,
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
      selectedScriptCount: document.querySelectorAll("script#trustedserver-js")
        .length,
      deferred,
      paintTime,
      preloadBeforePaintCount: window.__fixtureGpt
        .preloadTimes()
        .filter((time) => time < paintTime).length,
    };
  }, DEFERRED_IDS);
  observation.timingMs = comparison.timingMs;
  expect(observation.bidsScriptCount).toBe(1);
  expect(observation.firstDisplayCount).toBe(1);
  expect(observation.firstDisplayPaintCount).toBe(1);
  expect(observation.measureCount).toBe(1);
  expect(observation.runtimeState).toBe("kernel");
  expect(observation.displayCount).toBe(1);
  expect(observation.diagnosticsPresentationCount).toBe(1);
  expect(observation.selectedScriptCount).toBe(1);
  expect(run.selectedRequests).toHaveLength(1);
  expect(run.deferredRequests).toHaveLength(DEFERRED_IDS.length);
  expect(run.pageErrors).toEqual([]);
  expect(Number.isFinite(observation.timingMs)).toBe(true);
  expect(observation.timingMs).toBeGreaterThanOrEqual(0);
  expect(Number.isFinite(observation.markTimingMs)).toBe(true);
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
  const deferredByResponseEnd = [...observation.deferred].sort(
    (left, right) => left.responseEnd - right.responseEnd,
  );
  expect(deferredByResponseEnd[0]!.executionTime).toBeLessThan(
    deferredByResponseEnd.at(-1)!.responseEnd,
  );
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

async function collectHeapCheckpoints(
  browser: Browser,
  fixtureServer: FixtureServer,
  resources: FixtureResources,
): Promise<Record<HeapCheckpoint, number>> {
  const heapRun = await openFixture(browser, fixtureServer, resources, true);
  const retainedHeapBytes = {} as Record<HeapCheckpoint, number>;
  try {
    await expect
      .poll(() => heapRun.page.evaluate(() => typeof window.tsjs))
      .toBe("object");
    retainedHeapBytes.afterBoot = await collectHeap(heapRun.page);
    await heapRun.page.evaluate(() => window.__fixtureGpt.release());
    await observeComparisonFixture(heapRun, resources);
    if (resources.artifactModel === "release-v1") {
      await waitForMark(heapRun, "tsjs:first-display-paint");
    }
    retainedHeapBytes.afterFirstRender = await collectHeap(heapRun.page);
    await heapRun.page.evaluate(() => window.__fixtureGpt.publisherRefresh());
    retainedHeapBytes.afterRefresh = await collectHeap(heapRun.page);
    for (const id of resources.deferred.keys()) {
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
    return retainedHeapBytes;
  } finally {
    await heapRun.close();
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
    __tsjsAttemptResources(): {
      activeMessageListeners: number;
      activePorts: number;
      activeTimeouts: number;
      createdMessageListeners: number;
      createdPorts: number;
      createdTimeouts: number;
    };
    tsjs?: {
      _internal?: { initialDisplayCommitted?: boolean; state?: string };
      diagnostics?: {
        renderTrace?: { current(): unknown; history(): unknown };
      };
      releaseId?: string;
      requestAds(options?: { slots?: readonly string[] }): Promise<unknown>;
    };
  }
}

test("measures each distinct semantic transfer source exactly once", () => {
  const inlineBody = 'performance.mark("tsjs:bids-script");';
  const externalBody = "window.tsjs={};";
  const inline = measureBytes(Buffer.from(inlineBody, "utf8"));
  const external = measureBytes(Buffer.from(externalBody, "utf8"));
  const transfer = measureReferenceTransfer([
    {
      semanticEndpoint: "inline:boot-controller",
      delivery: "inline",
      body: inlineBody,
    },
    {
      semanticEndpoint: `external:/static/tsjs=tsjs-unified.min.js?v=${external.sha256}`,
      delivery: "external",
      body: externalBody,
    },
  ]);

  expect(transfer.sources).toHaveLength(2);
  for (const metric of ["rawBytes", "gzipBytes", "brotliBytes"] as const) {
    expect(transfer[metric]).toBe(inline[metric] + external[metric]);
  }
  expect(() =>
    measureReferenceTransfer([
      {
        semanticEndpoint: "inline:boot-controller",
        delivery: "inline",
        body: inlineBody,
      },
      {
        semanticEndpoint: "inline:boot-controller",
        delivery: "inline",
        body: inlineBody,
      },
    ]),
  ).toThrow("semantic transfer source is empty or duplicated");
});

test("gates semantic reference transfer against the exact current main build", () => {
  const mode = process.env.TSJS_PERF_MODE;
  const mainRoot = process.env.TSJS_PERF_MAIN_ROOT;
  const mainSha = process.env.TSJS_PERF_MAIN_SHA;
  if (!mainRoot || !mainSha) {
    if (
      mode === "preswitch" ||
      mode === "postswitch" ||
      mode === "pull-request"
    ) {
      throw new Error(
        "fresh-main semantic transfer comparison is required in CI and release modes",
      );
    }
    test.skip(
      true,
      "fresh-main semantic transfer comparison unavailable; check:bundle still enforces candidate ceilings",
    );
    return;
  }
  expect(mainSha).toMatch(/^[0-9a-f]{40}$/u);
  expect(
    execFileSync("git", ["-C", mainRoot, "rev-parse", "HEAD"], {
      encoding: "utf8",
    }).trim(),
  ).toBe(mainSha);
  const mainResources = loadMainFixtureResources(mainRoot);
  const candidateResources = loadReleaseFixtureResources(REPO_ROOT);

  for (const metric of ["rawBytes", "gzipBytes", "brotliBytes"] as const) {
    expect(
      candidateResources.referenceTransfer[metric],
      `candidate ${metric} semantic transfer`,
    ).toBeLessThanOrEqual(mainResources.referenceTransfer[metric]);
  }
});

test("boots the generated first-display artifact through persistent takeover", async ({
  browser,
}) => {
  const resources = loadReleaseFixtureResources(REPO_ROOT);
  const server = await startFixtureServer(resources);
  try {
    const run = await openFixture(browser, server, resources);
    try {
      try {
        await observeFixture(run, resources);
      } catch (error) {
        const diagnostics = await run.page.evaluate(() => ({
          runtimeState: window.tsjs?._internal?.state,
          runtimeReason: (
            window.tsjs?._internal as { reason?: string } | undefined
          )?.reason,
          names: window.tsjs ? Object.getOwnPropertyNames(window.tsjs) : [],
          marks: performance
            .getEntriesByType("mark")
            .map((entry) => ({ name: entry.name, startTime: entry.startTime })),
          displayCount: window.__fixtureGpt.displayCount(),
          gptCalls: window.__fixtureGpt.calls(),
        }));
        throw new Error(
          `first-display smoke failed: ${JSON.stringify({ ...diagnostics, consoleMessages: run.consoleMessages, selectedRequests: run.selectedRequests, runtimeRequests: run.runtimeRequests, deferredRequests: run.deferredRequests, pageErrors: run.pageErrors })}; ${String(error)}`,
        );
      }
    } finally {
      await run.close();
    }
  } finally {
    await server.close();
  }
});

test("generated first-display fallback releases its runtime and provisional resources", async ({
  browser,
}) => {
  const resources = loadReleaseFixtureResources(REPO_ROOT);
  const server = await startFixtureServer(resources, { rejectRuntime: true });
  try {
    const run = await openFixture(browser, server, resources, false, true);
    try {
      await expect
        .poll(() => run.page.evaluate(() => window.tsjs?._internal?.state))
        .toBe("fallback");
      const observation = await run.page.evaluate(() => ({
        initialDisplayCommitted:
          window.tsjs?._internal?.initialDisplayCommitted,
        privateFields: [
          "_firstDisplayTakeover",
          "_registerFirstDisplay",
        ].filter((name) =>
          Object.prototype.hasOwnProperty.call(window.tsjs ?? {}, name),
        ),
        resources: window.__tsjsAttemptResources(),
        runtimeScripts: document.querySelectorAll(
          "script#trustedserver-js-runtime",
        ).length,
        selectedScripts: document.querySelectorAll("script#trustedserver-js")
          .length,
      }));

      expect(run.runtimeRequests).toHaveLength(1);
      expect(observation.initialDisplayCommitted).toBe(true);
      expect(observation.privateFields).toEqual([]);
      expect(observation.runtimeScripts).toBe(0);
      expect(observation.selectedScripts).toBe(1);
      expect(observation.resources).toMatchObject({
        activeMessageListeners: 0,
        activePorts: 0,
        activeTimeouts: 0,
      });
      expect(observation.resources.createdTimeouts).toBeGreaterThan(0);
    } finally {
      await run.close();
    }
  } finally {
    await server.close();
  }
});

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
    // The paired comparison performs 110 cold navigations through the common
    // first-action endpoint, then one full candidate lifecycle observation and
    // four paired heap checkpoints. The 30-minute cap guards hosted-runner
    // variance and evidence finalization; it is not permission to await the
    // post-action lifecycle during every timing sample.
    test.setTimeout(1_800_000);
    const mode = process.env.TSJS_PERF_MODE;
    test.skip(
      mode !== "preswitch" && mode !== "postswitch" && mode !== "pull-request",
      "performance evidence run only",
    );
    expect(browserName).toBe("chromium");
    expect(browser.version()).toBe(EXPECTED_CHROMIUM);
    expect(process.env.TSJS_PERF_MACHINE_CLASS).toBe(MACHINE_CLASS);
    expect(process.env.TSJS_PERF_RUNNER_IMAGE).toBe(RUNNER_IMAGE);
    const mainRoot = process.env.TSJS_PERF_MAIN_ROOT;
    const mainSha = process.env.TSJS_PERF_MAIN_SHA;
    expect(mainRoot, "TSJS_PERF_MAIN_ROOT is required").toBeTruthy();
    expect(mainSha, "TSJS_PERF_MAIN_SHA is required").toMatch(
      /^[0-9a-f]{40}$/u,
    );
    expect(
      execFileSync("git", ["-C", mainRoot!, "rev-parse", "HEAD"], {
        encoding: "utf8",
      }).trim(),
    ).toBe(mainSha);
    const mainResources = loadMainFixtureResources(mainRoot!);
    const candidateResources = loadReleaseFixtureResources(REPO_ROOT);
    const mainServer = await startFixtureServer(mainResources);
    const candidateServer = await startFixtureServer(candidateResources);
    activeFixtureServers.push(mainServer, candidateServer);

    const observeTimingVariant = async (
      fixtureServer: FixtureServer,
      resources: FixtureResources,
    ): Promise<ComparisonObservation> => {
      const run = await openFixture(browser, fixtureServer, resources);
      try {
        const comparison = await observeComparisonFixture(run, resources);
        return comparison;
      } finally {
        await run.close();
      }
    };

    for (let index = 0; index < WARMUPS; index += 1) {
      const variants =
        index % 2 === 0
          ? ([
              [mainServer, mainResources],
              [candidateServer, candidateResources],
            ] as const)
          : ([
              [candidateServer, candidateResources],
              [mainServer, mainResources],
            ] as const);
      for (const [server, resources] of variants) {
        await observeTimingVariant(server, resources);
      }
    }

    const mainSamples: number[] = [];
    const candidateSamples: number[] = [];
    for (let index = 0; index < SAMPLES; index += 1) {
      const variants =
        index % 2 === 0
          ? ([
              ["main", mainServer, mainResources],
              ["candidate", candidateServer, candidateResources],
            ] as const)
          : ([
              ["candidate", candidateServer, candidateResources],
              ["main", mainServer, mainResources],
            ] as const);
      for (const [variant, server, resources] of variants) {
        const observation = await observeTimingVariant(server, resources);
        if (variant === "main") {
          mainSamples.push(observation.timingMs);
        } else {
          candidateSamples.push(observation.timingMs);
        }
      }
    }
    expect(mainSamples).toHaveLength(SAMPLES);
    expect(candidateSamples).toHaveLength(SAMPLES);
    const mainP90 = p90(mainSamples);
    const candidateP90 = p90(candidateSamples);
    expect(mainP90).toBeGreaterThan(0);
    expect
      .soft(candidateP90, "candidate p90")
      .toBeLessThanOrEqual(mainP90 * MAXIMUM_P90_RATIO);
    for (const metric of ["rawBytes", "gzipBytes", "brotliBytes"] as const) {
      expect
        .soft(
          candidateResources.referenceTransfer[metric],
          `candidate ${metric} semantic transfer`,
        )
        .toBeLessThanOrEqual(mainResources.referenceTransfer[metric]);
    }

    const representativeRun = await openFixture(
      browser,
      candidateServer,
      candidateResources,
    );
    const representative = await observeFixture(
      representativeRun,
      candidateResources,
    ).finally(() => representativeRun.close());
    expect(representative.releaseId).toBe(
      candidateResources.release?.releaseId,
    );

    const mainHeapBytes = await collectHeapCheckpoints(
      browser,
      mainServer,
      mainResources,
    );
    const candidateHeapBytes = await collectHeapCheckpoints(
      browser,
      candidateServer,
      candidateResources,
    );
    for (const name of HEAP_CHECKPOINTS) {
      expect
        .soft(mainHeapBytes[name], `${name} main retained heap`)
        .toBeLessThanOrEqual(HARD_HEAP_CEILING_BYTES);
      expect
        .soft(candidateHeapBytes[name], `${name} candidate retained heap`)
        .toBeLessThanOrEqual(HARD_HEAP_CEILING_BYTES);
      expect
        .soft(candidateHeapBytes[name], `${name} paired retained heap`)
        .toBeLessThanOrEqual(mainHeapBytes[name] * MAXIMUM_HEAP_RATIO);
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
    const deferredBeforePaint = representative.deferred.filter(
      ({ startTime }) => startTime < representative.paintTime,
    ).length;
    const deferredPreparationBeforePaint = representative.deferred.filter(
      ({ preparationTime }) => preparationTime < representative.paintTime,
    ).length;
    const deferredExecutionBeforePaint = representative.deferred.filter(
      ({ executionTime }) => executionTime < representative.paintTime,
    ).length;
    const deferredStarts = representative.deferred.map(
      ({ startTime }) => startTime,
    );
    const deferredByResponseEnd = [...representative.deferred].sort(
      (left, right) => left.responseEnd - right.responseEnd,
    );
    const npmVersion = execFileSync("npm", ["--version"], {
      encoding: "utf8",
    }).trim();
    const evidence = {
      schemaVersion: 5,
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
        interleaving: "alternating-main-candidate",
      },
      networkProfile: {
        mechanism: "cdp-Network.emulateNetworkConditions",
        appliedBeforeNavigation: true,
        latencyMs: NETWORK_PROFILE.latency,
        downloadThroughputBytesPerSecond: NETWORK_PROFILE.downloadThroughput,
        uploadThroughputBytesPerSecond: NETWORK_PROFILE.uploadThroughput,
        packetLossPercent: NETWORK_PROFILE.packetLoss,
      },
      marks: {
        source: "fixture-first-observable-action",
        comparisonStart: true,
        firstObservableAction: true,
        candidateBidsScript: representative.bidsScriptCount === 1,
        candidateFirstDisplay: representative.firstDisplayCount === 1,
        candidateFirstDisplayPaint: representative.firstDisplayPaintCount === 1,
      },
      performance: {
        requestToFirstActionMs: {
          main: {
            sha: mainSha,
            artifactModel: mainResources.artifactModel,
            selectedTransferBytes: mainResources.selectedTransferBytes,
            samples: mainSamples,
            p90: mainP90,
          },
          candidate: {
            artifactModel: candidateResources.artifactModel,
            selectedTransferBytes: candidateResources.selectedTransferBytes,
            samples: candidateSamples,
            p90: candidateP90,
          },
          percentile: PERCENTILE,
          maximumRatio: MAXIMUM_P90_RATIO,
          observedRatio: candidateP90 / mainP90,
        },
      },
      transfer: {
        algorithm: "semantic-tsjs-transfer-v1",
        mainReferenceTransfer: {
          sha: mainSha,
          artifactModel: mainResources.artifactModel,
          ...mainResources.referenceTransfer,
        },
        candidateReferenceTransfer: {
          sha: headSha,
          artifactModel: candidateResources.artifactModel,
          ...candidateResources.referenceTransfer,
        },
      },
      heap: {
        collection: "one-collectGarbage-then-immediate-getHeapUsage",
        maximumRatio: MAXIMUM_HEAP_RATIO,
        hardCeilingBytes: HARD_HEAP_CEILING_BYTES,
        main: {
          sha: mainSha,
          checkpoints: mainHeapBytes,
        },
        candidate: { checkpoints: candidateHeapBytes },
      },
      requests: {
        selected: { count: 1 },
        deferred: {
          count: representative.deferred.length,
          requestBeforePaintCount: deferredBeforePaint,
          preloadBeforePaintCount: representative.preloadBeforePaintCount,
          preparationBeforePaintCount: deferredPreparationBeforePaint,
          executionBeforePaintCount: deferredExecutionBeforePaint,
          independentlyTriggered:
            Math.max(...deferredStarts) - Math.min(...deferredStarts) <= 50,
          headOfLineBlocking:
            deferredByResponseEnd[0]!.executionTime >=
            deferredByResponseEnd.at(-1)!.responseEnd,
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

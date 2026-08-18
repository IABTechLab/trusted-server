import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  expect,
  type ConsoleMessage,
  type ElementHandle,
  type Frame,
  type Page,
  type Request,
  type TestInfo,
} from "@playwright/test";

type BrowserName = "chromium" | "firefox" | "webkit";

const RELEASE_ID_PATTERN = /^[0-9a-f]{64}$/u;
const APS_RENDERER_PATH = "/integrations/aps/renderer/v1";
const APS_RUNNER_PATH = "/integrations/aps/runner.js";
const EVIDENCE_ROOT = resolve(__dirname, "../real-gam-evidence");
const TERMINAL_STATE = "terminal";
const WIDTH = 300;
const HEIGHT = 250;

export const REAL_GAM_PUC_RELEASE = "1.17.2";

const PROTOCOL_MESSAGES = [
  "Prebid Request",
  "Prebid Response",
  "TS Render Owner Register",
  "TS Render Owner Registered",
  "TS Render Owner Refused",
  "TS APS Start",
  "TS ADM Start",
  "TS Owner Inserted",
  "TS Owner Settled",
  "TS ADM Loaded",
  "TS ADM Failed",
  "TS APS Document Accepted",
  "TS APS Runner Loaded",
  "TS APS Render Completed",
  "TS APS Render Failed",
] as const;

const GPT_EVENT_NAMES = [
  "slotRequested",
  "slotResponseReceived",
  "slotRenderEnded",
  "slotOnload",
  "impressionViewable",
  "slotVisibilityChanged",
] as const;

type RealGamOutcome = "accepted" | "failed";
type ExpectedReason =
  | "bridge_id_mismatch"
  | "descriptor_invalid"
  | "bridge_claim_timeout"
  | "owner_registration_timeout"
  | "renderer_document_no_load"
  | "runner_failed";

interface LifecycleCounts {
  readonly creativeRequests: number;
  readonly bridgeClaims: number;
  readonly rendererFrames: number;
  readonly runnerLoads: number;
  readonly renderCompletions: number;
  readonly acceptedResults: number;
  readonly duplicateRenders: number;
  readonly ownedFrames: number;
  readonly childRenders: number;
  readonly oldAttemptMutations: number;
}

interface OrderingExpectation {
  readonly parentTerminalBeforeChild: boolean;
}

export interface RealGamCaseContract {
  readonly caseId: string;
  readonly flow: string;
  readonly outcome: RealGamOutcome;
  readonly reason: ExpectedReason | null;
  readonly deadlineMs: number;
  readonly dimensions: Readonly<{ width: number; height: number }> | null;
  readonly slotCount: number;
  readonly gptCycles: number;
  readonly pucOwners: number;
  readonly terminalRendererFrames: number;
  readonly terminalOwnedFrames: number;
  readonly counts: LifecycleCounts;
  readonly ordering: OrderingExpectation | null;
}

export interface RealGamEnvironment {
  readonly pageUrl: string;
  readonly authorization: string;
  readonly expectedReleaseId: string;
}

interface CaseSnapshot {
  readonly version: 1;
  readonly caseId: string;
  readonly releaseId: string;
  readonly flow: string;
  readonly outcome: RealGamOutcome;
  readonly reason: ExpectedReason | null;
  readonly durationMs: number;
  readonly dimensions: Readonly<{ width: number; height: number }> | null;
  readonly counts: LifecycleCounts;
  readonly ordering: Readonly<{
    parentTerminalSequence: number;
    childRenderSequence: number;
  }> | null;
}

interface NetworkEvidence {
  readonly sequence: number;
  readonly atMs: number;
  readonly kind:
    | "test-page"
    | "first-party"
    | "gpt"
    | "renderer"
    | "runner-proxy"
    | "creative"
    | "third-party";
  readonly method: string;
  readonly resourceType: string;
  status?: number;
  failure?: boolean;
}

interface ConsoleEvidence {
  readonly sequence: number;
  readonly atMs: number;
  readonly type: string;
  readonly textBytes: number;
  readonly textSha256: string;
  readonly argumentCount: number;
  readonly sourceKind: NetworkEvidence["kind"];
}

interface PageErrorEvidence {
  readonly sequence: number;
  readonly atMs: number;
  readonly textBytes: number;
  readonly textSha256: string;
}

interface ProtocolEvidence {
  readonly message: (typeof PROTOCOL_MESSAGES)[number];
  readonly atMs: number;
}

interface GptEvidence {
  readonly event: (typeof GPT_EVENT_NAMES)[number];
  readonly atMs: number;
}

interface ObserverSnapshot {
  readonly protocol: readonly ProtocolEvidence[];
  readonly gpt: readonly GptEvidence[];
}

interface ElementGeometry {
  readonly role: "slot" | "puc-owner" | "owned-frame" | "renderer" | "creative";
  readonly width: number;
  readonly height: number;
  readonly clientWidth: number;
  readonly clientHeight: number;
  readonly scrollWidth: number;
  readonly scrollHeight: number;
  readonly marginTop: string;
  readonly marginRight: string;
  readonly marginBottom: string;
  readonly marginLeft: string;
  readonly overflowX: string;
  readonly overflowY: string;
}

interface DocumentGeometry {
  readonly role:
    "puc-owner-document" | "renderer-document" | "creative-document";
  readonly width: number;
  readonly height: number;
  readonly clientWidth: number;
  readonly clientHeight: number;
  readonly scrollWidth: number;
  readonly scrollHeight: number;
  readonly bodyMarginTop: string;
  readonly bodyMarginRight: string;
  readonly bodyMarginBottom: string;
  readonly bodyMarginLeft: string;
  readonly overflowX: string;
  readonly overflowY: string;
}

interface DomEvidence {
  readonly terminalRoots: number;
  readonly elements: readonly ElementGeometry[];
  readonly documents: readonly DocumentGeometry[];
}

interface RenderTraceEvidence {
  readonly path: "auction" | "ssat" | "gam-refresh" | null;
  readonly rendered: boolean;
  readonly servedFrom:
    "inline" | "gam" | "debug-adm" | "pbs-cache" | "prebid" | null;
  readonly gamEmpty: boolean | null;
  readonly injected: boolean | null;
  readonly visible: boolean | null;
  readonly count: number;
  readonly seq: number;
}

const ZERO_COUNTS: LifecycleCounts = Object.freeze({
  creativeRequests: 0,
  bridgeClaims: 0,
  rendererFrames: 0,
  runnerLoads: 0,
  renderCompletions: 0,
  acceptedResults: 0,
  duplicateRenders: 0,
  ownedFrames: 0,
  childRenders: 0,
  oldAttemptMutations: 0,
});

function counts(overrides: Partial<LifecycleCounts>): LifecycleCounts {
  return Object.freeze({ ...ZERO_COUNTS, ...overrides });
}

function acceptedCase(
  caseId: string,
  options: {
    flow?: string;
    slots?: number;
    cycles?: number;
    pucOwners?: number;
    ownedFrames?: number;
    childRenders?: number;
    bridgeClaims?: number;
    ordering?: OrderingExpectation;
    direct?: boolean;
  } = {},
): RealGamCaseContract {
  const slots = options.slots ?? 1;
  const cycles = options.cycles ?? slots;
  const direct = options.direct ?? false;
  const rendererCount = options.ownedFrames ? 0 : cycles;
  return Object.freeze({
    caseId,
    flow: options.flow ?? caseId,
    outcome: "accepted" as const,
    reason: null,
    deadlineMs: 45_000,
    dimensions: Object.freeze({ width: WIDTH, height: HEIGHT }),
    slotCount: slots,
    gptCycles: direct ? 0 : cycles,
    pucOwners: options.pucOwners ?? (direct ? 0 : slots),
    terminalRendererFrames: rendererCount === 0 ? 0 : slots,
    terminalOwnedFrames: options.ownedFrames ?? 0,
    counts: counts({
      creativeRequests: rendererCount,
      bridgeClaims: options.bridgeClaims ?? (direct ? 0 : cycles),
      rendererFrames: rendererCount,
      runnerLoads: rendererCount,
      renderCompletions: rendererCount,
      acceptedResults: cycles,
      ownedFrames: options.ownedFrames ?? 0,
      childRenders: options.childRenders ?? 0,
    }),
    ordering: options.ordering ?? null,
  });
}

function failedCase(
  caseId: string,
  reason: ExpectedReason,
  overrides: Partial<LifecycleCounts>,
): RealGamCaseContract {
  return Object.freeze({
    caseId,
    flow: caseId,
    outcome: "failed" as const,
    reason,
    deadlineMs: 15_000,
    dimensions: null,
    slotCount: 1,
    gptCycles: 1,
    pucOwners: 0,
    terminalRendererFrames: 0,
    terminalOwnedFrames: 0,
    counts: counts(overrides),
    ordering: null,
  });
}

/**
 * The protected page implements these fixed, non-secret case identifiers. Real GAM,
 * APS, Prebid, and PUC configuration stays in that environment, outside this source.
 */
export const REAL_GAM_CASES: readonly RealGamCaseContract[] = Object.freeze([
  acceptedCase("ssat-aps-puc"),
  acceptedCase("trusted-server-prebid-aps-puc"),
  acceptedCase("page-bids-aps-puc"),
  acceptedCase("direct-aps", { direct: true }),
  acceptedCase("direct-adm", { direct: true, ownedFrames: 1 }),
  acceptedCase("direct-cache", { direct: true, ownedFrames: 1 }),
  acceptedCase("attributable-empty-gam-fallback", {
    pucOwners: 0,
    childRenders: 1,
    bridgeClaims: 0,
    ordering: Object.freeze({ parentTerminalBeforeChild: true }),
  }),
  acceptedCase("sra-aps-puc", { slots: 2, cycles: 2, pucOwners: 2 }),
  acceptedCase("refresh-aps-puc", { cycles: 2 }),
  acceptedCase("spa-navigation", { cycles: 2 }),
  acceptedCase("gpt-handoff"),
  acceptedCase("hydrated-dom-replacement"),
  acceptedCase("collapsed-shell-resize"),
  failedCase("wrong-id", "bridge_id_mismatch", { bridgeClaims: 1 }),
  failedCase("wrong-source", "bridge_id_mismatch", { bridgeClaims: 1 }),
  failedCase("invalid-descriptor", "descriptor_invalid", { bridgeClaims: 1 }),
  failedCase("no-outer-claim", "bridge_claim_timeout", {}),
  failedCase("no-owner-registration", "owner_registration_timeout", {
    bridgeClaims: 1,
  }),
  failedCase("no-document-ack", "renderer_document_no_load", {
    bridgeClaims: 1,
    rendererFrames: 1,
  }),
  failedCase("aps-runner-failure", "runner_failed", {
    bridgeClaims: 1,
    rendererFrames: 1,
    runnerLoads: 1,
  }),
]);

function requiredEnvironment(name: string): string {
  const value = process.env[name];
  if (!value)
    throw new Error(`${name} is required for the protected real-GAM suite`);
  if (/\r|\n/u.test(value))
    throw new Error(`${name} must not contain line breaks`);
  return value;
}

export function getRealGamEnvironment(): RealGamEnvironment {
  const pageUrl = requiredEnvironment("TS_REAL_GAM_PAGE_URL");
  const authorization = requiredEnvironment("TS_REAL_GAM_AUTH_HEADER");
  const expectedReleaseId = requiredEnvironment(
    "TS_REAL_GAM_EXPECTED_RELEASE_ID",
  );
  let parsed: URL;
  try {
    parsed = new URL(pageUrl);
  } catch {
    throw new Error("TS_REAL_GAM_PAGE_URL must be an absolute HTTPS URL");
  }
  if (
    parsed.protocol !== "https:" ||
    parsed.username !== "" ||
    parsed.password !== "" ||
    parsed.hash !== ""
  ) {
    throw new Error(
      "TS_REAL_GAM_PAGE_URL must be HTTPS without credentials or a fragment",
    );
  }
  if (!RELEASE_ID_PATTERN.test(expectedReleaseId)) {
    throw new Error(
      "TS_REAL_GAM_EXPECTED_RELEASE_ID must be a lowercase 64-character release id",
    );
  }
  if (Buffer.byteLength(authorization, "utf8") < 16) {
    throw new Error(
      "TS_REAL_GAM_AUTH_HEADER must contain at least 16 UTF-8 bytes",
    );
  }
  return Object.freeze({ pageUrl, authorization, expectedReleaseId });
}

function sha256(value: string): string {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

function elapsed(startedAt: number): number {
  return Math.max(0, Date.now() - startedAt);
}

function safeMethod(method: string): string {
  return /^(?:GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS)$/u.test(method)
    ? method
    : "OTHER";
}

function safeResourceType(resourceType: string): string {
  return /^(?:document|stylesheet|image|media|font|script|texttrack|xhr|fetch|eventsource|websocket|manifest|other)$/u.test(
    resourceType,
  )
    ? resourceType
    : "other";
}

function parsedUrl(value: string): URL | undefined {
  try {
    return new URL(value);
  } catch {
    return undefined;
  }
}

function hasRendererAncestor(frame: Frame): boolean {
  let current: Frame | null = frame;
  while (current) {
    const url = parsedUrl(current.url());
    if (url?.pathname === APS_RENDERER_PATH) return true;
    current = current.parentFrame();
  }
  return false;
}

export function classifyRealGamNetworkUrl(
  rawUrl: string,
  pageOrigin: string,
  request?: Request,
): NetworkEvidence["kind"] {
  const url = parsedUrl(rawUrl);
  if (!url) return "third-party";
  if (url.origin === pageOrigin && url.pathname === APS_RENDERER_PATH) {
    return "renderer";
  }
  if (url.origin === pageOrigin && url.pathname === APS_RUNNER_PATH) {
    return "runner-proxy";
  }
  if (
    (url.hostname === "securepubads.g.doubleclick.net" &&
      url.pathname === "/tag/js/gpt.js") ||
    (url.hostname === "www.googletagservices.com" &&
      url.pathname === "/tag/js/gpt.js")
  ) {
    return "gpt";
  }
  if (request?.isNavigationRequest()) {
    try {
      if (hasRendererAncestor(request.frame())) return "creative";
    } catch {
      // A service-worker request has no associated frame.
    }
  }
  if (url.origin === pageOrigin) {
    try {
      if (request && request.frame() === request.frame().page().mainFrame()) {
        return "test-page";
      }
    } catch {
      // Detached frames are still safely classified as first party.
    }
    return "first-party";
  }
  return "third-party";
}

async function installBrowserObservers(page: Page): Promise<void> {
  await page.addInitScript(
    ({ protocolMessages, gptEventNames }) => {
      const observer = {
        protocol: [] as Array<{ message: string; atMs: number }>,
        gpt: [] as Array<{ event: string; atMs: number }>,
        gptArmed: false,
      };
      Object.defineProperty(window, "__tsRealGamObserver", {
        configurable: false,
        enumerable: false,
        writable: false,
        value: observer,
      });
      const allowedMessages = new Set(protocolMessages);
      window.addEventListener("message", (event) => {
        let message: unknown;
        try {
          message =
            typeof event.data === "object" && event.data !== null
              ? Reflect.get(event.data, "message")
              : undefined;
        } catch {
          return;
        }
        if (
          typeof message === "string" &&
          allowedMessages.has(message as (typeof protocolMessages)[number])
        ) {
          observer.protocol.push({ message, atMs: performance.now() });
        }
      });

      const armGpt = (): boolean => {
        if (observer.gptArmed) return true;
        let googletag: unknown;
        try {
          const descriptor = Object.getOwnPropertyDescriptor(
            window,
            "googletag",
          );
          googletag =
            descriptor && "value" in descriptor ? descriptor.value : undefined;
        } catch {
          return false;
        }
        if (typeof googletag !== "object" || googletag === null) return false;
        let commandQueue: unknown;
        try {
          commandQueue = Reflect.get(googletag, "cmd");
        } catch {
          return false;
        }
        if (!Array.isArray(commandQueue)) return false;
        commandQueue.push(() => {
          if (observer.gptArmed) return;
          let pubads: unknown;
          try {
            const method = Reflect.get(googletag as object, "pubads");
            pubads =
              typeof method === "function"
                ? Reflect.apply(method, googletag, [])
                : undefined;
          } catch {
            return;
          }
          if (typeof pubads !== "object" || pubads === null) return;
          let addEventListener: unknown;
          try {
            addEventListener = Reflect.get(pubads, "addEventListener");
          } catch {
            return;
          }
          if (typeof addEventListener !== "function") return;
          for (const eventName of gptEventNames) {
            try {
              Reflect.apply(addEventListener, pubads, [
                eventName,
                () => {
                  observer.gpt.push({
                    event: eventName,
                    atMs: performance.now(),
                  });
                },
              ]);
            } catch {
              // Unsupported informational GPT events do not weaken core event checks.
            }
          }
          observer.gptArmed = true;
        });
        return true;
      };

      let attempts = 0;
      const interval = window.setInterval(() => {
        attempts += 1;
        if (armGpt() || attempts >= 400) window.clearInterval(interval);
      }, 25);
      armGpt();
    },
    { protocolMessages: PROTOCOL_MESSAGES, gptEventNames: GPT_EVENT_NAMES },
  );
}

class EvidenceCollector {
  readonly #page: Page;
  readonly #pageOrigin: string;
  readonly #startedAt = Date.now();
  readonly #network: NetworkEvidence[] = [];
  readonly #requestIndexes = new Map<Request, number>();
  readonly #console: ConsoleEvidence[] = [];
  readonly #pageErrors: PageErrorEvidence[] = [];

  constructor(page: Page, pageUrl: string) {
    this.#page = page;
    this.#pageOrigin = new URL(pageUrl).origin;

    page.on("request", (request) => {
      const record: NetworkEvidence = {
        sequence: this.#network.length + 1,
        atMs: elapsed(this.#startedAt),
        kind: classifyRealGamNetworkUrl(
          request.url(),
          this.#pageOrigin,
          request,
        ),
        method: safeMethod(request.method()),
        resourceType: safeResourceType(request.resourceType()),
      };
      this.#requestIndexes.set(request, this.#network.length);
      this.#network.push(record);
    });
    page.on("response", (response) => {
      const index = this.#requestIndexes.get(response.request());
      if (index === undefined) return;
      const record = this.#network[index];
      if (record) record.status = response.status();
    });
    page.on("requestfailed", (request) => {
      const index = this.#requestIndexes.get(request);
      if (index === undefined) return;
      const record = this.#network[index];
      if (record) record.failure = true;
    });
    page.on("console", (message) => this.#recordConsole(message));
    page.on("pageerror", (error) => {
      const text = String(error.message);
      this.#pageErrors.push({
        sequence: this.#pageErrors.length + 1,
        atMs: elapsed(this.#startedAt),
        textBytes: Buffer.byteLength(text, "utf8"),
        textSha256: sha256(text),
      });
    });
  }

  #recordConsole(message: ConsoleMessage): void {
    const text = message.text();
    this.#console.push({
      sequence: this.#console.length + 1,
      atMs: elapsed(this.#startedAt),
      type: message.type(),
      textBytes: Buffer.byteLength(text, "utf8"),
      textSha256: sha256(text),
      argumentCount: message.args().length,
      sourceKind: classifyRealGamNetworkUrl(
        message.location().url,
        this.#pageOrigin,
      ),
    });
  }

  network(): readonly NetworkEvidence[] {
    return this.#network;
  }

  artifact(
    browserName: BrowserName,
    contract: RealGamCaseContract,
    snapshot: CaseSnapshot,
    observers: ObserverSnapshot,
    dom: DomEvidence,
    renderTrace: readonly RenderTraceEvidence[],
  ): Readonly<Record<string, unknown>> {
    return Object.freeze({
      schemaVersion: 1,
      browser: browserName,
      caseId: contract.caseId,
      releaseId: snapshot.releaseId,
      workflowRunId: process.env.GITHUB_RUN_ID ?? null,
      commitSha: process.env.GITHUB_SHA ?? null,
      snapshot,
      evidence: Object.freeze({
        console: this.#console,
        pageErrors: this.#pageErrors,
        network: this.#network,
        protocol: observers.protocol,
        gpt: observers.gpt,
        dom,
        renderTrace,
      }),
    });
  }

  failureArtifact(
    browserName: BrowserName,
    contract: RealGamCaseContract,
    phase: string,
    observers: ObserverSnapshot,
    renderTrace: readonly RenderTraceEvidence[],
  ): Readonly<Record<string, unknown>> {
    return Object.freeze({
      schemaVersion: 1,
      browser: browserName,
      caseId: contract.caseId,
      releaseId: process.env.TS_REAL_GAM_EXPECTED_RELEASE_ID ?? null,
      workflowRunId: process.env.GITHUB_RUN_ID ?? null,
      commitSha: process.env.GITHUB_SHA ?? null,
      harnessOutcome: "failed",
      phase,
      evidence: Object.freeze({
        console: this.#console,
        pageErrors: this.#pageErrors,
        network: this.#network,
        protocol: observers.protocol,
        gpt: observers.gpt,
        renderTrace,
      }),
    });
  }
}

function assertExactKeys(value: object, keys: readonly string[]): void {
  expect(Object.keys(value).sort()).toEqual([...keys].sort());
}

function isFiniteNonnegativeInteger(value: unknown): value is number {
  return Number.isInteger(value) && typeof value === "number" && value >= 0;
}

function parseCounts(value: unknown): LifecycleCounts {
  expect(value).toBeTruthy();
  expect(typeof value).toBe("object");
  const record = value as Record<string, unknown>;
  assertExactKeys(record, Object.keys(ZERO_COUNTS));
  for (const key of Object.keys(ZERO_COUNTS)) {
    expect(
      isFiniteNonnegativeInteger(record[key]),
      `${key} must be a count`,
    ).toBe(true);
  }
  return record as unknown as LifecycleCounts;
}

function parseSnapshot(value: unknown): CaseSnapshot {
  expect(value).toBeTruthy();
  expect(typeof value).toBe("object");
  const record = value as Record<string, unknown>;
  assertExactKeys(record, [
    "version",
    "caseId",
    "releaseId",
    "flow",
    "outcome",
    "reason",
    "durationMs",
    "dimensions",
    "counts",
    "ordering",
  ]);
  expect(record.version).toBe(1);
  expect(typeof record.caseId).toBe("string");
  expect(typeof record.releaseId).toBe("string");
  expect(typeof record.flow).toBe("string");
  expect(record.outcome === "accepted" || record.outcome === "failed").toBe(
    true,
  );
  expect(record.reason === null || typeof record.reason === "string").toBe(
    true,
  );
  expect(isFiniteNonnegativeInteger(record.durationMs)).toBe(true);
  if (record.dimensions !== null) {
    expect(typeof record.dimensions).toBe("object");
    assertExactKeys(record.dimensions as object, ["width", "height"]);
    expect(
      isFiniteNonnegativeInteger(
        (record.dimensions as Record<string, unknown>).width,
      ),
    ).toBe(true);
    expect(
      isFiniteNonnegativeInteger(
        (record.dimensions as Record<string, unknown>).height,
      ),
    ).toBe(true);
  }
  parseCounts(record.counts);
  if (record.ordering !== null) {
    expect(typeof record.ordering).toBe("object");
    assertExactKeys(record.ordering as object, [
      "parentTerminalSequence",
      "childRenderSequence",
    ]);
    const ordering = record.ordering as Record<string, unknown>;
    expect(isFiniteNonnegativeInteger(ordering.parentTerminalSequence)).toBe(
      true,
    );
    expect(isFiniteNonnegativeInteger(ordering.childRenderSequence)).toBe(true);
  }
  return value as CaseSnapshot;
}

export async function openProtectedRealGamPage(
  page: Page,
  environment: RealGamEnvironment,
): Promise<void> {
  try {
    await page.goto(environment.pageUrl, { waitUntil: "domcontentloaded" });
  } catch {
    throw new Error("the protected real-GAM page could not be opened");
  }
  const valid = await page.evaluate(
    ({ expectedReleaseId, expectedPucRelease, expectedCaseIds }) => {
      try {
        const descriptor = Object.getOwnPropertyDescriptor(
          window,
          "__tsRealGamTestNetwork",
        );
        if (!descriptor || !("value" in descriptor)) return false;
        const api = descriptor.value as Record<string, unknown>;
        if (
          typeof api !== "object" ||
          api === null ||
          !Object.isFrozen(api) ||
          Object.getOwnPropertySymbols(api).length !== 0 ||
          Object.getOwnPropertyNames(api).sort().join("\n") !==
            ["caseIds", "pucRelease", "releaseId", "run", "version"]
              .sort()
              .join("\n")
        ) {
          return false;
        }
        const version = Object.getOwnPropertyDescriptor(api, "version");
        const releaseId = Object.getOwnPropertyDescriptor(api, "releaseId");
        const pucRelease = Object.getOwnPropertyDescriptor(api, "pucRelease");
        const caseIds = Object.getOwnPropertyDescriptor(api, "caseIds");
        const run = Object.getOwnPropertyDescriptor(api, "run");
        if (
          !version ||
          !("value" in version) ||
          version.value !== 1 ||
          !releaseId ||
          !("value" in releaseId) ||
          releaseId.value !== expectedReleaseId ||
          !pucRelease ||
          !("value" in pucRelease) ||
          pucRelease.value !== expectedPucRelease ||
          !caseIds ||
          !("value" in caseIds) ||
          !Array.isArray(caseIds.value) ||
          !Object.isFrozen(caseIds.value) ||
          !run ||
          !("value" in run) ||
          typeof run.value !== "function"
        ) {
          return false;
        }
        return (
          caseIds.value.length === expectedCaseIds.length &&
          caseIds.value.every(
            (caseId: unknown, index: number) =>
              caseId === expectedCaseIds[index],
          )
        );
      } catch {
        return false;
      }
    },
    {
      expectedReleaseId: environment.expectedReleaseId,
      expectedPucRelease: REAL_GAM_PUC_RELEASE,
      expectedCaseIds: REAL_GAM_CASES.map(({ caseId }) => caseId),
    },
  );
  expect(
    valid,
    "the protected page must expose the exact frozen test-network contract",
  ).toBe(true);
}

async function invokeCase(
  page: Page,
  contract: RealGamCaseContract,
  expectedReleaseId: string,
): Promise<CaseSnapshot> {
  let raw: unknown;
  try {
    raw = await page.evaluate(
      async ({ expected, releaseId }) => {
        const exactKeys = (value: object, keys: readonly string[]): boolean =>
          Object.getOwnPropertySymbols(value).length === 0 &&
          Object.getOwnPropertyNames(value).sort().join("\n") ===
            [...keys].sort().join("\n");
        const integer = (value: unknown): value is number =>
          typeof value === "number" && Number.isInteger(value) && value >= 0;
        const descriptor = Object.getOwnPropertyDescriptor(
          window,
          "__tsRealGamTestNetwork",
        );
        if (!descriptor || !("value" in descriptor)) throw new Error();
        const api = descriptor.value as Record<string, unknown>;
        const run = Object.getOwnPropertyDescriptor(api, "run");
        if (!run || !("value" in run) || typeof run.value !== "function") {
          throw new Error();
        }
        const result = await Reflect.apply(run.value, api, [expected.caseId]);
        if (
          typeof result !== "object" ||
          result === null ||
          !exactKeys(result, [
            "version",
            "caseId",
            "releaseId",
            "flow",
            "outcome",
            "reason",
            "durationMs",
            "dimensions",
            "counts",
            "ordering",
          ])
        ) {
          throw new Error();
        }
        const source = result as Record<string, unknown>;
        if (
          source.version !== 1 ||
          source.caseId !== expected.caseId ||
          source.releaseId !== releaseId ||
          source.flow !== expected.flow ||
          source.outcome !== expected.outcome ||
          source.reason !== expected.reason ||
          !integer(source.durationMs) ||
          source.durationMs > expected.deadlineMs
        ) {
          throw new Error();
        }
        const dimensions = source.dimensions as Record<string, unknown> | null;
        if (
          (expected.dimensions === null && dimensions !== null) ||
          (expected.dimensions !== null &&
            (typeof dimensions !== "object" ||
              dimensions === null ||
              !exactKeys(dimensions, ["width", "height"]) ||
              dimensions.width !== expected.dimensions.width ||
              dimensions.height !== expected.dimensions.height))
        ) {
          throw new Error();
        }
        const resultCounts = source.counts as Record<string, unknown> | null;
        const countKeys = Object.keys(expected.counts);
        if (
          typeof resultCounts !== "object" ||
          resultCounts === null ||
          !exactKeys(resultCounts, countKeys) ||
          countKeys.some(
            (key) =>
              !integer(resultCounts[key]) ||
              resultCounts[key] !==
                expected.counts[key as keyof typeof expected.counts],
          )
        ) {
          throw new Error();
        }
        const resultOrdering = source.ordering as Record<
          string,
          unknown
        > | null;
        let safeOrdering: {
          parentTerminalSequence: number;
          childRenderSequence: number;
        } | null = null;
        if (expected.ordering === null) {
          if (resultOrdering !== null) throw new Error();
        } else {
          if (
            typeof resultOrdering !== "object" ||
            resultOrdering === null ||
            !exactKeys(resultOrdering, [
              "parentTerminalSequence",
              "childRenderSequence",
            ]) ||
            !integer(resultOrdering.parentTerminalSequence) ||
            !integer(resultOrdering.childRenderSequence) ||
            resultOrdering.parentTerminalSequence >=
              resultOrdering.childRenderSequence
          ) {
            throw new Error();
          }
          safeOrdering = {
            parentTerminalSequence: resultOrdering.parentTerminalSequence,
            childRenderSequence: resultOrdering.childRenderSequence,
          };
        }
        // Return only fixed expected values plus validated numbers. Descriptor,
        // account, bid, creative, ticket, nonce, headers, and bodies cannot cross.
        return {
          version: 1,
          caseId: expected.caseId,
          releaseId,
          flow: expected.flow,
          outcome: expected.outcome,
          reason: expected.reason,
          durationMs: source.durationMs,
          dimensions: expected.dimensions,
          counts: expected.counts,
          ordering: safeOrdering,
        };
      },
      { expected: contract, releaseId: expectedReleaseId },
    );
  } catch {
    throw new Error(
      "the protected case did not produce its exact attested snapshot",
    );
  }
  return parseSnapshot(raw);
}

function assertSnapshot(
  snapshot: CaseSnapshot,
  contract: RealGamCaseContract,
  expectedReleaseId: string,
): void {
  expect(snapshot.version).toBe(1);
  expect(snapshot.caseId).toBe(contract.caseId);
  expect(snapshot.releaseId).toBe(expectedReleaseId);
  expect(snapshot.flow).toBe(contract.flow);
  expect(snapshot.outcome).toBe(contract.outcome);
  expect(snapshot.reason).toBe(contract.reason);
  expect(snapshot.durationMs).toBeLessThanOrEqual(contract.deadlineMs);
  expect(snapshot.dimensions).toEqual(contract.dimensions);
  expect(snapshot.counts).toEqual(contract.counts);
  if (contract.ordering?.parentTerminalBeforeChild) {
    expect(snapshot.ordering).not.toBeNull();
    expect(snapshot.ordering!.parentTerminalSequence).toBeLessThan(
      snapshot.ordering!.childRenderSequence,
    );
  } else {
    expect(snapshot.ordering).toBeNull();
  }
}

async function assertRuntimeRelease(
  page: Page,
  expectedReleaseId: string,
): Promise<void> {
  const releaseMatches = await page.evaluate((releaseId) => {
    const tsjs = (window as unknown as { tsjs?: Record<string, unknown> }).tsjs;
    if (!tsjs || typeof tsjs !== "object") return false;
    const boot = tsjs.boot as Record<string, unknown> | undefined;
    const manifest = boot?.manifest as Record<string, unknown> | undefined;
    return (
      tsjs.releaseId === releaseId &&
      boot?.releaseId === releaseId &&
      manifest?.releaseId === releaseId
    );
  }, expectedReleaseId);
  expect(releaseMatches).toBe(true);
}

async function collectObservers(page: Page): Promise<ObserverSnapshot> {
  const protocol: ProtocolEvidence[] = [];
  const gpt: GptEvidence[] = [];
  for (const frame of page.frames()) {
    const snapshot = await frame
      .evaluate(() => {
        const observer = (
          window as unknown as {
            __tsRealGamObserver?: {
              protocol: Array<{ message: string; atMs: number }>;
              gpt: Array<{ event: string; atMs: number }>;
            };
          }
        ).__tsRealGamObserver;
        return observer
          ? { protocol: [...observer.protocol], gpt: [...observer.gpt] }
          : null;
      })
      .catch(() => null);
    if (!snapshot) continue;
    for (const event of snapshot.protocol) {
      if (
        PROTOCOL_MESSAGES.includes(
          event.message as (typeof PROTOCOL_MESSAGES)[number],
        ) &&
        typeof event.atMs === "number" &&
        Number.isFinite(event.atMs)
      ) {
        protocol.push(event as ProtocolEvidence);
      }
    }
    for (const event of snapshot.gpt) {
      if (
        GPT_EVENT_NAMES.includes(
          event.event as (typeof GPT_EVENT_NAMES)[number],
        ) &&
        typeof event.atMs === "number" &&
        Number.isFinite(event.atMs)
      ) {
        gpt.push(event as GptEvidence);
      }
    }
  }
  return Object.freeze({ protocol, gpt });
}

function countNamed(
  events: readonly GptEvidence[],
  eventName: GptEvidence["event"],
): number {
  return events.filter(({ event }) => event === eventName).length;
}

function assertObservers(
  observers: ObserverSnapshot,
  contract: RealGamCaseContract,
): void {
  expect(countNamed(observers.gpt, "slotRequested")).toBe(contract.gptCycles);
  expect(countNamed(observers.gpt, "slotResponseReceived")).toBe(
    contract.gptCycles,
  );
  expect(countNamed(observers.gpt, "slotRenderEnded")).toBe(contract.gptCycles);
  expect(
    observers.protocol.filter(({ message }) => message === "Prebid Request")
      .length,
  ).toBe(contract.counts.bridgeClaims);
}

function assertNetwork(
  network: readonly NetworkEvidence[],
  contract: RealGamCaseContract,
): void {
  const count = (kind: NetworkEvidence["kind"]): number =>
    network.filter((record) => record.kind === kind).length;
  expect(count("renderer")).toBe(contract.counts.rendererFrames);
  expect(count("runner-proxy")).toBe(contract.counts.runnerLoads);
  expect(count("creative")).toBe(contract.counts.creativeRequests);
  if (contract.gptCycles > 0) expect(count("gpt")).toBeGreaterThanOrEqual(1);
  for (const record of network) {
    if (record.kind === "renderer" || record.kind === "runner-proxy") {
      expect(record.failure).not.toBe(true);
      expect(record.status).toBe(200);
    }
  }
}

async function observeElements(
  page: Page,
  selector: string,
  role: ElementGeometry["role"],
): Promise<{
  evidence: ElementGeometry[];
  frames: Frame[];
}> {
  const evidence: ElementGeometry[] = [];
  const contentFrames: Frame[] = [];
  for (const frame of page.frames()) {
    const handles = await frame.locator(selector).elementHandles();
    for (const handle of handles) {
      const geometry = await elementGeometry(handle, role);
      evidence.push(geometry);
      if (role === "puc-owner") {
        const contentFrame = await (
          handle as ElementHandle<HTMLIFrameElement>
        ).contentFrame();
        if (contentFrame) contentFrames.push(contentFrame);
      }
    }
  }
  return { evidence, frames: contentFrames };
}

async function elementGeometry(
  handle: ElementHandle<Node>,
  role: ElementGeometry["role"],
): Promise<ElementGeometry> {
  const box = await handle.boundingBox();
  expect(box, `${role} must have a rendered box`).not.toBeNull();
  const css = await handle.evaluate((element) => {
    const htmlElement = element as HTMLElement;
    const style = getComputedStyle(htmlElement);
    return {
      clientWidth: htmlElement.clientWidth,
      clientHeight: htmlElement.clientHeight,
      scrollWidth: htmlElement.scrollWidth,
      scrollHeight: htmlElement.scrollHeight,
      marginTop: style.marginTop,
      marginRight: style.marginRight,
      marginBottom: style.marginBottom,
      marginLeft: style.marginLeft,
      overflowX: style.overflowX,
      overflowY: style.overflowY,
    };
  });
  return {
    role,
    width: box!.width,
    height: box!.height,
    ...css,
  };
}

async function documentGeometry(
  frame: Frame,
  role: DocumentGeometry["role"],
): Promise<DocumentGeometry> {
  return frame.evaluate((documentRole) => {
    const root = document.documentElement;
    const body = document.body;
    if (!root || !body) throw new Error(`${documentRole} must have a body`);
    const bodyStyle = getComputedStyle(body);
    const rootStyle = getComputedStyle(root);
    return {
      role: documentRole,
      width: innerWidth,
      height: innerHeight,
      clientWidth: root.clientWidth,
      clientHeight: root.clientHeight,
      scrollWidth: Math.max(root.scrollWidth, body.scrollWidth),
      scrollHeight: Math.max(root.scrollHeight, body.scrollHeight),
      bodyMarginTop: bodyStyle.marginTop,
      bodyMarginRight: bodyStyle.marginRight,
      bodyMarginBottom: bodyStyle.marginBottom,
      bodyMarginLeft: bodyStyle.marginLeft,
      overflowX:
        bodyStyle.overflowX === "visible"
          ? rootStyle.overflowX
          : bodyStyle.overflowX,
      overflowY:
        bodyStyle.overflowY === "visible"
          ? rootStyle.overflowY
          : bodyStyle.overflowY,
    };
  }, role);
}

function assertElementGeometry(
  geometry: ElementGeometry,
  dimensions: Readonly<{ width: number; height: number }>,
): void {
  expect(geometry.width).toBe(dimensions.width);
  expect(geometry.height).toBe(dimensions.height);
  expect(geometry.clientWidth).toBeLessThanOrEqual(dimensions.width);
  expect(geometry.clientHeight).toBeLessThanOrEqual(dimensions.height);
  expect(geometry.scrollWidth).toBeLessThanOrEqual(dimensions.width);
  expect(geometry.scrollHeight).toBeLessThanOrEqual(dimensions.height);
  expect(geometry.marginTop).toBe("0px");
  expect(geometry.marginRight).toBe("0px");
  expect(geometry.marginBottom).toBe("0px");
  expect(geometry.marginLeft).toBe("0px");
  expect(["hidden", "clip"]).toContain(geometry.overflowX);
  expect(["hidden", "clip"]).toContain(geometry.overflowY);
}

function assertDocumentGeometry(
  geometry: DocumentGeometry,
  dimensions: Readonly<{ width: number; height: number }>,
): void {
  expect(geometry.width).toBe(dimensions.width);
  expect(geometry.height).toBe(dimensions.height);
  expect(geometry.clientWidth).toBe(dimensions.width);
  expect(geometry.clientHeight).toBe(dimensions.height);
  expect(geometry.scrollWidth).toBeLessThanOrEqual(dimensions.width);
  expect(geometry.scrollHeight).toBeLessThanOrEqual(dimensions.height);
  expect(geometry.bodyMarginTop).toBe("0px");
  expect(geometry.bodyMarginRight).toBe("0px");
  expect(geometry.bodyMarginBottom).toBe("0px");
  expect(geometry.bodyMarginLeft).toBe("0px");
  expect(["hidden", "clip"]).toContain(geometry.overflowX);
  expect(["hidden", "clip"]).toContain(geometry.overflowY);
}

async function collectDom(
  page: Page,
  contract: RealGamCaseContract,
): Promise<DomEvidence> {
  const caseSelector = `[data-ts-real-gam-case="${contract.caseId}"]`;
  const root = page.locator(caseSelector);
  await expect(root).toHaveCount(1);
  await expect(root).toHaveAttribute("data-ts-real-gam-state", TERMINAL_STATE);
  const terminalRoots = await root.count();

  const slots = await observeElements(
    page,
    `${caseSelector} [data-ts-real-gam-slot]`,
    "slot",
  );
  expect(slots.evidence).toHaveLength(contract.slotCount);

  const owners = await observeElements(
    page,
    `[data-ts-real-gam-owner="${contract.caseId}"]`,
    "puc-owner",
  );
  expect(owners.evidence).toHaveLength(contract.pucOwners);

  const owned = await observeElements(
    page,
    `[data-ts-real-gam-owned-frame="${contract.caseId}"]`,
    "owned-frame",
  );
  expect(owned.evidence).toHaveLength(contract.terminalOwnedFrames);

  const renderer = await observeElements(
    page,
    `iframe[src*="${APS_RENDERER_PATH}"]`,
    "renderer",
  );
  expect(renderer.evidence).toHaveLength(contract.terminalRendererFrames);

  const rendererFrames = page.frames().filter((frame) => {
    const url = parsedUrl(frame.url());
    return url?.pathname === APS_RENDERER_PATH;
  });
  expect(rendererFrames).toHaveLength(contract.terminalRendererFrames);

  const creativeElements: ElementGeometry[] = [];
  const creativeFrames: Frame[] = [];
  for (const frame of rendererFrames) {
    const children = frame.childFrames();
    expect(children).toHaveLength(1);
    creativeFrames.push(...children);
    const handles = await frame.locator("iframe").elementHandles();
    expect(handles).toHaveLength(1);
    creativeElements.push(await elementGeometry(handles[0]!, "creative"));
  }

  const documents: DocumentGeometry[] = [];
  for (const frame of owners.frames) {
    documents.push(await documentGeometry(frame, "puc-owner-document"));
  }
  for (const frame of rendererFrames) {
    documents.push(await documentGeometry(frame, "renderer-document"));
  }
  for (const frame of creativeFrames) {
    documents.push(await documentGeometry(frame, "creative-document"));
  }

  if (contract.dimensions) {
    for (const geometry of [
      ...owners.evidence,
      ...owned.evidence,
      ...renderer.evidence,
      ...creativeElements,
    ]) {
      assertElementGeometry(geometry, contract.dimensions);
    }
    for (const geometry of documents) {
      assertDocumentGeometry(geometry, contract.dimensions);
    }
  }

  return Object.freeze({
    terminalRoots,
    elements: Object.freeze([
      ...slots.evidence,
      ...owners.evidence,
      ...owned.evidence,
      ...renderer.evidence,
      ...creativeElements,
    ]),
    documents: Object.freeze(documents),
  });
}

async function collectRenderTrace(
  page: Page,
): Promise<readonly RenderTraceEvidence[]> {
  return page.evaluate(() => {
    const tsjs = (window as unknown as { tsjs?: Record<string, unknown> }).tsjs;
    const diagnostics = tsjs?.diagnostics as
      Record<string, unknown> | undefined;
    const renderTrace = diagnostics?.renderTrace as
      Record<string, unknown> | undefined;
    const history = renderTrace?.history;
    if (typeof history !== "function") return [];
    let records: unknown;
    try {
      records = Reflect.apply(history, renderTrace, []);
    } catch {
      return [];
    }
    if (!Array.isArray(records)) return [];
    return records.map((record) => {
      const value =
        typeof record === "object" && record !== null
          ? (record as Record<string, unknown>)
          : {};
      const path = value.path;
      const servedFrom = value.servedFrom;
      return {
        path:
          path === "auction" || path === "ssat" || path === "gam-refresh"
            ? path
            : null,
        rendered: value.rendered === true,
        servedFrom:
          servedFrom === "inline" ||
          servedFrom === "gam" ||
          servedFrom === "debug-adm" ||
          servedFrom === "pbs-cache" ||
          servedFrom === "prebid"
            ? servedFrom
            : null,
        gamEmpty: typeof value.gamEmpty === "boolean" ? value.gamEmpty : null,
        injected: typeof value.injected === "boolean" ? value.injected : null,
        visible: typeof value.visible === "boolean" ? value.visible : null,
        count:
          typeof value.count === "number" && Number.isFinite(value.count)
            ? value.count
            : 0,
        seq:
          typeof value.seq === "number" && Number.isFinite(value.seq)
            ? value.seq
            : 0,
      };
    });
  });
}

function assertSafeArtifact(
  serialized: string,
  environment: RealGamEnvironment,
): void {
  if (
    serialized.includes(environment.authorization) ||
    serialized.includes(environment.pageUrl)
  ) {
    throw new Error(
      "a protected value reached the sanitized evidence boundary",
    );
  }
  if (
    /"(?:accountId|aaxResponse|adm|authorization|creativeBody|descriptor|lifecycleTicket|nonce|postData|requestHeaders|responseBody|responseHeaders)"\s*:/u.test(
      serialized,
    ) ||
    serialized.includes("<script") ||
    serialized.includes("<!doctype")
  ) {
    throw new Error(
      "an unsafe field or response body reached browser evidence",
    );
  }
}

async function writeEvidence(
  page: Page,
  testInfo: TestInfo,
  browserName: BrowserName,
  contract: RealGamCaseContract,
  artifact: Readonly<Record<string, unknown>>,
  environment: RealGamEnvironment,
): Promise<void> {
  const directory = resolve(EVIDENCE_ROOT, browserName, contract.caseId);
  mkdirSync(directory, { recursive: true });
  const evidencePath = resolve(directory, "sanitized-trace-v1.json");
  const serialized = `${JSON.stringify(artifact, null, 2)}\n`;
  assertSafeArtifact(serialized, environment);
  writeFileSync(evidencePath, serialized, { encoding: "utf8", mode: 0o600 });

  const screenshotPath = resolve(directory, "terminal.png");
  await page.screenshot({
    path: screenshotPath,
    fullPage: true,
    animations: "disabled",
    mask: [page.locator("[data-ts-real-gam-secret]")],
  });
  await testInfo.attach("sanitized-real-gam-trace", {
    path: evidencePath,
    contentType: "application/json",
  });
  await testInfo.attach("real-gam-terminal-screenshot", {
    path: screenshotPath,
    contentType: "image/png",
  });
}

export async function runAttestedRealGamCase(options: {
  readonly page: Page;
  readonly browserName: BrowserName;
  readonly testInfo: TestInfo;
  readonly environment: RealGamEnvironment;
  readonly contract: RealGamCaseContract;
}): Promise<void> {
  const { page, browserName, testInfo, environment, contract } = options;
  const collector = new EvidenceCollector(page, environment.pageUrl);
  let phase = "observer-installation";
  try {
    await installBrowserObservers(page);
    phase = "protected-page-contract";
    await openProtectedRealGamPage(page, environment);
    phase = "case-snapshot";
    const snapshot = await invokeCase(
      page,
      contract,
      environment.expectedReleaseId,
    );
    assertSnapshot(snapshot, contract, environment.expectedReleaseId);
    phase = "runtime-release-binding";
    await assertRuntimeRelease(page, environment.expectedReleaseId);
    phase = "browser-observers";
    const observers = await collectObservers(page);
    assertObservers(observers, contract);
    phase = "network-contract";
    assertNetwork(collector.network(), contract);
    phase = "terminal-dom";
    const dom = await collectDom(page, contract);
    phase = "sanitized-render-trace";
    const renderTrace = await collectRenderTrace(page);
    const artifact = collector.artifact(
      browserName,
      contract,
      snapshot,
      observers,
      dom,
      renderTrace,
    );
    phase = "evidence-write";
    await writeEvidence(
      page,
      testInfo,
      browserName,
      contract,
      artifact,
      environment,
    );
  } catch {
    const observers = await collectObservers(page).catch(() =>
      Object.freeze({ protocol: [], gpt: [] }),
    );
    const renderTrace = await collectRenderTrace(page).catch(() => []);
    const failureArtifact = collector.failureArtifact(
      browserName,
      contract,
      phase,
      observers,
      renderTrace,
    );
    await writeEvidence(
      page,
      testInfo,
      browserName,
      contract,
      failureArtifact,
      environment,
    ).catch(() => undefined);
    throw new Error(`${contract.caseId} failed during ${phase}`);
  }
}

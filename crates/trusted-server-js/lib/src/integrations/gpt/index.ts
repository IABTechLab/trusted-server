import { terminalSummaryStageOutcome } from '../../core/ad_trace';
import { log } from '../../core/log';
import type { AuctionSlot, AuctionBidData, AuctionTraceSummary, TsjsApi } from '../../core/types';
import {
  APS_UNIVERSAL_CREATIVE_RENDERER,
  APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
  apsRendererUrl,
  consumeApsPrebidRenderer,
  getApsPrebidRenderer,
  validateApsRenderer,
} from '../aps/render';

import { installGptGuard } from './script_guard';

/**
 * Google Publisher Tags (GPT) Integration Shim
 *
 * Hooks into the googletag.cmd command queue so the Trusted Server can
 * observe and augment ad-slot definitions before GPT processes them.
 * The shim ensures the googletag stub exists early (matching GPT's own
 * bootstrap pattern) and patches `cmd.push` to wrap queued callbacks.
 *
 * Current capabilities:
 *   - Installs a script guard that rewrites dynamically inserted GPT
 *     `<script>` elements to the first-party proxy endpoint.
 *   - Takes over the `googletag.cmd` array so that every callback runs
 *     through a wrapper that can inject targeting, logging, or consent
 *     signals before the real GPT processes the command.
 *
 * Future enhancements (driven by config or tsjs API):
 *   - Inject EC ID as page-level key-value targeting.
 *   - Gate ad requests on consent status.
 *   - Rewrite ad-unit paths for A/B testing.
 */

const TS_INITIAL_TARGETING_KEY = 'ts_initial' as const;
const TS_BID_TARGETING_KEYS = [
  'hb_pb',
  'hb_bidder',
  'hb_adid',
  'hb_cache_host',
  'hb_cache_path',
] as const;
const TS_BASE_TARGETING_KEYS = [
  ...TS_BID_TARGETING_KEYS,
  TS_INITIAL_TARGETING_KEY,
  'ts_trace',
] as const;

// ------------------------------------------------------------------
// googletag type stubs (minimal surface needed by the shim)
// ------------------------------------------------------------------

interface GoogleTagSlot {
  getAdUnitPath(): string;
  getSlotElementId(): string;
  setTargeting(key: string, value: string | string[]): GoogleTagSlot;
  clearTargeting?(key?: string): GoogleTagSlot;
  addService(service: GoogleTagPubAdsService): GoogleTagSlot;
  getTargeting?(key: string): string[];
}

interface SlotRenderEndedEvent {
  isEmpty?: boolean;
  isBackfill?: boolean;
  slot: GoogleTagSlot;
}

interface GptSlotEvent {
  slot: GoogleTagSlot;
  isEmpty?: boolean;
  isBackfill?: boolean;
}

interface RenderCandidate {
  slotId: string;
  generation: number;
  slot: GoogleTagSlot;
  divId: string;
  /** Renderable only when this record's own hb_adid matches the request snapshot. */
  bid?: Readonly<AuctionBidData>;
  adId?: string;
  traceToken?: string;
  createdAt: number;
  terminal: boolean;
  consumed: boolean;
  superseded: boolean;
}

interface ExpectedRender {
  candidate: RenderCandidate;
  source: MessageEventSource;
  expiresAt: number;
  consumed: boolean;
}

interface AdTraceRequestBoundarySnapshot {
  slotId?: string;
  bidder?: string;
  adId?: string;
  traceToken?: string;
  bid?: AuctionBidData;
}

const requestCandidates = new Map<string, RenderCandidate[]>();
const expectedRenders = new Map<string, ExpectedRender[]>();
const fallbackGenerations = new Map<string, number>();

const MAX_EXPECTED_RENDERS = 200;
const MAX_FALLBACK_GENERATIONS = 200;
const MAX_ACTIVE_CACHE_RENDERS = 64;
const MAX_PRIVATE_REQUEST_OWNERS = 64;
let privateNavigationGeneration = 0;

interface PrivateRequestOwner {
  slotId: string;
  adId?: string;
  bid?: Readonly<AuctionBidData>;
  generation?: number;
  element: HTMLElement | null;
  navigationGeneration: number;
  expiresAt: number;
  served: boolean;
}

const latestPrivateRequestBySlot = new Map<string, PrivateRequestOwner>();
const staleTsAdIdBits = new Uint32Array(64);

function staleAdIdHashes(value: string): [number, number] {
  let first = 2166136261;
  let second = 5381;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    first = Math.imul(first ^ code, 16777619) >>> 0;
    second = (Math.imul(second, 33) ^ code) >>> 0;
  }
  return [first % 2048, second % 2048];
}

function rememberStaleAdIdBits(adId: string): void {
  for (const hash of staleAdIdHashes(adId)) {
    staleTsAdIdBits[hash >>> 5] |= 1 << (hash & 31);
  }
}

function staleAdIdBitsContain(adId: string): boolean {
  return staleAdIdHashes(adId).every(
    (hash) => (staleTsAdIdBits[hash >>> 5] & (1 << (hash & 31))) !== 0
  );
}

interface ActiveCacheRender {
  controller: AbortController;
  slotId: string;
  adId: string;
  source: MessageEventSource | null;
  generation?: number;
  candidate?: RenderCandidate;
  cacheHost: string;
  cachePath: string;
  traceToken?: string;
  navigationGeneration: number;
  expiresAt: number;
  expiryTimer?: ReturnType<typeof setTimeout>;
}

const activeCacheRenders = new Set<ActiveCacheRender>();
const latestCacheRenderBySlot = new Map<string, ActiveCacheRender>();

function rememberStaleTsOwner(owner: PrivateRequestOwner): void {
  if (!owner.bid || !owner.adId) return;
  rememberStaleAdIdBits(owner.adId);
}

function retireActiveCacheRender(render: ActiveCacheRender): void {
  if (render.expiryTimer) clearTimeout(render.expiryTimer);
  render.controller.abort();
  activeCacheRenders.delete(render);
  if (latestCacheRenderBySlot.get(render.slotId) === render) {
    latestCacheRenderBySlot.delete(render.slotId);
  }
}

function invalidatePrivateRequestOwners(slotId?: string): void {
  const entries = slotId
    ? [[slotId, latestPrivateRequestBySlot.get(slotId)] as const]
    : [...latestPrivateRequestBySlot.entries()];
  for (const [key, owner] of entries) {
    if (!owner) continue;
    rememberStaleTsOwner(owner);
    latestPrivateRequestBySlot.delete(key);
  }
}

function abortActiveCacheRenders(slotId?: string): void {
  privateNavigationGeneration += slotId ? 0 : 1;
  for (const render of [...activeCacheRenders]) {
    if (!slotId || render.slotId === slotId) retireActiveCacheRender(render);
  }
  invalidatePrivateRequestOwners(slotId);
}

function claimPrivateRequestOwner(
  slotId: string,
  adId: string | undefined,
  bid: Readonly<AuctionBidData> | undefined,
  element: HTMLElement | null
): PrivateRequestOwner {
  for (const render of [...activeCacheRenders]) {
    if (render.slotId === slotId) retireActiveCacheRender(render);
  }
  const previous = latestPrivateRequestBySlot.get(slotId);
  if (previous) rememberStaleTsOwner(previous);
  const owner: PrivateRequestOwner = {
    slotId,
    adId,
    bid,
    element,
    navigationGeneration: privateNavigationGeneration,
    expiresAt: monotonicNow() + 30_000,
    served: false,
  };
  latestPrivateRequestBySlot.delete(slotId);
  latestPrivateRequestBySlot.set(slotId, owner);
  while (latestPrivateRequestBySlot.size > MAX_PRIVATE_REQUEST_OWNERS) {
    const oldest = latestPrivateRequestBySlot.keys().next().value as string | undefined;
    if (!oldest) break;
    const evicted = latestPrivateRequestBySlot.get(oldest);
    if (evicted) rememberStaleTsOwner(evicted);
    latestPrivateRequestBySlot.delete(oldest);
    for (const render of [...activeCacheRenders]) {
      if (render.slotId === oldest) retireActiveCacheRender(render);
    }
  }
  return owner;
}

function isKnownStaleTsAdId(adId: string): boolean {
  // The fixed-size bitset intentionally never forgets within the page session:
  // false positives fail closed, while bounded-map eviction cannot create a
  // false negative that lets a stale TS Universal Creative fall through.
  return staleAdIdBitsContain(adId);
}

function monotonicNow(): number {
  return typeof performance === 'undefined' ? Date.now() : performance.now();
}

function findSlotElementByDivId(divId: string): HTMLElement | null {
  const exact = document.getElementById(divId);
  if (exact) return exact;

  return (
    Array.from(document.querySelectorAll<HTMLElement>('[id]')).find(
      (el) => el.id.startsWith(divId) && !el.id.endsWith('-container')
    ) ?? null
  );
}

function candidateSlotRoots(divId: string): HTMLElement[] {
  const roots: HTMLElement[] = [];
  const slotEl = findSlotElementByDivId(divId);
  if (slotEl) {
    roots.push(slotEl);
    const container = document.getElementById(`${slotEl.id}-container`);
    if (container) roots.push(container);
  }

  const configuredContainer = document.getElementById(`${divId}-container`);
  if (configuredContainer && !roots.includes(configuredContainer)) {
    roots.push(configuredContainer);
  }

  return roots;
}

function slotIdForMessageSource(source: MessageEventSource | null): string | undefined {
  if (!source) return undefined;

  const slots = window.tsjs?.adSlots ?? [];
  return slots.find((slot) =>
    candidateSlotRoots(slot.div_id).some((root) =>
      Array.from(root.querySelectorAll('iframe')).some((iframe) => iframe.contentWindow === source)
    )
  )?.id;
}

function messageSourceBelongsToAdUnit(
  source: MessageEventSource | null,
  adUnitCode: string
): boolean {
  if (!source) return false;
  const roots = [
    document.getElementById(adUnitCode),
    document.getElementById(`${adUnitCode}-container`),
  ].filter((root): root is HTMLElement => root !== null);

  return roots.some((root) =>
    Array.from(root.querySelectorAll('iframe')).some((iframe) => iframe.contentWindow === source)
  );
}

function clearTargetingKeys(slot: GoogleTagSlot, keys: Iterable<string>): void {
  if (typeof slot.clearTargeting !== 'function') return;

  for (const key of new Set(keys)) {
    slot.clearTargeting(key);
  }
}

interface GoogleTagPubAdsService {
  setTargeting(key: string, value: string | string[]): GoogleTagPubAdsService;
  getTargeting(key: string): string[];
  enableSingleRequest(): void;
  addEventListener(event: string, fn: (e: GptSlotEvent) => void): void;
  refresh(slots?: GoogleTagSlot[]): void;
  getSlots?(): GoogleTagSlot[];
  disableInitialLoad?(): void;
}

interface GoogleTagConfig extends Record<string, unknown> {
  disableInitialLoad?: boolean;
}

interface GoogleTag {
  cmd: Array<() => void>;
  pubads(): GoogleTagPubAdsService;
  defineSlot(
    adUnitPath: string,
    size: Array<number | number[]>,
    elementId: string
  ): GoogleTagSlot | null;
  destroySlots(slots?: GoogleTagSlot[]): boolean;
  enableServices(): void;
  display(elementId: string): void;
  setConfig(config: GoogleTagConfig): void;
  _loaded_?: boolean;
}

type GptWindow = Window & {
  googletag?: Partial<GoogleTag>;
  __tsjs_slim_prebid_url?: string;
};

const cacheInvalidationHookedTags = new WeakSet<object>();
const cacheInvalidationHookedSlots = new WeakSet<object>();

function installSlotCacheInvalidationHook(slot: GoogleTagSlot): void {
  if (cacheInvalidationHookedSlots.has(slot) || typeof slot.clearTargeting !== 'function') return;
  const original = slot.clearTargeting.bind(slot);
  slot.clearTargeting = (key?: string) => {
    const slotId = slotIdForGptSlot(slot);
    if (slotId) abortActiveCacheRenders(slotId);
    return original(key);
  };
  cacheInvalidationHookedSlots.add(slot);
}

function installGoogleTagCacheInvalidationHooks(g: Partial<GoogleTag>): void {
  if (cacheInvalidationHookedTags.has(g)) return;
  if (typeof g.destroySlots === 'function') {
    const original = g.destroySlots.bind(g);
    g.destroySlots = (slots?: GoogleTagSlot[]) => {
      if (slots) {
        slots.forEach((slot) => {
          const slotId = slotIdForGptSlot(slot);
          if (slotId) abortActiveCacheRenders(slotId);
        });
      } else {
        abortActiveCacheRenders();
      }
      return original(slots);
    };
  }
  cacheInvalidationHookedTags.add(g);
}

// ------------------------------------------------------------------
// Shim implementation
// ------------------------------------------------------------------

/**
 * Ensure the `googletag` stub exists on `window`.
 *
 * This mirrors the official GPT bootstrap snippet:
 * ```js
 * window.googletag = window.googletag || {};
 * googletag.cmd = googletag.cmd || [];
 * ```
 * By running before the publisher's own snippet we can patch `cmd` early.
 */
function ensureGoogleTagStub(win: GptWindow): Partial<GoogleTag> {
  const tag = (win.googletag = win.googletag ?? {});
  tag.cmd = tag.cmd ?? [];
  return tag;
}

/**
 * Wrap a queued GPT callback to add instrumentation and future hook points.
 *
 * Today the wrapper only logs; as the integration matures it will inject
 * EC ID targeting and consent gates.
 */
function wrapCommand(fn: () => void): () => void {
  return () => {
    try {
      fn();
    } catch (err) {
      log.error('GPT shim: queued command threw', err);
    }
  };
}

/**
 * Patch `googletag.cmd` so every pushed callback runs through [`wrapCommand`].
 *
 * Preserves the existing `tag.cmd` array identity so that GPT's own custom
 * `cmd.push` behaviour (immediate execution when the library is already
 * loaded) is not lost. The original `push` is saved and delegated to after
 * wrapping each callback.
 *
 * Already-queued callbacks are re-wrapped in place so GPT processes them
 * through our wrapper when it drains the queue.
 */
function patchCommandQueue(tag: Partial<GoogleTag>): void {
  // Ensure the queue exists.
  if (!tag.cmd) {
    // Cast through unknown so an array satisfies the { push } type.
    tag.cmd = [];
  }

  const queue = tag.cmd;

  // Guard against double-patching (idempotent install).
  if ((queue as { __tsPushed?: boolean }).__tsPushed) {
    log.debug('GPT shim: command queue already patched, skipping');
    return;
  }

  const originalPush = queue.push.bind(queue);

  // Override push on the *existing* array — preserves object identity so
  // GPT (if already loaded) keeps its reference.
  (queue as { push: (...cbs: Array<() => void>) => unknown }).push = function (
    ...callbacks: Array<() => void>
  ): unknown {
    const wrapped = callbacks.map(wrapCommand);
    return originalPush(...wrapped);
  };

  // Mark as patched to prevent double-wrapping.
  (queue as { __tsPushed?: boolean }).__tsPushed = true;

  // Re-wrap any callbacks that were queued before we patched.
  // Only applicable when cmd is an array (pre-GPT-load case).
  if (Array.isArray(queue)) {
    for (let i = 0; i < queue.length; i++) {
      queue[i] = wrapCommand(queue[i]);
    }
    log.debug('GPT shim: command queue patched', { pendingCommands: queue.length });
  } else {
    log.debug('GPT shim: command queue patched');
  }
}

/**
 * Install the GPT integration shim.
 *
 * Sets up the script guard for dynamic script interception and patches the
 * `googletag.cmd` command queue.
 */
export function installGptShim(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const win = window as GptWindow;

  // Install DOM interception guard first so any dynamic GPT script insertions
  // are rewritten before the browser fetches them.
  installGptGuard();

  const tag = ensureGoogleTagStub(win);
  patchCommandQueue(tag);

  log.info('GPT shim installed');
  return true;
}

// ------------------------------------------------------------------
// GAM interceptor (testing only)
// ------------------------------------------------------------------

/**
 * Sandbox token list for debug ADM fallback iframes.
 *
 * Deliberately excludes `allow-same-origin`: combined with `allow-scripts` on
 * srcdoc (or first-party src) content, that pair effectively removes the
 * sandbox's origin isolation and would let SSP-provided markup run with the
 * publisher origin's privileges.
 */
export const ADM_IFRAME_SANDBOX = 'allow-scripts allow-popups allow-forms';

/**
 * Resolve an ADM-extracted iframe src to a safe, loadable URL.
 *
 * Protocol-relative URLs are upgraded to https. Only http(s) URLs (including
 * page-relative paths, which resolve against the page origin) are accepted —
 * anything else (javascript:, data:, blob:, …) is rejected so SSP-provided
 * markup cannot smuggle a script-executing URL into the unsandboxed GAM
 * iframe.
 */
export function safeAdmIframeSrc(src: string): string | undefined {
  const resolved = src.startsWith('//') ? `https:${src}` : src;
  try {
    const parsed = new URL(resolved, window.location.href);
    if (parsed.protocol === 'https:' || parsed.protocol === 'http:') {
      return resolved;
    }
  } catch {
    // Unparseable URL — treat as unsafe.
  }
  return undefined;
}

/**
 * Replace the GAM-rendered creative with the server-side auction adm.
 *
 * Adapted from PR #241 (github.com/IABTechLab/trusted-server/pull/241).
 * Instead of reading from pbjs, reads adm directly from window.tsjs.bids.
 *
 * This is the testing-only direct-replace path that bypasses GAM entirely. The
 * sanitized `adm` now ships in production for the pbRender bridge, so `adm`
 * presence no longer gates it; the caller gates on the per-bid `debug_bid`
 * signal (present only under `inject_adm_for_testing`) instead.
 *
 * Strategy:
 * 1. If adm contains an <iframe src="..."> with a safe http(s) src, set that
 *    src on the GAM iframe directly — avoids cross-origin document access.
 * 2. Otherwise replace the slot element's content with a sandboxed srcdoc
 *    iframe (no `allow-same-origin` — see [ADM_IFRAME_SANDBOX]).
 */
function injectAdmIntoSlot(divId: string, adm: string): void {
  try {
    // divId may be the container div (used by GPT slot) or the inner div.
    // Resolve it the same way the rest of adInit does (exact then prefix) so
    // a config div_id prefix with a render-time suffix still finds the element.
    const slotEl = findSlotElementByDivId(divId);
    if (!slotEl) return;

    // Extract the first iframe src from the adm (e.g. mocktioneer creative
    // wraps a first-party proxy iframe in a div). Reject non-http(s) schemes.
    const srcMatch = adm.match(/<iframe[^>]+\bsrc=["']([^"']+)["']/i);
    const innerSrc = srcMatch?.[1] ? safeAdmIframeSrc(srcMatch[1]) : undefined;
    const gamIframe = slotEl.querySelector('iframe') as HTMLIFrameElement | null;

    if (innerSrc && gamIframe) {
      // Set the GAM iframe src — works even cross-origin (no document access needed).
      gamIframe.src = innerSrc;
      log.debug(`[tsjs-gpt] gam-intercept: set iframe src for '${divId}'`);
    } else if (innerSrc) {
      // GAM iframe not yet in DOM (APS renders async after slotRenderEnded).
      // Retry on next animation frame so APS has a tick to insert its iframe;
      // if it still isn't there, replace slot content directly.
      requestAnimationFrame(() => {
        const retryIframe = slotEl!.querySelector('iframe') as HTMLIFrameElement | null;
        if (retryIframe) {
          retryIframe.src = innerSrc;
          log.debug(`[tsjs-gpt] gam-intercept: set iframe src (retry) for '${divId}'`);
        } else {
          slotEl!.innerHTML = '';
          const f = document.createElement('iframe');
          f.style.cssText = 'border:none';
          f.width = String(slotEl!.offsetWidth || 728);
          f.height = String(slotEl!.offsetHeight || 90);
          f.setAttribute('sandbox', ADM_IFRAME_SANDBOX);
          f.src = innerSrc;
          slotEl!.appendChild(f);
          log.debug(`[tsjs-gpt] gam-intercept: inserted src iframe for '${divId}'`);
        }
      });
    } else {
      // No extractable safe src — replace slot content with a sandboxed srcdoc iframe.
      slotEl.innerHTML = '';
      const f = document.createElement('iframe');
      f.style.border = 'none';
      f.width = String(slotEl.offsetWidth || 728);
      f.height = String(slotEl.offsetHeight || 90);
      f.setAttribute('sandbox', ADM_IFRAME_SANDBOX);
      f.srcdoc = adm;
      slotEl.appendChild(f);
      log.debug(`[tsjs-gpt] gam-intercept: replaced slot content for '${divId}'`);
    }
  } catch (err) {
    log.warn('[tsjs-gpt] gam-intercept: error injecting adm', err);
  }
}

const MAX_BILLING_DEDUPE_KEYS = 512;
const BILLING_DEDUPE_TTL_MS = 30 * 60_000;
const firedBillingKeys = new Map<string, number>();

function billingEntries(slotId: string, bid: AuctionBidData): Array<[string, string]> {
  const bidIdentity = bid.hb_adid ?? bid.nurl ?? bid.burl ?? '';
  return (
    [
      ['nurl', bid.nurl],
      ['burl', bid.burl],
    ] as const
  ).flatMap(([kind, url]) =>
    url ? [[`${slotId}|${bidIdentity}|${kind}|${url}`, url] as [string, string]] : []
  );
}

function billingCapacityAvailable(slotId: string, bid: AuctionBidData): boolean {
  const now = monotonicNow();
  for (const [key, expiresAt] of firedBillingKeys) {
    if (expiresAt <= now) firedBillingKeys.delete(key);
  }
  const additional = billingEntries(slotId, bid).filter(
    ([key]) => !firedBillingKeys.has(key)
  ).length;
  return firedBillingKeys.size + additional <= MAX_BILLING_DEDUPE_KEYS;
}

function fireWinBillingBeacons(slotId: string, bid: AuctionBidData): void {
  if (!slotId) return;
  const now = monotonicNow();
  for (const [key, url] of billingEntries(slotId, bid)) {
    if (firedBillingKeys.has(key)) continue;
    if (queueWinBillingBeacon(url)) {
      firedBillingKeys.set(key, now + BILLING_DEDUPE_TTL_MS);
    }
  }
}

function queueWinBillingBeacon(url: string): boolean {
  if (typeof navigator !== 'undefined' && typeof navigator.sendBeacon === 'function') {
    try {
      if (navigator.sendBeacon(url)) {
        return true;
      }
    } catch (err) {
      log.warn('[tsjs-gpt] win/billing sendBeacon failed', err);
    }
  }

  if (typeof fetch === 'function') {
    try {
      void fetch(url, { method: 'POST', keepalive: true, mode: 'no-cors' });
      return true;
    } catch (err) {
      log.warn('[tsjs-gpt] win/billing fetch fallback failed', err);
    }
  }

  return false;
}

// ------------------------------------------------------------------
// Trusted Server ad-init
// ------------------------------------------------------------------

/**
 * Install `window.tsjs.adInit`.
 *
 * Reads `window.tsjs.adSlots` (injected at head-open) and `window.tsjs.bids`
 * (injected before </body>) synchronously — no fetch, no Promise. Applies bid
 * targeting to GPT slots, sets the `ts_initial` sentinel, then calls refresh().
 * Win/billing beacons fire from the TS render bridge, where a matching Prebid
 * Universal Creative request proves the TS creative actually rendered.
 *
 * Idempotent: destroys previously created TS-managed slots before redefining them,
 * so it is safe to call again after SPA navigation updates `tsjs.adSlots`/`tsjs.bids`.
 */
/**
 * Track whether the publisher disabled GPT initial load.
 *
 * GPT exposes no getter for the initial-load-disabled flag, so wrap both the
 * modern `googletag.setConfig({ disableInitialLoad: true })` API and the legacy
 * `pubads().disableInitialLoad()` method to record it on `window.tsjs`. With
 * initial load disabled, `display()` only registers a slot — the ad request
 * must come from a later `refresh()`. adInit() reads this to refresh its own
 * freshly defined slots so they are not left blank.
 *
 * Installed via the command queue so it runs before the publisher's own GPT
 * configuration (the TS core script is injected ahead of the publisher's GPT
 * setup). Idempotent per googletag object and pubads service.
 *
 * Only hooks an existing `googletag` stub — it never creates one. A plain module
 * import that does not activate the GPT integration must not touch
 * `window.googletag`. When the GPT shim is active it creates the stub before
 * `installTsAdInit` runs, so the detector is still queued ahead of the
 * publisher's GPT setup.
 */
function installInitialLoadDetector(ts: TsjsApi): void {
  const win = window as GptWindow;
  const cmd = win.googletag?.cmd;
  if (!cmd) return;
  cmd.push(() => {
    const gpt = win.googletag as
      | (Partial<GoogleTag> & { __tsInitialLoadConfigHooked?: boolean })
      | undefined;
    if (!gpt) return;

    if (typeof gpt.setConfig === 'function' && !gpt.__tsInitialLoadConfigHooked) {
      const originalSetConfig = gpt.setConfig.bind(gpt);
      gpt.setConfig = function (config: GoogleTagConfig) {
        if (config?.disableInitialLoad === true) {
          ts.gptInitialLoadDisabled = true;
        }
        return originalSetConfig(config);
      };
      gpt.__tsInitialLoadConfigHooked = true;
    }

    const pubads = gpt.pubads?.();
    if (!pubads) return;
    const service = pubads as GoogleTagPubAdsService & { __tsInitialLoadHooked?: boolean };
    if (typeof service.disableInitialLoad !== 'function' || service.__tsInitialLoadHooked) {
      return;
    }
    const originalDisableInitialLoad = service.disableInitialLoad.bind(service);
    service.disableInitialLoad = function () {
      ts.gptInitialLoadDisabled = true;
      return originalDisableInitialLoad();
    };
    service.__tsInitialLoadHooked = true;
  });
}

function slotIdForGptSlot(slot: GoogleTagSlot): string | undefined {
  const divId = slot.getSlotElementId?.() ?? '';
  return (
    window.tsjs?.divToSlotId?.[divId] ??
    window.tsjs?.adSlots?.find((item) => {
      return (
        divId === item.div_id ||
        divId === `${item.div_id}-container` ||
        divId.startsWith(item.div_id)
      );
    })?.id
  );
}

function firstSlotTarget(slot: GoogleTagSlot, key: string): string | undefined {
  return slot.getTargeting?.(key)?.find((value) => value.length > 0);
}

function supersedeCandidate(candidate: RenderCandidate, reason: string): void {
  if (candidate.superseded) return;
  candidate.superseded = true;
  for (const render of [...activeCacheRenders]) {
    if (render.candidate === candidate) retireActiveCacheRender(render);
  }
  window.tsjs?.recordAdTrace?.({
    kind: 'generation_superseded',
    slotId: candidate.slotId,
    generation: candidate.generation,
    bidTraceId: candidate.traceToken,
    reason,
  });
}

export function supersedeAdTraceSlot(slot: GoogleTagSlot, reason: string): void {
  const slotId = slotIdForGptSlot(slot);
  if (slotId) {
    abortActiveCacheRenders(slotId);
    if (window.tsjs?.prebidSelectedParticipants) {
      window.tsjs.prebidSelectedParticipants = window.tsjs.prebidSelectedParticipants.filter(
        (entry) => entry.slotId !== slotId
      );
    }
  }
  for (const candidates of requestCandidates.values()) {
    candidates
      .filter((candidate) => candidate.slot === slot && !candidate.superseded)
      .forEach((candidate) => supersedeCandidate(candidate, reason));
  }
}

/** Capture immutable attribution immediately before one concrete GPT request. */
export function captureAdTraceRequest(
  slot: GoogleTagSlot,
  trigger: string,
  snapshot?: AdTraceRequestBoundarySnapshot
): number {
  const ts = window.tsjs;
  const hasBoundarySnapshot = snapshot !== undefined;
  const slotId = hasBoundarySnapshot ? snapshot.slotId : slotIdForGptSlot(slot);
  if (!slotId) return 0;
  installSlotCacheInvalidationHook(slot);

  // Private service ownership is captured for every GPT request, even when the
  // diagnostic recorder is disabled. It must precede all asynchronous render
  // work so a later request or navigation can invalidate the exact owner.
  const bidder = hasBoundarySnapshot ? snapshot.bidder : firstSlotTarget(slot, 'hb_bidder');
  const adId = hasBoundarySnapshot ? snapshot.adId : firstSlotTarget(slot, 'hb_adid');
  const rawTraceToken = hasBoundarySnapshot
    ? snapshot.traceToken
    : firstSlotTarget(slot, 'ts_trace');
  const traceToken =
    rawTraceToken && TRACE_TOKEN_RE.test(rawTraceToken) ? rawTraceToken : undefined;
  const liveBid = hasBoundarySnapshot ? snapshot.bid : ts?.bids?.[slotId];
  const renderBidMatches =
    !!liveBid &&
    !!adId &&
    liveBid.hb_adid === adId &&
    (!traceToken || liveBid.trace?.bidTraceId === traceToken);
  const divId = slot.getSlotElementId?.() ?? '';
  const privateBid = renderBidMatches ? Object.freeze({ ...liveBid }) : undefined;
  const privateOwner = claimPrivateRequestOwner(
    slotId,
    adId,
    privateBid,
    divId ? findSlotElementByDivId(divId) : null
  );

  if (!ts?.recordAdTrace) return 0;
  (requestCandidates.get(slotId) ?? [])
    .filter((candidate) => !candidate.superseded && !candidate.consumed)
    .forEach((candidate) => supersedeCandidate(candidate, 'request_replaced'));
  const generation =
    ts.nextAdTraceGeneration?.(slotId) ?? (fallbackGenerations.get(slotId) ?? 0) + 1;
  privateOwner.generation = generation;
  fallbackGenerations.delete(slotId);
  fallbackGenerations.set(slotId, generation);
  while (fallbackGenerations.size > MAX_FALLBACK_GENERATIONS) {
    const oldest = fallbackGenerations.keys().next().value as string | undefined;
    if (!oldest) break;
    fallbackGenerations.delete(oldest);
  }

  // Diagnostic attribution reads the same immutable request-boundary values as
  // the private owner, but remains optional and independently gated.
  const ledger = ts.prebidCorrelation ?? [];
  const selectedMatches = traceToken
    ? ledger.filter((entry) => entry.slotId === slotId && entry.traceToken === traceToken)
    : adId
      ? ledger.filter((entry) => entry.slotId === slotId && entry.adId === adId)
      : [];
  const selectedParticipant = selectedMatches.length === 1 ? selectedMatches[0] : undefined;
  const completedAuction = [...(ts.prebidCompletedAuctions ?? [])]
    .reverse()
    .find((entry) => entry.slotIds.includes(slotId));
  const auctionId =
    selectedParticipant?.auctionId ?? (!adId ? completedAuction?.auctionId : undefined);
  const participants = auctionId
    ? ledger.filter((entry) => entry.slotId === slotId && entry.auctionId === auctionId)
    : [];
  const hasTracedTsParticipant = participants.some((entry) => !!entry.traceToken);
  const tracedServerParticipant = participants.find((entry) => entry.serverTrace);
  const serverSummary = auctionId
    ? (ts.prebidServerSummaries ?? []).find(
        (entry) => entry.auctionId === auctionId && entry.slotId === slotId
      )?.summary
    : undefined;
  if (selectedParticipant) {
    const selected = (ts.prebidSelectedParticipants ??= []).filter(
      (entry) => monotonicNow() - entry.selectedAt <= 30_000
    );
    selected.push({
      auctionId: selectedParticipant.auctionId,
      slotId,
      requestId: selectedParticipant.requestId,
      adId: selectedParticipant.adId,
      traceToken: selectedParticipant.traceToken,
      bidder: selectedParticipant.bidder,
      generation,
      selectedAt: monotonicNow(),
    });
    while (selected.length > 128) selected.shift();
    ts.prebidSelectedParticipants = selected;
  }
  if (auctionId) {
    ts.prebidCorrelation = ledger.filter(
      (entry) => !(entry.slotId === slotId && entry.auctionId === auctionId)
    );
    ts.prebidCompletedAuctions = (ts.prebidCompletedAuctions ?? []).filter(
      (entry) => entry.auctionId !== auctionId
    );
    ts.prebidServerSummaries = (ts.prebidServerSummaries ?? []).filter(
      (entry) => !(entry.auctionId === auctionId && entry.slotId === slotId)
    );
  }

  const candidate: RenderCandidate = {
    slotId,
    generation,
    slot,
    divId,
    ...(privateBid ? { bid: privateBid } : {}),
    adId,
    traceToken,
    createdAt: monotonicNow(),
    terminal: false,
    consumed: false,
    superseded: false,
  };
  const capturedElement = candidate.divId ? findSlotElementByDivId(candidate.divId) : null;
  if (capturedElement) ts.bindAdTraceElement?.(slotId, generation, capturedElement);
  if (!requestCandidates.has(slotId) && requestCandidates.size >= 64) {
    const oldestSlotId = requestCandidates.keys().next().value as string | undefined;
    if (oldestSlotId) {
      requestCandidates
        .get(oldestSlotId)
        ?.forEach((item) => supersedeCandidate(item, 'slot_evicted'));
      requestCandidates.delete(oldestSlotId);
    }
  }
  const candidates = requestCandidates.get(slotId) ?? [];
  candidates
    .filter((item) => !item.superseded && monotonicNow() - item.createdAt > 30_000)
    .forEach((item) => supersedeCandidate(item, 'generation_expired'));
  candidates.push(candidate);
  if (candidates.length > 8) {
    const evicted = candidates.shift();
    if (evicted) supersedeCandidate(evicted, 'generation_evicted');
  }
  requestCandidates.set(slotId, candidates);

  const serverTrace = tracedServerParticipant?.serverTrace;
  if (serverTrace) {
    ts.recordAdTrace({
      kind: 'ts_winner_observed',
      slotId,
      generation,
      auctionTraceId: serverTrace.auctionTraceId,
      bidTraceId: serverTrace.bidTraceId,
      provider: serverTrace.provider,
      bidder: serverTrace.bidder,
    });
  } else if (serverSummary) {
    ts.recordAdTrace({
      kind: 'ts_auction_observed',
      slotId,
      generation,
      auctionTraceId: serverSummary.auctionTraceId,
      outcome: terminalSummaryStageOutcome(serverSummary.outcome),
      confidence: 'definitive',
      reason: 'terminal_summary',
    });
  }

  let outcome = 'no_bid';
  let reason = 'no_selected_targeting';
  let confidence: 'definitive' | 'none' = 'definitive';
  if (selectedMatches.length > 1) {
    outcome = 'unresolved';
    reason = 'ambiguous_prebid_request';
    confidence = 'none';
  } else if (selectedParticipant) {
    if (traceToken && selectedParticipant.traceToken === traceToken) outcome = 'won';
    else if (!traceToken) outcome = hasTracedTsParticipant ? 'lost' : 'client_bid_won';
    else outcome = hasTracedTsParticipant ? 'lost' : 'unresolved';
    reason = 'selected_targeting';
  } else if (completedAuction && !bidder && !adId && !traceToken) {
    outcome = 'no_bid';
    reason = 'prebid_no_bid';
  } else if (bidder || adId || traceToken) {
    outcome = traceToken && renderBidMatches ? 'not_run' : 'client_bid_won';
    reason = traceToken && renderBidMatches ? 'direct_gpt_request' : 'unjoined_targeting';
    if (!traceToken && !renderBidMatches) confidence = 'none';
  }
  ts.recordAdTrace({
    kind: 'prebid_targeting_selected',
    slotId,
    generation,
    bidTraceId: traceToken,
    bidder,
    outcome,
    confidence,
    reason,
  });
  for (const kind of selectedParticipant?.events ?? []) {
    ts.recordAdTrace({
      kind,
      slotId,
      generation,
      bidTraceId: traceToken,
      bidder,
    });
  }
  if (liveBid?.hb_bidder === 'aps' || liveBid?.hb_bidder === 'amazon-aps') {
    ts.recordAdTrace({
      kind: 'aps_display_bids_set',
      slotId,
      generation,
      bidTraceId: traceToken,
    });
  }
  ts.recordAdTrace({
    kind: 'gpt_request_started',
    slotId,
    generation,
    auctionTraceId: liveBid?.trace?.auctionTraceId ?? ts.auctionTrace?.auctionTraceId,
    bidTraceId: traceToken,
    provider: liveBid?.trace?.provider,
    bidder,
    reason: trigger,
  });
  return generation;
}

function candidateForSlot(
  slot: GoogleTagSlot,
  includeTerminal = false
): RenderCandidate | undefined {
  const slotId = slotIdForGptSlot(slot);
  if (!slotId) return undefined;
  const candidates = (requestCandidates.get(slotId) ?? []).filter(
    (candidate) =>
      candidate.slot === slot &&
      !candidate.superseded &&
      (includeTerminal || !candidate.terminal) &&
      // Terminal evidence such as iframe load and viewability can arrive well
      // after the 30-second render-request window. Retain the exact terminal
      // candidate until a replacement, navigation, or bounded eviction supersedes it.
      (includeTerminal && candidate.terminal
        ? true
        : monotonicNow() - candidate.createdAt <= 30_000)
  );
  if (candidates.length !== 1) {
    if (candidates.length > 1) {
      candidates.forEach((candidate) =>
        window.tsjs?.recordAdTrace?.({
          kind: 'gpt_slot_render_ended',
          slotId,
          generation: candidate.generation,
          bidTraceId: candidate.traceToken,
          outcome: 'unresolved',
          confidence: 'none',
          reason: 'overlapping_request',
        })
      );
    } else {
      window.tsjs?.recordAdTrace?.({
        kind: 'gpt_slot_response_received',
        slotId,
        outcome: 'unresolved',
        confidence: 'none',
        reason: 'missing_generation',
      });
    }
    return undefined;
  }
  return candidates[0];
}

function installGptEvidenceListeners(service: GoogleTagPubAdsService): void {
  if (!window.tsjs?.recordAdTrace) return;
  const instrumented = service as GoogleTagPubAdsService & { __tsAdTraceListeners?: boolean };
  if (instrumented.__tsAdTraceListeners) return;
  instrumented.__tsAdTraceListeners = true;
  const record =
    (
      kind:
        | 'gpt_slot_requested'
        | 'gpt_slot_response_received'
        | 'gpt_slot_onload'
        | 'gpt_impression_viewable'
    ) =>
    (event: GptSlotEvent): void => {
      const candidate = candidateForSlot(
        event.slot,
        kind === 'gpt_slot_onload' || kind === 'gpt_impression_viewable'
      );
      if (!candidate) return;
      window.tsjs?.recordAdTrace?.({
        kind,
        slotId: candidate.slotId,
        generation: candidate.generation,
        bidTraceId: candidate.traceToken,
      });
    };
  service.addEventListener('slotRequested', record('gpt_slot_requested'));
  service.addEventListener('slotResponseReceived', record('gpt_slot_response_received'));
  service.addEventListener('slotOnload', record('gpt_slot_onload'));
  service.addEventListener('impressionViewable', record('gpt_impression_viewable'));
  service.addEventListener('slotRenderEnded', (event: GptSlotEvent) => {
    const candidate = candidateForSlot(event.slot);
    if (!candidate) return;
    candidate.terminal = true;
    window.tsjs?.recordAdTrace?.({
      kind: 'gpt_slot_render_ended',
      slotId: candidate.slotId,
      generation: candidate.generation,
      bidTraceId: candidate.traceToken,
      isEmpty: event.isEmpty,
      isBackfill: event.isBackfill,
    });
  });
}

export function installTsAdInit(): void {
  const ts = (window.tsjs ??= {} as TsjsApi);
  const pendingBootstrapRequests = ts.pendingAdTraceRequests ?? [];
  ts.pendingAdTraceRequests = [];
  ts.captureAdTraceRequest = (slot, trigger, snapshot) =>
    captureAdTraceRequest(slot as GoogleTagSlot, trigger, snapshot);
  pendingBootstrapRequests.forEach(({ slot, trigger, snapshot }) =>
    ts.captureAdTraceRequest?.(slot, trigger, snapshot)
  );
  installInitialLoadDetector(ts);
  ts.adInit = function () {
    const slots = ts.adSlots ?? [];
    // Snapshot bids at adInit() call time — correct for targeting setup.
    // The slotRenderEnded listener below reads ts.bids live so SPA navigation
    // updates (new ts.bids injected before </body>) are picked up at render time.
    const bids = ts.bids ?? {};
    const summary = ts.auctionTrace;
    for (const slot of slots) {
      const bid = bids[slot.id];
      if (bid?.trace && TRACE_TOKEN_RE.test(bid.trace.bidTraceId)) {
        ts.recordAdTrace?.({
          kind: 'ts_winner_observed',
          slotId: slot.id,
          auctionTraceId: bid.trace.auctionTraceId,
          bidTraceId: bid.trace.bidTraceId,
          provider: bid.trace.provider,
          bidder: bid.trace.bidder,
        });
      } else if (summary) {
        ts.recordAdTrace?.({
          kind: 'ts_auction_observed',
          slotId: slot.id,
          auctionTraceId: summary.auctionTraceId,
          outcome: terminalSummaryStageOutcome(summary.outcome),
          confidence: 'definitive',
          reason: 'terminal_summary',
        });
      }
    }
    const g = (window as GptWindow).googletag;
    if (!g) return;

    g.cmd?.push(() => {
      installGoogleTagCacheInvalidationHooks(g);
      // Destroy previously defined TS slots before redefining for the new page.
      if (ts.prevGptSlots && ts.prevGptSlots.length > 0) {
        (ts.prevGptSlots as GoogleTagSlot[]).forEach((slot) =>
          supersedeAdTraceSlot(slot, 'slot_destroyed')
        );
        g.destroySlots?.(ts.prevGptSlots as GoogleTagSlot[]);
        ts.prevGptSlots = [];
      }

      // Slots TS defined itself — tracked for SPA destroy. Publisher-owned
      // slots are reused but never destroyed by TS on navigation.
      installGptEvidenceListeners(g.pubads!());
      const newSlots: GoogleTagSlot[] = [];
      // Publisher-owned slots TS reused — refreshed to pick up server-side
      // targeting. The publisher already display()ed these.
      const slotsToRefresh: GoogleTagSlot[] = [];
      // Element IDs of slots TS defined itself this call. GPT requires a
      // display() call to register/render a freshly-defined slot; refresh()
      // alone no-ops for a slot that was never displayed, so these are
      // display()ed instead of refreshed.
      const slotsToDisplay: string[] = [];
      const divToSlotId: Record<string, string> = {};
      const prevSlotTargetingKeys = ts.prevSlotTargetingKeys ?? {};
      const nextSlotTargetingKeys: Record<string, string[]> = {};

      // Clear TS-managed targeting from every previously TS-touched GPT slot
      // before applying the current route. Without this sweep, navigating to a
      // route with no matching TS slots (or one where a previously touched
      // publisher-owned slot is absent from the new slot list) leaves stale
      // hb_* / ts_initial / route targeting that later publisher refreshes
      // would reuse.
      const prevTouchedDivIds = new Set([
        ...Object.keys(prevSlotTargetingKeys),
        ...Object.keys(ts.divToSlotId ?? {}),
      ]);
      if (prevTouchedDivIds.size > 0) {
        (g.pubads!().getSlots?.() ?? []).forEach((gptSlot: GoogleTagSlot) => {
          const elementId = gptSlot.getSlotElementId();
          if (!prevTouchedDivIds.has(elementId)) return;
          supersedeAdTraceSlot(gptSlot, 'targeting_cleared');
          clearTargetingKeys(gptSlot, [
            ...TS_BASE_TARGETING_KEYS,
            ...(prevSlotTargetingKeys[elementId] ?? []),
          ]);
        });
      }

      slots.forEach((slot) => {
        // Resolve actual div ID: exact match first, then prefix query.
        // div_id in config may be a stable prefix (e.g. "ad-header-0-") when
        // the suffix is dynamically generated by the framework at render time.
        const el = findSlotElementByDivId(slot.div_id);
        if (!el) return;
        const actualDivId = el.id;
        const bid = bids[slot.id] ?? {};

        const existingSlot = g.pubads!()
          .getSlots?.()
          ?.find?.((s: GoogleTagSlot) => s.getSlotElementId() === actualDivId);
        let gptSlot: GoogleTagSlot;
        let tsOwned = false;
        if (existingSlot) {
          gptSlot = existingSlot;
        } else {
          // Use outer container div for TS's slot when publisher hasn't defined
          // theirs yet — keeps both slots on separate divs so publisher's
          // later defineSlot on the inner div doesn't conflict.
          const containerEl = document.getElementById(`${actualDivId}-container`);
          const slotDivId = containerEl?.id ?? actualDivId;
          const defined = g.defineSlot?.(slot.gam_unit_path, slot.formats, slotDivId);
          if (!defined) return;
          defined.addService(g.pubads!());
          gptSlot = defined;
          tsOwned = true;
        }

        installSlotCacheInvalidationHook(gptSlot);
        const slotDivId2 = gptSlot.getSlotElementId?.() ?? actualDivId;
        clearTargetingKeys(gptSlot, [
          ...TS_BASE_TARGETING_KEYS,
          ...(prevSlotTargetingKeys[actualDivId] ?? []),
          ...(prevSlotTargetingKeys[slotDivId2] ?? []),
        ]);

        Object.entries(slot.targeting ?? {}).forEach(([k, v]) => gptSlot.setTargeting(k, v));
        TS_BID_TARGETING_KEYS.forEach((key) => {
          if (bid[key]) gptSlot.setTargeting(key, String(bid[key]!));
        });
        if (bid.trace?.bidTraceId && TRACE_TOKEN_RE.test(bid.trace.bidTraceId)) {
          gptSlot.setTargeting('ts_trace', bid.trace.bidTraceId);
        }
        gptSlot.setTargeting(TS_INITIAL_TARGETING_KEY, '1');
        ts.recordAdTrace?.({
          kind: 'gpt_targeting_applied',
          slotId: slot.id,
          auctionTraceId: bid.trace?.auctionTraceId,
          bidTraceId: bid.trace?.bidTraceId,
          provider: bid.trace?.provider,
          bidder: bid.trace?.bidder,
        });
        // Map both inner div and container div → slot ID so slotRenderEnded
        // (which reports the GPT slot's div, i.e. slotDivId/container) can look up
        // the slot, while adm injection (which targets the inner div) also works.
        divToSlotId[actualDivId] = slot.id;
        if (slotDivId2 !== actualDivId) divToSlotId[slotDivId2] = slot.id;
        const slotTargetingKeys = Object.keys(slot.targeting ?? {});
        nextSlotTargetingKeys[actualDivId] = slotTargetingKeys;
        if (slotDivId2 !== actualDivId) nextSlotTargetingKeys[slotDivId2] = slotTargetingKeys;
        if (tsOwned) {
          newSlots.push(gptSlot);
          slotsToDisplay.push(slotDivId2);
        } else {
          slotsToRefresh.push(gptSlot);
        }

        // Typed Trusted Server APS winners render through their own descriptor.
        // Only publisher-native APS bids should enter the apstag handoff.
        if (
          bid.renderer === undefined &&
          (bid.hb_bidder === 'aps' || bid.hb_bidder === 'amazon-aps')
        ) {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (window as any).apstag?.setDisplayBids?.();
          ts.recordAdTrace?.({
            kind: 'aps_display_bids_set',
            slotId: slot.id,
            bidTraceId: bid.trace?.bidTraceId,
          });
        }
      });

      ts.prevGptSlots = newSlots as unknown[];
      // Replace (not merge) so destroyed slots from previous navigation don't linger.
      ts.divToSlotId = divToSlotId;
      ts.prevSlotTargetingKeys = nextSlotTargetingKeys;

      // Whether this call produced any TS slot to render. A gated page-bids
      // response (auction kill switch or consent denial) returns no slots, so
      // the loops above leave these empty.
      const hasRenderableWork = slotsToDisplay.length > 0 || slotsToRefresh.length > 0;

      // enableSingleRequest and enableServices must only be called once per page
      // load. Skip activating GPT services when TS has nothing to display or
      // refresh and has not already enabled them: a consent-denied or
      // kill-switched navigation must not turn on the publisher's GPT services
      // or race their own setup. The targeting sweep above still runs so stale
      // TS targeting from a prior navigation is cleared.
      if (!ts.servicesEnabled && hasRenderableWork) {
        g.pubads!().enableSingleRequest();
        g.enableServices?.();
        ts.servicesEnabled = true;

        g.pubads!().addEventListener?.('slotRenderEnded', (event: SlotRenderEndedEvent) => {
          const divId: string = event.slot?.getSlotElementId?.() ?? '';
          const slotId = (ts.divToSlotId ?? {})[divId];
          if (!slotId) return;
          // Read ts.bids live (not the snapshot above) so post-navigation bid data is used.
          const bid = (ts.bids ?? {})[slotId] ?? {};

          // GAM interceptor (testing bypass): directly replace the GAM creative.
          // `adm` is now always injected in production, so it can no longer gate
          // this path. `debug_bid` is present only when inject_adm_for_testing is
          // on, so it is the per-bid signal that the testing bypass is enabled.
          // In production the render bridge serves the creative and GAM stays in
          // the loop; this direct replace stays testing-only.
          if (bid.adm && bid.debug_bid) {
            injectAdmIntoSlot(divId, bid.adm);
          }
        });
      }

      // Register and render TS-defined slots. GPT requires display() for a
      // freshly-defined slot — without it the slot no-ops ("defineSlot was
      // called without a matching display call") and misses its impression.
      // Must run after enableServices(); on SPA navigation services are already
      // enabled, so this runs unconditionally for any newly-defined slots.
      slotsToDisplay.forEach((divId) => {
        const gptSlot = newSlots.find((slot) => slot.getSlotElementId() === divId);
        if (gptSlot && !ts.gptInitialLoadDisabled) captureAdTraceRequest(gptSlot, 'display');
        g.display?.(divId);
      });

      // Slots needing an explicit ad request via refresh(). Reused
      // publisher-owned slots always need one to pick up the just-applied
      // server-side targeting. TS-defined slots are normally fetched by the
      // display() above — but when the publisher disabled initial load through
      // setConfig() or the legacy pubads() method, display() only registers the
      // slot and the ad request must come from refresh(). Without this, a TS-owned
      // first-impression slot renders blank on initial-load-disabled pages. Only
      // add them in that case; otherwise display() + refresh() would
      // double-request the impression.
      const slotsNeedingRefresh = ts.gptInitialLoadDisabled
        ? slotsToRefresh.concat(newSlots)
        : slotsToRefresh;

      if (slotsNeedingRefresh.length > 0) {
        // One-shot bypass: this internal refresh delivers the just-applied
        // server-side targeting to GAM. If slim-Prebid has wrapped refresh(), it
        // must pass this call straight through — not clear the targeting and run
        // a duplicate client-side auction. Later publisher-initiated refreshes of
        // the same slots still go through the wrapper normally.
        ts.adInitRefreshInProgress = true;
        try {
          slotsNeedingRefresh.forEach((slot) => captureAdTraceRequest(slot, 'refresh'));
          g.pubads!().refresh(slotsNeedingRefresh);
        } finally {
          ts.adInitRefreshInProgress = false;
        }
      }
    });
  };
}

interface PageBidsResponse {
  auctionTrace?: AuctionTraceSummary;
  slots: AuctionSlot[];
  bids: Record<string, AuctionBidData>;
}

/**
 * Upper bound (ms) on how long the SPA hook waits for a route's ad containers
 * to appear before applying bids anyway.
 */
const SPA_SLOT_WAIT_MS = 2000;

/**
 * Resolve once every configured `slots` entry has a container element in the DOM, or
 * after `SPA_SLOT_WAIT_MS`, whichever comes first.
 *
 * Many SPA routers update `history` before the new route's markup commits. If
 * bids were applied immediately, `adInit()` would look up each slot element
 * once and silently skip every not-yet-rendered slot, dropping that route's
 * server-side bids with no retry. Waiting via `MutationObserver` lets the apply
 * step run as soon as the route's full slot set exists; the timeout guarantees
 * a slot that never renders cannot hang the hook (the subsequent `adInit()`
 * skips missing elements exactly as before). Resolves immediately when there is
 * nothing to wait for, or when `MutationObserver` is unavailable.
 */
function waitForSlotElements(slots: AuctionSlot[], signal: AbortSignal): Promise<void> {
  // A newer navigation may have aborted this signal before we were called; skip
  // installing an observer/timer that the stale run would only tear down.
  if (signal.aborted) return Promise.resolve();
  const allPresent = (): boolean => slots.every((slot) => !!findSlotElementByDivId(slot.div_id));
  if (slots.length === 0 || allPresent() || typeof MutationObserver === 'undefined') {
    return Promise.resolve();
  }

  return new Promise<void>((resolve) => {
    let settled = false;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      observer.disconnect();
      clearTimeout(timer);
      signal.removeEventListener('abort', finish);
      resolve();
    };
    const observer = new MutationObserver(() => {
      if (allPresent()) finish();
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
    const timer = setTimeout(finish, SPA_SLOT_WAIT_MS);
    signal.addEventListener('abort', finish);
  });
}

/**
 * Install SPA navigation hook.
 *
 * Patches `history.pushState` and `history.replaceState`, and listens to
 * `popstate`, so that after each client-side route change the trusted server
 * fetches fresh slots + bids from `/__ts/page-bids?path=<new_path>`, updates
 * `window.tsjs.adSlots` / `window.tsjs.bids`, and calls `window.tsjs.adInit()`.
 *
 * Idempotent: guarded by `window.tsjs.spaHookInstalled` so multiple calls are safe.
 */
export function installSpaAuctionHook(): void {
  if (typeof window === 'undefined') return;
  const ts = (window.tsjs ??= {} as TsjsApi);
  if (ts.spaHookInstalled) return;
  ts.spaHookInstalled = true;

  let inflight: AbortController | null = null;
  // Last path and query an auction was run for. popstate fires for hash-only
  // changes and pushState/replaceState can be called with the current URL, so
  // guard every entry point against re-requesting impressions already loaded.
  let currentPath = `${location.pathname}${location.search}`;
  // Last path whose slots/bids were actually applied — the initial SSR page
  // counts. A failed navigation rolls `currentPath` back to this rather than to
  // the immediately-previous committed value: on rapid A→B where A was aborted
  // mid-flight and B then fails, rolling back to A (never loaded) would strand
  // it behind the no-op guard, so we roll back to the last applied route instead.
  let lastAppliedPath = `${location.pathname}${location.search}`;

  async function onNavigate(path: string): Promise<void> {
    // Navigation invalidates private render ownership even when the resulting
    // route key is unchanged (for example a state-only replaceState call).
    abortActiveCacheRenders();
    if (path === currentPath) return;
    ts.prebidSelectedParticipants = [];
    currentPath = path;
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;

    try {
      const res = await fetch(`/__ts/page-bids?path=${encodeURIComponent(path)}`, {
        credentials: 'include',
        // Non-simple header doubles as a CSRF token: the server rejects
        // requests that carry neither same-origin Fetch Metadata nor this
        // header, and cross-origin pages cannot send it without a CORS
        // preflight the endpoint never grants.
        headers: { 'X-TSJS-Page-Bids': '1' },
        signal: controller.signal,
      });
      if (!res.ok) {
        // A transient page-bids failure must not strand this route: roll the
        // committed path back so a later navigation here retries instead of
        // being skipped by the no-op guard at the top. Only roll back when no
        // newer navigation has already advanced currentPath.
        if (inflight === controller) currentPath = lastAppliedPath;
        return;
      }
      const data = (await res.json()) as PageBidsResponse;
      if (inflight !== controller) return;
      // Defer applying bids until the new route's ad containers exist, so a
      // fast edge response cannot beat the DOM and drop server-side bids.
      await waitForSlotElements(data.slots, controller.signal);
      if (inflight !== controller) return;
      ts.adSlots = data.slots;
      ts.auctionTrace = data.auctionTrace;
      ts.bids = data.bids;
      // This route is now the committed, loaded state — a later failed
      // navigation rolls back here, and a return trip no-ops correctly.
      lastAppliedPath = path;
      // An empty page-bids response (auction kill switch or consent gate) carries
      // no TS slots. Only run adInit() when there are slots to apply or prior TS
      // state to sweep — otherwise a consent-denied or kill-switched navigation
      // must not enter the GPT command queue and risk activating services.
      const hasPriorTsState =
        (ts.prevGptSlots?.length ?? 0) > 0 ||
        Object.keys(ts.prevSlotTargetingKeys ?? {}).length > 0 ||
        Object.keys(ts.divToSlotId ?? {}).length > 0;
      if (data.slots.length > 0 || hasPriorTsState) {
        ts.adInit?.();
      }
    } catch (err) {
      if (err instanceof DOMException && err.name === 'AbortError') return;
      if (inflight === controller) currentPath = lastAppliedPath;
      log.warn('SPA auction hook: fetch failed', err);
    }
  }

  function patchHistoryMethod(method: 'pushState' | 'replaceState'): void {
    const original = history[method].bind(history);
    history[method] = function (state: unknown, unused: string, url?: string | URL | null): void {
      original(state, unused, url);
      const locationUrl = url ? new URL(String(url), location.href) : location;
      const newPath = `${locationUrl.pathname}${locationUrl.search}`;
      // onNavigate no-ops when newPath equals the last loaded path and query.
      void onNavigate(newPath);
    };
  }

  patchHistoryMethod('pushState');
  patchHistoryMethod('replaceState');

  window.addEventListener('popstate', () => {
    void onNavigate(`${location.pathname}${location.search}`);
  });
}

/**
 * Register the slim-Prebid lazy loader. Fires after window.load — off the
 * critical path. Slim-Prebid handles scroll/refresh auctions and userID
 * module warm-up (ID5, sharedID, LiveRamp ATS, Lockr).
 *
 * Phase 1: no-op unless `window.__tsjs_slim_prebid_url` is set (the slim
 * bundle build target ships in a later phase).
 */
export function installSlimPrebidLoader(): void {
  if (typeof window === 'undefined') return;
  const url = (window as GptWindow).__tsjs_slim_prebid_url;
  if (!url) return;
  window.addEventListener('load', () => {
    const script = document.createElement('script');
    script.src = url;
    script.defer = true;
    document.head.appendChild(script);
  });
}

/** Minimal display renderer injected into the ad iframe by pbRender. */
const TS_DISPLAY_RENDERER =
  '(function(){window.render=function(d,h,w){' +
  'var f=h.mkFrame(w.document,{width:d.width||"100%",height:d.height||"100%"});' +
  'if(typeof d.traceToken==="string"){f.addEventListener("load",function(){' +
  'top.postMessage({type:"ts-creative-load",version:1,traceToken:d.traceToken},"*");},{once:true});}' +
  'if(d.adUrl&&!d.ad){f.src=d.adUrl;}else{f.srcdoc=d.ad;}' +
  'w.document.body.appendChild(f);};})();';

/** The clear-price auction macro DSPs embed in creative markup and tracking URLs. */
const AUCTION_PRICE_MACRO = '${AUCTION_PRICE}';

/**
 * Substitute the `${AUCTION_PRICE}` macro with a clearing price. Mirrors the
 * server-side `expand_auction_price_macro`: only the exact clear-price token is
 * replaced, so the encrypted `${AUCTION_PRICE:B64}` variant is left intact.
 */
function expandAuctionPriceMacro(markup: string, cpm: number): string {
  return markup.includes(AUCTION_PRICE_MACRO)
    ? markup.split(AUCTION_PRICE_MACRO).join(String(cpm))
    : markup;
}

/** A decoded PBS Cache bid: the renderable creative plus its render metadata. */
export interface CachedBid {
  adm: string;
  width?: number;
  height?: number;
  price?: number;
}

/**
 * Decode a PBS Cache GET response into a renderable bid.
 *
 * Prebid Cache entries are JSON bid objects (`{ "adm": "<div…>", "w": …, … }`);
 * the Prebid Universal Creative's own cache path `JSON.parse`s the response and
 * renders `bidObject.adm`, sizing from the cached dimensions. This mirrors that,
 * retaining the fields the fallback render needs — creative dimensions (`w`/`h`
 * or `width`/`height`) and clearing `price` for macro expansion — rather than
 * reducing the payload to a bare `adm` string that forces the first slot format
 * and leaves price macros unresolved.
 *
 * A non-JSON body is treated as raw creative markup (the `{ adm }`-only variant)
 * for backward compatibility with caches that store the creative directly.
 * Returns `undefined` when the JSON payload carries no usable string `adm`, so
 * the caller can decline to render instead of injecting a serialized object.
 */
export function parseCachedBid(body: string): CachedBid | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    // Not JSON — a cache that returned the creative markup directly. No render
    // metadata is available, so only the raw markup variant is returned.
    return body.trim().length > 0 ? { adm: body } : undefined;
  }
  if (!parsed || typeof parsed !== 'object') {
    // A JSON primitive (string/number/bool) is not a valid cached bid object.
    return undefined;
  }
  const obj = parsed as Record<string, unknown>;
  const adm = obj.adm;
  if (typeof adm !== 'string' || adm.length === 0) return undefined;

  const num = (v: unknown): number | undefined =>
    typeof v === 'number' && Number.isFinite(v) ? v : undefined;
  // A zero (or missing) dimension is not usable render metadata; treat it as
  // absent so the caller falls back to the slot format rather than sizing to 0.
  const dim = (v: unknown): number | undefined => {
    const n = num(v);
    return n !== undefined && n > 0 ? n : undefined;
  };

  return {
    adm,
    // PBS OpenRTB bids carry w/h; the Prebid.js cache format uses width/height.
    width: dim(obj.w) ?? dim(obj.width),
    height: dim(obj.h) ?? dim(obj.height),
    price: num(obj.price),
  };
}

const TRACE_TOKEN_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function pruneExpectedRenders(): void {
  const now = monotonicNow();
  for (const [token, entries] of expectedRenders) {
    const retained = entries.filter((entry) => !entry.consumed && entry.expiresAt >= now);
    if (retained.length > 0) expectedRenders.set(token, retained);
    else expectedRenders.delete(token);
  }
}

function armExpectedRender(
  candidate: RenderCandidate | undefined,
  source: MessageEventSource | null
): string | undefined {
  if (
    !candidate?.bid ||
    candidate.superseded ||
    !candidate.traceToken ||
    !TRACE_TOKEN_RE.test(candidate.traceToken) ||
    !source
  ) {
    return undefined;
  }
  pruneExpectedRenders();
  const expectedCount = [...expectedRenders.values()].reduce(
    (count, entries) => count + entries.length,
    0
  );
  if (expectedCount >= MAX_EXPECTED_RENDERS) return undefined;
  candidate.consumed = true;
  const entries = expectedRenders.get(candidate.traceToken) ?? [];
  entries.push({
    candidate,
    source,
    expiresAt: monotonicNow() + 30_000,
    consumed: false,
  });
  expectedRenders.set(candidate.traceToken, entries);
  return candidate.traceToken;
}

/**
 * Install the TS → pbRender bridge.
 *
 * Must be installed synchronously at module init — before `adInit()` fires
 * `refresh()`, which triggers GAM to serve the Prebid creative. Installing
 * post-load would miss first-impression `"Prebid Request"` messages.
 *
 * When `adId` matches a TS server-side bid in `window.tsjs.bids` AND the bid
 * has renderable markup, the bridge:
 *   1. Uses the inline `adm` directly when present (the sanitized winning
 *      creative, now shipped in production), otherwise fetches from PBS Cache
 *      and extracts `adm` from the cached bid JSON (see `extractCachedAdm`).
 *   2. Replies via the MessageChannel port with a `"Prebid Response"`.
 *   3. Calls `stopImmediatePropagation()` so Prebid.js does not also process
 *      the message and log spurious failures.
 *
 * Lives in gpt/index.ts (not prebid/index.ts) to avoid pulling the full
 * Prebid bundle into tsjs-gpt.js via inlineDynamicImports.
 */
export function installTsRenderBridge(): void {
  if (typeof window === 'undefined') return;

  // `slotId|adId` renders whose PBS Cache fetch is in flight. `fireWinBillingBeacons`
  // only dedups after the async fetch resolves, so two Prebid Request messages for
  // the same render arriving before the first fetch settles would both fetch and
  // both fire the nurl/burl beacons. Tracking the in-flight render prevents the
  // concurrent double-fire; the entry is cleared once the fetch settles. The key
  // is scoped to the slot, not the bare adId: hb_adid is not unique per bid, so
  // keying on it alone would let one slot block a distinct slot's render.
  const renderingKeys = new Set<string>();
  const consumedPrebidApsIds = new Map<string, { adUnitCode: string; expiresAt: number }>();
  const rememberConsumedPrebidApsId = (
    adId: string,
    entry: { adUnitCode: string; expiresAt: number }
  ): void => {
    const now = Date.now();
    for (const [consumedAdId, consumed] of consumedPrebidApsIds) {
      if (consumed.expiresAt <= now) consumedPrebidApsIds.delete(consumedAdId);
    }
    if (!consumedPrebidApsIds.has(adId) && consumedPrebidApsIds.size >= 256) {
      const oldestAdId = consumedPrebidApsIds.keys().next().value as string | undefined;
      if (oldestAdId) consumedPrebidApsIds.delete(oldestAdId);
    }
    consumedPrebidApsIds.set(adId, entry);
  };

  window.addEventListener('message', (e: MessageEvent) => {
    let data: Record<string, unknown>;
    try {
      data =
        typeof e.data === 'object'
          ? (e.data as Record<string, unknown>)
          : (JSON.parse(e.data as string) as Record<string, unknown>);
    } catch {
      return;
    }

    if (data['type'] === 'ts-creative-load') {
      const token = data['traceToken'];
      if (data['version'] !== 1 || typeof token !== 'string' || !TRACE_TOKEN_RE.test(token)) return;
      const entries = expectedRenders.get(token) ?? [];
      entries
        .filter((entry) => !entry.consumed && entry.expiresAt < monotonicNow())
        .forEach((entry) => supersedeCandidate(entry.candidate, 'ack_expired'));
      const matches = entries.filter(
        (entry) =>
          !entry.consumed &&
          !entry.candidate.superseded &&
          entry.expiresAt >= monotonicNow() &&
          entry.source === e.source &&
          (requestCandidates.get(entry.candidate.slotId) ?? []).includes(entry.candidate)
      );
      if (matches.length !== 1) {
        const candidate = entries[0]?.candidate;
        window.tsjs?.recordAdTrace?.({
          kind: 'pb_render_rejected',
          slotId: candidate?.slotId,
          generation: candidate?.generation,
          bidTraceId: TRACE_TOKEN_RE.test(token) ? token : undefined,
          reason: matches.length > 1 ? 'ambiguous_generation' : 'invalid_acknowledgement',
        });
        return;
      }
      const expected = matches[0];
      expected.consumed = true;
      window.tsjs?.recordAdTrace?.({
        kind: 'creative_load_acknowledged',
        slotId: expected.candidate.slotId,
        generation: expected.candidate.generation,
        bidTraceId: token,
      });
      return;
    }

    if (data['message'] !== 'Prebid Request') return;
    const adId = data['adId'] as string | undefined;
    if (!adId) return;

    const port = e.ports?.[0];
    if (!port) return;

    const consumedPrebidAps = consumedPrebidApsIds.get(adId);
    if (consumedPrebidAps) {
      if (consumedPrebidAps.expiresAt <= Date.now()) {
        consumedPrebidApsIds.delete(adId);
      } else {
        if (messageSourceBelongsToAdUnit(e.source, consumedPrebidAps.adUnitCode)) {
          e.stopImmediatePropagation();
        }
        return;
      }
    }

    // Client-side trustedServer adapter bids receive a new adId from Prebid. Bind
    // that ID to the server-validated APS descriptor only after confirming the
    // requesting Universal Creative frame belongs to the same GPT ad unit.
    const prebidRendererEntry = getApsPrebidRenderer(adId);
    if (prebidRendererEntry) {
      if (!messageSourceBelongsToAdUnit(e.source, prebidRendererEntry.adUnitCode)) return;
      const renderer = validateApsRenderer(prebidRendererEntry.renderer);
      const rendererUrl = apsRendererUrl();
      if (!renderer || !rendererUrl) return;
      if (!consumeApsPrebidRenderer(adId, prebidRendererEntry)) return;
      rememberConsumedPrebidApsId(adId, {
        adUnitCode: prebidRendererEntry.adUnitCode,
        expiresAt: prebidRendererEntry.expiresAt,
      });

      e.stopImmediatePropagation();
      try {
        prebidRendererEntry.markWinner();
      } catch (err) {
        log.warn('[tsjs-gpt] pbRender bridge: Prebid APS winner lifecycle failed', err);
        return;
      }

      try {
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            renderer: APS_UNIVERSAL_CREATIVE_RENDERER,
            rendererVersion: APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
            rendererUrl,
            apsRenderer: renderer,
            width: renderer.width,
            height: renderer.height,
          })
        );
      } catch (err) {
        log.warn('[tsjs-gpt] pbRender bridge: Prebid APS response failed', err);
        return;
      }

      try {
        prebidRendererEntry.markRendered();
      } catch (err) {
        log.warn('[tsjs-gpt] pbRender bridge: Prebid APS rendered lifecycle failed', err);
      }
      log.debug(
        `[tsjs-gpt] pbRender bridge served Prebid APS bid for '${prebidRendererEntry.adUnitCode}'`
      );
      return;
    }

    const sourceSlotId = slotIdForMessageSource(e.source);
    if (!sourceSlotId) return;

    const allCandidates = requestCandidates.get(sourceSlotId) ?? [];
    allCandidates
      .filter((candidate) => !candidate.superseded && monotonicNow() - candidate.createdAt > 30_000)
      .forEach((candidate) => supersedeCandidate(candidate, 'generation_expired'));
    const candidates = allCandidates.filter(
      (candidate) =>
        candidate.adId === adId &&
        !candidate.consumed &&
        !candidate.superseded &&
        monotonicNow() - candidate.createdAt <= 30_000
    );
    const exactCandidate = candidates.length === 1 ? candidates[0] : undefined;
    window.tsjs?.recordAdTrace?.({
      kind: candidates.length === 1 ? 'pb_render_requested' : 'pb_render_rejected',
      slotId: sourceSlotId,
      generation: exactCandidate?.generation,
      bidTraceId: exactCandidate?.traceToken,
      reason:
        candidates.length === 1
          ? 'exact_generation'
          : candidates.length > 1
            ? 'ambiguous_generation'
            : 'missing_generation',
    });

    const slotId = sourceSlotId;
    const requestOwner = latestPrivateRequestBySlot.get(slotId);
    const liveBid = window.tsjs?.bids?.[slotId];
    const ownerCurrent =
      !!requestOwner?.bid &&
      requestOwner.adId === adId &&
      requestOwner.bid.hb_adid === adId &&
      requestOwner.navigationGeneration === privateNavigationGeneration &&
      requestOwner.expiresAt >= monotonicNow() &&
      !requestOwner.served &&
      !!requestOwner.element?.isConnected &&
      findSlotElementByDivId(requestOwner.element.id) === requestOwner.element &&
      slotIdForMessageSource(e.source) === slotId &&
      liveBid?.hb_adid === requestOwner.bid.hb_adid &&
      liveBid.hb_cache_host === requestOwner.bid.hb_cache_host &&
      liveBid.hb_cache_path === requestOwner.bid.hb_cache_path &&
      liveBid.trace?.bidTraceId === requestOwner.bid.trace?.bidTraceId;
    if (!ownerCurrent || !requestOwner?.bid) {
      // A once-TS-owned message must not escape to ordinary Prebid after its
      // request owner was replaced or invalidated.
      if (isKnownStaleTsAdId(adId) || liveBid?.hb_adid === adId) {
        e.stopImmediatePropagation();
      }
      return;
    }
    const matchedBid = requestOwner.bid;

    const slot = window.tsjs?.adSlots?.find((s) => s.id === slotId);
    // Prefer the winning creative's own dimensions; the first configured slot
    // format is only a fallback and mis-sizes a multi-size slot whose winner is
    // not the first format.
    const [fallbackWidth, fallbackHeight] = slot?.formats?.[0] ?? [728, 90];
    const width = matchedBid.w ?? fallbackWidth;
    const height = matchedBid.h ?? fallbackHeight;

    if (matchedBid.renderer !== undefined) {
      const renderer = validateApsRenderer(matchedBid.renderer);
      const rendererUrl = apsRendererUrl();
      if (!renderer || !rendererUrl) return;

      // Ownership and the complete consumed envelope are valid before this
      // handler claims the message or suppresses another legitimate handler.
      e.stopImmediatePropagation();
      const rendererKey = `${slotId}|${adId}`;
      if (renderingKeys.has(rendererKey)) return;
      renderingKeys.add(rendererKey);
      requestOwner.served = true;

      try {
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            renderer: APS_UNIVERSAL_CREATIVE_RENDERER,
            rendererVersion: APS_UNIVERSAL_CREATIVE_RENDERER_VERSION,
            rendererUrl,
            apsRenderer: renderer,
            width: renderer.width,
            height: renderer.height,
          })
        );
        window.tsjs?.recordAdTrace?.({
          kind: 'pb_render_served',
          slotId,
          generation: exactCandidate?.generation,
          bidTraceId: exactCandidate?.traceToken,
          reason: 'aps_renderer',
        });
        log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' through APS renderer`);
      } catch (err) {
        requestOwner.served = false;
        renderingKeys.delete(rendererKey);
        log.warn('[tsjs-gpt] pbRender bridge: APS response failed', err);
      }
      return;
    }

    if (matchedBid.adm) {
      if (!billingCapacityAvailable(slotId, matchedBid)) return;
      const traceToken = armExpectedRender(exactCandidate, e.source);
      requestOwner.served = true;
      e.stopImmediatePropagation();
      port.postMessage(
        JSON.stringify({
          message: 'Prebid Response',
          adId,
          ad: matchedBid.adm,
          renderer: TS_DISPLAY_RENDERER,
          width,
          height,
          ...(traceToken ? { traceToken } : {}),
        })
      );
      fireWinBillingBeacons(slotId, matchedBid);
      window.tsjs?.recordAdTrace?.({
        kind: 'pb_render_served',
        slotId,
        generation: exactCandidate?.generation,
        bidTraceId: traceToken,
      });
      log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from inline adm`);
      return;
    }

    // No TS render source — let Prebid.js handle it.
    if (!matchedBid.hb_cache_host || !matchedBid.hb_cache_path) return;

    const capturedSource = e.source;
    const capturedElement = requestOwner.element;
    const capturedCacheHost = matchedBid.hb_cache_host;
    const capturedCachePath = matchedBid.hb_cache_path;
    const capturedTraceToken = matchedBid.trace?.bidTraceId;

    const previousOwner = latestCacheRenderBySlot.get(slotId);
    if (
      previousOwner &&
      previousOwner.adId === adId &&
      previousOwner.source === capturedSource &&
      previousOwner.generation === requestOwner.generation &&
      previousOwner.cacheHost === capturedCacheHost &&
      previousOwner.cachePath === capturedCachePath &&
      previousOwner.traceToken === capturedTraceToken &&
      !previousOwner.controller.signal.aborted &&
      previousOwner.expiresAt >= monotonicNow()
    ) {
      // A duplicate message for the exact accepted owner must not start a
      // second fetch or escape to the ordinary Prebid renderer.
      e.stopImmediatePropagation();
      return;
    }
    if (previousOwner) retireActiveCacheRender(previousOwner);

    // Capacity overflow must not evict a different live billing owner. Leave
    // the message untouched so the ordinary Prebid path can process it.
    if (activeCacheRenders.size >= MAX_ACTIVE_CACHE_RENDERS) return;

    const controller = new AbortController();
    const activeRender: ActiveCacheRender = {
      controller,
      slotId,
      adId,
      source: capturedSource,
      generation: requestOwner.generation,
      ...(exactCandidate?.generation === requestOwner.generation
        ? { candidate: exactCandidate }
        : {}),
      cacheHost: capturedCacheHost,
      cachePath: capturedCachePath,
      traceToken: capturedTraceToken,
      navigationGeneration: privateNavigationGeneration,
      expiresAt: requestOwner.expiresAt,
    };

    activeCacheRenders.add(activeRender);
    latestCacheRenderBySlot.set(slotId, activeRender);
    activeRender.expiryTimer = setTimeout(
      () => retireActiveCacheRender(activeRender),
      Math.max(0, activeRender.expiresAt - monotonicNow())
    );
    // TS owns this accepted render — stop Prebid from also processing it.
    e.stopImmediatePropagation();

    const stillCurrent = (): boolean => {
      const liveBid = window.tsjs?.bids?.[slotId];
      const candidateCurrent =
        !exactCandidate ||
        (!exactCandidate.superseded &&
          (requestCandidates.get(slotId) ?? []).includes(exactCandidate));
      return (
        !controller.signal.aborted &&
        latestCacheRenderBySlot.get(slotId) === activeRender &&
        latestPrivateRequestBySlot.get(slotId) === requestOwner &&
        requestOwner.navigationGeneration === privateNavigationGeneration &&
        requestOwner.expiresAt >= monotonicNow() &&
        !requestOwner.served &&
        activeRender.navigationGeneration === privateNavigationGeneration &&
        activeRender.expiresAt >= monotonicNow() &&
        candidateCurrent &&
        !!capturedElement?.isConnected &&
        findSlotElementByDivId(capturedElement.id) === capturedElement &&
        slotIdForMessageSource(capturedSource) === slotId &&
        liveBid?.hb_adid === adId &&
        liveBid.hb_cache_host === capturedCacheHost &&
        liveBid.hb_cache_path === capturedCachePath &&
        liveBid.trace?.bidTraceId === capturedTraceToken
      );
    };

    const cacheUrl = `https://${capturedCacheHost}${capturedCachePath}?uuid=${encodeURIComponent(adId)}`;

    fetch(cacheUrl, { mode: 'cors', signal: controller.signal })
      .then((res) => (res.ok ? res.text() : Promise.reject(res.status)))
      .then((body) => {
        // PBS Cache returns the cached bid as a JSON object; decode its creative
        // and render metadata the same way the Prebid Universal Creative does.
        const cached = parseCachedBid(body);
        if (!cached) {
          log.warn(
            `[tsjs-gpt] pbRender bridge: PBS Cache response for '${slotId}' had no renderable adm`
          );
          return;
        }
        if (!stillCurrent()) {
          window.tsjs?.recordAdTrace?.({
            kind: 'pb_render_rejected',
            slotId,
            generation: exactCandidate?.generation,
            bidTraceId: exactCandidate?.traceToken,
            reason: 'stale_cache_completion',
          });
          return;
        }
        if (!billingCapacityAvailable(slotId, matchedBid)) {
          window.tsjs?.recordAdTrace?.({
            kind: 'pb_render_rejected',
            slotId,
            generation: exactCandidate?.generation,
            bidTraceId: exactCandidate?.traceToken,
            reason: 'billing_capacity',
          });
          return;
        }
        // Resolve the auction-price macro from the cached clearing price, and
        // size from the cached bid's own dimensions, falling back to the slot
        // format only when the cache omits them.
        const ad =
          cached.price !== undefined
            ? expandAuctionPriceMacro(cached.adm, cached.price)
            : cached.adm;
        const traceToken = armExpectedRender(exactCandidate, capturedSource);
        requestOwner.served = true;
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            ad,
            renderer: TS_DISPLAY_RENDERER,
            width: cached.width ?? width,
            height: cached.height ?? height,
            ...(traceToken ? { traceToken } : {}),
          })
        );
        fireWinBillingBeacons(slotId, matchedBid);
        window.tsjs?.recordAdTrace?.({
          kind: 'pb_render_served',
          slotId,
          generation: exactCandidate?.generation,
          bidTraceId: traceToken,
        });
        log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from PBS Cache`);
      })
      .catch((err) => {
        if (err instanceof DOMException && err.name === 'AbortError') return;
        log.warn(`[tsjs-gpt] pbRender bridge: PBS Cache fetch failed for '${slotId}'`, err);
      })
      .finally(() => {
        if (activeRender.expiryTimer) clearTimeout(activeRender.expiryTimer);
        activeCacheRenders.delete(activeRender);
        if (latestCacheRenderBySlot.get(slotId) === activeRender) {
          latestCacheRenderBySlot.delete(slotId);
        }
      });
  });
}

// Register the activation function on `window` so the server-injected inline
// script can call it explicitly. The server emits:
//   <script>window.__tsjs_gpt_enabled=true;
//          window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>
// The HTML pipeline currently injects that inline script before the unified
// bundle, so the explicit call is best-effort only. To make activation robust
// regardless of script order, the module also checks for a pre-set enable flag
// immediately after registering the function.
if (typeof window !== 'undefined') {
  const win = window as unknown as Record<string, unknown>;

  win.__tsjs_installGptShim = installGptShim;

  if (win.__tsjs_gpt_enabled === true) {
    installGptShim();
  }

  installTsAdInit();
  installSpaAuctionHook();
  installSlimPrebidLoader();
  installTsRenderBridge();
}

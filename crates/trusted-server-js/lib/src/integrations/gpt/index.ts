import { log } from '../../core/log';
import type { AuctionSlot, AuctionBidData, GptSlotHandoff, TsjsApi } from '../../core/types';

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
const TS_BASE_TARGETING_KEYS = [...TS_BID_TARGETING_KEYS, TS_INITIAL_TARGETING_KEY] as const;

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
  isEmpty: boolean;
  slot: GoogleTagSlot;
}

function isElementVisible(element: HTMLElement): boolean {
  const style = window.getComputedStyle(element);
  return (
    style.display !== 'none' && style.visibility !== 'hidden' && style.visibility !== 'collapse'
  );
}

function slotElementHasLayout(element: HTMLElement): boolean {
  if (!isElementVisible(element)) return false;
  const elementRect = element.getBoundingClientRect();
  if (elementRect.width > 0 && elementRect.height > 0) return true;

  const container = document.getElementById(`${element.id}-container`);
  if (!container || !isElementVisible(container)) return false;
  const containerRect = container.getBoundingClientRect();
  return containerRect.width > 0 && containerRect.height > 0;
}

function findSlotElementByDivId(divId: string): HTMLElement | null {
  if (!divId) return null;
  const exact = document.getElementById(divId);
  if (exact) return exact;

  const prefixMatches = Array.from(document.querySelectorAll<HTMLElement>('[id]')).filter(
    (element) => element.id.startsWith(divId) && !element.id.endsWith('-container')
  );
  // A unique prefix match may be a lazy slot that has not been sized yet.
  // Geometry is only needed to disambiguate multiple responsive siblings.
  if (prefixMatches.length === 1) return prefixMatches[0] ?? null;

  const visibleMatches = prefixMatches.filter(isElementVisible);
  if (visibleMatches.length === 1) return visibleMatches[0] ?? null;

  const activeMatches = visibleMatches.filter(slotElementHasLayout);
  if (activeMatches.length === 1) return activeMatches[0] ?? null;

  if (prefixMatches.length > 1) {
    log.warn('GPT slot prefix did not resolve to one active element', {
      divId,
      prefixMatchCount: prefixMatches.length,
      activeMatchCount: activeMatches.length,
    });
  }
  return null;
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

function clearTargetingKeys(slot: GoogleTagSlot, keys: Iterable<string>): void {
  if (typeof slot.clearTargeting !== 'function') return;

  for (const key of new Set(keys)) {
    slot.clearTargeting(key);
  }
}

interface GoogleTagRefreshOptions {
  changeCorrelator?: boolean;
}

interface GoogleTagPubAdsService {
  setTargeting(key: string, value: string | string[]): GoogleTagPubAdsService;
  getTargeting(key: string): string[];
  enableSingleRequest(): void;
  addEventListener(event: string, fn: (e: SlotRenderEndedEvent) => void): void;
  refresh(slots?: GoogleTagSlot[], options?: GoogleTagRefreshOptions): void;
  getSlots?(): GoogleTagSlot[];
  disableInitialLoad?(): void;
}

interface GoogleTagConfig extends Record<string, unknown> {
  disableInitialLoad?: boolean | null;
}

interface GoogleTagEffectiveConfig {
  disableInitialLoad?: boolean;
}

type GoogleTagDisplayTarget = string | Element | GoogleTagSlot;

interface GoogleTag {
  cmd: Array<() => void>;
  pubads(): GoogleTagPubAdsService;
  defineSlot(
    adUnitPath: string,
    size: Array<number | number[]>,
    elementId?: string
  ): GoogleTagSlot | null;
  destroySlots(slots?: GoogleTagSlot[]): boolean;
  enableServices(): void;
  display(target: GoogleTagDisplayTarget): void;
  setConfig?(config: GoogleTagConfig): void;
  getConfig?(keys: string | string[]): GoogleTagEffectiveConfig | undefined;
  _loaded_?: boolean;
}

type GptWindow = Window & {
  googletag?: Partial<GoogleTag>;
  __tsjs_slim_prebid_url?: string;
};

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

function fireWinBillingBeacons(slotId: string, bid: AuctionBidData): void {
  if (!slotId || (!bid.nurl && !bid.burl)) return;

  const fired = (window.tsjs!.firedBeacons ??= {});
  const bidIdentity = bid.hb_adid ?? bid.nurl ?? bid.burl ?? '';
  const urls = [
    ['nurl', bid.nurl],
    ['burl', bid.burl],
  ] as const;

  for (const [kind, url] of urls) {
    if (!url) continue;

    const beaconKey = `${slotId}|${bidIdentity}|${kind}|${url}`;
    if (fired[beaconKey]) continue;

    if (queueWinBillingBeacon(url)) {
      fired[beaconKey] = true;
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
 * Read GPT's effective state through `getConfig()` when available and wrap both
 * configuration APIs so changes are synchronized immediately. The wrappers also
 * provide a fallback for runtimes where the getter is unavailable. With initial
 * load disabled, `display()` only registers a slot — the ad request
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
function syncInitialLoadDisabled(gpt: Partial<GoogleTag>, ts: TsjsApi): boolean {
  if (typeof gpt.getConfig !== 'function') return false;

  const config = gpt.getConfig('disableInitialLoad');
  if (!config || config.disableInitialLoad === undefined) return false;

  ts.gptInitialLoadDisabled = config.disableInitialLoad === true;
  return true;
}

function installInitialLoadDetector(ts: TsjsApi): void {
  const win = window as GptWindow;
  const cmd = win.googletag?.cmd;
  if (!cmd) return;
  cmd.push(() => {
    const gpt = win.googletag as
      | (Partial<GoogleTag> & { __tsInitialLoadConfigHooked?: boolean })
      | undefined;
    if (!gpt) return;

    syncInitialLoadDisabled(gpt, ts);

    if (typeof gpt.setConfig === 'function' && !gpt.__tsInitialLoadConfigHooked) {
      const originalSetConfig = gpt.setConfig.bind(gpt);
      gpt.setConfig = function (...args: Parameters<typeof originalSetConfig>) {
        const config = args[0];
        const result = originalSetConfig(...args);
        if (!syncInitialLoadDisabled(gpt, ts) && config && 'disableInitialLoad' in config) {
          ts.gptInitialLoadDisabled = config.disableInitialLoad === true;
        }
        return result;
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
      const result = originalDisableInitialLoad();
      if (!syncInitialLoadDisabled(gpt, ts)) {
        ts.gptInitialLoadDisabled = true;
      }
      return result;
    };
    service.__tsInitialLoadHooked = true;
  });
}

/**
 * Install `window.tsjs.scheduleInitialAdInit`.
 *
 * The server-injected `</body>` bids script calls this to run the initial
 * `adInit()` after React hydration instead of synchronously at body-parse
 * time. `adInit()` defines GPT slots on the publisher's `-container`
 * wrappers, mutating those ad-slot subtrees; on a Next.js App Router page a
 * synchronous call lands that mutation inside React's hydration window and
 * trips a #418 hydration mismatch. Deferral: gate on window `load` (the
 * client bundles that hydrate the tree have executed by then), then a double
 * `requestAnimationFrame` so the call runs after React has committed. A
 * single deferred call — no retry timer.
 *
 * The SSR document is navigation generation 0 by definition, so the scheduler
 * pins the whole initial pass to generation 0 rather than capturing whatever
 * the counter reads when the `</body>` script runs: the SPA hook is installed
 * by the synchronous head bundle, so a navigation can commit while the HTML
 * is still streaming, and capturing that advanced value would adopt the stale
 * SSR bootstrap as current. For the same reason the initial bids payload is
 * passed in and applied here, generation-guarded — assigning it
 * unconditionally at body end would clobber the live bids a faster SPA
 * navigation already applied. When a navigation has committed since — or
 * commits while the deferred callback is pending — the SSR payload is
 * dropped and `adInit()` is not run: running anyway would re-run the newer
 * route's live slots/bids, destroying and redefining that route's TS slots
 * and double-refreshing it. The generation counter (not a URL comparison)
 * keeps this guard aligned with the SPA auction hook's own navigation
 * identity: a query-only history change the hook ignores must not cancel the
 * initial call, while an `/a → /b → /a` round trip — where the URL compares
 * equal again — must.
 *
 * Hidden documents: browsers do not service `requestAnimationFrame` while a
 * document is hidden, so a background-tab load (Cmd+click, open-in-new-tab)
 * holds the initial `adInit()` until the tab is first viewed. This is
 * intended, not an oversight: the initial ad request then spends its
 * impression on a tab someone is actually looking at instead of firing —
 * unviewable — at parse time in a tab that may never be foregrounded, and
 * riding rAF keeps a single code path whose post-hydration-commit guarantee
 * holds whenever the request is actually issued.
 */
function installScheduleInitialAdInit(ts: TsjsApi): void {
  ts.scheduleInitialAdInit = function (initialBids?: Record<string, AuctionBidData>) {
    if ((ts.navGeneration ?? 0) !== 0) return;
    if (initialBids) ts.bids = initialBids;
    const runUnlessNavigated = (): void => {
      if ((ts.navGeneration ?? 0) !== 0) return;
      ts.adInit?.();
    };
    const afterHydrationFrames = (): void => {
      window.requestAnimationFrame(() => {
        window.requestAnimationFrame(runUnlessNavigated);
      });
    };
    if (document.readyState === 'complete') {
      afterHydrationFrames();
    } else {
      window.addEventListener('load', afterHydrationFrames, { once: true });
    }
  };
}

interface HandoffPatchedFunction {
  __tsSlotHandoffPatched?: boolean;
}

function findGptSlotByElementId(
  pubads: GoogleTagPubAdsService,
  elementId: string
): GoogleTagSlot | undefined {
  return pubads.getSlots?.().find((slot) => slot.getSlotElementId() === elementId);
}

function handoffForSlot(ts: TsjsApi, slot: GoogleTagSlot): GptSlotHandoff | undefined {
  return ts.gptSlotHandoffs?.[slot.getSlotElementId()];
}

function displayTargetElementId(target: GoogleTagDisplayTarget): string | undefined {
  if (typeof target === 'string') return target;
  if (typeof (target as GoogleTagSlot).getSlotElementId === 'function') {
    return (target as GoogleTagSlot).getSlotElementId();
  }
  return (target as Element).id || undefined;
}

function normalizedGptFormats(formats: Array<number | number[]>): Array<number | number[]> {
  return formats.length === 2 && formats.every((format) => typeof format === 'number')
    ? [formats as number[]]
    : formats;
}

function handoffFormatsMatch(handoff: GptSlotHandoff, formats: Array<number | number[]>): boolean {
  return JSON.stringify(handoff.formats) === JSON.stringify(normalizedGptFormats(formats));
}

function matchingHandoff(
  ts: TsjsApi,
  pubads: GoogleTagPubAdsService,
  adUnitPath: string,
  formats: Array<number | number[]>,
  elementId: string
): GptSlotHandoff | undefined {
  const exact = ts.gptSlotHandoffs?.[elementId];
  if (exact) return exact.publisherClaimed ? undefined : exact;

  const candidates = new Set(Object.values(ts.gptSlotHandoffs ?? {})).values();
  const matching = Array.from(candidates).filter(
    (handoff) =>
      !handoff.publisherClaimed &&
      !document.getElementById(handoff.slotElementId) &&
      elementId.startsWith(handoff.divIdPrefix) &&
      handoff.gamUnitPath === adUnitPath &&
      handoffFormatsMatch(handoff, formats) &&
      findGptSlotByElementId(pubads, handoff.slotElementId)
  );
  return matching.length === 1 ? matching[0] : undefined;
}

function registerHandoffAlias(ts: TsjsApi, elementId: string, handoff: GptSlotHandoff): void {
  (ts.gptSlotHandoffs ??= {})[elementId] = handoff;
}


function withGptSlotHandoffInternal<T>(ts: TsjsApi, callback: () => T): T {
  const wasInternal = ts.gptSlotHandoffInternal;
  ts.gptSlotHandoffInternal = true;
  try {
    return callback();
  } finally {
    ts.gptSlotHandoffInternal = wasInternal;
  }
}

/**
 * Reuse a TS-created inner-div slot when its publisher defines that div later.
 *
 * TS cannot wait an arbitrary amount of time for framework hydration: doing so
 * would leave placements blank when no publisher slot is ever defined. Instead,
 * TS creates its fallback on the publisher's actual div and aliases only a later
 * `defineSlot()` for that exact div, or for a hydration-renamed replacement after
 * the original div is gone. The first duplicate publisher request is suppressed
 * because TS has already issued the initial request with TS targeting.
 */
function installLatePublisherSlotHandoff(ts: TsjsApi): void {
  const win = window as GptWindow;
  const cmd = win.googletag?.cmd;
  if (!cmd) return;

  cmd.push(() => {
    const g = win.googletag;
    const pubads = g?.pubads?.();
    if (!g?.defineSlot || !g.display || !pubads) return;

    const defineSlot = g.defineSlot;
    if (!(defineSlot as HandoffPatchedFunction).__tsSlotHandoffPatched) {
      const originalDefineSlot = defineSlot.bind(g);
      const patchedDefineSlot = (
        adUnitPath: string,
        formats: Array<number | number[]>,
        elementId?: string
      ): GoogleTagSlot | null => {
        if (!ts.gptSlotHandoffInternal && typeof elementId === 'string') {
          const handoff = matchingHandoff(ts, pubads, adUnitPath, formats, elementId);
          if (handoff) {
            const existingSlot = findGptSlotByElementId(pubads, handoff.slotElementId);
            if (existingSlot) {
              registerHandoffAlias(ts, elementId, handoff);
              handoff.publisherClaimed = true;
              // The supported publisher lifecycle is defineSlot → addService → display.
              // Intentionally wait for that display instead of applying a time heuristic.
              handoff.suppressPublisherDisplay = true;
              handoff.suppressPublisherRefresh = ts.gptInitialLoadDisabled === true;
              ts.prevGptSlots = (ts.prevGptSlots ?? []).filter(
                (ownedSlot) => ownedSlot !== existingSlot
              );
              if (handoff.gamUnitPath !== adUnitPath || !handoffFormatsMatch(handoff, formats)) {
                log.warn('GPT slot handoff: publisher definition differs from TS configuration', {
                  elementId,
                  tsGamUnitPath: handoff.gamUnitPath,
                  publisherGamUnitPath: adUnitPath,
                });
              }
              return existingSlot;
            }
          }
        }
        return elementId === undefined
          ? originalDefineSlot(adUnitPath, formats)
          : originalDefineSlot(adUnitPath, formats, elementId);
      };
      (patchedDefineSlot as HandoffPatchedFunction).__tsSlotHandoffPatched = true;
      g.defineSlot = patchedDefineSlot;
    }

    const display = g.display;
    if (!(display as HandoffPatchedFunction).__tsSlotHandoffPatched) {
      const originalDisplay = display.bind(g);
      const patchedDisplay = (target: GoogleTagDisplayTarget): void => {
        const elementId = displayTargetElementId(target);
        const handoff = elementId ? ts.gptSlotHandoffs?.[elementId] : undefined;
        if (!ts.gptSlotHandoffInternal && handoff?.suppressPublisherDisplay) {
          handoff.suppressPublisherDisplay = false;
          return;
        }
        originalDisplay(target);
      };
      (patchedDisplay as HandoffPatchedFunction).__tsSlotHandoffPatched = true;
      g.display = patchedDisplay;
    }

    const refresh = pubads.refresh;
    if (!(refresh as HandoffPatchedFunction).__tsSlotHandoffPatched) {
      const originalRefresh = refresh.bind(pubads);
      const callRefresh = (
        slots: GoogleTagSlot[] | undefined,
        options: GoogleTagRefreshOptions | undefined
      ): void => {
        if (options === undefined) {
          originalRefresh(slots);
        } else {
          originalRefresh(slots, options);
        }
      };
      const patchedRefresh = (
        requestedSlots?: GoogleTagSlot[],
        options?: GoogleTagRefreshOptions
      ): void => {
        if (ts.gptSlotHandoffInternal) {
          callRefresh(requestedSlots, options);
          return;
        }

        const slots = requestedSlots ?? pubads.getSlots?.();
        if (!slots) {
          callRefresh(requestedSlots, options);
          return;
        }

        let suppressed = false;
        const remainingSlots = slots.filter((slot) => {
          const handoff = handoffForSlot(ts, slot);
          if (!handoff?.suppressPublisherRefresh) return true;
          handoff.suppressPublisherRefresh = false;
          suppressed = true;
          return false;
        });
        if (!suppressed) {
          callRefresh(requestedSlots, options);
        } else if (remainingSlots.length > 0) {
          callRefresh(remainingSlots, options);
        }
      };
      (patchedRefresh as HandoffPatchedFunction).__tsSlotHandoffPatched = true;
      pubads.refresh = patchedRefresh;
    }
  });
}

export function installTsAdInit(): void {
  const ts = (window.tsjs ??= {} as TsjsApi);
  installInitialLoadDetector(ts);
  installScheduleInitialAdInit(ts);

  installLatePublisherSlotHandoff(ts);
  ts.adInit = function () {
    const slots = ts.adSlots ?? [];
    // Snapshot bids at adInit() call time — correct for targeting setup.
    // The slotRenderEnded listener below reads ts.bids live so SPA navigation
    // updates (new ts.bids injected before </body>) are picked up at render time.
    const bids = ts.bids ?? {};
    // Generation this invocation belongs to. The destructive slot work below
    // is queued on googletag.cmd, which only drains when GPT itself loads —
    // possibly much later (e.g. consent-gated GPT). A navigation can commit
    // in that gap, so the queued callback rechecks the generation as its
    // first act and stands down rather than applying this invocation's
    // slots/bids to the newer route's DOM and double-requesting it.
    const generation = ts.navGeneration ?? 0;
    const g = (window as GptWindow).googletag;
    if (!g) return;

    g.cmd?.push(() => {
      if ((ts.navGeneration ?? 0) !== generation) return;
      // Destroy previously defined TS slots before redefining for the new page.
      if (ts.prevGptSlots && ts.prevGptSlots.length > 0) {
        const destroyedSlotElementIds = new Set(
          (ts.prevGptSlots as GoogleTagSlot[]).map((slot) => slot.getSlotElementId())
        );
        g.destroySlots?.(ts.prevGptSlots as GoogleTagSlot[]);
        if (ts.gptSlotHandoffs) {
          for (const [elementId, handoff] of Object.entries(ts.gptSlotHandoffs)) {
            if (destroyedSlotElementIds.has(handoff.slotElementId)) {
              delete ts.gptSlotHandoffs[elementId];
            }
          }
        }
        ts.prevGptSlots = [];
      }

      // Slots TS defined itself — tracked for SPA destroy. Publisher-owned
      // slots are reused but never destroyed by TS on navigation.
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
          // Define TS's fallback on the publisher's actual div. A late publisher
          // defineSlot() for this div is handed the same slot by the scoped GPT
          // wrapper, preventing a competing container-slot request.
          const defined = withGptSlotHandoffInternal(ts, () =>
            g.defineSlot?.(slot.gam_unit_path, slot.formats, actualDivId)
          );
          if (!defined) return;
          defined.addService(g.pubads!());
          gptSlot = defined;
          tsOwned = true;
          (ts.gptSlotHandoffs ??= {})[actualDivId] = {
            gamUnitPath: slot.gam_unit_path,
            formats: slot.formats,
            divIdPrefix: slot.div_id,
            slotElementId: actualDivId,
            publisherClaimed: false,
            suppressPublisherDisplay: false,
            suppressPublisherRefresh: false,
          };
        }

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
        gptSlot.setTargeting(TS_INITIAL_TARGETING_KEY, '1');
        // Map the resolved inner div to the slot ID so slotRenderEnded and ADM
        // injection address the same, single GPT slot.
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

        // APS: signal to apstag that bids are ready so Amazon's GAM creative
        // can render.  apstag must already be initialised on the page (which it
        // is on production publisher pages).  Safe no-op if apstag is absent.
        if (bid.hb_bidder === 'aps' || bid.hb_bidder === 'amazon-aps') {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (window as any).apstag?.setDisplayBids?.();
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
      slotsToDisplay.forEach((divId) => withGptSlotHandoffInternal(ts, () => g.display?.(divId)));

      syncInitialLoadDisabled(g, ts);
      // Slots needing an explicit ad request via refresh(). Reused
      // publisher-owned slots always need one to pick up the just-applied
      // server-side targeting. TS-defined slots are normally fetched by the
      // display() above — but when the publisher called
      // pubads().disableInitialLoad(), display() only registers the slot and the
      // ad request must come from refresh(). Without this, a TS-owned
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
          withGptSlotHandoffInternal(ts, () => g.pubads!().refresh(slotsNeedingRefresh));
        } finally {
          ts.adInitRefreshInProgress = false;
        }
      }
    });
  };
}

interface PageBidsResponse {
  slots: AuctionSlot[];
  bids: Record<string, AuctionBidData>;
}

/** Canonical SPA re-auction endpoint. Mirrors `PAGE_BIDS_PATH` in Rust. */
const PAGE_BIDS_PATH = '/_ts/page-bids';

/**
 * Deprecated alias of {@link PAGE_BIDS_PATH}, kept registered server-side so
 * pre-rename bundles keep working. This bundle falls back to it when the
 * canonical path does not serve page-bids: a server rolled back to before the
 * rename does not register the canonical path, and an operator `[[handlers]]`
 * auth regex broad enough to cover `/_ts` answers it with `401` that no
 * anonymous browser fetch can satisfy. Without the fallback either case
 * silently drops ads on every SPA navigation.
 *
 * Removed together with the server-side alias in IABTechLab/trusted-server#970.
 */
const PAGE_BIDS_LEGACY_PATH = '/__ts/page-bids';

/**
 * `X-TSJS-Page-Bids` value sent on a fallback request, so the server can tell
 * a current bundle that could not use the canonical path from a pre-rename
 * bundle that only knows the alias. Only the former signals a deployment that
 * needs fixing. Mirrors `PAGE_BIDS_FALLBACK_MARKER` in Rust.
 */
const PAGE_BIDS_FALLBACK_MARKER = 'fallback';

/** Outcome of one page-bids request against a specific endpoint path. */
interface PageBidsAttempt {
  /** Parsed payload, or `null` when this endpoint did not serve one. */
  data: PageBidsResponse | null;
  /**
   * The response says this path does not serve page-bids on this deployment,
   * so the other registered path is worth trying. Transient failures and the
   * endpoint's own cross-site denial apply equally to both paths and do not
   * set this.
   */
  wrongEndpoint: boolean;
}

async function fetchPageBids(
  endpoint: string,
  path: string,
  signal: AbortSignal
): Promise<PageBidsAttempt> {
  const res = await fetch(`${endpoint}?path=${encodeURIComponent(path)}`, {
    credentials: 'include',
    // Non-simple header doubles as a CSRF token: the server rejects
    // requests that carry neither same-origin Fetch Metadata nor this
    // header, and cross-origin pages cannot send it without a CORS
    // preflight the endpoint never grants. The server checks presence, not
    // value, so the value carries the fallback diagnostic.
    headers: {
      'X-TSJS-Page-Bids': endpoint === PAGE_BIDS_LEGACY_PATH ? PAGE_BIDS_FALLBACK_MARKER : '1',
    },
    signal,
  });
  if (!res.ok) {
    // 401: an operator auth handler regex covers this path. 404: this server
    // does not know the route. Either way the other path may still answer.
    // 403 (cross-site gate) and 5xx would repeat on both, so they do not.
    return { data: null, wrongEndpoint: res.status === 401 || res.status === 404 };
  }
  try {
    return { data: (await res.json()) as PageBidsResponse, wrongEndpoint: false };
  } catch (err) {
    if (err instanceof DOMException && err.name === 'AbortError') throw err;
    // A server with no route for this path proxies it to the publisher origin,
    // which answers 200 HTML. An unparseable body is the wrong endpoint, not a
    // transient failure.
    return { data: null, wrongEndpoint: true };
  }
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
    let animationFrame: number | undefined;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      if (animationFrame !== undefined) cancelAnimationFrame(animationFrame);
      observer.disconnect();
      clearTimeout(timer);
      signal.removeEventListener('abort', finish);
      resolve();
    };
    const observer = new MutationObserver(() => {
      if (animationFrame !== undefined) return;
      if (typeof requestAnimationFrame === 'undefined') {
        if (allPresent()) finish();
        return;
      }
      animationFrame = requestAnimationFrame(() => {
        animationFrame = undefined;
        if (allPresent()) finish();
      });
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
 * fetches fresh slots + bids from `/_ts/page-bids?path=<new_path>`, updates
 * `window.tsjs.adSlots` / `window.tsjs.bids`, and calls `window.tsjs.adInit()`.
 *
 * Idempotent: guarded by `window.tsjs.spaHookInstalled` so multiple calls are safe.
 */
export function installSpaAuctionHook(): void {
  if (typeof window === 'undefined') return;
  const ts = (window.tsjs ??= {} as TsjsApi);
  if (ts.spaHookInstalled) return;
  ts.spaHookInstalled = true;
  // Navigation identity for the deferred initial-adInit bootstrap (see
  // installScheduleInitialAdInit). Initialized here, and incremented
  // synchronously in onNavigate the moment a route change is accepted, so the
  // counter can never lag the auction decision. Deliberately NOT rolled back
  // when a navigation later fails: the framework has already swapped the
  // route's DOM by then, so a pending initial callback must still stand down.
  ts.navGeneration ??= 0;

  let inflight: AbortController | null = null;
  // Last path an auction was run for. popstate fires for hash-only and
  // same-pathname back/forward (scroll restoration), and pushState/replaceState
  // can be called with the current URL, so guard every entry point against
  // re-requesting impressions for a path we already loaded.
  let currentPath = location.pathname;
  // Last path whose slots/bids were actually applied — the initial SSR page
  // counts. A failed navigation rolls `currentPath` back to this rather than to
  // the immediately-previous committed value: on rapid A→B where A was aborted
  // mid-flight and B then fails, rolling back to A (never loaded) would strand
  // it behind the no-op guard, so we roll back to the last applied route instead.
  let lastAppliedPath = location.pathname;
  // Endpoint this session requests. Starts canonical; if the deployment does
  // not serve page-bids there, one navigation retries on the deprecated alias
  // and the session stays on it rather than re-probing every navigation.
  let pageBidsEndpoint = PAGE_BIDS_PATH;

  async function requestPageBids(
    path: string,
    signal: AbortSignal
  ): Promise<PageBidsResponse | null> {
    const attempt = await fetchPageBids(pageBidsEndpoint, path, signal);
    if (attempt.data || !attempt.wrongEndpoint || pageBidsEndpoint !== PAGE_BIDS_PATH) {
      return attempt.data;
    }

    const fallback = await fetchPageBids(PAGE_BIDS_LEGACY_PATH, path, signal);
    if (!fallback.data) return null;
    log.warn(
      `SPA auction hook: ${PAGE_BIDS_PATH} does not serve page-bids here, ` +
        `falling back to ${PAGE_BIDS_LEGACY_PATH}`
    );
    pageBidsEndpoint = PAGE_BIDS_LEGACY_PATH;
    return fallback.data;
  }

  async function onNavigate(path: string): Promise<void> {
    if (path === currentPath) return;
    currentPath = path;
    ts.navGeneration = (ts.navGeneration ?? 0) + 1;
    // A route change invalidates hydration aliases before the new route's
    // publisher can define a same-prefix slot while page-bids is in flight.
    for (const [elementId, handoff] of Object.entries(ts.gptSlotHandoffs ?? {})) {
      if (!handoff.publisherClaimed) delete ts.gptSlotHandoffs![elementId];
    }
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;

    try {
      const data = await requestPageBids(path, controller.signal);
      if (!data) {
        // A transient page-bids failure must not strand this route: roll the
        // committed path back so a later navigation here retries instead of
        // being skipped by the no-op guard at the top. Only roll back when no
        // newer navigation has already advanced currentPath.
        if (inflight === controller) currentPath = lastAppliedPath;
        return;
      }
      if (inflight !== controller) return;
      // Defer applying bids until the new route's ad containers exist, so a
      // fast edge response cannot beat the DOM and drop server-side bids.
      await waitForSlotElements(data.slots, controller.signal);
      if (inflight !== controller) return;
      ts.adSlots = data.slots;
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
      const newPath = url ? new URL(String(url), location.href).pathname : location.pathname;
      // onNavigate no-ops when newPath equals the last loaded path.
      void onNavigate(newPath);
    };
  }

  patchHistoryMethod('pushState');
  patchHistoryMethod('replaceState');

  window.addEventListener('popstate', () => {
    void onNavigate(location.pathname);
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

    if (data['message'] !== 'Prebid Request') return;
    const adId = data['adId'] as string | undefined;
    if (!adId) return;

    const port = e.ports?.[0];
    if (!port) return;
    const sourceSlotId = slotIdForMessageSource(e.source);
    if (!sourceSlotId) return;

    // Resolve the bid by the requesting slot, not by the first bid whose hb_adid
    // matches. hb_adid is not unique per bid: absent PBS Cache, it falls back to a
    // creative id a bidder may reuse across slots. A first-match-by-adId lookup
    // would resolve every duplicate to one slot, so all but that slot render blank.
    const bids = window.tsjs?.bids ?? {};
    const slotId = sourceSlotId;
    const matchedBid = bids[slotId];

    // Not a TS bid, or the requesting slot's bid does not own this adId — let
    // Prebid.js handle it. The adId guard also prevents an iframe under slot A from
    // pulling slot B's creative and firing slot B's win/billing beacons.
    if (!matchedBid || matchedBid.hb_adid !== adId) return;

    const slot = window.tsjs?.adSlots?.find((s) => s.id === slotId);
    // Prefer the winning creative's own dimensions; the first configured slot
    // format is only a fallback and mis-sizes a multi-size slot whose winner is
    // not the first format.
    const [fallbackWidth, fallbackHeight] = slot?.formats?.[0] ?? [728, 90];
    const width = matchedBid.w ?? fallbackWidth;
    const height = matchedBid.h ?? fallbackHeight;

    if (matchedBid.adm) {
      e.stopImmediatePropagation();
      port.postMessage(
        JSON.stringify({
          message: 'Prebid Response',
          adId,
          ad: matchedBid.adm,
          renderer: TS_DISPLAY_RENDERER,
          width,
          height,
        })
      );
      fireWinBillingBeacons(slotId, matchedBid);
      log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from inline adm`);
      return;
    }

    // No TS render source — let Prebid.js handle it.
    if (!matchedBid.hb_cache_host || !matchedBid.hb_cache_path) return;

    // TS owns this adId — stop Prebid from also processing it.
    e.stopImmediatePropagation();

    // Skip a concurrent re-render of the same slot's adId so its win/billing
    // beacons fire at most once even before the first cache fetch resolves.
    const renderingKey = `${slotId}|${adId}`;
    if (renderingKeys.has(renderingKey)) return;
    renderingKeys.add(renderingKey);

    const cacheUrl = `https://${matchedBid.hb_cache_host}${matchedBid.hb_cache_path}?uuid=${encodeURIComponent(adId)}`;

    fetch(cacheUrl, { mode: 'cors' })
      .then((res) => (res.ok ? res.text() : Promise.reject(res.status)))
      .then((body) => {
        // PBS Cache returns the cached bid as a JSON object; decode its creative
        // and render metadata the same way the Prebid Universal Creative does.
        const cached = parseCachedBid(body);
        if (!cached) {
          // No renderable creative in the cache payload — decline rather than
          // ship a serialized bid document to PUC. Beacons stay unfired.
          log.warn(
            `[tsjs-gpt] pbRender bridge: PBS Cache response for '${slotId}' had no renderable adm`
          );
          return;
        }
        // Resolve the auction-price macro from the cached clearing price, and
        // size from the cached bid's own dimensions, falling back to the slot
        // format only when the cache omits them.
        const ad =
          cached.price !== undefined
            ? expandAuctionPriceMacro(cached.adm, cached.price)
            : cached.adm;
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            ad,
            renderer: TS_DISPLAY_RENDERER,
            width: cached.width ?? width,
            height: cached.height ?? height,
          })
        );
        // Beacons carry the server-expanded ${AUCTION_PRICE} from the auction's
        // clearing price, not `cached.price` — the auction result is the
        // authoritative clearing price, and the cached copy is only the render
        // source. Do not re-expand them here.
        fireWinBillingBeacons(slotId, matchedBid);
        log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from PBS Cache`);
      })
      .catch((err) => {
        log.warn(`[tsjs-gpt] pbRender bridge: PBS Cache fetch failed for '${slotId}'`, err);
      })
      .finally(() => {
        renderingKeys.delete(renderingKey);
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

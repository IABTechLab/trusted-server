import { log } from '../../core/log';
import {
  recordRender,
  updateRender,
  stampCreativeTrace,
  isEffectivelyVisible,
} from '../../core/trace';
import type {
  AuctionSlot,
  AuctionBidData,
  RenderRecord,
  RenderServedFrom,
  TsjsApi,
} from '../../core/types';

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

// ---- Orphaned-slot recovery (client re-render / hydration race) ------------
// A client framework can replace the ad divs *after* GPT slots were bound to
// them: server-rendered React ids (`…-_R_abc_`) are swapped for client ids
// (`…-_r_1_`) during hydration, leaving GPT holding slots whose element no
// longer exists ("defineSlot was called without a corresponding DIV"). GAM
// still fetches a creative for those slots, but it has nowhere to render, so
// the bid is silently wasted. These bound the recovery re-bind.
/** Quiet period after DOM mutations before checking for orphaned slots. */
const ORPHAN_RECONCILE_DEBOUNCE_MS = 250;
/** How long after an adInit() to keep watching for a re-render. */
const ORPHAN_RECONCILE_WINDOW_MS = 5000;
/**
 * Maximum re-binds per page load. Each re-bind re-requests the affected slots,
 * so this is deliberately small: it recovers a hydration swap without letting a
 * continuously-mutating page loop on ad requests.
 */
const MAX_ORPHAN_RECONCILE_ATTEMPTS = 2;

// ---- Render generation -----------------------------------------------------
// Every `adInit()` and every SPA navigation opens a new generation. Async work
// (a PBS Cache fetch, a debounced orphan re-bind, a GAM render event) captures
// the generation it started in and re-checks it before acting, so a result that
// belongs to a route the page has already left is discarded instead of being
// applied to the route now on screen.

function currentGeneration(ts: TsjsApi): number {
  return ts.renderGeneration ?? 0;
}

function bumpGeneration(ts: TsjsApi): number {
  const next = currentGeneration(ts) + 1;
  ts.renderGeneration = next;
  return next;
}

/** Immutable context for one GPT request TS initiated for a slot. */
interface PendingGptRender {
  generation: number;
  slotId: string;
  bid: AuctionBidData;
  attributed: boolean;
}

/**
 * Per-slot FIFO of requests awaiting `slotRenderEnded`.
 *
 * Publisher-owned GPT slots are reused across SPA routes, so stamping a single
 * generation on the slot object is insufficient: route B overwrites that stamp
 * before route A's late event arrives. GPT emits one render event per request in
 * request order, so retaining each immutable request context lets the old event
 * retire only route A's entry while route B's attribution remains queued.
 */
const pendingGptRenders = new WeakMap<GoogleTagSlot, PendingGptRender[]>();

function enqueueGptRender(slot: GoogleTagSlot, pending: PendingGptRender): void {
  const queue = pendingGptRenders.get(slot) ?? [];
  queue.push(pending);
  pendingGptRenders.set(slot, queue);
}

function takeGptRender(slot: GoogleTagSlot): PendingGptRender | undefined {
  const queue = pendingGptRenders.get(slot);
  const pending = queue?.shift();
  if (queue?.length === 0) pendingGptRenders.delete(slot);
  return pending;
}

/**
 * The render record for the GAM impression currently open on a slot.
 *
 * One GAM ad request is observed twice: `slotRenderEnded` carries GAM's own
 * fill signal, and the Prebid Universal Creative bridge carries the proof that
 * TS served the markup. They describe the same impression, so whichever arrives
 * second enriches the first one's record rather than appending its own —
 * otherwise a single creative counts as two renders, inflating the slot's
 * `count`, the history length, the page-global sequence numbers and the panel
 * totals.
 */
interface OpenImpression {
  record: RenderRecord;
  /** Generation the impression was opened in. */
  generation: number;
  /** `hb_adid` both signals must agree on to be the same impression. */
  adId?: string;
  /** Whether `slotRenderEnded` has reported on this impression. */
  gamSeen: boolean;
  /** Whether the Universal Creative bridge has reported on this impression. */
  bridgeSeen: boolean;
}

const openImpressions = new Map<string, OpenImpression>();

/** PBS Cache fetches still in flight, so navigation can abort them. */
const inflightCacheFetches = new Set<AbortController>();

/**
 * Retire everything armed for the route being left.
 *
 * Runs at the *start* of a navigation, not after `/__ts/page-bids` returns: a
 * route whose markup commits faster than that request can otherwise trip the
 * orphan watch or land a cache fetch on the new DOM while `ts.adSlots` and
 * `ts.bids` still hold the previous route's auction.
 */
function beginNavigation(ts: TsjsApi): void {
  bumpGeneration(ts);
  stopOrphanWatch();
  for (const controller of inflightCacheFetches) {
    controller.abort();
  }
  inflightCacheFetches.clear();
  openImpressions.clear();
}

/**
 * The impression `side` may still enrich, or `undefined` if it must open a new
 * one.
 *
 * Requires the impression to be from the current route, for the same bid, not
 * already reported on by this side, and still the slot's live render — the last
 * check is what stops a signal that arrives after a *newer* render of the same
 * slot from rewriting an impression the page has moved past.
 */
function enrichableImpression(
  ts: TsjsApi,
  slotId: string,
  adId: string | undefined,
  side: 'gam' | 'bridge'
): OpenImpression | undefined {
  const open = openImpressions.get(slotId);
  if (!open) return undefined;
  if (open.generation !== currentGeneration(ts)) return undefined;
  if (open.adId !== adId) return undefined;
  if (side === 'gam' ? open.gamSeen : open.bridgeSeen) return undefined;
  if (ts.renders?.[slotId] !== open.record) return undefined;
  return open;
}

function openImpression(
  ts: TsjsApi,
  slotId: string,
  adId: string | undefined,
  record: RenderRecord,
  side: 'gam' | 'bridge'
): void {
  openImpressions.set(slotId, {
    record,
    generation: currentGeneration(ts),
    adId,
    gamSeen: side === 'gam',
    bridgeSeen: side === 'bridge',
  });
}

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
  addEventListener(event: string, fn: (e: SlotRenderEndedEvent) => void): void;
  refresh(slots?: GoogleTagSlot[]): void;
  getSlots?(): GoogleTagSlot[];
  disableInitialLoad?(): void;
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
 * Only active when inject_adm_for_testing injects adm server-side.
 *
 * Strategy:
 * 1. If adm contains an <iframe src="..."> with a safe http(s) src, set that
 *    src on the GAM iframe directly — avoids cross-origin document access.
 * 2. Otherwise replace the slot element's content with a sandboxed srcdoc
 *    iframe (no `allow-same-origin` — see [ADM_IFRAME_SANDBOX]).
 */
/**
 * Returns whether the TS creative was placed **synchronously**. The
 * animation-frame retry branch resolves after this returns, so it reports
 * `false` (placement deferred/unconfirmed) — the render trace must not claim a
 * placement that has not happened yet.
 *
 * That deferred branch reports its real outcome through `onDeferredPlacement`
 * once the animation frame runs. Without it a placement that ultimately
 * succeeded would stay recorded as unconfirmed and the panel would report
 * `gam-only` forever, even though Trusted Server did place the creative.
 */
function injectAdmIntoSlot(
  divId: string,
  adm: string,
  onDeferredPlacement?: (placed: boolean) => void,
  mayPlaceDeferred?: () => boolean
): boolean {
  try {
    // divId may be the container div (used by GPT slot) or the inner div.
    // Resolve it the same way the rest of adInit does (exact then prefix) so
    // a config div_id prefix with a render-time suffix still finds the element.
    const slotEl = findSlotElementByDivId(divId);
    if (!slotEl) return false;

    // Extract the first iframe src from the adm (e.g. mocktioneer creative
    // wraps a first-party proxy iframe in a div). Reject non-http(s) schemes.
    const srcMatch = adm.match(/<iframe[^>]+\bsrc=["']([^"']+)["']/i);
    const innerSrc = srcMatch?.[1] ? safeAdmIframeSrc(srcMatch[1]) : undefined;
    const gamIframe = slotEl.querySelector('iframe') as HTMLIFrameElement | null;

    if (innerSrc && gamIframe) {
      // Set the GAM iframe src — works even cross-origin (no document access needed).
      gamIframe.src = innerSrc;
      log.debug(`[tsjs-gpt] gam-intercept: set iframe src for '${divId}'`);
      return true;
    } else if (innerSrc) {
      // GAM iframe not yet in DOM (APS renders async after slotRenderEnded).
      // Retry on next animation frame so APS has a tick to insert its iframe;
      // if it still isn't there, replace slot content directly.
      requestAnimationFrame(() => {
        let placed = false;
        try {
          // Validate before touching the DOM. A newer SPA route may reuse the
          // same connected publisher slot element, so suppressing only the
          // later trace update would still let the old callback overwrite the
          // new route's creative.
          if (mayPlaceDeferred && !mayPlaceDeferred()) {
            onDeferredPlacement?.(false);
            return;
          }
          const retryIframe = slotEl!.querySelector('iframe') as HTMLIFrameElement | null;
          if (retryIframe) {
            retryIframe.src = innerSrc;
            placed = true;
            log.debug(`[tsjs-gpt] gam-intercept: set iframe src (retry) for '${divId}'`);
          } else {
            slotEl!.innerHTML = '';
            const f = document.createElement('iframe');
            f.style.cssText = 'border:none';
            f.width = String(slotEl!.offsetWidth || 728);
            f.height = String(slotEl!.offsetHeight || 90);
            f.setAttribute('sandbox', ADM_IFRAME_SANDBOX);
            f.setAttribute('data-ts-injected-adm', 'true');
            f.src = innerSrc;
            slotEl!.appendChild(f);
            placed = true;
            log.debug(`[tsjs-gpt] gam-intercept: inserted src iframe for '${divId}'`);
          }
        } catch (err) {
          log.warn('[tsjs-gpt] gam-intercept: deferred placement failed', err);
        }
        onDeferredPlacement?.(placed);
      });
      // Placement deferred to the animation frame — not confirmed yet. The
      // callback above corrects the record once it resolves.
      return false;
    } else {
      // No extractable safe src — replace slot content with a sandboxed srcdoc iframe.
      slotEl.innerHTML = '';
      const f = document.createElement('iframe');
      f.style.border = 'none';
      f.width = String(slotEl.offsetWidth || 728);
      f.height = String(slotEl.offsetHeight || 90);
      f.setAttribute('sandbox', ADM_IFRAME_SANDBOX);
      f.setAttribute('data-ts-injected-adm', 'true');
      f.srcdoc = adm;
      slotEl.appendChild(f);
      log.debug(`[tsjs-gpt] gam-intercept: replaced slot content for '${divId}'`);
      return true;
    }
  } catch (err) {
    log.warn('[tsjs-gpt] gam-intercept: error injecting adm', err);
    return false;
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
 * GPT exposes no getter for the initial-load-disabled flag, so wrap
 * `pubads().disableInitialLoad()` to record it on `window.tsjs`. With initial
 * load disabled, `display()` only registers a slot — the ad request must come
 * from a later `refresh()`. adInit() reads this to refresh its own freshly
 * defined slots so they are not left blank.
 *
 * Installed via the command queue so it runs before the publisher's own
 * `disableInitialLoad()` call (the TS core script is injected ahead of the
 * publisher's GPT setup). Idempotent per pubads service.
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
    const pubads = win.googletag?.pubads?.();
    if (!pubads) return;
    const service = pubads as GoogleTagPubAdsService & { __tsInitialLoadHooked?: boolean };
    if (typeof service.disableInitialLoad !== 'function' || service.__tsInitialLoadHooked) {
      return;
    }
    const original = service.disableInitialLoad.bind(service);
    service.disableInitialLoad = function () {
      ts.gptInitialLoadDisabled = true;
      return original();
    };
    service.__tsInitialLoadHooked = true;
  });
}

// Orphan-watch state. Module-scoped so a re-bind cannot stack observers, and
// so the attempt budget is shared across the page load rather than reset by the
// adInit() the watcher itself triggers.
let orphanObserver: MutationObserver | null = null;
let orphanDebounceTimer: ReturnType<typeof setTimeout> | undefined;
let orphanWindowTimer: ReturnType<typeof setTimeout> | undefined;
let orphanReconcileAttempts = 0;

function stopOrphanWatch(): void {
  orphanObserver?.disconnect();
  orphanObserver = null;
  clearTimeout(orphanDebounceTimer);
  clearTimeout(orphanWindowTimer);
}

/**
 * TS-defined GPT slots whose bound element is no longer in the document.
 *
 * Exported for testing.
 */
export function orphanedTsSlots(ts: TsjsApi): GoogleTagSlot[] {
  return ((ts.prevGptSlots ?? []) as GoogleTagSlot[]).filter((slot) => {
    const elementId = slot?.getSlotElementId?.();
    return !!elementId && !document.getElementById(elementId);
  });
}

/**
 * Watch for a client re-render that orphans TS's GPT slots and re-bind once it
 * happens.
 *
 * Waiting for the divs to merely *exist* (as the SPA hook does) cannot help
 * here: at `adInit()` time the server-rendered divs are present — they are
 * later *replaced*. So instead of delaying the initial ad request, this detects
 * the swap after the fact and re-runs `adInit()`, which destroys the orphaned
 * slots and re-binds against the live DOM (reusing the publisher's own slot for
 * that div when they have since defined one).
 *
 * Bounded by {@link MAX_ORPHAN_RECONCILE_ATTEMPTS} and
 * {@link ORPHAN_RECONCILE_WINDOW_MS} so a page whose DOM never settles cannot
 * spin on ad requests.
 *
 * Scoped to the generation it was armed in. SPA navigation is exactly the kind
 * of DOM churn this observer reacts to, so without that scope a route change
 * whose markup commits faster than `/__ts/page-bids` responds would trip the
 * debounce while `ts.adSlots`/`ts.bids` still describe the *previous* route —
 * re-binding the finished auction onto the new route's divs, issuing another
 * billable GAM request, and burning the recovery budget on an ordinary
 * navigation. `beginNavigation()` also disconnects it outright.
 */
function watchForOrphanedSlots(ts: TsjsApi): void {
  if (typeof MutationObserver === 'undefined' || typeof document === 'undefined') return;
  // Re-arm: a previous window may still be open from an earlier adInit().
  stopOrphanWatch();
  if (orphanReconcileAttempts >= MAX_ORPHAN_RECONCILE_ATTEMPTS) return;

  const generation = currentGeneration(ts);
  orphanObserver = new MutationObserver(() => {
    clearTimeout(orphanDebounceTimer);
    orphanDebounceTimer = setTimeout(() => {
      // A navigation (or a newer adInit) has superseded the state this watch
      // was armed for — recovering against it would apply the old route.
      if (currentGeneration(ts) !== generation) {
        stopOrphanWatch();
        return;
      }
      const orphans = orphanedTsSlots(ts);
      if (orphans.length === 0) return;
      orphanReconcileAttempts += 1;
      log.warn(
        `[tsjs-gpt] ${orphans.length} TS slot(s) orphaned by a DOM re-render; re-binding`,
        orphans.map((slot) => slot.getSlotElementId?.())
      );
      // Stop before re-running: adInit() re-arms the watch itself.
      stopOrphanWatch();
      ts.adInit?.();
    }, ORPHAN_RECONCILE_DEBOUNCE_MS);
  });
  orphanObserver.observe(document.documentElement, { childList: true, subtree: true });
  orphanWindowTimer = setTimeout(stopOrphanWatch, ORPHAN_RECONCILE_WINDOW_MS);
}

export function installTsAdInit(): void {
  const ts = (window.tsjs ??= {} as TsjsApi);
  installInitialLoadDetector(ts);
  ts.adInit = function () {
    const slots = ts.adSlots ?? [];
    // Snapshot bids at adInit() call time — correct for targeting setup.
    // The slotRenderEnded listener below reads ts.bids live so SPA navigation
    // updates (new ts.bids injected before </body>) are picked up at render time.
    const bids = ts.bids ?? {};
    const g = (window as GptWindow).googletag;
    if (!g) return;

    g.cmd?.push(() => {
      // A new ad-init opens a new generation: everything armed by the previous
      // one (SSAT attribution, orphan watch, in-flight bridge fetches) belongs
      // to state this call is about to replace.
      const generation = bumpGeneration(ts);
      // Destroy previously defined TS slots before redefining for the new page.
      if (ts.prevGptSlots && ts.prevGptSlots.length > 0) {
        g.destroySlots?.(ts.prevGptSlots as GoogleTagSlot[]);
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
        // Arm SSAT attribution for the next render of this slot only. Without
        // this, every later publisher refresh would still read the page-load
        // bid out of ts.bids and claim the (long finished) server-side auction
        // rendered it.
        //
        // The tuple is snapshotted here rather than re-read from `ts.bids` when
        // the render event arrives: by then a newer navigation may have
        // replaced `bids`, and stamping this render with that route's auction
        // would both mislabel it and leave the real render of that auction
        // demoted to `gam-refresh`.
        const armed = TS_BID_TARGETING_KEYS.some((key) => Boolean(bid[key]));
        enqueueGptRender(gptSlot, {
          generation,
          slotId: slot.id,
          // Snapshot request data instead of reading a newer route's `ts.bids`
          // when this request's render event eventually arrives.
          bid: { ...bid },
          attributed: armed,
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
          const pending = takeGptRender(event.slot);
          if (pending && pending.generation !== currentGeneration(ts)) {
            log.debug('[tsjs-gpt] ignoring slotRenderEnded from a superseded route', { divId });
            return;
          }

          const slotId = pending?.slotId ?? (ts.divToSlotId ?? {})[divId];
          if (!slotId) return;
          const bid = pending?.bid ?? (ts.bids ?? {})[slotId] ?? {};

          // Trace: registry entry + DOM markers joining the GAM render to the
          // server-side auction bid. `rendered` is GAM's own non-empty signal;
          // `injected`/`visible` carry the honest "is the TS creative actually
          // on screen" state so the panel does not overclaim (a non-empty GAM
          // slot is not proof the TS creative rendered).
          const slotEl = document.getElementById(divId);
          const eventGeneration = currentGeneration(ts);

          // The server-side auction runs once per navigation. Only the render
          // that consumes the targeting adInit just applied may be attributed
          // to it; a later publisher refresh re-requests GAM on its own and the
          // creative it returns has no traceable link to any TS auction, so no
          // bid tuple is stamped. Single-shot: the armed tuple is consumed here.
          const attributed = pending?.attributed
            ? {
                path: 'ssat' as const,
                auctionId: bid.hb_auction_id,
                bidder: bid.hb_bidder,
                adId: bid.hb_adid,
                bidId: bid.hb_bid_id,
                creativeId: bid.hb_crid,
                admHash: bid.hb_adm_hash,
              }
            : { path: 'gam-refresh' as const };

          // The record this event belongs to, resolved below. Captured by the
          // deferred-placement callback, which fires an animation frame later.
          let record: RenderRecord | undefined;

          // GAM interceptor (testing): when adm is present, replace the GAM creative.
          // Adapted from PR #241 — uses window.tsjs.bids[slotId].adm instead of pbjs.
          // Only active when inject_adm_for_testing injects adm into bids server-side.
          // Run before recording so the trace reflects the post-injection state.
          // No adm to inject means TS only applied GAM targeting: whatever GAM
          // rendered lives in a cross-origin iframe TS cannot read, so this is
          // explicitly *not* a confirmed TS placement (status: gam-only).
          const injected = bid.adm
            ? injectAdmIntoSlot(
                divId,
                bid.adm,
                (placed) => {
                  // The animation-frame retry has resolved. Promote the record
                  // unless a newer render already replaced this impression.
                  if (!placed || !record || ts.renders?.[slotId] !== record || !slotEl) return;
                  updateRender(record, { injected: true, visible: isEffectivelyVisible(slotEl) });
                  stampCreativeTrace(slotEl, record);
                },
                () =>
                  currentGeneration(ts) === eventGeneration &&
                  !!slotEl?.isConnected &&
                  document.getElementById(divId) === slotEl
              )
            : false;

          const gamSignal = {
            rendered: !event.isEmpty,
            gamEmpty: event.isEmpty,
            injected,
            visible: isEffectivelyVisible(slotEl),
            elementId: divId,
          };
          // The bridge may already have opened this impression's record (it
          // serves the creative the same GAM request asked for). Enrich that
          // record rather than appending a second one for the same impression.
          const open = event.isEmpty
            ? undefined
            : enrichableImpression(ts, slotId, bid.hb_adid, 'gam');
          if (open) {
            open.gamSeen = true;
            record = updateRender(open.record, gamSignal);
          } else {
            record = recordRender({ slotId, servedFrom: 'gam', ...gamSignal, ...attributed });
            openImpression(ts, slotId, bid.hb_adid, record, 'gam');
          }
          if (slotEl) stampCreativeTrace(slotEl, record);
        });
      }

      // Register and render TS-defined slots. GPT requires display() for a
      // freshly-defined slot — without it the slot no-ops ("defineSlot was
      // called without a matching display call") and misses its impression.
      // Must run after enableServices(); on SPA navigation services are already
      // enabled, so this runs unconditionally for any newly-defined slots.
      slotsToDisplay.forEach((divId) => g.display?.(divId));

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
          g.pubads!().refresh(slotsNeedingRefresh);
        } finally {
          ts.adInitRefreshInProgress = false;
        }
      }

      // Only TS-defined slots can be orphaned by a re-render — publisher-owned
      // slots are theirs to manage, and TS never destroys them.
      if (newSlots.length > 0) {
        watchForOrphanedSlots(ts);
      }
    });
  };
}

interface PageBidsResponse {
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

  async function onNavigate(path: string): Promise<void> {
    if (path === currentPath) return;
    currentPath = path;
    inflight?.abort();
    // Retire the route being left before awaiting anything. The new DOM can
    // commit long before page-bids answers, and until then every watcher and
    // in-flight render still refers to the previous route's auction.
    beginNavigation(ts);
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

/**
 * Install the TS → pbRender bridge.
 *
 * Must be installed synchronously at module init — before `adInit()` fires
 * `refresh()`, which triggers GAM to serve the Prebid creative. Installing
 * post-load would miss first-impression `"Prebid Request"` messages.
 *
 * When `adId` matches a TS server-side bid in `window.tsjs.bids` AND the bid
 * has renderable markup, the bridge:
 *   1. Uses debug `adm` directly when present, otherwise fetches from PBS Cache.
 *   2. Replies via the MessageChannel port with a `"Prebid Response"`.
 *   3. Calls `stopImmediatePropagation()` so Prebid.js does not also process
 *      the message and log spurious failures.
 *
 * Lives in gpt/index.ts (not prebid/index.ts) to avoid pulling the full
 * Prebid bundle into tsjs-gpt.js via inlineDynamicImports.
 */
/**
 * Trace a creative served by the pbRender bridge: registry entry + DOM markers
 * on the slot element. `servedFrom` distinguishes debug adm injection from a
 * PBS Cache fetch so verification tooling knows which mechanism delivered the
 * markup into the GAM iframe.
 *
 * `el` must be resolved by the caller at message-receipt time: the PBS Cache
 * path stamps only after an async fetch, and re-resolving from live
 * `tsjs.adSlots`/DOM at that point could stamp a *new* route's slot with the
 * previous page's trace data after an SPA navigation. The connectivity check
 * below drops the stamp when the captured element has left the document.
 */
function recordBridgeRender(
  ts: TsjsApi,
  slotId: string,
  bid: AuctionBidData,
  servedFrom: RenderServedFrom,
  el: HTMLElement | null
): void {
  // The bridge serves TS's own markup into the Prebid Universal Creative, so
  // this is a confirmed TS placement (injected: true).
  const bridgeSignal = {
    rendered: true,
    injected: true,
    visible: isEffectivelyVisible(el),
    elementId: el?.id,
    servedFrom,
  };
  // `slotRenderEnded` may already have opened this impression's record for the
  // same GAM request. Enrich it — appending here would count one creative as
  // two renders, and this confirmed placement must not be a separate row from
  // the GAM fill signal describing it.
  const open = enrichableImpression(ts, slotId, bid.hb_adid, 'bridge');
  let record: RenderRecord;
  if (open) {
    open.bridgeSeen = true;
    record = updateRender(open.record, bridgeSignal);
  } else {
    record = recordRender({
      slotId,
      path: 'ssat',
      auctionId: bid.hb_auction_id,
      bidder: bid.hb_bidder,
      adId: bid.hb_adid,
      bidId: bid.hb_bid_id,
      creativeId: bid.hb_crid,
      admHash: bid.hb_adm_hash,
      ...bridgeSignal,
    });
    openImpression(ts, slotId, bid.hb_adid, record, 'bridge');
  }
  if (el && el.isConnected) stampCreativeTrace(el, record);
}

/**
 * Whether a PBS Cache result may still be applied when its fetch resolves.
 *
 * The fetch is started for one specific impression on one specific route, but
 * settles arbitrarily later. By then an SPA navigation may have rebound the
 * slot to a new auction — writing the record anyway would make a finished
 * auction the slot's current render, fire a render event for a creative nobody
 * can see, and stamp the new route's element with the old route's tuple.
 *
 * So all four anchors of that impression must still hold: the route it started
 * in, the element it resolved to, the message source still living under that
 * same slot, and the slot still showing the very bid it was fetched for.
 */
function bridgeResultStillCurrent(
  ts: TsjsApi,
  generation: number,
  slotId: string,
  bid: AuctionBidData,
  el: HTMLElement | null,
  source: MessageEventSource | null
): boolean {
  if (currentGeneration(ts) !== generation) return false;
  if (!el?.isConnected) return false;
  if (slotIdForMessageSource(source) !== slotId) return false;
  const live = (ts.bids ?? {})[slotId];
  if (!live) return false;
  return (
    live.hb_adid === bid.hb_adid &&
    live.hb_auction_id === bid.hb_auction_id &&
    live.hb_bid_id === bid.hb_bid_id &&
    live.hb_bidder === bid.hb_bidder &&
    live.hb_crid === bid.hb_crid &&
    live.hb_adm_hash === bid.hb_adm_hash &&
    live.hb_cache_host === bid.hb_cache_host &&
    live.hb_cache_path === bid.hb_cache_path &&
    live.nurl === bid.nurl &&
    live.burl === bid.burl
  );
}

export function installTsRenderBridge(): void {
  if (typeof window === 'undefined') return;

  // adIds whose PBS Cache render is in flight. `fireWinBillingBeacons` only
  // dedups after the async cache fetch resolves, so two Prebid Request messages
  // for the same adId arriving before the first fetch settles would both fetch
  // and both fire the nurl/burl beacons. Tracking in-flight adIds prevents the
  // concurrent double-fire; the entry is cleared once the fetch settles.
  const renderingAdIds = new Set<string>();

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

    // Build reverse map adId → slotId from live window.tsjs.bids.
    const bids = window.tsjs?.bids ?? {};
    let slotId: string | undefined;
    let matchedBid: (typeof bids)[string] | undefined;
    for (const [sid, bid] of Object.entries(bids)) {
      if (bid.hb_adid === adId) {
        slotId = sid;
        matchedBid = bid;
        break;
      }
    }

    // Not a TS bid — let Prebid.js handle it.
    if (!slotId || !matchedBid) return;

    // The requesting iframe's slot must own the resolved adId. Without this an
    // iframe under slot A could request slot B's hb_adid and receive slot B's
    // creative/dimensions while firing slot B's win/billing beacons.
    if (slotId !== sourceSlotId) return;

    const ts = (window.tsjs ??= {} as TsjsApi);
    // Snapshot every field the asynchronous cache completion will consume.
    const capturedBid: AuctionBidData = { ...matchedBid };
    const slot = ts.adSlots?.find((s) => s.id === slotId);
    const [width, height] = slot?.formats?.[0] ?? [728, 90];
    // Resolve the slot element now, at message-receipt time: the PBS Cache
    // branch stamps after an async fetch, and by then an SPA navigation may
    // have swapped tsjs.adSlots/DOM for a new route with the same slot IDs.
    const slotEl = slot ? findSlotElementByDivId(slot.div_id) : null;
    // The route this render belongs to, re-checked when the async fetch below
    // resolves.
    const generation = currentGeneration(ts);

    if (capturedBid.adm) {
      e.stopImmediatePropagation();
      port.postMessage(
        JSON.stringify({
          message: 'Prebid Response',
          adId,
          ad: capturedBid.adm,
          renderer: TS_DISPLAY_RENDERER,
          width,
          height,
        })
      );
      fireWinBillingBeacons(slotId, capturedBid);
      recordBridgeRender(ts, slotId, capturedBid, 'debug-adm', slotEl);
      log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from debug adm`);
      return;
    }

    // No TS render source — let Prebid.js handle it.
    if (!capturedBid.hb_cache_host || !capturedBid.hb_cache_path) return;

    // TS owns this adId — stop Prebid from also processing it.
    e.stopImmediatePropagation();

    // Skip a concurrent re-render of the same adId so its win/billing beacons
    // fire at most once even before the first cache fetch resolves.
    if (renderingAdIds.has(adId)) return;
    renderingAdIds.add(adId);

    const cacheUrl = `https://${capturedBid.hb_cache_host}${capturedBid.hb_cache_path}?uuid=${encodeURIComponent(adId)}`;

    // Abortable so a navigation can cancel a render belonging to the route it
    // is leaving instead of letting it land on the new one.
    const controller = new AbortController();
    inflightCacheFetches.add(controller);

    fetch(cacheUrl, { mode: 'cors', signal: controller.signal })
      .then((res) => (res.ok ? res.text() : Promise.reject(res.status)))
      .then((ad) => {
        if (!bridgeResultStillCurrent(ts, generation, slotId, capturedBid, slotEl, e.source)) {
          log.debug(`[tsjs-gpt] pbRender bridge: dropping stale PBS Cache result for '${slotId}'`);
          return;
        }
        port.postMessage(
          JSON.stringify({
            message: 'Prebid Response',
            adId,
            ad,
            renderer: TS_DISPLAY_RENDERER,
            width,
            height,
          })
        );
        fireWinBillingBeacons(slotId, capturedBid);
        recordBridgeRender(ts, slotId, capturedBid, 'pbs-cache', slotEl);
        log.debug(`[tsjs-gpt] pbRender bridge served '${slotId}' from PBS Cache`);
      })
      .catch((err) => {
        if (err instanceof DOMException && err.name === 'AbortError') return;
        log.warn(`[tsjs-gpt] pbRender bridge: PBS Cache fetch failed for '${slotId}'`, err);
      })
      .finally(() => {
        inflightCacheFetches.delete(controller);
        renderingAdIds.delete(adId);
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
